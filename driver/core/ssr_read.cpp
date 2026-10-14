// SPDX-License-Identifier: MS-PL
// Pending READ completion and saturating deadline rules.
#include "ssr_core.h"

namespace ssr {

bool ReadGate::ShouldComplete(const RecordRing& ring, uint32_t min_bytes,
                              uint32_t out_capacity, uint64_t now_qpc,
                              uint64_t deadline_qpc) {
    const uint32_t threshold = min_bytes < out_capacity ? min_bytes : out_capacity;
    return threshold == 0 || ring.QueuedBytes() >= threshold || ring.UrgentQueued() ||
           now_qpc >= deadline_qpc;
}

uint32_t ReadGate::Capacity(uint32_t out_bytes) {
    return out_bytes < kReadMaxBytes ? out_bytes : kReadMaxBytes;
}

uint64_t ReadGate::Deadline(uint64_t now_qpc, uint64_t qpc_frequency, uint32_t timeout_ms) {
    const uint64_t whole = qpc_frequency / 1000U;
    const uint64_t remainder = qpc_frequency % 1000U;
    if (whole != 0 && timeout_ms > UINT64_MAX / whole) {
        return UINT64_MAX;
    }
    const uint64_t base = whole * timeout_ms;
    const uint64_t extra = (remainder * timeout_ms) / 1000U;
    if (base > UINT64_MAX - extra || now_qpc > UINT64_MAX - base - extra) {
        return UINT64_MAX;
    }
    return now_qpc + base + extra;
}

}  // namespace ssr
