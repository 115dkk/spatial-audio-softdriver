// SPDX-License-Identifier: MS-PL
// Copyright (c) 2026 115dkk
// Driver core: ring, READ gate, DMA clock and format checks, no kernel APIs (ADR 0027).
#pragma once
#include <stddef.h>
#include <stdint.h>
#include "ssr_abi.h"

namespace ssr {

// Largest WAVEFORMATEX(TENSIBLE) the ring keeps per endpoint to repeat after an
// overrun. WAVEFORMATEXTENSIBLE_IEC61937 is 64 bytes.
constexpr uint32_t kMaxFormatBytes = 128;

// Contract limits (ssr-abi wire.rs) as unsigned values.
constexpr uint32_t kMaxDataPayload = SSR_MAX_DATA_PAYLOAD;
constexpr uint32_t kReadMaxBytes = SSR_READ_MAX_BYTES;

struct EndpointState {
    uint64_t pos;            // data bytes since the endpoint's stream started
    uint64_t total_bytes;    // data bytes ever written
    uint64_t overruns;       // overrun records emitted
    uint64_t lost_bytes;     // data bytes dropped by overwrite
    uint64_t pending_lost;   // dropped since the last overrun record
    uint32_t epoch;
    uint32_t state;          // SSR_ENDPOINT_STATE_* bits
    uint32_t format_bytes;   // 0 until the first format change
    uint8_t  format[kMaxFormatBytes];
    SSR_VOLUME_PAYLOAD volume; // channel_count == 0 until configured
    bool     discontinuity;  // flag the next data record
};

// Fixed-size ring of whole records (ssr-abi wire format). Single producer and
// single consumer, both called under the caller's lock (a spin lock in the
// kernel). Never allocates.
class RecordRing {
public:
    // `storage` must be 8-byte aligned and `bytes` a multiple of SSR_RECORD_ALIGN,
    // at least 64 KiB. Clears all state.
    bool Init(uint8_t* storage, uint32_t bytes);

    // Producer side. `ep` < SSR_MAX_ENDPOINTS (others return false).
    // Each call appends one record (plus, after an overrun on `ep`, an overrun
    // record and repeats of current format, volume, and stream state first). When the ring is full
    // the oldest whole records are overwritten and their data bytes counted
    // as lost for their own endpoint. Returns false only when the record can
    // never fit (bigger than the ring) or the arguments are invalid.
    // Configure once after Init, before exposing endpoints. Default 0 dB/unmuted.
    bool ConfigureVolume(uint16_t ep, uint32_t channels);
    // -1 means all channels; GET -1 reads channel zero (SysVAD convention).
    // SET clamps and quantizes to the advertised half-dB range. All operations
    // share the caller's ring lock; unchanged SETs emit nothing.
    bool SetVolume(uint16_t ep, int32_t channel, int32_t level, uint64_t qpc);
    bool SetMute(uint16_t ep, int32_t channel, bool mute, uint64_t qpc);
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
    // pending_lost, and sets discontinuity. Before the next READ or producer
    // record, the ring queues Overrun, repeated format/volume, and repeated current
    // Start/Stop; the next data carries SSR_RECORD_FLAG_DISCONTINUITY. Returns
    // false only for an invalid endpoint or lost_bytes == 0.
    bool SourceGap(uint16_t ep, uint64_t qpc, uint64_t lost_bytes);

    // Consumer side. Queues every pending endpoint's overrun, repeated format,
    // volume and repeated current Start/Stop before a READ is gated or returned.
    bool PrepareRead(uint64_t qpc);
    uint32_t QueuedBytes() const;      // bytes of whole records waiting
    bool UrgentQueued() const;         // a format change, volume or stream stop waits
    // Moves whole records into `out` (never splits one); returns bytes moved.
    uint32_t Read(uint8_t* out, uint32_t capacity, uint64_t qpc = 0);
    // Bytes of the first queued record (0 if none): a READ whose buffer is
    // smaller can never complete and is failed by the caller.
    uint32_t FirstRecordBytes() const;
    // Drops queued records (the handle closed), then queues each known format,
    // configured volume and current Start/Stop as repeats for the next reader. The first
    // following data for every known endpoint is discontinuous.
    void DiscardQueued(uint64_t qpc = 0);

    const EndpointState& Endpoint(uint16_t ep) const;
    uint32_t CapacityBytes() const;
    void FillStats(SSR_STATS* out, uint32_t open_count, uint32_t max_timer_lateness_us) const;

private:
    bool Emit(uint16_t ep, uint16_t kind, uint16_t flags, uint32_t epoch,
              uint64_t pos, uint64_t qpc, uint64_t qpc_written,
              const uint8_t* a, uint32_t a_len, const uint8_t* b, uint32_t b_len);
    bool MakeRoom(uint32_t bytes);
    bool ChangeVolume(uint16_t ep, const SSR_VOLUME_PAYLOAD& value, uint64_t qpc);
    bool PrepareRecovery(uint16_t ep, uint64_t qpc, uint32_t main_bytes,
                         bool main_is_format_change = false);
    void DropFirst();
    void CopyIn(uint32_t offset, const void* source, uint32_t bytes);
    void CopyOut(uint32_t offset, void* destination, uint32_t bytes) const;
    SSR_RECORD_HEADER HeaderAt(uint32_t offset) const;
    uint32_t RecordBytesAt(uint32_t offset) const;
    static uint32_t RecordBytes(uint32_t payload_bytes);

    uint8_t* storage_ = nullptr;
    uint32_t capacity_ = 0;
    uint32_t head_ = 0;
    uint32_t tail_ = 0;
    uint32_t queued_ = 0;
    uint32_t urgent_ = 0;
    EndpointState endpoints_[SSR_MAX_ENDPOINTS]{};
    bool needs_overrun_[SSR_MAX_ENDPOINTS]{};
    bool needs_repeat_[SSR_MAX_ENDPOINTS]{};
    bool needs_state_repeat_[SSR_MAX_ENDPOINTS]{};
    bool needs_volume_repeat_[SSR_MAX_ENDPOINTS]{};
    bool recovery_prepared_[SSR_MAX_ENDPOINTS]{};
};

// READ completion rule (ADR 0011): complete when `min_bytes` of records are
// queued, a format change, volume or stop is queued, or the deadline passes.
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

// Portable unsigned 128-bit arithmetic. These operations use only uint64_t so
// the same core builds for x64 and ARM64 MSVC kernel targets.
struct U128 {
    uint64_t high;
    uint64_t low;
};
U128 U128Multiply(uint64_t a, uint64_t b);
U128 U128Add(U128 a, U128 b);
int U128Compare(U128 a, U128 b);
U128 U128Subtract(U128 a, U128 b);
U128 U128Divide(U128 numerator, U128 denominator, U128* remainder);

// ceil(a * b / c). Division by zero and a quotient wider than uint64_t
// saturate to UINT64_MAX.
uint64_t MulDivCeil(uint64_t a, uint64_t b, uint64_t c);

// Virtual DMA position with the rate trim (ADR 0011, 0027). Exact unsigned
// 128-bit rational arithmetic, without floating point or compiler-specific
// integer types. The rational and block-alignment remainders carry across
// calls and trim changes, so elapsed time is not discarded.
class DmaClock {
public:
    void Start(uint64_t qpc_now, uint64_t qpc_frequency, uint32_t bytes_per_second,
               uint32_t block_align);
    // ppm × 65536, clamped to ±SSR_TRIM_LIMIT_PPM × 65536; returns the value applied.
    int32_t SetTrim(int32_t ppm_q16);
    int32_t Trim() const;
    // Bytes the DMA has advanced since the last call, a multiple of
    // `block_align`; the fraction is carried. Time going backwards yields 0.
    uint64_t Advance(uint64_t qpc_now);

private:
    uint64_t last_qpc_ = 0;
    uint64_t qpc_frequency_ = 0;
    uint64_t bytes_per_second_ = 0;
    U128 remainder_{};
    uint64_t unaligned_bytes_ = 0;
    uint32_t block_align_ = 1;
    int32_t trim_ = 0;
};

// Codec classification keeps the IEC 61937 GUID families and aliases from wave.rs.
// PCM is a product of rate, sample shape and channel rules, never a codec bit.
uint64_t CodecBit(const void* wfx, uint32_t bytes);
bool PcmAllowed(const SSR_PCM_FORMATS& enabled, const void* wfx, uint32_t bytes);
bool FormatAllowed(const SSR_ENDPOINT_FORMATS& enabled, const void* wfx, uint32_t bytes);

struct PcmCandidate {
    uint16_t channels;
    uint32_t rate;
    uint32_t channel_mask;
    uint16_t bits;
    uint16_t valid_bits;
    bool floating;
};
using PcmCandidateAccept = bool (*)(const PcmCandidate&, void* context);
bool ChoosePcmIntersection(const SSR_PCM_FORMATS& pcm, const PcmCandidate& oem,
                           PcmCandidateAccept accepts, void* context, PcmCandidate* result);

}  // namespace ssr
