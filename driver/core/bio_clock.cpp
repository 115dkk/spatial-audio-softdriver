// SPDX-License-Identifier: MS-PL
// Integer virtual DMA clock with Q16 ppm rate trim.
#include "bio_core.h"

namespace bio {
namespace {

constexpr uint64_t kQ16 = 65536U;
constexpr uint64_t kPpm = 1000000U;
constexpr uint64_t kTrimDenominator = kPpm * kQ16;

uint64_t GreatestCommonDivisor(uint64_t a, uint64_t b) {
    while (b != 0) {
        const uint64_t next = a % b;
        a = b;
        b = next;
    }
    return a;
}

}  // namespace

void DmaClock::Start(uint64_t qpc_now, uint64_t qpc_frequency, uint32_t bytes_per_second,
                     uint32_t block_align) {
    last_qpc_ = qpc_now;
    qpc_frequency_ = qpc_frequency;
    bytes_per_second_ = bytes_per_second;
    remainder_ = 0;
    unaligned_bytes_ = 0;
    block_align_ = block_align == 0 ? 1U : block_align;
    trim_ = 0;
}

int32_t DmaClock::SetTrim(int32_t ppm_q16) {
    constexpr int32_t limit = BIO_TRIM_LIMIT_PPM * static_cast<int32_t>(kQ16);
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
    if (qpc_now < last_qpc_) {
        return 0;
    }
    const uint64_t ticks = qpc_now - last_qpc_;
    last_qpc_ = qpc_now;
    if (ticks == 0 || qpc_frequency_ == 0 || bytes_per_second_ == 0) {
        return 0;
    }

    uint64_t rate_numerator = kTrimDenominator;
    if (trim_ >= 0) {
        rate_numerator += static_cast<uint32_t>(trim_);
    } else {
        rate_numerator -= static_cast<uint32_t>(-(static_cast<int64_t>(trim_)));
    }
    if (qpc_frequency_ > UINT64_MAX / kTrimDenominator) {
        return 0;
    }
    const uint64_t denominator = qpc_frequency_ * kTrimDenominator;

    uint64_t a = ticks;
    uint64_t b = bytes_per_second_;
    uint64_t c = rate_numerator;
    uint64_t reduced_denominator = denominator;
    uint64_t divisor = GreatestCommonDivisor(a, reduced_denominator);
    a /= divisor;
    reduced_denominator /= divisor;
    divisor = GreatestCommonDivisor(b, reduced_denominator);
    b /= divisor;
    reduced_denominator /= divisor;
    divisor = GreatestCommonDivisor(c, reduced_denominator);
    c /= divisor;
    reduced_denominator /= divisor;

    // Supported audio rates and timer intervals fit after reduction. Refuse
    // pathological inputs rather than wrap.
    if ((a != 0 && b > UINT64_MAX / a) ||
        (a * b != 0 && c > UINT64_MAX / (a * b))) {
        return 0;
    }
    const uint64_t reduced_numerator = a * b * c;
    const uint64_t quotient = reduced_numerator / reduced_denominator;
    const uint64_t reduced_fraction = reduced_numerator % reduced_denominator;
    const uint64_t scale = denominator / reduced_denominator;
    if (reduced_fraction != 0 && scale > UINT64_MAX / reduced_fraction) {
        return 0;
    }
    const uint64_t fraction = reduced_fraction * scale;
    uint64_t bytes = quotient;
    if (fraction > UINT64_MAX - remainder_) {
        ++bytes;
        remainder_ = fraction - (UINT64_MAX - remainder_) - 1U;
    } else {
        remainder_ += fraction;
        bytes += remainder_ / denominator;
        remainder_ %= denominator;
    }
    if (bytes > UINT64_MAX - unaligned_bytes_) {
        return 0;
    }
    unaligned_bytes_ += bytes;
    const uint64_t aligned = (unaligned_bytes_ / block_align_) * block_align_;
    unaligned_bytes_ -= aligned;
    return aligned;
}

}  // namespace bio
