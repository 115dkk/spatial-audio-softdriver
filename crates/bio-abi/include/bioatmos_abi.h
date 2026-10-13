/* Generated from crates/bio-abi/src/wire.rs by crates/bio-abi/tests/c_header.rs.
 * Do not edit: change the Rust side and run the test with BIO_ABI_BLESS=1. */

#ifndef BIOATMOS_ABI_H
#define BIOATMOS_ABI_H

#include <stddef.h>
#include <stdint.h>

/**
 * Bumped only by a frozen-contract revision; the current v1 is not frozen yet.
 */
#define BIO_ABI_VERSION 1

/**
 * `CTL_CODE(FILE_DEVICE_UNKNOWN, 0x900, METHOD_BUFFERED, FILE_READ_ACCESS)`;
 * output: [`AbiInfo`].
 */
#define IOCTL_BIO_QUERY_INFO (((34 << 16) | (1 << 14)) | (2304 << 2))

/**
 * `CTL_CODE(FILE_DEVICE_UNKNOWN, 0x901, METHOD_OUT_DIRECT, FILE_READ_ACCESS)`;
 * input: [`ReadRequest`], output: a record stream of whole records, at most
 * [`READ_MAX_BYTES`] whatever the buffer. Pends until `min_bytes` of records
 * are queued (clamped to the buffer and to [`READ_MAX_BYTES`]), a
 * format-change, volume or stream-stop record arrives, or `timeout_ms` passes.
 * Cancellable.
 */
#define IOCTL_BIO_READ ((((34 << 16) | (1 << 14)) | (2305 << 2)) | 2)

/**
 * `CTL_CODE(FILE_DEVICE_UNKNOWN, 0x902, METHOD_BUFFERED, FILE_WRITE_ACCESS)`;
 * input: [`RateTrim`]. Clamped to ±[`TRIM_LIMIT_PPM`], applied from the next
 * timer tick, reset to 0 when the handle closes.
 */
#define IOCTL_BIO_SET_RATE_TRIM (((34 << 16) | (2 << 14)) | (2306 << 2))

/**
 * `CTL_CODE(FILE_DEVICE_UNKNOWN, 0x903, METHOD_BUFFERED, FILE_READ_ACCESS)`;
 * output: [`BioStats`].
 */
#define IOCTL_BIO_GET_STATS (((34 << 16) | (1 << 14)) | (2307 << 2))

#define BIO_RECORD_KIND_DATA 1

#define BIO_RECORD_KIND_FORMAT_CHANGE 2

#define BIO_RECORD_KIND_STREAM_START 3

#define BIO_RECORD_KIND_STREAM_STOP 4

#define BIO_RECORD_KIND_OVERRUN 5

#define BIO_RECORD_KIND_VOLUME 6

#define BIO_MAX_VOLUME_CHANNELS 32

#define BIO_VOLUME_MIN_Q16 -6291456

#define BIO_VOLUME_MAX_Q16 0

#define BIO_VOLUME_STEP_Q16 32768

/**
 * Record kinds at or above this value belong to dump recorders, never the
 * driver contract. The driver must not emit them into a READ stream.
 */
#define BIO_RECORD_KIND_RECORDER_MIN 32768

/**
 * Dump-format-2 marker written after every READ returned to a recorder.
 */
#define BIO_RECORD_KIND_READ_END 32769

/**
 * Dump recorder marker reporting READ bytes the recorder could not retain.
 */
#define BIO_RECORD_KIND_RECORDER_GAP 32770

/**
 * Marker records do not belong to a render endpoint.
 */
#define BIO_RECORD_ENDPOINT_RECORDER 65535

/**
 * Endpoint numbers in [`RecordHeader::endpoint`] and the per-endpoint arrays.
 * ADR 0028 assigns 0 to primary, 1 to music, 2 to passthrough, and reserves 3.
 */
#define BIO_ENDPOINT_PRIMARY 0

#define BIO_ENDPOINT_SECOND 1

/**
 * ADR 0028: exclusive streams only (bitstreams and exclusive PCM).
 */
#define BIO_ENDPOINT_PASSTHROUGH 2

/**
 * Contract slots for render endpoints, including reserved endpoint 3.
 */
#define BIO_MAX_ENDPOINTS 4

/**
 * Data record flag: the bytes do not follow the previous data record. The
 * kernel sets it after an overrun, a source gap, and on the first data returned
 * to a newly opened handle.
 */
#define BIO_RECORD_FLAG_DISCONTINUITY 1

/**
 * State-record flag: this is not a new event; it restates the endpoint's
 * current format, volume or streaming state. A repeated format carries the current
 * epoch instead of incrementing it.
 */
#define BIO_RECORD_FLAG_REPEAT 2

/**
 * Every record starts at a multiple of this many bytes; the gap after a
 * payload is zero padding.
 */
#define BIO_RECORD_ALIGN 8

/**
 * Largest payload of one data record. The driver splits a longer span of the
 * audio engine's buffer into several records, so a READ buffer of
 * `record_size(MAX_DATA_PAYLOAD)` bytes always makes progress.
 */
#define BIO_MAX_DATA_PAYLOAD (64 * 1024)

/**
 * Most bytes one READ returns. The driver copies records out under a spin
 * lock and keeps that copy short; a reader behind by more drains it over
 * several READs.
 */
#define BIO_READ_MAX_BYTES (256 * 1024)

#define BIO_TRIM_LIMIT_PPM 2000

/**
 * The 18 ksmedia.h speaker positions, FRONT_LEFT through TOP_BACK_RIGHT.
 */
#define BIO_PCM_SPEAKER_BITS 262143

#define BIO_MAX_PCM_CHANNELS 32

/**
 * Codec mask: bits 1..8 retain their values; bit 0 and bits 9..63 are reserved.
 */
#define BIO_FORMAT_AC3 (1 << 1)

#define BIO_FORMAT_EAC3 (1 << 2)

#define BIO_FORMAT_EAC3_ATMOS (1 << 3)

#define BIO_FORMAT_TRUEHD (1 << 4)

#define BIO_FORMAT_MAT20 (1 << 5)

#define BIO_FORMAT_MAT21 (1 << 6)

#define BIO_FORMAT_DTS (1 << 7)

#define BIO_FORMAT_DTS_HD (1 << 8)

#define BIO_COMPILED_CODECS 510

#define BIO_DEFAULT_ENABLED_CODECS 108

/**
 * Feature bits for [`AbiInfo::features`].
 */
#define BIO_FEATURE_PENDING_READ (1 << 0)

#define BIO_FEATURE_RATE_TRIM (1 << 1)

#define BIO_FEATURE_WRITE_PACKET (1 << 2)

#define BIO_ENDPOINT_STATE_STREAMING (1 << 0)

/**
 * 0: recorded before records carried an endpoint (every record is the primary's).
 * 1: records carry `endpoint` but not READ boundaries.
 * 2: each READ's bytes are followed by a [`RECORD_KIND_READ_END`] marker.
 * 3: volume records carry up to 32 channels (136-byte payload). A volume
 *    record in an older dump has the 72-byte, 16-channel payload; readers
 *    still accept it and zero the remaining levels.
 */
#define BIO_DUMP_FORMAT_VERSION 3

typedef struct BIO_PCM_FORMATS {
  /**
   * Bit i selects `PCM_RATES[i]`.
   */
  uint32_t rates;
  /**
   * Bit i selects PcmShape i.
   */
  uint32_t shapes;
  /**
   * Nonzero masks use only PCM_SPEAKER_BITS, have nChannels set bits,
   * and no more than this many channels (zero disables masked PCM).
   */
  uint32_t max_masked_channels;
  /**
   * Bit c-1 permits c channels with mask zero (direct out, no speaker meaning).
   */
  uint32_t direct_channels;
} BIO_PCM_FORMATS;

typedef struct BIO_ENDPOINT_FORMATS {
  uint64_t codecs;
  struct BIO_PCM_FORMATS pcm;
} BIO_ENDPOINT_FORMATS;

/**
 * Header of one record. The payload (`len` bytes) follows immediately.
 *
 * * Data: the bytes the audio engine wrote, in the epoch's format.
 * * FormatChange: the WAVEFORMATEX(TENSIBLE) Windows negotiated, verbatim.
 * * StreamStart / StreamStop: no payload.
 * * Overrun: [`OverrunPayload`].
 * * Volume: [`VolumePayload`].
 */
typedef struct BIO_RECORD_HEADER {
  uint16_t kind;
  uint16_t flags;
  /**
   * +1 with every format-change record (the record carries the new
   * epoch), except one flagged [`RECORD_FLAG_REPEAT`], which keeps it.
   */
  uint32_t epoch;
  /**
   * Bytes of data since the stream started, at the first payload byte.
   */
  uint64_t byte_pos;
  /**
   * QPC at which the virtual DMA passed the last byte of this record.
   */
  uint64_t qpc;
  /**
   * QPC at which the audio engine wrote the bytes, when write-packet
   * notification is implemented (feature bit `FEATURE_WRITE_PACKET`); else 0.
   */
  uint64_t qpc_written;
  /**
   * Payload length in bytes, excluding padding.
   */
  uint32_t len;
  /**
   * The render endpoint that emitted this record. Epoch, `byte_pos`, start,
   * stop, format change, and overrun state are counted per endpoint. Old
   * records have zero here and therefore belong to the primary endpoint.
   */
  uint16_t endpoint;
  /**
   * Writers write zero; readers ignore this field for forward compatibility.
   */
  uint16_t reserved;
} BIO_RECORD_HEADER;

/**
 * Payload of an overrun record.
 */
typedef struct BIO_OVERRUN_PAYLOAD {
  uint64_t lost_bytes;
} BIO_OVERRUN_PAYLOAD;

/**
 * Endpoint hardware volume state. Little-endian, 136 bytes; records and their
 * payloads start at 8-byte boundaries. Unused levels and mute bits are zero.
 * `channel_count` is the endpoint's default device-format channel count (1..=32),
 * not the currently negotiated stream's channel count. Levels are dB * 65536.
 */
typedef struct BIO_VOLUME_PAYLOAD {
  uint32_t channel_count;
  uint32_t mute_mask;
  int32_t level_q16[BIO_MAX_VOLUME_CHANNELS];
} BIO_VOLUME_PAYLOAD;

typedef struct BIO_READ_REQUEST {
  uint32_t min_bytes;
  uint32_t timeout_ms;
} BIO_READ_REQUEST;

typedef struct BIO_RATE_TRIM {
  /**
   * Parts per million × 65536; positive runs the virtual DMA faster.
   */
  int32_t ppm_q16;
  uint32_t reserved;
} BIO_RATE_TRIM;

typedef struct BIO_ABI_INFO {
  uint32_t abi_version;
  /**
   * `sizeof(AbiInfo)` as the driver knows it; fields added later go after.
   */
  uint32_t struct_size;
  /**
   * Driver build as `major << 16 | minor`.
   */
  uint32_t driver_version;
  uint32_t ring_bytes;
  struct BIO_ENDPOINT_FORMATS compiled;
  uint64_t features;
  /**
   * `KeQueryPerformanceCounter` frequency, so records' QPCs convert to time.
   */
  uint64_t qpc_frequency;
  /**
   * Bit i is one when the driver exposes endpoint i.
   */
  uint32_t endpoint_mask;
  uint32_t reserved;
  /**
   * Registry capabilities per endpoint, indexed by endpoint number; zero
   * for endpoints absent from [`Self::endpoint_mask`].
   */
  struct BIO_ENDPOINT_FORMATS endpoints[BIO_MAX_ENDPOINTS];
} BIO_ABI_INFO;

typedef struct BIO_ENDPOINT_STATS {
  uint64_t total_bytes;
  uint64_t overruns;
  uint64_t lost_bytes;
  /**
   * The endpoint's current epoch.
   */
  uint32_t epoch;
  /**
   * `ENDPOINT_STATE_*` bits.
   */
  uint32_t state;
} BIO_ENDPOINT_STATS;

typedef struct BIO_STATS {
  uint32_t struct_size;
  uint32_t open_count;
  /**
   * Worst lateness of the virtual DMA timer, microseconds; all endpoints share the timer.
   */
  uint32_t max_timer_lateness_us;
  uint32_t reserved;
  struct BIO_ENDPOINT_STATS endpoints[BIO_MAX_ENDPOINTS];
} BIO_STATS;

/**
 * Header of a diagnostic recording. The record stream follows; format 2 adds
 * recorder-only boundary and loss markers that the driver never emits.
 */
typedef struct BIO_DUMP_HEADER {
  uint8_t magic[8];
  uint32_t abi_version;
  uint32_t header_size;
  /**
   * QPC frequency of the machine that recorded, for latency figures.
   */
  uint64_t qpc_frequency;
  uint32_t format_version;
  uint32_t reserved;
} BIO_DUMP_HEADER;

/**
 * PCM sample rates; bit i of `PcmFormats::rates` is `PCM_RATES[i]`.
 */
static const uint32_t BIO_PCM_RATES[6] = { 44100, 48000, 88200, 96000, 176400, 192000, };

static const BIO_PCM_FORMATS BIO_DEFAULT_PCM = { 63, 31, 18, 4294967295U };

static const BIO_PCM_FORMATS BIO_COMPILED_PCM = { 63, 31, 18, 4294967295U };

#define BIO_CONTROL_DEVICE_PATH "\\\\.\\SpatialSoftReceiver"

#if defined(__cplusplus)
#define BIO_STATIC_ASSERT(e, m) static_assert(e, m)
#elif defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L
#define BIO_STATIC_ASSERT(e, m) _Static_assert(e, m)
#else
#define BIO_ASSERT_CAT_(a, b) a##b
#define BIO_ASSERT_CAT(a, b) BIO_ASSERT_CAT_(a, b)
#define BIO_STATIC_ASSERT(e, m) typedef char BIO_ASSERT_CAT(bio_static_assert_, __LINE__)[(e) ? 1 : -1]
#endif

BIO_STATIC_ASSERT(sizeof(BIO_RECORD_HEADER) == 40, "BIO_RECORD_HEADER size");
BIO_STATIC_ASSERT(offsetof(BIO_RECORD_HEADER, kind) == 0, "BIO_RECORD_HEADER.kind offset");
BIO_STATIC_ASSERT(offsetof(BIO_RECORD_HEADER, flags) == 2, "BIO_RECORD_HEADER.flags offset");
BIO_STATIC_ASSERT(offsetof(BIO_RECORD_HEADER, epoch) == 4, "BIO_RECORD_HEADER.epoch offset");
BIO_STATIC_ASSERT(offsetof(BIO_RECORD_HEADER, byte_pos) == 8, "BIO_RECORD_HEADER.byte_pos offset");
BIO_STATIC_ASSERT(offsetof(BIO_RECORD_HEADER, qpc) == 16, "BIO_RECORD_HEADER.qpc offset");
BIO_STATIC_ASSERT(offsetof(BIO_RECORD_HEADER, qpc_written) == 24, "BIO_RECORD_HEADER.qpc_written offset");
BIO_STATIC_ASSERT(offsetof(BIO_RECORD_HEADER, len) == 32, "BIO_RECORD_HEADER.len offset");
BIO_STATIC_ASSERT(offsetof(BIO_RECORD_HEADER, endpoint) == 36, "BIO_RECORD_HEADER.endpoint offset");
BIO_STATIC_ASSERT(offsetof(BIO_RECORD_HEADER, reserved) == 38, "BIO_RECORD_HEADER.reserved offset");
BIO_STATIC_ASSERT(sizeof(BIO_VOLUME_PAYLOAD) == 136, "BIO_VOLUME_PAYLOAD size");
BIO_STATIC_ASSERT(offsetof(BIO_VOLUME_PAYLOAD, channel_count) == 0, "BIO_VOLUME_PAYLOAD.channel_count offset");
BIO_STATIC_ASSERT(offsetof(BIO_VOLUME_PAYLOAD, mute_mask) == 4, "BIO_VOLUME_PAYLOAD.mute_mask offset");
BIO_STATIC_ASSERT(offsetof(BIO_VOLUME_PAYLOAD, level_q16) == 8, "BIO_VOLUME_PAYLOAD.level_q16 offset");
BIO_STATIC_ASSERT(BIO_MAX_VOLUME_CHANNELS == 32, "volume channels");
BIO_STATIC_ASSERT(sizeof(BIO_OVERRUN_PAYLOAD) == 8, "BIO_OVERRUN_PAYLOAD size");
BIO_STATIC_ASSERT(offsetof(BIO_OVERRUN_PAYLOAD, lost_bytes) == 0, "BIO_OVERRUN_PAYLOAD.lost_bytes offset");
BIO_STATIC_ASSERT(sizeof(BIO_READ_REQUEST) == 8, "BIO_READ_REQUEST size");
BIO_STATIC_ASSERT(offsetof(BIO_READ_REQUEST, min_bytes) == 0, "BIO_READ_REQUEST.min_bytes offset");
BIO_STATIC_ASSERT(offsetof(BIO_READ_REQUEST, timeout_ms) == 4, "BIO_READ_REQUEST.timeout_ms offset");
BIO_STATIC_ASSERT(sizeof(BIO_RATE_TRIM) == 8, "BIO_RATE_TRIM size");
BIO_STATIC_ASSERT(offsetof(BIO_RATE_TRIM, ppm_q16) == 0, "BIO_RATE_TRIM.ppm_q16 offset");
BIO_STATIC_ASSERT(offsetof(BIO_RATE_TRIM, reserved) == 4, "BIO_RATE_TRIM.reserved offset");
BIO_STATIC_ASSERT(sizeof(BIO_PCM_FORMATS) == 16, "BIO_PCM_FORMATS size");
BIO_STATIC_ASSERT(offsetof(BIO_PCM_FORMATS, rates) == 0, "BIO_PCM_FORMATS.rates offset");
BIO_STATIC_ASSERT(offsetof(BIO_PCM_FORMATS, shapes) == 4, "BIO_PCM_FORMATS.shapes offset");
BIO_STATIC_ASSERT(offsetof(BIO_PCM_FORMATS, max_masked_channels) == 8, "BIO_PCM_FORMATS.max_masked_channels offset");
BIO_STATIC_ASSERT(offsetof(BIO_PCM_FORMATS, direct_channels) == 12, "BIO_PCM_FORMATS.direct_channels offset");
BIO_STATIC_ASSERT(sizeof(BIO_ENDPOINT_FORMATS) == 24, "BIO_ENDPOINT_FORMATS size");
BIO_STATIC_ASSERT(offsetof(BIO_ENDPOINT_FORMATS, codecs) == 0, "BIO_ENDPOINT_FORMATS.codecs offset");
BIO_STATIC_ASSERT(offsetof(BIO_ENDPOINT_FORMATS, pcm) == 8, "BIO_ENDPOINT_FORMATS.pcm offset");
BIO_STATIC_ASSERT(sizeof(BIO_ABI_INFO) == 160, "BIO_ABI_INFO size");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, abi_version) == 0, "BIO_ABI_INFO.abi_version offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, struct_size) == 4, "BIO_ABI_INFO.struct_size offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, driver_version) == 8, "BIO_ABI_INFO.driver_version offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, ring_bytes) == 12, "BIO_ABI_INFO.ring_bytes offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, compiled) == 16, "BIO_ABI_INFO.compiled offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, features) == 40, "BIO_ABI_INFO.features offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, qpc_frequency) == 48, "BIO_ABI_INFO.qpc_frequency offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, endpoint_mask) == 56, "BIO_ABI_INFO.endpoint_mask offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, reserved) == 60, "BIO_ABI_INFO.reserved offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, endpoints) == 64, "BIO_ABI_INFO.endpoints offset");
BIO_STATIC_ASSERT(sizeof(BIO_ENDPOINT_STATS) == 32, "BIO_ENDPOINT_STATS size");
BIO_STATIC_ASSERT(offsetof(BIO_ENDPOINT_STATS, total_bytes) == 0, "BIO_ENDPOINT_STATS.total_bytes offset");
BIO_STATIC_ASSERT(offsetof(BIO_ENDPOINT_STATS, overruns) == 8, "BIO_ENDPOINT_STATS.overruns offset");
BIO_STATIC_ASSERT(offsetof(BIO_ENDPOINT_STATS, lost_bytes) == 16, "BIO_ENDPOINT_STATS.lost_bytes offset");
BIO_STATIC_ASSERT(offsetof(BIO_ENDPOINT_STATS, epoch) == 24, "BIO_ENDPOINT_STATS.epoch offset");
BIO_STATIC_ASSERT(offsetof(BIO_ENDPOINT_STATS, state) == 28, "BIO_ENDPOINT_STATS.state offset");
BIO_STATIC_ASSERT(sizeof(BIO_STATS) == 144, "BIO_STATS size");
BIO_STATIC_ASSERT(offsetof(BIO_STATS, struct_size) == 0, "BIO_STATS.struct_size offset");
BIO_STATIC_ASSERT(offsetof(BIO_STATS, open_count) == 4, "BIO_STATS.open_count offset");
BIO_STATIC_ASSERT(offsetof(BIO_STATS, max_timer_lateness_us) == 8, "BIO_STATS.max_timer_lateness_us offset");
BIO_STATIC_ASSERT(offsetof(BIO_STATS, reserved) == 12, "BIO_STATS.reserved offset");
BIO_STATIC_ASSERT(offsetof(BIO_STATS, endpoints) == 16, "BIO_STATS.endpoints offset");
BIO_STATIC_ASSERT(sizeof(BIO_DUMP_HEADER) == 32, "BIO_DUMP_HEADER size");
BIO_STATIC_ASSERT(offsetof(BIO_DUMP_HEADER, magic) == 0, "BIO_DUMP_HEADER.magic offset");
BIO_STATIC_ASSERT(offsetof(BIO_DUMP_HEADER, abi_version) == 8, "BIO_DUMP_HEADER.abi_version offset");
BIO_STATIC_ASSERT(offsetof(BIO_DUMP_HEADER, header_size) == 12, "BIO_DUMP_HEADER.header_size offset");
BIO_STATIC_ASSERT(offsetof(BIO_DUMP_HEADER, qpc_frequency) == 16, "BIO_DUMP_HEADER.qpc_frequency offset");
BIO_STATIC_ASSERT(offsetof(BIO_DUMP_HEADER, format_version) == 24, "BIO_DUMP_HEADER.format_version offset");
BIO_STATIC_ASSERT(offsetof(BIO_DUMP_HEADER, reserved) == 28, "BIO_DUMP_HEADER.reserved offset");
BIO_STATIC_ASSERT(BIO_MAX_ENDPOINTS == 4, "BIO_MAX_ENDPOINTS value");

#endif /* BIOATMOS_ABI_H */
