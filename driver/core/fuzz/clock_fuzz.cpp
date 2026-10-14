// SPDX-License-Identifier: MS-PL
// libFuzzer target for the exact virtual DMA clock.
#include "ssr_core.h"
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#if defined(__SIZEOF_INT128__)
__extension__ typedef unsigned __int128 NativeU128;

namespace {

template <typename T>
bool Take(const uint8_t* data, size_t size, size_t* at, T* value) {
    if (size - *at < sizeof(T)) return false;
    memcpy(value, data + *at, sizeof(T));
    *at += sizeof(T);
    return true;
}

}  // namespace

extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
    size_t at = 0;
    uint64_t frequency;
    uint32_t rate;
    uint32_t alignSeed;
    if (!Take(data, size, &at, &frequency) || !Take(data, size, &at, &rate) ||
        !Take(data, size, &at, &alignSeed)) return 0;
    frequency = frequency % (UINT64_C(1) << 48U) + 1U;
    const uint32_t align = alignSeed % 256U + 1U;
    ssr::DmaClock clock;
    clock.Start(0, frequency, rate, align);
    constexpr uint64_t trimDenominator = UINT64_C(1000000) * 65536U;
    const NativeU128 denominator = static_cast<NativeU128>(frequency) * trimDenominator;
    NativeU128 remainder = 0;
    uint64_t referenceUnaligned = 0;
    uint64_t expectedTotal = 0;
    uint64_t actualTotal = 0;
    uint64_t now = 0;
    while (size - at >= sizeof(int32_t) + sizeof(uint64_t)) {
        int32_t trim;
        uint64_t ticks;
        (void)Take(data, size, &at, &trim);
        (void)Take(data, size, &at, &ticks);
        constexpr int32_t limit = SSR_TRIM_LIMIT_PPM * 65536;
        trim = trim < -limit ? -limit : (trim > limit ? limit : trim);
        ticks %= frequency * 8U + 1U;
        (void)clock.SetTrim(trim);
        now += ticks;
        actualTotal += clock.Advance(now);
        const uint64_t rateNumerator = trim >= 0
            ? trimDenominator + static_cast<uint32_t>(trim)
            : trimDenominator - static_cast<uint32_t>(-static_cast<int64_t>(trim));
        const NativeU128 numerator = static_cast<NativeU128>(ticks) * rate * rateNumerator + remainder;
        const uint64_t bytes = static_cast<uint64_t>(numerator / denominator);
        remainder = numerator % denominator;
        if (bytes > UINT64_MAX - referenceUnaligned) return 0;
        referenceUnaligned += bytes;
        const uint64_t aligned = (referenceUnaligned / align) * align;
        referenceUnaligned -= aligned;
        if (aligned > UINT64_MAX - expectedTotal) return 0;
        expectedTotal += aligned;
        if (actualTotal != expectedTotal) abort();
    }
    return 0;
}
#else
extern "C" int LLVMFuzzerTestOneInput(const uint8_t*, size_t) { return 0; }
#endif
