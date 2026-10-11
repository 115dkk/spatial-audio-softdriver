//! Wire structures shared with the driver.
//!
//! cbindgen reads this file to generate `include/bioatmos_abi.h`, so it holds
//! only items C can express: `#[repr(C)]` structs of fixed-width integers and
//! integer constants written as literals or shifts. Every IOCTL is checked
//! against [`ctl_code`] by a unit test. Bit constants stay below bit 31 so the
//! C macros (plain `int` shifts) do not overflow.

use core::mem::{offset_of, size_of};

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// Bumped only by a frozen-contract revision; v1 is what the signed driver
/// ships with.
pub const ABI_VERSION: u32 = 1;

// ---- IOCTL codes (FILE_DEVICE_UNKNOWN, vendor functions 0x900..) ----

/// `CTL_CODE(FILE_DEVICE_UNKNOWN, 0x900, METHOD_BUFFERED, FILE_READ_ACCESS)`;
/// output: [`AbiInfo`].
pub const IOCTL_BIO_QUERY_INFO: u32 = (0x22 << 16) | (1 << 14) | (0x900 << 2);
/// `CTL_CODE(FILE_DEVICE_UNKNOWN, 0x901, METHOD_OUT_DIRECT, FILE_READ_ACCESS)`;
/// input: [`ReadRequest`], output: a record stream. Pends until `min_bytes` of
/// records are queued, a format-change or stream-stop record arrives, or
/// `timeout_ms` passes. Cancellable.
pub const IOCTL_BIO_READ: u32 = (0x22 << 16) | (1 << 14) | (0x901 << 2) | 2;
/// `CTL_CODE(FILE_DEVICE_UNKNOWN, 0x902, METHOD_BUFFERED, FILE_WRITE_ACCESS)`;
/// input: [`RateTrim`]. Clamped to ±[`TRIM_LIMIT_PPM`], applied from the next
/// timer tick, reset to 0 when the handle closes.
pub const IOCTL_BIO_SET_RATE_TRIM: u32 = (0x22 << 16) | (2 << 14) | (0x902 << 2);
/// `CTL_CODE(FILE_DEVICE_UNKNOWN, 0x903, METHOD_BUFFERED, FILE_READ_ACCESS)`;
/// output: [`BioStats`].
pub const IOCTL_BIO_GET_STATS: u32 = (0x22 << 16) | (1 << 14) | (0x903 << 2);

/// `CTL_CODE` from `devioctl.h`.
pub const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
    (device_type << 16) | (access << 14) | (function << 2) | method
}

// ---- Records ----

pub const RECORD_KIND_DATA: u16 = 1;
pub const RECORD_KIND_FORMAT_CHANGE: u16 = 2;
pub const RECORD_KIND_STREAM_START: u16 = 3;
pub const RECORD_KIND_STREAM_STOP: u16 = 4;
pub const RECORD_KIND_OVERRUN: u16 = 5;

/// Data record flag: the bytes do not follow the previous data record
/// (the stream restarted or an overrun dropped bytes in between).
pub const RECORD_FLAG_DISCONTINUITY: u16 = 0x0001;

/// Every record starts at a multiple of this many bytes; the gap after a
/// payload is zero padding.
pub const RECORD_ALIGN: u32 = 8;

/// Header of one record. The payload (`len` bytes) follows immediately.
///
/// * Data: the bytes the audio engine wrote, in the epoch's format.
/// * FormatChange: the WAVEFORMATEX(TENSIBLE) Windows negotiated, verbatim.
/// * StreamStart / StreamStop: no payload.
/// * Overrun: [`OverrunPayload`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct RecordHeader {
    pub kind: u16,
    pub flags: u16,
    /// +1 with every format-change record (the record carries the new epoch).
    pub epoch: u32,
    /// Bytes of data since the stream started, at the first payload byte.
    pub byte_pos: u64,
    /// QPC at which the virtual DMA passed the last byte of this record.
    pub qpc: u64,
    /// QPC at which the audio engine wrote the bytes, when write-packet
    /// notification is implemented (feature bit `FEATURE_WRITE_PACKET`); else 0.
    pub qpc_written: u64,
    /// Payload length in bytes, excluding padding.
    pub len: u32,
    pub reserved: u32,
}

/// Payload of an overrun record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct OverrunPayload {
    pub lost_bytes: u64,
}

// ---- IOCTL payloads ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct ReadRequest {
    pub min_bytes: u32,
    pub timeout_ms: u32,
}

pub const TRIM_LIMIT_PPM: i32 = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct RateTrim {
    /// Parts per million × 65536; positive runs the virtual DMA faster.
    pub ppm_q16: i32,
    pub reserved: u32,
}

/// Format bits for [`AbiInfo::compiled_formats`] and `enabled_formats` (the
/// registry mask bio-setup writes).
pub const FORMAT_PCM: u64 = 1 << 0;
pub const FORMAT_AC3: u64 = 1 << 1;
pub const FORMAT_EAC3: u64 = 1 << 2;
pub const FORMAT_EAC3_ATMOS: u64 = 1 << 3;
pub const FORMAT_TRUEHD: u64 = 1 << 4;
pub const FORMAT_MAT20: u64 = 1 << 5;
pub const FORMAT_MAT21: u64 = 1 << 6;
pub const FORMAT_DTS: u64 = 1 << 7;
pub const FORMAT_DTS_HD: u64 = 1 << 8;

/// Feature bits for [`AbiInfo::features`].
pub const FEATURE_PENDING_READ: u64 = 1 << 0;
pub const FEATURE_RATE_TRIM: u64 = 1 << 1;
pub const FEATURE_WRITE_PACKET: u64 = 1 << 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct AbiInfo {
    pub abi_version: u32,
    /// `sizeof(AbiInfo)` as the driver knows it; fields added later go after.
    pub struct_size: u32,
    /// Driver build as `major << 16 | minor`.
    pub driver_version: u32,
    pub ring_bytes: u32,
    pub compiled_formats: u64,
    pub enabled_formats: u64,
    pub features: u64,
    /// `KeQueryPerformanceCounter` frequency, so records' QPCs convert to time.
    pub qpc_frequency: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct BioStats {
    pub struct_size: u32,
    pub open_count: u32,
    pub total_bytes: u64,
    pub overruns: u64,
    pub lost_bytes: u64,
    /// Worst lateness of the virtual DMA timer, microseconds.
    pub max_timer_lateness_us: u32,
    pub reserved: u32,
}

// ---- Dump files ----

/// First bytes of a dump file: `BIODUMP` and a NUL.
pub const DUMP_MAGIC: [u8; 8] = *b"BIODUMP\0";

/// Header of a diagnostic recording. The record stream follows, exactly as
/// READ returned it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct DumpHeader {
    pub magic: [u8; 8],
    pub abi_version: u32,
    pub header_size: u32,
    /// QPC frequency of the machine that recorded, for latency figures.
    pub qpc_frequency: u64,
    pub reserved: u64,
}

// Layout assertions. tests/c_header.rs writes the same numbers into the C
// header as _Static_assert, so the driver fails to compile on a mismatch.
const _: () = {
    assert!(size_of::<RecordHeader>() == 40);
    assert!(offset_of!(RecordHeader, kind) == 0);
    assert!(offset_of!(RecordHeader, flags) == 2);
    assert!(offset_of!(RecordHeader, epoch) == 4);
    assert!(offset_of!(RecordHeader, byte_pos) == 8);
    assert!(offset_of!(RecordHeader, qpc) == 16);
    assert!(offset_of!(RecordHeader, qpc_written) == 24);
    assert!(offset_of!(RecordHeader, len) == 32);
    assert!(offset_of!(RecordHeader, reserved) == 36);
    assert!(size_of::<RecordHeader>().is_multiple_of(RECORD_ALIGN as usize));
    assert!(size_of::<OverrunPayload>() == 8);
    assert!(size_of::<ReadRequest>() == 8);
    assert!(size_of::<RateTrim>() == 8);
    assert!(size_of::<AbiInfo>() == 48);
    assert!(offset_of!(AbiInfo, compiled_formats) == 16);
    assert!(offset_of!(AbiInfo, qpc_frequency) == 40);
    assert!(size_of::<BioStats>() == 40);
    assert!(offset_of!(BioStats, max_timer_lateness_us) == 32);
    assert!(size_of::<DumpHeader>() == 32);
    assert!(FORMAT_DTS_HD < 1 << 31 && FEATURE_WRITE_PACKET < 1 << 31);
};

#[cfg(test)]
mod tests {
    use super::*;

    const FILE_DEVICE_UNKNOWN: u32 = 0x22;
    const METHOD_BUFFERED: u32 = 0;
    const METHOD_OUT_DIRECT: u32 = 2;
    const FILE_READ_ACCESS: u32 = 1;
    const FILE_WRITE_ACCESS: u32 = 2;

    #[test]
    fn ioctl_literals_match_ctl_code() {
        let code = |f, m, a| ctl_code(FILE_DEVICE_UNKNOWN, f, m, a);
        assert_eq!(
            IOCTL_BIO_QUERY_INFO,
            code(0x900, METHOD_BUFFERED, FILE_READ_ACCESS)
        );
        assert_eq!(
            IOCTL_BIO_READ,
            code(0x901, METHOD_OUT_DIRECT, FILE_READ_ACCESS)
        );
        assert_eq!(
            IOCTL_BIO_SET_RATE_TRIM,
            code(0x902, METHOD_BUFFERED, FILE_WRITE_ACCESS)
        );
        assert_eq!(
            IOCTL_BIO_GET_STATS,
            code(0x903, METHOD_BUFFERED, FILE_READ_ACCESS)
        );
    }

    #[test]
    fn record_kinds_and_format_bits_are_distinct() {
        let kinds = [
            RECORD_KIND_DATA,
            RECORD_KIND_FORMAT_CHANGE,
            RECORD_KIND_STREAM_START,
            RECORD_KIND_STREAM_STOP,
            RECORD_KIND_OVERRUN,
        ];
        for (i, a) in kinds.iter().enumerate() {
            assert!(kinds[i + 1..].iter().all(|b| a != b));
        }
        let formats = [
            FORMAT_PCM,
            FORMAT_AC3,
            FORMAT_EAC3,
            FORMAT_EAC3_ATMOS,
            FORMAT_TRUEHD,
            FORMAT_MAT20,
            FORMAT_MAT21,
            FORMAT_DTS,
            FORMAT_DTS_HD,
        ];
        assert_eq!(
            formats.iter().fold(0, |acc, f| acc | f).count_ones() as usize,
            formats.len()
        );
    }
}
