// SPDX-License-Identifier: MS-PL
// Copyright (c) 2026 115dkk
// Driver core: ring, READ gate, DMA clock and format checks, no kernel APIs (ADR 0027).
#pragma once
#include <stddef.h>
#include <stdint.h>
#include "bioatmos_abi.h"

namespace bio {

// Largest WAVEFORMATEX(TENSIBLE) the ring keeps per endpoint to repeat after an
// overrun. WAVEFORMATEXTENSIBLE_IEC61937 is 64 bytes.
constexpr uint32_t kMaxFormatBytes = 128;

// Contract limits (bio-abi wire.rs) as unsigned values.
constexpr uint32_t kMaxDataPayload = BIO_MAX_DATA_PAYLOAD;
constexpr uint32_t kReadMaxBytes = BIO_READ_MAX_BYTES;

struct EndpointState {
    uint64_t pos;            // data bytes since the endpoint's stream started
    uint64_t total_bytes;    // data bytes ever written
    uint64_t overruns;       // overrun records emitted
    uint64_t lost_bytes;     // data bytes dropped by overwrite
    uint64_t pending_lost;   // dropped since the last overrun record
    uint32_t epoch;
    uint32_t state;          // BIO_ENDPOINT_STATE_* bits
    uint32_t format_bytes;   // 0 until the first format change
    uint8_t  format[kMaxFormatBytes];
    bool     discontinuity;  // flag the next data record
};

// Fixed-size ring of whole records (bio-abi wire format). Single producer and
// single consumer, both called under the caller's lock (a spin lock in the
// kernel). Never allocates.
class RecordRing {
public:
    // `storage` must be 8-byte aligned and `bytes` a multiple of BIO_RECORD_ALIGN,
    // at least 64 KiB. Clears all state.
    bool Init(uint8_t* storage, uint32_t bytes);

    // Producer side. `ep` < BIO_MAX_ENDPOINTS (others return false).
    // Each call appends one record (plus, after an overrun on `ep`, an overrun
    // record and a repeat of the current format first). When the ring is full
    // the oldest whole records are overwritten and their data bytes counted
    // as lost for their own endpoint. Returns false only when the record can
    // never fit (bigger than the ring) or the arguments are invalid.
    bool StreamStart(uint16_t ep, uint64_t qpc);
    bool StreamStop(uint16_t ep, uint64_t qpc);
    // Bumps the endpoint's epoch, keeps a copy of `wfx` for repeats, and
    // resets its position to 0.
    bool FormatChange(uint16_t ep, uint64_t qpc, const void* wfx, uint32_t wfx_bytes);
    // Two spans so a DMA buffer wrap needs no copy; `b` may be null/0. Together
    // at most kMaxDataPayload bytes (more returns false): the caller splits.
    bool Data(uint16_t ep, uint64_t qpc, uint64_t qpc_written,
              const uint8_t* a, uint32_t a_len, const uint8_t* b, uint32_t b_len);
    // The audio engine's bytes for `lost_bytes` were lost before the driver
    // copied them (the DMA timer ran more than a buffer lap late). Advances the
    // endpoint's pos by lost_bytes, adds them to total_bytes, lost_bytes and
    // pending_lost, and sets discontinuity, so the endpoint's next record is
    // preceded by an overrun record and a FORMAT_REPEAT, and its next data
    // carries BIO_RECORD_FLAG_DISCONTINUITY. Writes nothing by itself. Returns
    // false only for an invalid endpoint or lost_bytes == 0.
    bool SourceGap(uint16_t ep, uint64_t qpc, uint64_t lost_bytes);

    // Consumer side.
    uint32_t QueuedBytes() const;      // bytes of whole records waiting
    bool UrgentQueued() const;         // a format change or stream stop waits
    // Moves whole records into `out` (never splits one); returns bytes moved.
    uint32_t Read(uint8_t* out, uint32_t capacity);
    // Bytes of the first queued record (0 if none): a READ whose buffer is
    // smaller can never complete and is failed by the caller.
    uint32_t FirstRecordBytes() const;
    // Drops queued records (the handle closed). Endpoint counters are kept; an
    // endpoint with a known format repeats it and marks its next data
    // discontinuous for the next reader.
    void DiscardQueued();

    const EndpointState& Endpoint(uint16_t ep) const;
    uint32_t CapacityBytes() const;
    void FillStats(BIO_STATS* out, uint32_t open_count, uint32_t max_timer_lateness_us) const;

private:
    bool Emit(uint16_t ep, uint16_t kind, uint16_t flags, uint32_t epoch,
              uint64_t pos, uint64_t qpc, uint64_t qpc_written,
              const uint8_t* a, uint32_t a_len, const uint8_t* b, uint32_t b_len);
    bool MakeRoom(uint32_t bytes);
    bool PrepareRecovery(uint16_t ep, uint64_t qpc, uint32_t main_bytes,
                         bool main_is_format_change = false);
    void DropFirst();
    void CopyIn(uint32_t offset, const void* source, uint32_t bytes);
    void CopyOut(uint32_t offset, void* destination, uint32_t bytes) const;
    BIO_RECORD_HEADER HeaderAt(uint32_t offset) const;
    uint32_t RecordBytesAt(uint32_t offset) const;
    static uint32_t RecordBytes(uint32_t payload_bytes);

    uint8_t* storage_ = nullptr;
    uint32_t capacity_ = 0;
    uint32_t head_ = 0;
    uint32_t tail_ = 0;
    uint32_t queued_ = 0;
    uint32_t urgent_ = 0;
    EndpointState endpoints_[BIO_MAX_ENDPOINTS]{};
    bool needs_repeat_[BIO_MAX_ENDPOINTS]{};
    bool recovery_prepared_[BIO_MAX_ENDPOINTS]{};
};

// READ completion rule (ADR 0011): complete when `min_bytes` of records are
// queued, a format change or stop is queued, or the deadline passes.
// A min_bytes of 0 completes at once. A min_bytes larger than the caller's
// output buffer is clamped to the buffer.
struct ReadGate {
    static bool ShouldComplete(const RecordRing& ring, uint32_t min_bytes,
                               uint32_t out_capacity, uint64_t now_qpc,
                               uint64_t deadline_qpc);
    // The part of a READ buffer one completion fills: `out_bytes` clamped to
    // kReadMaxBytes. Callers use it for the gate, the size check and Read.
    static uint32_t Capacity(uint32_t out_bytes);
    // Deadline for `timeout_ms` from `now_qpc`; saturates instead of wrapping.
    static uint64_t Deadline(uint64_t now_qpc, uint64_t qpc_frequency, uint32_t timeout_ms);
};

// Virtual DMA position with the rate trim (ADR 0011, 0027). Exact integer
// arithmetic: no floating point in the kernel. Carries the remainder so the
// long-run rate is exact.
class DmaClock {
public:
    void Start(uint64_t qpc_now, uint64_t qpc_frequency, uint32_t bytes_per_second,
               uint32_t block_align);
    // ppm × 65536, clamped to ±BIO_TRIM_LIMIT_PPM × 65536; returns the value applied.
    int32_t SetTrim(int32_t ppm_q16);
    int32_t Trim() const;
    // Bytes the DMA has advanced since the last call, a multiple of
    // `block_align`; the fraction is carried. Time going backwards yields 0.
    uint64_t Advance(uint64_t qpc_now);

private:
    uint64_t last_qpc_ = 0;
    uint64_t qpc_frequency_ = 0;
    uint64_t bytes_per_second_ = 0;
    uint64_t remainder_ = 0;
    uint64_t unaligned_bytes_ = 0;
    uint32_t block_align_ = 1;
    int32_t trim_ = 0;
};

// Maps a WAVEFORMATEX(TENSIBLE) to its BIO_FORMAT_* bit, or 0 when unknown or
// malformed (too short, cbSize inconsistent). The mapping is exactly the one
// crates/bio-abi/src/wave.rs implements (its GUID families and subtype
// numbers are the source of truth): PCM and IEEE float -> BIO_FORMAT_PCM,
// AC-3 (format tag 0x0092 or its subtype) -> AC3, IEC 61937 0x000a -> EAC3,
// 0x010a -> EAC3_ATMOS, 0x000c -> TRUEHD, 0x010c -> MAT20,
// 0x030c and 0x070c -> MAT21, DTS (0x0008) -> DTS, 0x000b and the DTS:X
// subtypes 0x010b, 0x030b -> DTS_HD.
uint64_t FormatBit(const void* wfx, uint32_t bytes);
bool FormatAllowed(uint64_t enabled_mask, const void* wfx, uint32_t bytes);

}  // namespace bio
