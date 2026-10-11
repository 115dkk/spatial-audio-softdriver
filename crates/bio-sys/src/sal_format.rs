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
    let _ = buf;
    todo!("M2: bio-sys worker")
}

impl SalHeader {
    /// The WAVEFORMATEXTENSIBLE this header describes (allocates; called only
    /// when the format changes).
    pub fn wave_format(&self) -> Vec<u8> {
        todo!("M2: bio-sys worker")
    }

    pub fn source_format(&self) -> Result<SourceFormat, SalError> {
        todo!("M2: bio-sys worker")
    }
}

/// Turns successive READ results into records: a stream-start and a
/// format-change record first and whenever the format changes (epoch + 1),
/// a data record per non-empty payload with the running byte position, and an
/// overrun record whenever `dropped_bytes` grows.
#[derive(Debug, Default)]
pub struct SalTranslator {
    // Implementation detail: epoch, last format key, byte position, last
    // dropped count, scratch for the WAVEFORMATEXTENSIBLE.
    _private: (),
}

impl SalTranslator {
    pub fn new() -> Self {
        todo!("M2: bio-sys worker")
    }

    /// Translates one READ result taken at `qpc`; returns the number of
    /// records written. Does not allocate unless the format changed.
    pub fn translate(
        &mut self,
        read: &[u8],
        qpc: u64,
        out: &mut RecordWriter<'_>,
    ) -> Result<usize, SalError> {
        let _ = (read, qpc, out);
        todo!("M2: bio-sys worker")
    }
}
