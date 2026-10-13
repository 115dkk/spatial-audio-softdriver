//! Wire structures shared with the driver.
//!
//! cbindgen reads this file to generate `include/bioatmos_abi.h`, so it holds
//! only items C can express: `#[repr(C)]` structs of fixed-width integers and
//! integer constants written as literals or shifts. Every IOCTL is checked
//! against [`ctl_code`] by a unit test. Bit constants stay below bit 31 so the
//! C macros (plain `int` shifts) do not overflow.

use core::mem::{offset_of, size_of};

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// Bumped only by a frozen-contract revision; the current v1 is not frozen yet.
pub const ABI_VERSION: u32 = 1;

/// Stable user-mode control-device path; display names are registry data.
pub const CONTROL_DEVICE_PATH: &str = "\\\\.\\SpatialSoftReceiver";

// ---- IOCTL codes (FILE_DEVICE_UNKNOWN, vendor functions 0x900..) ----

/// `CTL_CODE(FILE_DEVICE_UNKNOWN, 0x900, METHOD_BUFFERED, FILE_READ_ACCESS)`;
/// output: [`AbiInfo`].
pub const IOCTL_BIO_QUERY_INFO: u32 = (0x22 << 16) | (1 << 14) | (0x900 << 2);
/// `CTL_CODE(FILE_DEVICE_UNKNOWN, 0x901, METHOD_OUT_DIRECT, FILE_READ_ACCESS)`;
/// input: [`ReadRequest`], output: a record stream of whole records, at most
/// [`READ_MAX_BYTES`] whatever the buffer. Pends until `min_bytes` of records
/// are queued (clamped to the buffer and to [`READ_MAX_BYTES`]), a
/// format-change, volume or stream-stop record arrives, or `timeout_ms` passes.
/// Cancellable.
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
pub const RECORD_KIND_VOLUME: u16 = 6;

pub const MAX_VOLUME_CHANNELS: usize = 32;
pub const VOLUME_MIN_Q16: i32 = -6291456;
pub const VOLUME_MAX_Q16: i32 = 0;
pub const VOLUME_STEP_Q16: i32 = 32768;

/// Endpoint hardware volume state. Little-endian, 136 bytes; records and their
/// payloads start at 8-byte boundaries. Unused levels and mute bits are zero.
/// `channel_count` is the endpoint's default device-format channel count (1..=32),
/// not the currently negotiated stream's channel count. Levels are dB * 65536.
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct VolumePayload {
    pub channel_count: u32,
    pub mute_mask: u32,
    pub level_q16: [i32; MAX_VOLUME_CHANNELS],
}

/// Record kinds at or above this value belong to dump recorders, never the
/// driver contract. The driver must not emit them into a READ stream.
pub const RECORD_KIND_RECORDER_MIN: u16 = 0x8000;
/// Dump-format-2 marker written after every READ returned to a recorder.
pub const RECORD_KIND_READ_END: u16 = 0x8001;
/// Dump recorder marker reporting READ bytes the recorder could not retain.
pub const RECORD_KIND_RECORDER_GAP: u16 = 0x8002;
/// Marker records do not belong to a render endpoint.
pub const RECORD_ENDPOINT_RECORDER: u16 = 0xFFFF;

/// Endpoint numbers in [`RecordHeader::endpoint`] and the per-endpoint arrays.
/// ADR 0028 assigns 0 to primary, 1 to music, 2 to passthrough, and reserves 3.
pub const ENDPOINT_PRIMARY: u16 = 0;
pub const ENDPOINT_SECOND: u16 = 1;
/// ADR 0028: exclusive streams only (bitstreams and exclusive PCM).
pub const ENDPOINT_PASSTHROUGH: u16 = 2;
/// Contract slots for render endpoints, including reserved endpoint 3.
pub const MAX_ENDPOINTS: usize = 4;

/// Returns the exposure-mask bit for `endpoint`, or zero outside the contract.
pub const fn endpoint_bit(endpoint: u16) -> u32 {
    if endpoint < MAX_ENDPOINTS as u16 {
        1 << endpoint
    } else {
        0
    }
}

/// Data record flag: the bytes do not follow the previous data record. The
/// kernel sets it after an overrun, a source gap, and on the first data returned
/// to a newly opened handle.
pub const RECORD_FLAG_DISCONTINUITY: u16 = 0x0001;

/// State-record flag: this is not a new event; it restates the endpoint's
/// current format, volume or streaming state. A repeated format carries the current
/// epoch instead of incrementing it.
pub const RECORD_FLAG_REPEAT: u16 = 0x0002;

/// Every record starts at a multiple of this many bytes; the gap after a
/// payload is zero padding.
pub const RECORD_ALIGN: u32 = 8;

/// Largest payload of one data record. The driver splits a longer span of the
/// audio engine's buffer into several records, so a READ buffer of
/// `record_size(MAX_DATA_PAYLOAD)` bytes always makes progress.
pub const MAX_DATA_PAYLOAD: u32 = 64 * 1024;

/// Most bytes one READ returns. The driver copies records out under a spin
/// lock and keeps that copy short; a reader behind by more drains it over
/// several READs.
pub const READ_MAX_BYTES: u32 = 256 * 1024;

/// Header of one record. The payload (`len` bytes) follows immediately.
///
/// * Data: the bytes the audio engine wrote, in the epoch's format.
/// * FormatChange: the WAVEFORMATEX(TENSIBLE) Windows negotiated, verbatim.
/// * StreamStart / StreamStop: no payload.
/// * Overrun: [`OverrunPayload`].
/// * Volume: [`VolumePayload`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct RecordHeader {
    pub kind: u16,
    pub flags: u16,
    /// +1 with every format-change record (the record carries the new
    /// epoch), except one flagged [`RECORD_FLAG_REPEAT`], which keeps it.
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
    /// The render endpoint that emitted this record. Epoch, `byte_pos`, start,
    /// stop, format change, and overrun state are counted per endpoint. Old
    /// records have zero here and therefore belong to the primary endpoint.
    pub endpoint: u16,
    /// Writers write zero; readers ignore this field for forward compatibility.
    pub reserved: u16,
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

/// PCM sample rates; bit i of `PcmFormats::rates` is `PCM_RATES[i]`.
pub const PCM_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

/// PCM sample shapes; bit i of `PcmFormats::shapes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PcmShape {
    Int16 = 0,
    /// Packed three-byte integer.
    Int24 = 1,
    Int24In32 = 2,
    Int32 = 3,
    Float32 = 4,
}

/// The 18 ksmedia.h speaker positions, FRONT_LEFT through TOP_BACK_RIGHT.
pub const PCM_SPEAKER_BITS: u32 = 0x0003_ffff;
pub const MAX_PCM_CHANNELS: u16 = 32;

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable,
)]
#[repr(C)]
pub struct PcmFormats {
    /// Bit i selects `PCM_RATES[i]`.
    pub rates: u32,
    /// Bit i selects PcmShape i.
    pub shapes: u32,
    /// Nonzero masks use only PCM_SPEAKER_BITS, have nChannels set bits,
    /// and no more than this many channels (zero disables masked PCM).
    pub max_masked_channels: u32,
    /// Bit c-1 permits c channels with mask zero (direct out, no speaker meaning).
    pub direct_channels: u32,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable,
)]
#[repr(C)]
pub struct EndpointFormats {
    pub codecs: u64,
    pub pcm: PcmFormats,
}

pub const DEFAULT_PCM: PcmFormats = PcmFormats {
    rates: 0x3f,
    shapes: 0x1f,
    max_masked_channels: 18,
    direct_channels: 0xffff_ffff,
};
pub const COMPILED_PCM: PcmFormats = DEFAULT_PCM;

/// Codec mask: bits 1..8 retain their values; bit 0 and bits 9..63 are reserved.
pub const FORMAT_AC3: u64 = 1 << 1;
pub const FORMAT_EAC3: u64 = 1 << 2;
pub const FORMAT_EAC3_ATMOS: u64 = 1 << 3;
pub const FORMAT_TRUEHD: u64 = 1 << 4;
pub const FORMAT_MAT20: u64 = 1 << 5;
pub const FORMAT_MAT21: u64 = 1 << 6;
pub const FORMAT_DTS: u64 = 1 << 7;
pub const FORMAT_DTS_HD: u64 = 1 << 8;

pub const COMPILED_CODECS: u64 = 0x1fe;
pub const DEFAULT_ENABLED_CODECS: u64 = 0x6c;

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
    pub compiled: EndpointFormats,
    pub features: u64,
    /// `KeQueryPerformanceCounter` frequency, so records' QPCs convert to time.
    pub qpc_frequency: u64,
    /// Bit i is one when the driver exposes endpoint i.
    pub endpoint_mask: u32,
    pub reserved: u32,
    /// Registry capabilities per endpoint, indexed by endpoint number; zero
    /// for endpoints absent from [`Self::endpoint_mask`].
    pub endpoints: [EndpointFormats; MAX_ENDPOINTS],
}

impl AbiInfo {
    /// Whether the driver exposes `endpoint`; out-of-contract values are false.
    pub const fn exposes(&self, endpoint: u16) -> bool {
        self.endpoint_mask & endpoint_bit(endpoint) != 0
    }
}

pub const ENDPOINT_STATE_STREAMING: u32 = 1 << 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct EndpointStats {
    pub total_bytes: u64,
    pub overruns: u64,
    pub lost_bytes: u64,
    /// The endpoint's current epoch.
    pub epoch: u32,
    /// `ENDPOINT_STATE_*` bits.
    pub state: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct BioStats {
    pub struct_size: u32,
    pub open_count: u32,
    /// Worst lateness of the virtual DMA timer, microseconds; all endpoints share the timer.
    pub max_timer_lateness_us: u32,
    pub reserved: u32,
    pub endpoints: [EndpointStats; MAX_ENDPOINTS],
}

// ---- Dump files ----

/// First bytes of a dump file: `BIODUMP` and a NUL.
pub const DUMP_MAGIC: [u8; 8] = *b"BIODUMP\0";

/// 0: recorded before records carried an endpoint (every record is the primary's).
/// 1: records carry `endpoint` but not READ boundaries.
/// 2: each READ's bytes are followed by a [`RECORD_KIND_READ_END`] marker.
/// 3: volume records carry up to 32 channels (136-byte payload). A volume
///    record in an older dump has the 72-byte, 16-channel payload; readers
///    still accept it and zero the remaining levels.
pub const DUMP_FORMAT_VERSION: u32 = 3;

/// Header of a diagnostic recording. The record stream follows; format 2 adds
/// recorder-only boundary and loss markers that the driver never emits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub struct DumpHeader {
    pub magic: [u8; 8],
    pub abi_version: u32,
    pub header_size: u32,
    /// QPC frequency of the machine that recorded, for latency figures.
    pub qpc_frequency: u64,
    pub format_version: u32,
    pub reserved: u32,
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
    assert!(offset_of!(RecordHeader, endpoint) == 36);
    assert!(offset_of!(RecordHeader, reserved) == 38);
    assert!(size_of::<RecordHeader>().is_multiple_of(RECORD_ALIGN as usize));
    assert!(size_of::<VolumePayload>() == 136);
    assert!(offset_of!(VolumePayload, channel_count) == 0);
    assert!(offset_of!(VolumePayload, mute_mask) == 4);
    assert!(offset_of!(VolumePayload, level_q16) == 8);
    assert!(size_of::<VolumePayload>().is_multiple_of(RECORD_ALIGN as usize));
    assert!(MAX_VOLUME_CHANNELS == 32);
    assert!(size_of::<OverrunPayload>() == 8);
    assert!(size_of::<ReadRequest>() == 8);
    assert!(size_of::<RateTrim>() == 8);
    assert!(size_of::<AbiInfo>() == 160);
    assert!(offset_of!(AbiInfo, compiled) == 16);
    assert!(offset_of!(AbiInfo, features) == 40);
    assert!(offset_of!(AbiInfo, qpc_frequency) == 48);
    assert!(offset_of!(AbiInfo, endpoint_mask) == 56);
    assert!(offset_of!(AbiInfo, endpoints) == 64);
    assert!(size_of::<EndpointStats>() == 32);
    assert!(offset_of!(EndpointStats, epoch) == 24);
    assert!(offset_of!(EndpointStats, state) == 28);
    assert!(size_of::<BioStats>() == 144);
    assert!(offset_of!(BioStats, max_timer_lateness_us) == 8);
    assert!(offset_of!(BioStats, endpoints) == 16);
    assert!(size_of::<DumpHeader>() == 32);
    assert!(offset_of!(DumpHeader, format_version) == 24);
    assert!(offset_of!(DumpHeader, reserved) == 28);
    assert!(MAX_ENDPOINTS == 4);
    assert!(FORMAT_DTS_HD < 1 << 31 && FEATURE_WRITE_PACKET < 1 << 31);
    assert!(size_of::<PcmFormats>() == 16);
    assert!(size_of::<EndpointFormats>() == 24);
    assert!(offset_of!(EndpointFormats, pcm) == 8);
    assert!(core::mem::align_of::<AbiInfo>() == 8);
    assert!(COMPILED_CODECS == 0x1fe);
    assert!(DEFAULT_ENABLED_CODECS == 0x6c);
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
    fn the_largest_data_record_fits_one_read() {
        assert!(crate::record_size(MAX_DATA_PAYLOAD as usize) <= READ_MAX_BYTES as usize);
    }

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
    fn endpoint_numbers_are_distinct_and_in_range() {
        let endpoints = [ENDPOINT_PRIMARY, ENDPOINT_SECOND, ENDPOINT_PASSTHROUGH];
        for (index, endpoint) in endpoints.iter().enumerate() {
            assert!((*endpoint as usize) < MAX_ENDPOINTS);
            assert!(endpoints[index + 1..].iter().all(|other| endpoint != other));
        }
    }

    #[test]
    fn endpoint_mask_helpers_reject_out_of_range_values() {
        assert_eq!(endpoint_bit(ENDPOINT_PRIMARY), 1);
        assert_eq!(endpoint_bit(ENDPOINT_SECOND), 2);
        assert_eq!(endpoint_bit(ENDPOINT_PASSTHROUGH), 4);
        assert_eq!(endpoint_bit(3), 8);
        assert_eq!(endpoint_bit(MAX_ENDPOINTS as u16), 0);
        assert_eq!(endpoint_bit(u16::MAX), 0);

        let info = AbiInfo {
            abi_version: ABI_VERSION,
            struct_size: size_of::<AbiInfo>() as u32,
            driver_version: 0,
            ring_bytes: 0,
            compiled: EndpointFormats::default(),
            features: 0,
            qpc_frequency: 0,
            endpoint_mask: endpoint_bit(ENDPOINT_PRIMARY) | endpoint_bit(ENDPOINT_PASSTHROUGH),
            reserved: 0,
            endpoints: [EndpointFormats::default(); MAX_ENDPOINTS],
        };
        assert!(info.exposes(ENDPOINT_PRIMARY));
        assert!(!info.exposes(ENDPOINT_SECOND));
        assert!(info.exposes(ENDPOINT_PASSTHROUGH));
        assert!(!info.exposes(3));
        assert!(!info.exposes(MAX_ENDPOINTS as u16));
    }

    #[test]
    fn record_kinds_and_format_bits_are_distinct() {
        let kinds = [
            RECORD_KIND_DATA,
            RECORD_KIND_FORMAT_CHANGE,
            RECORD_KIND_STREAM_START,
            RECORD_KIND_STREAM_STOP,
            RECORD_KIND_OVERRUN,
            RECORD_KIND_VOLUME,
        ];
        for (i, a) in kinds.iter().enumerate() {
            assert!(kinds[i + 1..].iter().all(|b| a != b));
        }
        let formats = [
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
        assert_eq!(formats.iter().fold(0, |acc, f| acc | f), COMPILED_CODECS);
        assert_eq!(DEFAULT_ENABLED_CODECS & 1, 0);
    }

    #[test]
    fn query_info_round_trips() {
        let formats = EndpointFormats {
            codecs: COMPILED_CODECS,
            pcm: COMPILED_PCM,
        };
        let info = AbiInfo {
            abi_version: ABI_VERSION,
            struct_size: size_of::<AbiInfo>() as u32,
            driver_version: 1,
            ring_bytes: 8 * 1024 * 1024,
            compiled: formats,
            features: FEATURE_PENDING_READ,
            qpc_frequency: 10_000_000,
            endpoint_mask: 7,
            reserved: 0,
            endpoints: [formats, formats, formats, EndpointFormats::default()],
        };
        assert_eq!(AbiInfo::read_from_bytes(info.as_bytes()), Ok(info));
        assert_eq!(offset_of!(AbiInfo, reserved), 60);
        assert_eq!(offset_of!(PcmFormats, rates), 0);
        assert_eq!(offset_of!(PcmFormats, shapes), 4);
        assert_eq!(offset_of!(PcmFormats, max_masked_channels), 8);
        assert_eq!(offset_of!(PcmFormats, direct_channels), 12);
    }
}
