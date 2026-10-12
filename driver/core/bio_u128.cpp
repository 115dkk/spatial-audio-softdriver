// SPDX-License-Identifier: MS-PL
// Portable unsigned 128-bit arithmetic for kernel and user-mode builds.
#include "bio_core.h"

namespace bio {
namespace {

U128 ShiftLeftOne(U128 value) {
    return {static_cast<uint64_t>((value.high << 1U) | (value.low >> 63U)),
            static_cast<uint64_t>(value.low << 1U)};
}

bool Bit(U128 value, uint32_t index) {
    if (index < 64U) return ((value.low >> index) & 1U) != 0;
    return ((value.high >> (index - 64U)) & 1U) != 0;
}

void SetBit(U128* value, uint32_t index) {
    if (index < 64U) value->low |= UINT64_C(1) << index;
    else value->high |= UINT64_C(1) << (index - 64U);
}

}  // namespace

U128 U128Multiply(uint64_t a, uint64_t b) {
    const uint64_t a0 = static_cast<uint32_t>(a);
    const uint64_t a1 = a >> 32U;
    const uint64_t b0 = static_cast<uint32_t>(b);
    const uint64_t b1 = b >> 32U;
    const uint64_t p00 = a0 * b0;
    const uint64_t p01 = a0 * b1;
    const uint64_t p10 = a1 * b0;
    const uint64_t p11 = a1 * b1;
    const uint64_t middle = (p00 >> 32U) + static_cast<uint32_t>(p01) +
                            static_cast<uint32_t>(p10);
    return {p11 + (p01 >> 32U) + (p10 >> 32U) + (middle >> 32U),
            (middle << 32U) | static_cast<uint32_t>(p00)};
}

U128 U128Add(U128 a, U128 b) {
    U128 result{a.high + b.high, a.low + b.low};
    if (result.low < a.low) ++result.high;
    return result;
}

int U128Compare(U128 a, U128 b) {
    if (a.high != b.high) return a.high < b.high ? -1 : 1;
    if (a.low != b.low) return a.low < b.low ? -1 : 1;
    return 0;
}

U128 U128Subtract(U128 a, U128 b) {
    U128 result{a.high - b.high, a.low - b.low};
    if (a.low < b.low) --result.high;
    return result;
}

U128 U128Divide(U128 numerator, U128 denominator, U128* remainder) {
    if (denominator.high == 0 && denominator.low == 0) {
        if (remainder) *remainder = numerator;
        return {UINT64_MAX, UINT64_MAX};
    }
    U128 quotient{};
    U128 current{};
    for (uint32_t index = 128U; index != 0; --index) {
        const bool carry = (current.high >> 63U) != 0;
        current = ShiftLeftOne(current);
        if (Bit(numerator, index - 1U)) current.low |= 1U;
        // `current` is less than denominator before the shift. A carried bit is
        // therefore the only possible 129th bit, and guarantees subtraction.
        if (carry || U128Compare(current, denominator) >= 0) {
            current = U128Subtract(current, denominator);
            SetBit(&quotient, index - 1U);
        }
    }
    if (remainder) *remainder = current;
    return quotient;
}

uint64_t MulDivCeil(uint64_t a, uint64_t b, uint64_t c) {
    if (c == 0) return UINT64_MAX;
    U128 remainder{};
    U128 quotient = U128Divide(U128Multiply(a, b), U128{0, c}, &remainder);
    if (quotient.high != 0) return UINT64_MAX;
    if ((remainder.high != 0 || remainder.low != 0) && quotient.low == UINT64_MAX)
        return UINT64_MAX;
    return quotient.low + ((remainder.high != 0 || remainder.low != 0) ? 1U : 0U);
}

}  // namespace bio
