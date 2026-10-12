/* Generated from crates/bio-abi/src/wire.rs by crates/bio-abi/tests/c_header.rs.
 * Do not edit: change the Rust side and run the test with BIO_ABI_BLESS=1. */

#ifndef BIOATMOS_ABI_H
#define BIOATMOS_ABI_H

#include <stddef.h>
#include <stdint.h>

/**
 * Bumped only by a frozen-contract revision; v1 is what the signed driver
 * ships with.
 */
#define BIO_ABI_VERSION 1

/**
 * `CTL_CODE(FILE_DEVICE_UNKNOWN, 0x900, METHOD_BUFFERED, FILE_READ_ACCESS)`;
 * output: [`AbiInfo`].
 */
#define IOCTL_BIO_QUERY_INFO (((34 << 16) | (1 << 14)) | (2304 << 2))

/**
 * `CTL_CODE(FILE_DEVICE_UNKNOWN, 0x901, METHOD_OUT_DIRECT, FILE_READ_ACCESS)`;
 * input: [`ReadRequest`], output: a record stream. Pends until `min_bytes` of
 * records are queued, a format-change or stream-stop record arrives, or
 * `timeout_ms` passes. Cancellable.
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

/**
 * Endpoint numbers in [`RecordHeader::endpoint`] and the per-endpoint arrays.
 */
#define BIO_ENDPOINT_PRIMARY 0

#define BIO_ENDPOINT_SECOND 1

/**
 * Render endpoints the v1 driver can expose; sizes the per-endpoint arrays.
 */
#define BIO_MAX_ENDPOINTS 2

/**
 * Data record flag: the bytes do not follow the previous data record
 * (the stream restarted or an overrun dropped bytes in between).
 */
#define BIO_RECORD_FLAG_DISCONTINUITY 1

/**
 * Format-change record flag: the record restates the endpoint's current
 * format after an overrun and carries the current epoch instead of a new one.
 * The ring may have overwritten the format change that opened the epoch, so
 * the driver repeats it before the endpoint's next data; user mode treats it
 * like any format change (reset the decoder, select by format).
 */
#define BIO_RECORD_FLAG_FORMAT_REPEAT 2

/**
 * Every record starts at a multiple of this many bytes; the gap after a
 * payload is zero padding.
 */
#define BIO_RECORD_ALIGN 8

#define BIO_TRIM_LIMIT_PPM 2000

/**
 * Format bits for [`AbiInfo::compiled_formats`] and `enabled_formats` (the
 * registry mask bio-setup writes).
 */
#define BIO_FORMAT_PCM (1 << 0)

#define BIO_FORMAT_AC3 (1 << 1)

#define BIO_FORMAT_EAC3 (1 << 2)

#define BIO_FORMAT_EAC3_ATMOS (1 << 3)

#define BIO_FORMAT_TRUEHD (1 << 4)

#define BIO_FORMAT_MAT20 (1 << 5)

#define BIO_FORMAT_MAT21 (1 << 6)

#define BIO_FORMAT_DTS (1 << 7)

#define BIO_FORMAT_DTS_HD (1 << 8)

/**
 * Feature bits for [`AbiInfo::features`].
 */
#define BIO_FEATURE_PENDING_READ (1 << 0)

#define BIO_FEATURE_RATE_TRIM (1 << 1)

#define BIO_FEATURE_WRITE_PACKET (1 << 2)

#define BIO_ENDPOINT_STATE_STREAMING (1 << 0)

/**
 * 0: recorded before records carried an endpoint (every record is the primary's).
 * 1: records carry `endpoint`.
 */
#define BIO_DUMP_FORMAT_VERSION 1

/**
 * Header of one record. The payload (`len` bytes) follows immediately.
 *
 * * Data: the bytes the audio engine wrote, in the epoch's format.
 * * FormatChange: the WAVEFORMATEX(TENSIBLE) Windows negotiated, verbatim.
 * * StreamStart / StreamStop: no payload.
 * * Overrun: [`OverrunPayload`].
 */
typedef struct BIO_RECORD_HEADER {
  uint16_t kind;
  uint16_t flags;
  /**
   * +1 with every format-change record (the record carries the new
   * epoch), except one flagged [`RECORD_FLAG_FORMAT_REPEAT`], which keeps it.
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
  uint16_t reserved;
} BIO_RECORD_HEADER;

/**
 * Payload of an overrun record.
 */
typedef struct BIO_OVERRUN_PAYLOAD {
  uint64_t lost_bytes;
} BIO_OVERRUN_PAYLOAD;

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
  uint64_t compiled_formats;
  uint64_t features;
  /**
   * `KeQueryPerformanceCounter` frequency, so records' QPCs convert to time.
   */
  uint64_t qpc_frequency;
  /**
   * Endpoints exposed now: 1, or 2 when the service's Parameters key enables the second.
   */
  uint32_t endpoint_count;
  uint32_t reserved;
  /**
   * The registry format mask per endpoint, indexed by endpoint number; 0 past `endpoint_count`.
   */
  uint64_t enabled_formats[BIO_MAX_ENDPOINTS];
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
   * Worst lateness of the virtual DMA timer, microseconds; both endpoints share the timer.
   */
  uint32_t max_timer_lateness_us;
  uint32_t reserved;
  struct BIO_ENDPOINT_STATS endpoints[BIO_MAX_ENDPOINTS];
} BIO_STATS;

/**
 * Header of a diagnostic recording. The record stream follows, exactly as
 * READ returned it.
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
BIO_STATIC_ASSERT(sizeof(BIO_OVERRUN_PAYLOAD) == 8, "BIO_OVERRUN_PAYLOAD size");
BIO_STATIC_ASSERT(offsetof(BIO_OVERRUN_PAYLOAD, lost_bytes) == 0, "BIO_OVERRUN_PAYLOAD.lost_bytes offset");
BIO_STATIC_ASSERT(sizeof(BIO_READ_REQUEST) == 8, "BIO_READ_REQUEST size");
BIO_STATIC_ASSERT(offsetof(BIO_READ_REQUEST, min_bytes) == 0, "BIO_READ_REQUEST.min_bytes offset");
BIO_STATIC_ASSERT(offsetof(BIO_READ_REQUEST, timeout_ms) == 4, "BIO_READ_REQUEST.timeout_ms offset");
BIO_STATIC_ASSERT(sizeof(BIO_RATE_TRIM) == 8, "BIO_RATE_TRIM size");
BIO_STATIC_ASSERT(offsetof(BIO_RATE_TRIM, ppm_q16) == 0, "BIO_RATE_TRIM.ppm_q16 offset");
BIO_STATIC_ASSERT(offsetof(BIO_RATE_TRIM, reserved) == 4, "BIO_RATE_TRIM.reserved offset");
BIO_STATIC_ASSERT(sizeof(BIO_ABI_INFO) == 64, "BIO_ABI_INFO size");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, abi_version) == 0, "BIO_ABI_INFO.abi_version offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, struct_size) == 4, "BIO_ABI_INFO.struct_size offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, driver_version) == 8, "BIO_ABI_INFO.driver_version offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, ring_bytes) == 12, "BIO_ABI_INFO.ring_bytes offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, compiled_formats) == 16, "BIO_ABI_INFO.compiled_formats offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, features) == 24, "BIO_ABI_INFO.features offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, qpc_frequency) == 32, "BIO_ABI_INFO.qpc_frequency offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, endpoint_count) == 40, "BIO_ABI_INFO.endpoint_count offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, reserved) == 44, "BIO_ABI_INFO.reserved offset");
BIO_STATIC_ASSERT(offsetof(BIO_ABI_INFO, enabled_formats) == 48, "BIO_ABI_INFO.enabled_formats offset");
BIO_STATIC_ASSERT(sizeof(BIO_ENDPOINT_STATS) == 32, "BIO_ENDPOINT_STATS size");
BIO_STATIC_ASSERT(offsetof(BIO_ENDPOINT_STATS, total_bytes) == 0, "BIO_ENDPOINT_STATS.total_bytes offset");
BIO_STATIC_ASSERT(offsetof(BIO_ENDPOINT_STATS, overruns) == 8, "BIO_ENDPOINT_STATS.overruns offset");
BIO_STATIC_ASSERT(offsetof(BIO_ENDPOINT_STATS, lost_bytes) == 16, "BIO_ENDPOINT_STATS.lost_bytes offset");
BIO_STATIC_ASSERT(offsetof(BIO_ENDPOINT_STATS, epoch) == 24, "BIO_ENDPOINT_STATS.epoch offset");
BIO_STATIC_ASSERT(offsetof(BIO_ENDPOINT_STATS, state) == 28, "BIO_ENDPOINT_STATS.state offset");
BIO_STATIC_ASSERT(sizeof(BIO_STATS) == 80, "BIO_STATS size");
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

#endif /* BIOATMOS_ABI_H */
