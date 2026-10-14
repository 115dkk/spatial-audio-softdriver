//! Dump files: a [`DumpHeader`] followed by the record stream exactly as READ
//! returned it. The diagnostic recording writes them and ssr-replay reads them.

use zerocopy::{FromBytes, IntoBytes};

use crate::record::record_size;
use crate::wire::{
    ABI_VERSION, DUMP_FORMAT_VERSION, DUMP_MAGIC, DumpHeader, LEGACY_DUMP_MAGIC, OverrunPayload,
    RECORD_KIND_READ_END, RECORD_KIND_RECORDER_GAP, RecordHeader,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DumpError {
    #[error("not a dump file (the first 8 bytes are not SSRDUMP)")]
    NotADump,
    #[error("the dump is {have} bytes, shorter than its {need}-byte header")]
    Truncated { need: usize, have: usize },
    #[error("the dump was recorded with ABI {found}; this build reads ABI {expected}")]
    AbiMismatch { found: u32, expected: u32 },
    #[error("the dump format is {found}; this build supports up to {supported}")]
    FormatTooNew { found: u32, supported: u32 },
}

/// The header for a new dump.
pub fn dump_header(qpc_frequency: u64) -> DumpHeader {
    DumpHeader {
        magic: DUMP_MAGIC,
        abi_version: ABI_VERSION,
        header_size: size_of::<DumpHeader>() as u32,
        qpc_frequency,
        format_version: DUMP_FORMAT_VERSION,
        reserved: 0,
    }
}

/// The header bytes to write first.
pub fn dump_header_bytes(qpc_frequency: u64) -> Vec<u8> {
    dump_header(qpc_frequency).as_bytes().to_vec()
}

/// Splits a dump into its header and its record stream.
///
/// A header larger than this build's (a newer recorder) is accepted: the
/// record stream starts at `header_size`.
pub fn split_dump(bytes: &[u8]) -> Result<(DumpHeader, &[u8]), DumpError> {
    let magic = bytes.get(..DUMP_MAGIC.len());
    if magic != Some(&DUMP_MAGIC[..]) && magic != Some(&LEGACY_DUMP_MAGIC[..]) {
        return Err(DumpError::NotADump);
    }
    let (header, _) = DumpHeader::read_from_prefix(bytes).map_err(|_| DumpError::Truncated {
        need: size_of::<DumpHeader>(),
        have: bytes.len(),
    })?;
    if header.abi_version != ABI_VERSION {
        return Err(DumpError::AbiMismatch {
            found: header.abi_version,
            expected: ABI_VERSION,
        });
    }
    if header.format_version > DUMP_FORMAT_VERSION {
        return Err(DumpError::FormatTooNew {
            found: header.format_version,
            supported: DUMP_FORMAT_VERSION,
        });
    }
    let start = (header.header_size as usize).max(size_of::<DumpHeader>());
    if bytes.len() < start {
        return Err(DumpError::Truncated {
            need: start,
            have: bytes.len(),
        });
    }
    Ok((header, &bytes[start..]))
}

/// Bytes returned by one recorded READ, excluding recorder-only markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadChunk<'a> {
    pub bytes: &'a [u8],
    pub end_qpc: Option<u64>,
    pub lost_bytes: u64,
}

/// Iterates the READ chunks retained by a dump without allocating.
pub fn reads<'a>(header: &DumpHeader, stream: &'a [u8]) -> Reads<'a> {
    Reads {
        stream,
        offset: 0,
        legacy: header.format_version <= 1,
        done: false,
        pending_lost: 0,
    }
}

#[derive(Clone, Debug)]
pub struct Reads<'a> {
    stream: &'a [u8],
    offset: usize,
    legacy: bool,
    done: bool,
    pending_lost: u64,
}

impl<'a> Iterator for Reads<'a> {
    type Item = ReadChunk<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self.legacy {
            self.done = true;
            return (!self.stream.is_empty()).then_some(ReadChunk {
                bytes: self.stream,
                end_qpc: None,
                lost_bytes: 0,
            });
        }
        let mut start = self.offset;
        while self.offset < self.stream.len() {
            let rest = &self.stream[self.offset..];
            let Ok((record, _)) = RecordHeader::read_from_prefix(rest) else {
                self.done = true;
                return Some(ReadChunk {
                    bytes: &self.stream[start..],
                    end_qpc: None,
                    lost_bytes: core::mem::take(&mut self.pending_lost),
                });
            };
            let size = record_size(record.len as usize);
            if rest.len() < size {
                self.done = true;
                return Some(ReadChunk {
                    bytes: &self.stream[start..],
                    end_qpc: None,
                    lost_bytes: core::mem::take(&mut self.pending_lost),
                });
            }
            if record.kind == RECORD_KIND_RECORDER_GAP {
                if start < self.offset {
                    return Some(ReadChunk {
                        bytes: &self.stream[start..self.offset],
                        end_qpc: None,
                        lost_bytes: core::mem::take(&mut self.pending_lost),
                    });
                }
                if let Ok(payload) = OverrunPayload::read_from_bytes(
                    &rest[size_of::<RecordHeader>()
                        ..size_of::<RecordHeader>() + record.len as usize],
                ) {
                    self.pending_lost = self.pending_lost.saturating_add(payload.lost_bytes);
                }
                self.offset += size;
                start = self.offset;
                continue;
            }
            if record.kind == RECORD_KIND_READ_END {
                let end = self.offset;
                self.offset += size;
                return Some(ReadChunk {
                    bytes: &self.stream[start..end],
                    end_qpc: Some(record.qpc),
                    lost_bytes: core::mem::take(&mut self.pending_lost),
                });
            }
            self.offset += size;
        }
        self.done = true;
        (start < self.stream.len() || self.pending_lost != 0).then(|| ReadChunk {
            bytes: &self.stream[start..],
            end_qpc: None,
            lost_bytes: core::mem::take(&mut self.pending_lost),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ENDPOINT_PRIMARY, Record, RecordWriter, records};

    fn recorded_read(records: &[u8], qpc: u64) -> Vec<u8> {
        let mut marker = [0; 64];
        let mut writer = RecordWriter::new(&mut marker);
        assert!(writer.read_end(qpc));
        let len = writer.len();
        let mut out = records.to_vec();
        out.extend_from_slice(&marker[..len]);
        out
    }

    #[test]
    fn header_round_trips_and_guards_the_magic() {
        let mut file = dump_header_bytes(10_000_000);
        file.extend_from_slice(&[1, 2, 3]);
        let (header, stream) = split_dump(&file).unwrap();
        assert_eq!(header.qpc_frequency, 10_000_000);
        assert_eq!(stream, &[1, 2, 3]);
        assert_eq!(split_dump(b"RIFF...."), Err(DumpError::NotADump));
        let mut legacy = file.clone();
        legacy[..8].copy_from_slice(&LEGACY_DUMP_MAGIC);
        assert_eq!(split_dump(&legacy).unwrap().1, &[1, 2, 3]);
        assert!(matches!(
            split_dump(&file[..12]),
            Err(DumpError::Truncated { .. })
        ));
        let mut other = file.clone();
        other[8] = 9;
        assert!(matches!(
            split_dump(&other),
            Err(DumpError::AbiMismatch { found: 9, .. })
        ));
    }

    #[test]
    fn format_two_reads_round_trip_boundaries_empty_reads_tail_and_gaps() {
        let mut first = [0; 64];
        let mut writer = RecordWriter::new(&mut first);
        assert!(writer.stream_start(1, 2));
        let first_len = writer.len();
        let mut stream = recorded_read(&first[..first_len], 10);
        stream.extend_from_slice(&recorded_read(&[], 20));

        let mut gap = [0; 128];
        let mut writer = RecordWriter::new(&mut gap);
        assert!(writer.recorder_gap(21, 1234));
        assert!(writer.data(1, 0, 22, &[7]));
        let gap_len = writer.len();
        stream.extend_from_slice(&gap[..gap_len]);

        let header = dump_header(10_000_000);
        let chunks: Vec<_> = reads(&header, &stream).collect();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].bytes, &first[..first_len]);
        assert_eq!(chunks[0].end_qpc, Some(10));
        assert!(chunks[1].bytes.is_empty());
        assert_eq!(chunks[1].end_qpc, Some(20));
        assert_eq!(chunks[2].lost_bytes, 1234);
        assert_eq!(chunks[2].end_qpc, None);
        assert!(matches!(
            records(chunks[2].bytes).next(),
            Some(Ok(Record::Data { bytes: &[7], .. }))
        ));
    }

    #[test]
    fn recorder_gap_inside_a_read_keeps_the_bytes_before_it() {
        let mut stream = [0; 256];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.data(1, 0, 1, &[1, 2, 3]));
        let before_gap = writer.len();
        assert!(writer.recorder_gap(2, 99));
        assert!(writer.data(1, 3, 3, &[4, 5]));
        assert!(writer.read_end(4));
        let len = writer.len();

        let chunks: Vec<_> = reads(&dump_header(1), &stream[..len]).collect();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].bytes, &stream[..before_gap]);
        assert_eq!(chunks[0].end_qpc, None);
        assert_eq!(chunks[0].lost_bytes, 0);
        assert!(matches!(
            records(chunks[1].bytes).next(),
            Some(Ok(Record::Data { bytes: &[4, 5], .. }))
        ));
        assert_eq!(chunks[1].end_qpc, Some(4));
        assert_eq!(chunks[1].lost_bytes, 99);
    }

    #[test]
    fn formats_zero_and_one_are_one_unbounded_read() {
        for version in [0, 1] {
            let mut header = dump_header(1);
            header.format_version = version;
            let chunks: Vec<_> = reads(&header, &[1, 2, 3]).collect();
            assert_eq!(
                chunks,
                [ReadChunk {
                    bytes: &[1, 2, 3],
                    end_qpc: None,
                    lost_bytes: 0
                }]
            );
        }
    }

    #[test]
    fn old_records_iterator_skips_recorder_markers() {
        let mut stream = [0; 256];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.stream_start(1, 1));
        assert!(writer.recorder_gap(2, 9));
        assert!(writer.read_end(3));
        assert!(writer.stream_stop(1, 4));
        let len = writer.len();
        let parsed: Vec<_> = records(&stream[..len]).collect::<Result<_, _>>().unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(matches!(parsed[0], Record::StreamStart { .. }));
        assert!(matches!(parsed[1], Record::StreamStop { .. }));
    }

    #[test]
    fn old_dump_format_is_primary_and_newer_format_is_rejected() {
        let mut stream = [0; 64];
        let mut writer = RecordWriter::new(&mut stream);
        assert!(writer.stream_start(1, 2));
        let length = writer.len();
        let mut header = dump_header(10_000_000);
        header.format_version = 0;
        let mut file = header.as_bytes().to_vec();
        file.extend_from_slice(&stream[..length]);
        let (_, old_stream) = split_dump(&file).unwrap();
        assert!(matches!(
            records(old_stream).next(),
            Some(Ok(Record::StreamStart {
                endpoint: ENDPOINT_PRIMARY,
                ..
            }))
        ));

        header.format_version = DUMP_FORMAT_VERSION + 1;
        file[..size_of::<DumpHeader>()].copy_from_slice(header.as_bytes());
        assert_eq!(
            split_dump(&file),
            Err(DumpError::FormatTooNew {
                found: DUMP_FORMAT_VERSION + 1,
                supported: DUMP_FORMAT_VERSION,
            })
        );
    }
}
