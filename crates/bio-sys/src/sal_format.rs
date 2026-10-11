//! The SpatialAudioLab (SAL) capture ring as its driver exposes it, and the
//! translation of its READ results into the `bio_abi` record stream, so the
//! engine sees SAL exactly as it will see `bioatmos.sys`.
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

use bio_abi::RecordWriter;
use bio_types::SourceFormat;

pub const SAL_DEVICE_PATH: &str = r"\\.\DolbyDecoderMat";
pub const SAL_IOCTL_READ: u32 = (0x22 << 16) | (1 << 14) | (0x800 << 2);
pub const SAL_IOCTL_RESET: u32 = (0x22 << 16) | (2 << 14) | (0x802 << 2);
pub const SAL_HEADER_LEN: usize = 80;

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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SalError {
    #[error("a SAL READ result is {have} bytes; the header alone needs {need}")]
    Truncated { need: usize, have: usize },
    #[error("the SAL header says the payload lies outside the result")]
    BadLengths,
    #[error("the SAL format is not one this app reads: {0}")]
    Format(#[from] bio_abi::WaveFormatError),
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
        Ok(bio_abi::parse_wave_format(&bytes[..len])?)
    }

    fn validate_byte_rate(&self) -> Result<(), SalError> {
        if self.block_align == 0 || self.rate.checked_mul(u32::from(self.block_align)).is_none() {
            return Err(
                bio_abi::WaveFormatError::Inconsistent("byte rate or block alignment").into(),
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

/// Turns successive READ results into records: a stream-start and a
/// format-change record first and whenever the format changes (epoch + 1),
/// a data record per non-empty payload with the running byte position, and an
/// overrun record whenever `dropped_bytes` grows.
#[derive(Debug, Default)]
pub struct SalTranslator {
    epoch: u32,
    previous: Option<SalHeader>,
    position: u64,
    dropped: u64,
}

impl SalTranslator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Translates one READ result taken at `qpc`; returns the number of
    /// records written. Does not allocate unless the format changed.
    pub fn translate(
        &mut self,
        read: &[u8],
        qpc: u64,
        out: &mut RecordWriter<'_>,
    ) -> Result<usize, SalError> {
        let h = parse_header(read)?;
        // A newly opened SAL device can report an idle ring before its first
        // format negotiation. No audio or format has been published yet; do
        // not turn that state into an invalid WAVEFORMATEXTENSIBLE or commit
        // an epoch. Once anything was published, normal validation applies.
        if self.previous.is_none()
            && h.payload_bytes == 0
            && h.format_changes == 0
            && h.total_bytes == 0
            && h.dropped_bytes == 0
        {
            return Ok(0);
        }
        let changed = self.previous.is_none_or(|old| !old.same_format(&h));
        let lost = h.dropped_bytes.saturating_sub(self.dropped);
        let (wave, wave_len) = h.wave_bytes();
        if changed {
            h.source_format()?;
        }
        let needed = if changed {
            bio_abi::record_size(0) + bio_abi::record_size(wave_len)
        } else {
            0
        } + if lost > 0 { bio_abi::record_size(8) } else { 0 }
            + if h.payload_bytes > 0 {
                bio_abi::record_size(h.payload_bytes as usize)
            } else {
                0
            };
        // Capacity and format checks precede any output or state changes. A
        // caller can retry an entire READ without duplicating earlier records.
        if out.remaining() < needed {
            return Err(SalError::BufferFull);
        }
        let mut count = 0;
        if changed {
            self.epoch = self.epoch.wrapping_add(1);
            out.stream_start(self.epoch, qpc);
            out.format_change(self.epoch, qpc, &wave[..wave_len]);
            count += 2;
        }
        if lost > 0 {
            out.overrun(self.position, qpc, lost);
            self.position = self.position.saturating_add(lost);
            count += 1;
        }
        if h.payload_bytes > 0 {
            let start = h.header_bytes as usize;
            let end = start + h.payload_bytes as usize;
            out.data(self.epoch, self.position, qpc, &read[start..end]);
            self.position = self.position.saturating_add(u64::from(h.payload_bytes));
            count += 1;
        }
        self.previous = Some(h);
        self.dropped = h.dropped_bytes;
        Ok(count)
    }
}
