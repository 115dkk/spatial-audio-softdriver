#![forbid(unsafe_code)]

//! IEC 61937 deframing: word order, preamble sync and burst extraction.
//!
//! The virtual device receives IEC 61937 as 16-bit little-endian PCM words
//! (2 channels for AC-3 and DD+, 8 channels "HBR" for MAT and TrueHD). The
//! channel layout does not matter here: the burst stream is the sequence of
//! 16-bit words in memory order. Every burst starts with the preamble
//! Pa = 0xF872, Pb = 0x4E1F (bytes `72 F8 1F 4E`), followed by Pc (data type
//! and type-dependent bits) and Pd (payload length, in bits or bytes depending
//! on the data type). The payload's 16-bit words carry codec bytes most
//! significant first, so every pair is swapped back into codec byte order.
//!
//! PCM never comes through here; the engine routes it straight to `decode`.

/// IEC 61937 data type, Pc bits 0..=6.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DataType {
    Null,
    /// 1: AC-3.
    Ac3,
    /// 3: pause burst (no payload worth decoding).
    Pause,
    /// 21: E-AC-3 (DD+), payload of one or more syncframes.
    Eac3,
    /// 22: MAT, carrying TrueHD (MLP) or MAT PCM.
    Mat,
    /// 11, 12, 13: DTS type I, II, III.
    Dts,
    /// 17: DTS type IV (DTS-HD).
    DtsHd,
    Other(u8),
}

impl DataType {
    pub fn from_pc(pc: u16) -> Self {
        match pc & 0x7f {
            0 => Self::Null,
            1 => Self::Ac3,
            3 => Self::Pause,
            21 => Self::Eac3,
            22 => Self::Mat,
            11..=13 => Self::Dts,
            17 => Self::DtsHd,
            other => Self::Other(other as u8),
        }
    }
}

/// One burst, borrowed from the deframer's buffer until the callback returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Burst<'a> {
    pub data_type: DataType,
    /// Pc as received (data type plus the type-dependent bits).
    pub pc: u16,
    /// Payload in codec byte order (the 16-bit swap already undone), exactly
    /// the length Pd announces.
    pub payload: &'a [u8],
    /// Byte offset of this burst's Pa in the stream pushed since the last
    /// [`Deframer::reset`], so the caller can map it to a record's QPC.
    pub start: u64,
}

/// Counters for diagnostics and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeframerStats {
    pub bursts: u64,
    /// Times sync was lost and found again.
    pub resyncs: u64,
    /// Bytes discarded while searching for a preamble.
    pub skipped_bytes: u64,
}

/// Largest burst payload any supported type carries (MAT: under 61,440 bytes,
/// its repetition period); the internal buffer is allocated once.
pub const MAX_PAYLOAD: usize = 65_536;

/// Stateful deframer. Bytes may arrive in chunks of any size, cut anywhere,
/// including inside a 16-bit word, a preamble or a payload; the bursts that
/// come out do not depend on where the input was cut.
#[derive(Debug)]
pub struct Deframer {
    // Implementation detail: owned buffers sized at construction.
}

impl Deframer {
    /// Allocates every buffer the deframer will ever need.
    pub fn new() -> Self {
        todo!("M1: bitstream worker")
    }

    /// Feeds bytes; calls `out` for every complete burst. Never allocates.
    pub fn push(&mut self, bytes: &[u8], out: &mut dyn FnMut(Burst<'_>)) {
        let _ = (bytes, out);
        todo!("M1: bitstream worker")
    }

    /// The burst being assembled, if any: its type and the payload bytes
    /// received so far (codec byte order). Lets a decoder start on the first
    /// half of a MAT burst before the second arrives.
    pub fn partial(&self) -> Option<(DataType, &[u8])> {
        todo!("M1: bitstream worker")
    }

    /// Forgets all state (after an overrun or a format change).
    pub fn reset(&mut self) {
        todo!("M1: bitstream worker")
    }

    pub fn stats(&self) -> DeframerStats {
        todo!("M1: bitstream worker")
    }
}

impl Default for Deframer {
    fn default() -> Self {
        Self::new()
    }
}

/// Wraps `payload` (codec byte order) into one IEC 61937 burst padded with
/// zeros to `period_bytes`, appending it to `out`. Builds fixtures and test
/// streams; the inverse of [`Deframer`].
///
/// # Panics
/// When the payload does not fit in the period after the 8-byte preamble.
pub fn frame_burst(data_type: DataType, payload: &[u8], period_bytes: usize, out: &mut Vec<u8>) {
    let _ = (data_type, payload, period_bytes, out);
    todo!("M1: bitstream worker")
}

/// Repetition period in bytes of one burst of `data_type` in its usual
/// container: AC-3 6,144; E-AC-3 24,576; MAT 61,440.
pub fn repetition_period(data_type: DataType) -> Option<usize> {
    let _ = data_type;
    todo!("M1: bitstream worker")
}
