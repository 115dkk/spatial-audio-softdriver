#![forbid(unsafe_code)]

//! The driver contract (ADR 0011): the only thing `bioatmos.sys` and user mode
//! share.
//!
//! The kernel side is mechanism only: it receives a format, then hands over
//! bytes and times. Everything that interprets them (IEC 61937, decoding,
//! which formats to enable, how fast to run) lives in user mode, so new formats
//! and policies ship as app updates without re-signing the driver.
//!
//! * [`wire`]: the `#[repr(C)]` structures and constants. Their sizes and
//!   offsets are asserted at compile time here, and the C header the driver
//!   includes (`include/bioatmos_abi.h`) is generated from this module, with
//!   the same assertions, by `tests/c_header.rs`.
//! * [`record`]: the record stream a READ returns and a dump file holds.
//! * [`wave`]: the WAVEFORMATEXTENSIBLE a format-change record carries.
//!
//! Promises the contract makes:
//! * every record carries its payload length, so user mode skips kinds it does
//!   not know and an older app survives a newer driver;
//! * the epoch rises only with a format-change record, and all data within one
//!   epoch has the same format;
//! * on overrun the kernel does not wait: it overwrites, then reports the lost
//!   byte count in an overrun record; user mode resets the affected decoder and
//!   resumes at the next sync;
//! * structure sizes and offsets are asserted at compile time on both sides.

pub mod dump;
pub mod record;
pub mod wave;
pub mod wire;

pub use dump::{DumpError, dump_header, dump_header_bytes, split_dump};
pub use record::{AbiError, Record, RecordWriter, record_size, records};
pub use wave::{WaveFormatError, parse_wave_format, wave_format_bytes};
pub use wire::*;

/// The control device user mode opens.
pub const DEVICE_PATH: &str = r"\\.\BioAtmos";
