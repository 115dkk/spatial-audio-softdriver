//! The SpatialAudioLab (SAL) capture ring as its driver exposes it, and the
//! translation of its READ results into the `ssr_abi` record stream, so the
//! engine sees SAL exactly as it sees `spatialsoftreceiver.sys`.
//!
//! Interface facts (from the build-log author's own `apple_bridge.py` and
//! `ring_dump.py`, which use the driver as a black box; ADR 0005, ADR 0008):
//! * device `\\.\DolbyDecoderMat`, opened with GENERIC_READ | GENERIC_WRITE,
//!   no sharing, OPEN_EXISTING; administrator rights are required;
//! * READ = `CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED,
//!   FILE_READ_ACCESS)`, no input, returns at once (an empty ring gives a
//!   header with no payload; the scripts poll every 2–3 ms);
//! * RESET = `CTL_CODE(FILE_DEVICE_UNKNOWN, 0x802, METHOD_BUFFERED,
//!   FILE_WRITE_ACCESS)`, no input or output;
//! * a READ result is an 80-byte little-endian header, `<4I3Q16sQ2I4H>`
//!   (version, header bytes, payload bytes, reserved, sequence, total bytes,
//!   dropped bytes, KS subformat GUID, format-change count, sample rate,
//!   channel mask, channels, bits per sample, valid bits, block align),
//!   followed at `header_bytes` by `payload_bytes` of the raw stream the audio
//!   engine wrote (IEC 61937 or PCM). Observed: version 3; DD+ arrives as
//!   subformat {0000000a-0cea-...}, 192 kHz, 2 ch, 16 bit, block align 4.

use ssr_abi::{MAX_DATA_PAYLOAD, RecordWriter, record_size};
use ssr_types::SourceFormat;

pub const SAL_DEVICE_PATH: &str = r"\\.\DolbyDecoderMat";
pub const SAL_IOCTL_READ: u32 = (0x22 << 16) | (1 << 14) | (0x800 << 2);
pub const SAL_IOCTL_RESET: u32 = (0x22 << 16) | (2 << 14) | (0x802 << 2);
pub const SAL_HEADER_LEN: usize = 80;
/// SAL has no stop indication. Five missing 20 ms game blocks is long enough
/// to distinguish a stopped stream from normal scheduling jitter while keeping
/// restart latency bounded. Elapsed time is measured only between READ QPCs.
pub const SAL_STOP_IDLE_MS: u64 = 100;

/// One parsed READ header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SalHeader {
    pub version: u32,
    pub header_bytes: u32,
    pub payload_bytes: u32,
    pub sequence: u64,
    pub total_bytes: u64,
    /// Bytes the driver dropped since it started (cumulative).
    pub dropped_bytes: u64,
    /// KSDATAFORMAT subtype, GUID bytes as stored (little-endian fields).
    pub subformat: [u8; 16],
    pub format_changes: u64,
    pub rate: u32,
    pub channel_mask: u32,
    pub channels: u16,
    pub bits: u16,
    pub valid_bits: u16,
    pub block_align: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SalError {
    #[error("a SAL READ result is {have} bytes; the header alone needs {need}")]
    Truncated { need: usize, have: usize },
    #[error("the SAL header says the payload lies outside the result")]
    BadLengths,
    #[error("the SAL format is not one this app reads: {0}")]
    Format(#[from] ssr_abi::WaveFormatError),
    #[error("the record buffer is too small for this READ result")]
    BufferFull,
}

/// Parses the header of one READ result.
pub fn parse_header(buf: &[u8]) -> Result<SalHeader, SalError> {
    if buf.len() < SAL_HEADER_LEN {
        return Err(SalError::Truncated {
            need: SAL_HEADER_LEN,
            have: buf.len(),
        });
    }
    let u16_at = |i| u16::from_le_bytes(buf[i..i + 2].try_into().expect("checked header"));
    let u32_at = |i| u32::from_le_bytes(buf[i..i + 4].try_into().expect("checked header"));
    let u64_at = |i| u64::from_le_bytes(buf[i..i + 8].try_into().expect("checked header"));
    let header_bytes = u32_at(4);
    let payload_bytes = u32_at(8);
    if header_bytes < SAL_HEADER_LEN as u32
        || u64::from(header_bytes) + u64::from(payload_bytes) > buf.len() as u64
    {
        return Err(SalError::BadLengths);
    }
    Ok(SalHeader {
        version: u32_at(0),
        header_bytes,
        payload_bytes,
        sequence: u64_at(16),
        total_bytes: u64_at(24),
        dropped_bytes: u64_at(32),
        subformat: buf[40..56].try_into().expect("checked header"),
        format_changes: u64_at(56),
        rate: u32_at(64),
        channel_mask: u32_at(68),
        channels: u16_at(72),
        bits: u16_at(74),
        valid_bits: u16_at(76),
        block_align: u16_at(78),
    })
}

impl SalHeader {
    /// The WAVEFORMATEXTENSIBLE this header describes (allocates; called only
    /// when the format changes).
    pub fn wave_format(&self) -> Vec<u8> {
        let (bytes, len) = self.wave_bytes();
        bytes[..len].to_vec()
    }

    pub fn source_format(&self) -> Result<SourceFormat, SalError> {
        self.validate_byte_rate()?;
        let (bytes, len) = self.wave_bytes();
        Ok(ssr_abi::parse_wave_format(&bytes[..len])?)
    }

    fn validate_byte_rate(&self) -> Result<(), SalError> {
        if self.block_align == 0 || self.rate.checked_mul(u32::from(self.block_align)).is_none() {
            return Err(
                ssr_abi::WaveFormatError::Inconsistent("byte rate or block alignment").into(),
            );
        }
        Ok(())
    }

    fn wave_bytes(&self) -> ([u8; 52], usize) {
        let mut b = [0; 52];
        let iec = self.subformat[4..6] == [0xea, 0x0c];
        b[0..2].copy_from_slice(&0xfffe_u16.to_le_bytes());
        b[2..4].copy_from_slice(&self.channels.to_le_bytes());
        b[4..8].copy_from_slice(&self.rate.to_le_bytes());
        // The public conversion is infallible; source_format rejects overflow.
        b[8..12].copy_from_slice(
            &self
                .rate
                .wrapping_mul(u32::from(self.block_align))
                .to_le_bytes(),
        );
        b[12..14].copy_from_slice(&self.block_align.to_le_bytes());
        b[14..16].copy_from_slice(&self.bits.to_le_bytes());
        b[16..18].copy_from_slice(&22_u16.to_le_bytes());
        b[18..20].copy_from_slice(&self.valid_bits.to_le_bytes());
        b[20..24].copy_from_slice(&self.channel_mask.to_le_bytes());
        b[24..40].copy_from_slice(&self.subformat);
        (b, if iec { 52 } else { 40 })
    }

    fn same_format(&self, other: &Self) -> bool {
        self.subformat == other.subformat
            && self.rate == other.rate
            && self.channels == other.channels
            && self.bits == other.bits
            && self.channel_mask == other.channel_mask
            && self.format_changes == other.format_changes
            && self.valid_bits == other.valid_bits
            && self.block_align == other.block_align
    }
}

/// Turns successive READ results into the record stream consumed by the engine.
#[derive(Debug)]
pub struct SalTranslator {
    epoch: u32,
    previous: Option<SalHeader>,
    position: u64,
    dropped: u64,
    active: bool,
    last_payload_qpc: Option<u64>,
    qpc_frequency: u64,
    pending: Option<PendingRead>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingStage {
    Start,
    Format,
    Overrun,
    Data,
    Done,
}

#[derive(Clone, Copy, Debug)]
struct PendingRead {
    header: SalHeader,
    qpc: u64,
    stage: PendingStage,
    payload_offset: usize,
    lost: u64,
    format_repeat: bool,
    stop: bool,
}

impl Default for SalTranslator {
    fn default() -> Self {
        Self::with_qpc_frequency(u64::MAX)
    }
}

impl SalTranslator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_qpc_frequency(qpc_frequency: u64) -> Self {
        Self {
            epoch: 0,
            previous: None,
            position: 0,
            dropped: 0,
            active: false,
            last_payload_qpc: None,
            qpc_frequency: qpc_frequency.max(1),
            pending: None,
        }
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// A newly opened SAL device can report an idle ring before its first
    /// format negotiation. Nothing has been published yet, so this READ is
    /// waited out: no invalid WAVEFORMATEXTENSIBLE, no epoch. Once anything
    /// was published, normal validation applies.
    fn idle_before_first_format(&self, h: &SalHeader) -> bool {
        self.previous.is_none()
            && h.payload_bytes == 0
            && h.format_changes == 0
            && h.total_bytes == 0
            && h.dropped_bytes == 0
    }

    /// Stages one READ; `false` when it carries nothing to translate.
    fn prepare(&mut self, read: &[u8], qpc: u64) -> Result<bool, SalError> {
        let h = parse_header(read)?;
        if self.idle_before_first_format(&h) {
            return Ok(false);
        }
        let changed = self.previous.is_none_or(|old| !old.same_format(&h));
        let restarting = !self.active && h.payload_bytes > 0;
        let stop_ticks = self.qpc_frequency.saturating_mul(SAL_STOP_IDLE_MS) / 1_000;
        let stop = self.active
            && h.payload_bytes == 0
            && self
                .last_payload_qpc
                .is_some_and(|last| qpc.saturating_sub(last) >= stop_ticks);
        if changed {
            h.source_format()?;
        }
        let stage = if stop || changed || restarting {
            PendingStage::Start
        } else if h.dropped_bytes > self.dropped {
            PendingStage::Overrun
        } else if h.payload_bytes > 0 {
            PendingStage::Data
        } else {
            PendingStage::Done
        };
        self.pending = Some(PendingRead {
            header: h,
            qpc,
            stage,
            payload_offset: 0,
            lost: h.dropped_bytes.saturating_sub(self.dropped),
            format_repeat: restarting && !changed,
            stop,
        });
        Ok(true)
    }

    fn payload_record_bytes(payload_bytes: usize) -> usize {
        let full = payload_bytes / MAX_DATA_PAYLOAD as usize;
        let tail = payload_bytes % MAX_DATA_PAYLOAD as usize;
        full * record_size(MAX_DATA_PAYLOAD as usize) + if tail > 0 { record_size(tail) } else { 0 }
    }

    fn whole_read_size(&self, read: &[u8], qpc: u64) -> Result<usize, SalError> {
        let h = parse_header(read)?;
        if self.idle_before_first_format(&h) {
            return Ok(0);
        }
        let changed = self.previous.is_none_or(|old| !old.same_format(&h));
        let restarting = !self.active && h.payload_bytes > 0;
        let stop_ticks = self.qpc_frequency.saturating_mul(SAL_STOP_IDLE_MS) / 1_000;
        let stop = self.active
            && h.payload_bytes == 0
            && self
                .last_payload_qpc
                .is_some_and(|last| qpc.saturating_sub(last) >= stop_ticks);
        let (_, wave_len) = h.wave_bytes();
        Ok(if stop {
            record_size(0)
        } else {
            (if changed || restarting {
                record_size(0) + record_size(wave_len)
            } else {
                0
            }) + if h.dropped_bytes > self.dropped {
                record_size(8)
            } else {
                0
            } + Self::payload_record_bytes(h.payload_bytes as usize)
        })
    }

    /// Translates one complete READ transactionally. This compatibility path
    /// keeps the old all-or-nothing contract; capture uses `translate_chunked`.
    pub fn translate(
        &mut self,
        read: &[u8],
        qpc: u64,
        out: &mut RecordWriter<'_>,
    ) -> Result<usize, SalError> {
        if self.pending.is_some() || out.remaining() < self.whole_read_size(read, qpc)? {
            return Err(SalError::BufferFull);
        }
        self.translate_chunked(read, qpc, out)
    }

    /// Translates as many whole records as fit. A large data payload is split
    /// into records and retained internally until later calls drain it.
    pub fn translate_chunked(
        &mut self,
        read: &[u8],
        qpc: u64,
        out: &mut RecordWriter<'_>,
    ) -> Result<usize, SalError> {
        if self.pending.is_none() && !self.prepare(read, qpc)? {
            return Ok(0);
        }
        let mut count = 0;
        loop {
            let pending = self.pending.as_mut().expect("prepared above");
            match pending.stage {
                PendingStage::Start if pending.stop => {
                    if !out.stream_stop(self.epoch, pending.qpc) {
                        break;
                    }
                    self.active = false;
                    pending.stage = PendingStage::Done;
                    count += 1;
                }
                PendingStage::Start => {
                    if !out.stream_start(
                        if self
                            .previous
                            .is_none_or(|old| !old.same_format(&pending.header))
                        {
                            self.epoch.wrapping_add(1)
                        } else {
                            self.epoch
                        },
                        pending.qpc,
                    ) {
                        break;
                    }
                    if self
                        .previous
                        .is_none_or(|old| !old.same_format(&pending.header))
                    {
                        self.epoch = self.epoch.wrapping_add(1);
                    }
                    self.position = 0;
                    self.active = true;
                    pending.stage = PendingStage::Format;
                    count += 1;
                }
                PendingStage::Format => {
                    let (wave, wave_len) = pending.header.wave_bytes();
                    let written = if pending.format_repeat {
                        out.format_repeat(self.epoch, pending.qpc, &wave[..wave_len])
                    } else {
                        out.format_change(self.epoch, pending.qpc, &wave[..wave_len])
                    };
                    if !written {
                        break;
                    }
                    pending.stage = if pending.lost > 0 {
                        PendingStage::Overrun
                    } else if pending.header.payload_bytes > 0 {
                        PendingStage::Data
                    } else {
                        PendingStage::Done
                    };
                    count += 1;
                }
                PendingStage::Overrun => {
                    if !out.overrun(self.position, pending.qpc, pending.lost) {
                        break;
                    }
                    self.position = self.position.saturating_add(pending.lost);
                    pending.stage = if pending.header.payload_bytes > 0 {
                        PendingStage::Data
                    } else {
                        PendingStage::Done
                    };
                    count += 1;
                }
                PendingStage::Data => {
                    let remaining = pending.header.payload_bytes as usize - pending.payload_offset;
                    let header_bytes = record_size(0);
                    if out.remaining() <= header_bytes {
                        break;
                    }
                    let chunk = remaining
                        .min(MAX_DATA_PAYLOAD as usize)
                        .min(out.remaining() - header_bytes);
                    if chunk == 0 {
                        break;
                    }
                    let start = pending.header.header_bytes as usize + pending.payload_offset;
                    if !out.data(
                        self.epoch,
                        self.position,
                        pending.qpc,
                        &read[start..start + chunk],
                    ) {
                        break;
                    }
                    pending.payload_offset += chunk;
                    self.position = self.position.saturating_add(chunk as u64);
                    self.last_payload_qpc = Some(pending.qpc);
                    if pending.payload_offset == pending.header.payload_bytes as usize {
                        pending.stage = PendingStage::Done;
                    }
                    count += 1;
                }
                PendingStage::Done => {
                    let finished = self.pending.take().expect("matched pending read");
                    self.previous = Some(finished.header);
                    self.dropped = finished.header.dropped_bytes;
                    return Ok(count);
                }
            }
        }
        if count == 0 && out.is_empty() {
            Err(SalError::BufferFull)
        } else {
            Ok(count)
        }
    }
}
