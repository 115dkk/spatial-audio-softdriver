//! The record stream: parsing what READ returns, and writing records for the
//! adapters that synthesize them (the SAL development ring, test fixtures).

use bio_types::SourceFormat;
use zerocopy::{FromBytes, IntoBytes};

use crate::wave::{WaveFormatError, parse_wave_format};
use crate::wire::{
    ENDPOINT_PRIMARY, OverrunPayload, RECORD_ALIGN, RECORD_FLAG_FORMAT_REPEAT, RECORD_KIND_DATA,
    RECORD_KIND_FORMAT_CHANGE, RECORD_KIND_OVERRUN, RECORD_KIND_STREAM_START,
    RECORD_KIND_STREAM_STOP, RecordHeader,
};

const HEADER: usize = size_of::<RecordHeader>();

/// One parsed record. Borrowed slices point into the buffer given to
/// [`records`]; nothing allocates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Record<'a> {
    Data {
        endpoint: u16,
        epoch: u32,
        flags: u16,
        pos: u64,
        qpc: u64,
        qpc_written: u64,
        bytes: &'a [u8],
    },
    FormatChange {
        endpoint: u16,
        epoch: u32,
        /// [`crate::RECORD_FLAG_FORMAT_REPEAT`] when the record restates the
        /// current format after an overrun (the epoch does not change).
        flags: u16,
        qpc: u64,
        format: SourceFormat,
    },
    StreamStart {
        endpoint: u16,
        epoch: u32,
        qpc: u64,
    },
    StreamStop {
        endpoint: u16,
        epoch: u32,
        qpc: u64,
    },
    Overrun {
        endpoint: u16,
        pos: u64,
        qpc: u64,
        lost_bytes: u64,
    },
}

impl Record<'_> {
    pub fn endpoint(&self) -> u16 {
        match *self {
            Self::Data { endpoint, .. }
            | Self::FormatChange { endpoint, .. }
            | Self::StreamStart { endpoint, .. }
            | Self::StreamStop { endpoint, .. }
            | Self::Overrun { endpoint, .. } => endpoint,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AbiError {
    /// The buffer ends inside a header or payload. A READ never returns a cut
    /// record, so this means a damaged buffer or a truncated dump.
    #[error(
        "record at offset {offset} (endpoint {endpoint:?}) is cut short: needs {need} bytes, {have} remain"
    )]
    Truncated {
        offset: usize,
        endpoint: Option<u16>,
        need: usize,
        have: usize,
    },
    #[error(
        "record at offset {offset} from endpoint {endpoint} has kind {kind} with a {len}-byte payload, which that kind cannot have"
    )]
    BadLength {
        offset: usize,
        endpoint: u16,
        kind: u16,
        len: u32,
    },
    #[error("format-change record at offset {offset} from endpoint {endpoint}: {source}")]
    BadFormat {
        offset: usize,
        endpoint: u16,
        source: WaveFormatError,
    },
}

/// Iterates the records in `buf`.
///
/// Kinds this version does not know are skipped. A record whose own length is
/// trustworthy but whose content is not (a format this crate cannot parse, a
/// wrong payload size for its kind) yields an error and the iteration goes on;
/// a cut record yields an error and ends it, since nothing after it can be
/// located.
pub fn records(buf: &[u8]) -> Records<'_> {
    Records {
        buf,
        offset: 0,
        done: false,
    }
}

#[derive(Debug, Clone)]
pub struct Records<'a> {
    buf: &'a [u8],
    offset: usize,
    done: bool,
}

impl<'a> Iterator for Records<'a> {
    type Item = Result<Record<'a>, AbiError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.done || self.offset >= self.buf.len() {
                return None;
            }
            let offset = self.offset;
            let rest = &self.buf[offset..];
            let Ok((header, _)) = RecordHeader::read_from_prefix(rest) else {
                self.done = true;
                return Some(Err(AbiError::Truncated {
                    offset,
                    endpoint: None,
                    need: HEADER,
                    have: rest.len(),
                }));
            };
            let len = header.len as usize;
            let total = HEADER + len;
            if rest.len() < total {
                self.done = true;
                return Some(Err(AbiError::Truncated {
                    offset,
                    endpoint: Some(header.endpoint),
                    need: total,
                    have: rest.len(),
                }));
            }
            // The padding after the last record may be absent.
            self.offset = (offset + padded(total)).min(self.buf.len());
            let payload = &rest[HEADER..total];
            let bad_length = || {
                Some(Err(AbiError::BadLength {
                    offset,
                    endpoint: header.endpoint,
                    kind: header.kind,
                    len: header.len,
                }))
            };
            return match header.kind {
                RECORD_KIND_DATA => Some(Ok(Record::Data {
                    endpoint: header.endpoint,
                    epoch: header.epoch,
                    flags: header.flags,
                    pos: header.byte_pos,
                    qpc: header.qpc,
                    qpc_written: header.qpc_written,
                    bytes: payload,
                })),
                RECORD_KIND_FORMAT_CHANGE => Some(
                    parse_wave_format(payload)
                        .map(|format| Record::FormatChange {
                            endpoint: header.endpoint,
                            epoch: header.epoch,
                            flags: header.flags,
                            qpc: header.qpc,
                            format,
                        })
                        .map_err(|source| AbiError::BadFormat {
                            offset,
                            endpoint: header.endpoint,
                            source,
                        }),
                ),
                RECORD_KIND_STREAM_START | RECORD_KIND_STREAM_STOP if len != 0 => bad_length(),
                RECORD_KIND_STREAM_START => Some(Ok(Record::StreamStart {
                    endpoint: header.endpoint,
                    epoch: header.epoch,
                    qpc: header.qpc,
                })),
                RECORD_KIND_STREAM_STOP => Some(Ok(Record::StreamStop {
                    endpoint: header.endpoint,
                    epoch: header.epoch,
                    qpc: header.qpc,
                })),
                RECORD_KIND_OVERRUN => match OverrunPayload::read_from_bytes(payload) {
                    Ok(p) => Some(Ok(Record::Overrun {
                        endpoint: header.endpoint,
                        pos: header.byte_pos,
                        qpc: header.qpc,
                        lost_bytes: p.lost_bytes,
                    })),
                    Err(_) => bad_length(),
                },
                _ => continue,
            };
        }
    }
}

fn padded(n: usize) -> usize {
    let align = RECORD_ALIGN as usize;
    n.div_ceil(align) * align
}

/// Bytes a record with a `payload_len`-byte payload occupies, padding included.
pub fn record_size(payload_len: usize) -> usize {
    padded(HEADER + payload_len)
}

/// Writes records into a caller-provided buffer without allocating.
#[derive(Debug)]
pub struct RecordWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
    endpoint: u16,
}

impl<'a> RecordWriter<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            len: 0,
            endpoint: ENDPOINT_PRIMARY,
        }
    }

    /// Bytes written so far.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.len
    }

    pub fn set_endpoint(&mut self, endpoint: u16) {
        self.endpoint = endpoint;
    }

    pub fn endpoint(&self) -> u16 {
        self.endpoint
    }

    /// Appends one record; returns `false`, writing nothing, when it does not
    /// fit. The endpoint already present in `header` is preserved.
    pub fn push(&mut self, header: RecordHeader, payload: &[u8]) -> bool {
        let total = record_size(payload.len());
        if self.remaining() < total || payload.len() > u32::MAX as usize {
            return false;
        }
        let header = RecordHeader {
            len: payload.len() as u32,
            ..header
        };
        let dst = &mut self.buf[self.len..self.len + total];
        dst[..HEADER].copy_from_slice(header.as_bytes());
        dst[HEADER..HEADER + payload.len()].copy_from_slice(payload);
        dst[HEADER + payload.len()..].fill(0);
        self.len += total;
        true
    }

    pub fn data(&mut self, epoch: u32, pos: u64, qpc: u64, bytes: &[u8]) -> bool {
        self.push(
            header(self.endpoint, RECORD_KIND_DATA, epoch, pos, qpc),
            bytes,
        )
    }

    /// `wave_format` is the raw WAVEFORMATEX(TENSIBLE); see
    /// [`crate::wave_format_bytes`].
    pub fn format_change(&mut self, epoch: u32, qpc: u64, wave_format: &[u8]) -> bool {
        self.push(
            header(self.endpoint, RECORD_KIND_FORMAT_CHANGE, epoch, 0, qpc),
            wave_format,
        )
    }

    /// Restates the current format after an overrun, keeping `epoch`; the
    /// ring may have dropped the format change that opened it.
    pub fn format_repeat(&mut self, epoch: u32, qpc: u64, wave_format: &[u8]) -> bool {
        let mut header = header(self.endpoint, RECORD_KIND_FORMAT_CHANGE, epoch, 0, qpc);
        header.flags = RECORD_FLAG_FORMAT_REPEAT;
        self.push(header, wave_format)
    }

    pub fn stream_start(&mut self, epoch: u32, qpc: u64) -> bool {
        self.push(
            header(self.endpoint, RECORD_KIND_STREAM_START, epoch, 0, qpc),
            &[],
        )
    }

    pub fn stream_stop(&mut self, epoch: u32, qpc: u64) -> bool {
        self.push(
            header(self.endpoint, RECORD_KIND_STREAM_STOP, epoch, 0, qpc),
            &[],
        )
    }

    pub fn overrun(&mut self, pos: u64, qpc: u64, lost_bytes: u64) -> bool {
        self.push(
            header(self.endpoint, RECORD_KIND_OVERRUN, 0, pos, qpc),
            OverrunPayload { lost_bytes }.as_bytes(),
        )
    }
}

fn header(endpoint: u16, kind: u16, epoch: u32, byte_pos: u64, qpc: u64) -> RecordHeader {
    RecordHeader {
        kind,
        flags: 0,
        epoch,
        byte_pos,
        qpc,
        qpc_written: 0,
        len: 0,
        endpoint,
        reserved: 0,
    }
}

#[cfg(test)]
mod tests {
    use bio_types::{BitstreamCodec, BitstreamFormat};
    use proptest::prelude::*;

    use super::*;
    use crate::wave::wave_format_bytes;
    use crate::wire::ENDPOINT_SECOND;

    fn mat21() -> SourceFormat {
        SourceFormat::Bitstream(BitstreamFormat {
            codec: BitstreamCodec::Mat21,
            container_rate: 192_000,
            container_channels: 8,
            encoded_rate: 48_000,
            encoded_channels: 0,
        })
    }

    fn sample_stream() -> Vec<u8> {
        let mut buf = vec![0; 4096];
        let mut w = RecordWriter::new(&mut buf);
        assert!(w.stream_start(1, 10));
        assert!(w.format_change(1, 11, &wave_format_bytes(&mat21())));
        assert!(w.data(1, 0, 12, &[1, 2, 3]));
        assert!(w.overrun(3, 13, 4096));
        assert!(w.stream_stop(1, 14));
        let n = w.len();
        buf.truncate(n);
        buf
    }

    #[test]
    fn written_records_parse_back() {
        let buf = sample_stream();
        let parsed: Vec<_> = records(&buf).collect::<Result<_, _>>().unwrap();
        assert_eq!(parsed.len(), 5);
        assert!(
            parsed
                .iter()
                .all(|record| record.endpoint() == ENDPOINT_PRIMARY)
        );
        assert!(matches!(
            parsed[0],
            Record::StreamStart {
                epoch: 1,
                qpc: 10,
                ..
            }
        ));
        assert!(
            matches!(parsed[1], Record::FormatChange { epoch: 1, qpc: 11, format, .. } if format == mat21())
        );
        assert!(matches!(
            parsed[2],
            Record::Data {
                epoch: 1,
                flags: 0,
                pos: 0,
                qpc: 12,
                qpc_written: 0,
                bytes: &[1, 2, 3],
                ..
            }
        ));
        assert!(matches!(
            parsed[3],
            Record::Overrun {
                pos: 3,
                qpc: 13,
                lost_bytes: 4096,
                ..
            }
        ));
        assert!(matches!(
            parsed[4],
            Record::StreamStop {
                epoch: 1,
                qpc: 14,
                ..
            }
        ));
        assert_eq!(buf.len() % RECORD_ALIGN as usize, 0);
    }

    #[test]
    fn writer_defaults_to_primary_and_preserves_mixed_endpoint_order() {
        let mut buf = vec![0; 512];
        let len = {
            let mut writer = RecordWriter::new(&mut buf);
            assert_eq!(writer.endpoint(), ENDPOINT_PRIMARY);
            assert!(writer.stream_start(1, 1));
            writer.set_endpoint(ENDPOINT_SECOND);
            assert_eq!(writer.endpoint(), ENDPOINT_SECOND);
            assert!(writer.data(2, 0, 2, &[2]));
            writer.set_endpoint(ENDPOINT_PRIMARY);
            assert!(writer.stream_stop(1, 3));
            writer.len()
        };
        let endpoints: Vec<_> = records(&buf[..len])
            .map(|record| record.unwrap().endpoint())
            .collect();
        assert_eq!(
            endpoints,
            [ENDPOINT_PRIMARY, ENDPOINT_SECOND, ENDPOINT_PRIMARY]
        );
    }

    #[test]
    fn old_zero_reserved_bytes_are_the_primary_endpoint() {
        let mut bytes = vec![0; record_size(0)];
        let old = RecordHeader {
            kind: RECORD_KIND_STREAM_START,
            flags: 0,
            epoch: 7,
            byte_pos: 0,
            qpc: 9,
            qpc_written: 0,
            len: 0,
            endpoint: 0,
            reserved: 0,
        };
        bytes[..HEADER].copy_from_slice(old.as_bytes());
        assert!(matches!(
            records(&bytes).next(),
            Some(Ok(Record::StreamStart {
                endpoint: ENDPOINT_PRIMARY,
                ..
            }))
        ));
    }

    #[test]
    fn errors_report_the_endpoint_when_a_header_is_available() {
        let mut buf = vec![0; 512];
        let mut writer = RecordWriter::new(&mut buf);
        writer.set_endpoint(ENDPOINT_SECOND);
        assert!(writer.push(
            header(ENDPOINT_SECOND, RECORD_KIND_STREAM_STOP, 0, 0, 0),
            &[1]
        ));
        assert!(writer.push(
            header(ENDPOINT_SECOND, RECORD_KIND_FORMAT_CHANGE, 0, 0, 0),
            &[0]
        ));
        assert!(writer.data(0, 0, 0, &[1, 2, 3]));
        let len = writer.len();
        let mut parsed = records(&buf[..len]);
        assert!(matches!(
            parsed.next(),
            Some(Err(AbiError::BadLength {
                endpoint: ENDPOINT_SECOND,
                ..
            }))
        ));
        assert!(matches!(
            parsed.next(),
            Some(Err(AbiError::BadFormat {
                endpoint: ENDPOINT_SECOND,
                ..
            }))
        ));
        let last = len - record_size(3);
        let cut = &buf[..last + HEADER + 2];
        assert!(matches!(
            records(cut).last(),
            Some(Err(AbiError::Truncated {
                endpoint: Some(ENDPOINT_SECOND),
                ..
            }))
        ));
        assert!(matches!(
            records(&[0; HEADER - 1]).next(),
            Some(Err(AbiError::Truncated { endpoint: None, .. }))
        ));
    }

    #[test]
    fn unknown_kinds_are_skipped() {
        let mut buf = vec![0; 256];
        let mut w = RecordWriter::new(&mut buf);
        assert!(w.push(header(77, 77, 0, 0, 0), &[9; 13]));
        assert!(w.data(0, 0, 1, &[5]));
        let n = w.len();
        let parsed: Vec<_> = records(&buf[..n]).collect::<Result<_, _>>().unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(matches!(parsed[0], Record::Data { bytes: &[5], .. }));
    }

    #[test]
    fn a_cut_record_ends_the_iteration_with_an_error() {
        let buf = sample_stream();
        let cut = &buf[..buf.len() - 3];
        let results: Vec<_> = records(cut).collect();
        assert!(matches!(
            results.last(),
            Some(Err(AbiError::Truncated { .. }))
        ));
    }

    #[test]
    fn a_payload_on_a_stop_record_is_an_error_but_not_the_end() {
        let mut buf = vec![0; 256];
        let mut w = RecordWriter::new(&mut buf);
        assert!(w.push(
            header(ENDPOINT_PRIMARY, RECORD_KIND_STREAM_STOP, 0, 0, 0),
            &[1]
        ));
        assert!(w.stream_start(0, 2));
        let n = w.len();
        let results: Vec<_> = records(&buf[..n]).collect();
        assert!(matches!(results[0], Err(AbiError::BadLength { .. })));
        assert!(matches!(
            results[1],
            Ok(Record::StreamStart {
                endpoint: ENDPOINT_PRIMARY,
                epoch: 0,
                qpc: 2
            })
        ));
    }

    #[test]
    fn a_format_repeat_keeps_the_epoch_and_carries_its_flag() {
        let mut buf = vec![0; 512];
        let mut w = RecordWriter::new(&mut buf);
        w.set_endpoint(crate::ENDPOINT_SECOND);
        assert!(w.format_change(4, 1, &wave_format_bytes(&mat21())));
        assert!(w.format_repeat(4, 2, &wave_format_bytes(&mat21())));
        let n = w.len();
        let parsed: Vec<_> = records(&buf[..n]).collect::<Result<_, _>>().unwrap();
        assert_eq!(
            parsed,
            vec![
                Record::FormatChange {
                    endpoint: crate::ENDPOINT_SECOND,
                    epoch: 4,
                    flags: 0,
                    qpc: 1,
                    format: mat21()
                },
                Record::FormatChange {
                    endpoint: crate::ENDPOINT_SECOND,
                    epoch: 4,
                    flags: crate::RECORD_FLAG_FORMAT_REPEAT,
                    qpc: 2,
                    format: mat21()
                },
            ]
        );
    }

    #[test]
    fn the_writer_refuses_what_does_not_fit() {
        let mut buf = vec![0; 48];
        let mut w = RecordWriter::new(&mut buf);
        assert!(!w.data(0, 0, 0, &[0; 9]));
        assert!(w.data(0, 0, 0, &[0; 8]));
        assert_eq!(w.len(), 48);
    }

    proptest! {
        // The parser is the first code to touch bytes a source application
        // controls; whatever arrives, it must not panic.
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            for r in records(&bytes) {
                let _ = r;
            }
        }

        #[test]
        fn arbitrary_endpoint_kind_length_sequences_round_trip(
            specs in proptest::collection::vec((any::<u16>(), 0_u8..5, 0_usize..32), 0..32)
        ) {
            let capacity = specs
                .iter()
                .map(|(_, kind, len)| match kind {
                    0 => record_size(*len),
                    1 => record_size(wave_format_bytes(&mat21()).len()),
                    4 => record_size(size_of::<OverrunPayload>()),
                    _ => record_size(0),
                })
                .sum();
            let mut buf = vec![0; capacity];
            let len = {
                let mut writer = RecordWriter::new(&mut buf);
                for &(endpoint, kind, len) in &specs {
                    writer.set_endpoint(endpoint);
                    let ok = match kind {
                        0 => writer.data(1, 0, 0, &vec![kind; len]),
                        1 => writer.format_change(1, 0, &wave_format_bytes(&mat21())),
                        2 => writer.stream_start(1, 0),
                        3 => writer.stream_stop(1, 0),
                        4 => writer.overrun(0, 0, len as u64),
                        _ => unreachable!(),
                    };
                    assert!(ok);
                }
                writer.len()
            };
            let actual: Vec<_> = records(&buf[..len])
                .map(|record| {
                    let record = record.unwrap();
                    let kind = match record {
                        Record::Data { .. } => 0,
                        Record::FormatChange { .. } => 1,
                        Record::StreamStart { .. } => 2,
                        Record::StreamStop { .. } => 3,
                        Record::Overrun { .. } => 4,
                    };
                    (record.endpoint(), kind)
                })
                .collect();
            let expected: Vec<_> = specs.iter().map(|&(endpoint, kind, _)| (endpoint, kind)).collect();
            prop_assert_eq!(actual, expected);
        }
    }
}
