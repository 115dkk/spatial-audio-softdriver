// SPDX-License-Identifier: MS-PL
// Integer virtual DMA clock with Q16 ppm rate trim.
#include "ssr_core.h"

namespace ssr {
namespace {

constexpr uint64_t kQ16 = 65536U;
constexpr uint64_t kPpm = 1000000U;
constexpr uint64_t kTrimDenominator = kPpm * kQ16;
constexpr uint64_t kMaxQpcFrequency = UINT64_C(1) << 48U;

U128 Multiply(U128 value, uint64_t factor) {
    const U128 low = U128Multiply(value.low, factor);
    const U128 high = U128Multiply(value.high, factor);
    return {low.high + high.low, low.low};
}

}  // namespace

void DmaClock::Start(uint64_t qpc_now, uint64_t qpc_frequency, uint32_t bytes_per_second,
                     uint32_t block_align) {
    last_qpc_ = qpc_now;
    qpc_frequency_ = qpc_frequency >= 1U && qpc_frequency <= kMaxQpcFrequency
        ? qpc_frequency : 0;
    bytes_per_second_ = bytes_per_second;
    remainder_ = {};
    unaligned_bytes_ = 0;
    block_align_ = block_align == 0 ? 1U : block_align;
    trim_ = 0;
}

int32_t DmaClock::SetTrim(int32_t ppm_q16) {
    constexpr int32_t limit = SSR_TRIM_LIMIT_PPM * static_cast<int32_t>(kQ16);
    if (ppm_q16 > limit) {
        ppm_q16 = limit;
    } else if (ppm_q16 < -limit) {
        ppm_q16 = -limit;
    }
    trim_ = ppm_q16;
    return trim_;
}

int32_t DmaClock::Trim() const {
    return trim_;
}

uint64_t DmaClock::Advance(uint64_t qpc_now) {
    if (qpc_now < last_qpc_) return 0;
    uint64_t ticks = qpc_now - last_qpc_;
    last_qpc_ = qpc_now;
    if (ticks == 0 || qpc_frequency_ == 0 || bytes_per_second_ == 0) return 0;

    uint64_t rate_numerator = kTrimDenominator;
    if (trim_ >= 0) rate_numerator += static_cast<uint32_t>(trim_);
    else rate_numerator -= static_cast<uint32_t>(-(static_cast<int64_t>(trim_)));
    const U128 denominator = U128Multiply(qpc_frequency_, kTrimDenominator);
    uint64_t bytes = 0;
    while (ticks != 0) {
        const uint64_t chunk = ticks < qpc_frequency_ ? ticks : qpc_frequency_;
        U128 numerator = Multiply(U128Multiply(chunk, bytes_per_second_), rate_numerator);
        numerator = U128Add(numerator, remainder_);
        U128 quotient = U128Divide(numerator, denominator, &remainder_);
        if (quotient.high != 0 || quotient.low > UINT64_MAX - bytes) {
            bytes = UINT64_MAX;
            break;
        }
        bytes += quotient.low;
        ticks -= chunk;
    }
    if (bytes > UINT64_MAX - unaligned_bytes_) return 0;
    unaligned_bytes_ += bytes;
    const uint64_t aligned = (unaligned_bytes_ / block_align_) * block_align_;
    unaligned_bytes_ -= aligned;
    return aligned;
}

}  // namespace ssr
