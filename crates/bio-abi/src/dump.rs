//! Dump files: a [`DumpHeader`] followed by the record stream exactly as READ
//! returned it. The diagnostic recording writes them and bio-replay reads them.

use zerocopy::{FromBytes, IntoBytes};

use crate::wire::{ABI_VERSION, DUMP_FORMAT_VERSION, DUMP_MAGIC, DumpHeader};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DumpError {
    #[error("not a dump file (the first 8 bytes are not BIODUMP)")]
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
    if bytes.len() < DUMP_MAGIC.len() || bytes[..DUMP_MAGIC.len()] != DUMP_MAGIC {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ENDPOINT_PRIMARY, Record, RecordWriter, records};

    #[test]
    fn header_round_trips_and_guards_the_magic() {
        let mut file = dump_header_bytes(10_000_000);
        file.extend_from_slice(&[1, 2, 3]);
        let (header, stream) = split_dump(&file).unwrap();
        assert_eq!(header.qpc_frequency, 10_000_000);
        assert_eq!(stream, &[1, 2, 3]);
        assert_eq!(split_dump(b"RIFF...."), Err(DumpError::NotADump));
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
