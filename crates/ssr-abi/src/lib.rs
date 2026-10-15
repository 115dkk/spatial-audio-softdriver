#![forbid(unsafe_code)]

//! The driver contract (ADR 0011): the only thing `spatialsoftreceiver.sys` and user mode
//! share.
//!
//! The kernel side is mechanism only: it receives a format, then hands over
//! bytes and times. Everything that interprets them (IEC 61937, decoding,
//! which formats to enable, how fast to run) lives in user mode, so new formats
//! and policies ship as app updates without re-signing the driver.
//!
//! * [`wire`]: the `#[repr(C)]` structures and constants. Their sizes and
//!   offsets are asserted at compile time here, and the C header the driver
//!   includes (`include/ssr_abi.h`) is generated from this module, with
//!   the same assertions, by `tests/c_header.rs`.
//! * [`record`]: the record stream a READ returns and a dump file holds.
//! * [`wave`]: the WAVEFORMATEXTENSIBLE a format-change record carries.
//!
//! Promises the contract makes:
//! * every record carries its payload length, so user mode skips kinds it does
//!   not know and an older app survives a newer driver;
//! * the epoch rises only with a format-change record, and all data within one
//!   epoch has the same format;
//! * on overrun the kernel does not wait: it overwrites whole records, then
//!   reports the lost byte count, restates the current format and volume, then the
//!   current streaming state with [`RECORD_FLAG_REPEAT`], even for a stopped
//!   endpoint; the first following data is discontinuous;
//! * a new reader receives the known format, default-or-current volume and
//!   current Start/Stop state of each configured endpoint, all marked [`RECORD_FLAG_REPEAT`], before discontinuous data;
//! * records carry the render endpoint that wrote them; epoch, position and
//!   losses are counted per endpoint (ADR 0024);
//! * structure sizes and offsets are asserted at compile time on both sides.

pub mod dump;
pub mod exclusive_policy;
pub mod ks_path;
pub mod record;
pub mod setup_contract;
pub mod wave;
pub mod wire;

pub use dump::{DumpError, ReadChunk, Reads, dump_header, dump_header_bytes, reads, split_dump};
pub use record::{
    AbiError, READ_END_RECORD_BYTES, RECORDER_GAP_RECORD_BYTES, RawRecord, RawRecords, Record,
    RecordWriter, edit_header, raw_records, record_size, records, volume_gain,
};
pub use wave::{
    IEC61937_FAMILY, IEC61937_FORMAT_BYTES, WAVE_FAMILY, WAVE_FORMAT_DOLBY_AC3_SPDIF,
    WAVE_FORMAT_DTS, WAVE_FORMAT_EXTENSIBLE, WAVE_FORMAT_IEEE_FLOAT, WAVE_FORMAT_PCM,
    WAVE_GUID_TAIL, WAVEFORMATEX_BYTES, WAVEFORMATEXTENSIBLE_BYTES, WaveFormatError,
    parse_wave_format, wave_format_bytes,
};
pub use wire::*;

/// Compatibility alias for callers using the original constant name.
pub const DEVICE_PATH: &str = CONTROL_DEVICE_PATH;
