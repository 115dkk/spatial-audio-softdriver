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
    /// Recoveries after discarded bytes or an unexpected burst position,
    /// including initial acquisition after leading garbage. Counted once the
    /// recovered burst's complete header has been accepted.
    pub resyncs: u64,
    /// Bytes discarded while searching for a preamble. Excludes zero padding
    /// before the expected next burst; an undecided sync prefix is not counted.
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
    payload: Box<[u8]>,
    state: State,
    header: [u8; 4],
    matched: usize,
    low_byte: u8,
    pc: u16,
    start: u64,
    offset: u64,
    expected: Option<u64>,
    recovering: bool,
    stats: DeframerStats,
}

#[derive(Clone, Copy, Debug)]
enum State {
    Search,
    Header { received: usize },
    Payload { length: usize, received: usize },
}

const SYNC: [u8; 4] = [0x72, 0xf8, 0x1f, 0x4e];
const HEADER_BYTES: usize = 8;

impl Deframer {
    /// Allocates every buffer the deframer will ever need.
    pub fn new() -> Self {
        Self {
            payload: vec![0; MAX_PAYLOAD].into_boxed_slice(),
            state: State::Search,
            header: [0; 4],
            matched: 0,
            low_byte: 0,
            pc: 0,
            start: 0,
            offset: 0,
            expected: Some(0),
            recovering: false,
            stats: DeframerStats::default(),
        }
    }

    /// Feeds bytes; calls `out` for every complete burst. Never allocates
    /// internally; the callback must also avoid allocating on a real-time thread.
    ///
    /// Pd = 0 bursts, including null and pause, are delivered with an empty
    /// payload. Unknown types are preserved and use bit-count Pd, except the
    /// known byte-count types E-AC-3, MAT and DTS-HD. Non-byte-aligned bit counts
    /// retain the final byte, without changing its unused bits.
    ///
    /// Searches every byte position, even during padding, so an early preamble
    /// or an odd number of lost bytes cannot hide the following burst. Payload
    /// bytes are never searched. An over-limit payload is dropped and searching
    /// resumes after its header. Zero padding within a known repetition period
    /// is not a sync loss. Null, pause and types with no known period allow zero
    /// padding up to the next preamble.
    pub fn push(&mut self, bytes: &[u8], out: &mut dyn FnMut(Burst<'_>)) {
        for &byte in bytes {
            let position = self.offset;
            self.offset += 1;
            match self.state {
                State::Search => self.search(byte, position),
                State::Header { received } => {
                    self.header[received] = byte;
                    if received + 1 == self.header.len() {
                        self.pc = u16::from_le_bytes([self.header[0], self.header[1]]);
                        let pd = u16::from_le_bytes([self.header[2], self.header[3]]);
                        let length = if byte_count(DataType::from_pc(self.pc)) {
                            usize::from(pd)
                        } else {
                            usize::from(pd).div_ceil(8)
                        };
                        self.begin_payload(length, out);
                    } else {
                        self.state = State::Header {
                            received: received + 1,
                        };
                    }
                }
                State::Payload { length, received } => {
                    if received.is_multiple_of(2) {
                        self.low_byte = byte;
                    } else {
                        self.payload[received - 1] = byte;
                        if received < length {
                            self.payload[received] = self.low_byte;
                        }
                    }
                    let received = received + 1;
                    if received == length.next_multiple_of(2) {
                        self.finish(length, out);
                    } else {
                        self.state = State::Payload { length, received };
                    }
                }
            }
        }
    }

    /// The burst being assembled, if any: its type and the payload bytes
    /// received so far (codec byte order). Lets a decoder start on the first
    /// half of a MAT burst before the second arrives.
    ///
    /// Returns `None` until Pc and Pd are complete, and after delivery. An
    /// unmatched low byte of a payload word is withheld until its high byte
    /// arrives, so the returned slice is always a contiguous codec-order prefix.
    pub fn partial(&self) -> Option<(DataType, &[u8])> {
        match self.state {
            State::Payload { length, received } => Some((
                DataType::from_pc(self.pc),
                &self.payload[..(received & !1).min(length)],
            )),
            _ => None,
        }
    }

    /// Forgets all state, offsets and counters (after an overrun or a format
    /// change). Keeps the allocated buffer for reuse.
    pub fn reset(&mut self) {
        self.state = State::Search;
        self.header = [0; 4];
        self.matched = 0;
        self.low_byte = 0;
        self.pc = 0;
        self.start = 0;
        self.offset = 0;
        self.expected = Some(0);
        self.recovering = false;
        self.stats = DeframerStats::default();
    }

    pub fn stats(&self) -> DeframerStats {
        self.stats
    }

    fn search(&mut self, byte: u8, position: u64) {
        if byte == SYNC[self.matched] {
            self.matched += 1;
            if self.matched == SYNC.len() {
                self.start = position + 1 - SYNC.len() as u64;
                self.matched = 0;
                self.state = State::Header { received: 0 };
            }
            return;
        }

        // No proper suffix of this sync word is also a prefix. On a mismatch,
        // only this newest byte can start another candidate (72 72 F8 1F 4E).
        if self.matched != 0 {
            self.stats.skipped_bytes += self.matched as u64;
            self.recovering = true;
            self.matched = 0;
        }
        if byte == SYNC[0] {
            self.matched = 1;
        } else if byte != 0 || self.expected.is_some_and(|expected| position >= expected) {
            self.stats.skipped_bytes += 1;
            self.recovering = true;
        }
    }

    fn begin_payload(&mut self, length: usize, out: &mut dyn FnMut(Burst<'_>)) {
        if length > MAX_PAYLOAD {
            // Currently a 16-bit Pd cannot exceed MAX_PAYLOAD even in bytes;
            // keep the bound explicit if new length encodings are added later.
            self.stats.skipped_bytes += HEADER_BYTES as u64;
            self.recovering = true;
            self.expected = None;
            self.state = State::Search;
            return;
        }
        if self.recovering || self.expected.is_some_and(|expected| self.start != expected) {
            self.stats.resyncs += 1;
        }
        self.recovering = false;
        self.state = State::Payload {
            length,
            received: 0,
        };
        if length == 0 {
            self.finish(0, out);
        }
    }

    fn finish(&mut self, length: usize, out: &mut dyn FnMut(Burst<'_>)) {
        self.expected = period_for_pc(self.pc).map(|period| self.start + period as u64);
        self.state = State::Search;
        self.stats.bursts += 1;
        out(Burst {
            data_type: DataType::from_pc(self.pc),
            pc: self.pc,
            payload: &self.payload[..length],
            start: self.start,
        });
    }
}

fn byte_count(data_type: DataType) -> bool {
    matches!(data_type, DataType::Eac3 | DataType::Mat | DataType::DtsHd)
}

fn period_for_pc(pc: u16) -> Option<usize> {
    match pc & 0x7f {
        11 => Some(2048),
        12 => Some(4096),
        13 => Some(8192),
        17 => match (pc >> 8) & 0x1f {
            subtype @ 0..=5 => Some(2048 << subtype),
            _ => None,
        },
        _ => repetition_period(DataType::from_pc(pc)),
    }
}

impl Default for Deframer {
    fn default() -> Self {
        Self::new()
    }
}

/// Wraps `payload` (codec byte order) into one IEC 61937 burst padded with
/// zeros to `period_bytes`, appending it to `out`. Builds fixtures and test
/// streams; the inverse of [`Deframer`]. This wraps bytes only: it does not
/// create codec headers, a MAT frame or a DTS-HD envelope.
///
/// Pd counts bytes for E-AC-3, MAT and DTS-HD, and bits for other types. An odd
/// payload uses the high byte of its last word; its low byte is zero and is
/// not included in Pd. DTS I/II/III is selected by a period of 2,048/4,096/8,192
/// bytes. DTS-HD's period selects its Pc subtype (2,048 through 65,536 bytes,
/// powers of two). Other Pc information bits are zero.
///
/// # Panics
/// When the payload does not fit in the period after the 8-byte preamble,
/// including word alignment; when the period is odd; when the payload length
/// cannot be represented by the type's 16-bit Pd; when `Other` exceeds the
/// 7-bit type field; or when DTS/DTS-HD is given an unsupported period.
pub fn frame_burst(data_type: DataType, payload: &[u8], period_bytes: usize, out: &mut Vec<u8>) {
    let pc: u16 = match data_type {
        DataType::Null => 0,
        DataType::Ac3 => 1,
        DataType::Pause => 3,
        DataType::Eac3 => 21,
        DataType::Mat => 22,
        DataType::Dts => match period_bytes {
            2048 => 11,
            4096 => 12,
            8192 => 13,
            _ => panic!("DTS I/II/III needs a 2048, 4096 or 8192 byte period"),
        },
        DataType::DtsHd => {
            assert!(
                period_bytes.is_power_of_two() && (2048..=65_536).contains(&period_bytes),
                "DTS-HD needs a power-of-two period from 2048 through 65536 bytes"
            );
            17 | ((period_bytes / 2048).ilog2() as u16) << 8
        }
        DataType::Other(value) => {
            assert!(value <= 0x7f, "data type must fit in seven bits");
            u16::from(value)
        }
    };
    let bytes_per_pd = byte_count(DataType::from_pc(pc));
    let maximum = if bytes_per_pd {
        u16::MAX as usize
    } else {
        u16::MAX as usize / 8
    };
    assert!(payload.len() <= maximum, "payload length must fit in Pd");
    let pd = if bytes_per_pd {
        payload.len() as u16
    } else {
        (payload.len() * 8) as u16
    };
    let occupied = HEADER_BYTES + payload.len().next_multiple_of(2);
    assert!(
        period_bytes.is_multiple_of(2),
        "period must contain whole words"
    );
    assert!(occupied <= period_bytes, "payload does not fit in period");
    let end = out
        .len()
        .checked_add(period_bytes)
        .expect("stream too large");
    out.reserve(period_bytes);
    out.extend_from_slice(&SYNC);
    out.extend_from_slice(&pc.to_le_bytes());
    out.extend_from_slice(&pd.to_le_bytes());
    for word in payload.chunks(2) {
        out.push(word.get(1).copied().unwrap_or(0));
        out.push(word[0]);
    }
    out.resize(end, 0);
}

/// Repetition period in bytes of one burst of `data_type` in its usual
/// container: AC-3 6,144; E-AC-3 24,576; MAT 61,440.
///
/// Returns `None` for null, pause, unknown types and DTS/DTS-HD: those require
/// a subtype or information not retained by `DataType`. The deframer itself
/// uses the full Pc to distinguish the three DTS periods and DTS-HD subtypes.
pub fn repetition_period(data_type: DataType) -> Option<usize> {
    match data_type {
        DataType::Ac3 => Some(6144),
        DataType::Eac3 => Some(24_576),
        DataType::Mat => Some(61_440),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
