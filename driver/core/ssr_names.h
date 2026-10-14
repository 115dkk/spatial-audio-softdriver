// SPDX-License-Identifier: MS-PL
// Copyright (c) 2026 115dkk
#pragma once
#include <stddef.h>
#include <stdint.h>

namespace ssr {
constexpr size_t kEndpointNameCapacity = 32;
// Endpoint-number order; slot 3 is reserved and has no mini-pair or INF label.
inline constexpr char16_t kEndpointNames[][kEndpointNameCapacity] = {
    u"Spatial SoftReceiver", u"Spatial SoftReceiver Music",
    u"Spatial SoftReceiver PASS", u"Spatial SoftReceiver"
};

// REG_SZ payload in UTF-16 code units, including its required final NUL.
// Reject malformed lengths, embedded NULs and wrong types; never truncate.
// Char is WCHAR in the kernel and char16_t in the portable tests.
template <typename Char>
void EndpointName(size_t endpoint, bool stringType, const Char* value,
                  size_t bytes, Char (&output)[kEndpointNameCapacity]) {
    static_assert(sizeof(Char) == 2, "UTF-16 code units");
    size_t units = bytes / sizeof(Char);
    bool valid = stringType && value && bytes % sizeof(Char) == 0 &&
        units >= 2 && units <= kEndpointNameCapacity;
    if (valid) {
        valid = value[units - 1] == 0;
        for (size_t i = 0; i + 1 < units; ++i) if (!value[i]) valid = false;
    }
    const size_t slot = endpoint < sizeof(kEndpointNames) / sizeof(kEndpointNames[0]) ? endpoint : 0;
    for (size_t i = 0; i < kEndpointNameCapacity; ++i)
        output[i] = valid ? (i < units ? value[i] : static_cast<Char>(0)) : static_cast<Char>(kEndpointNames[slot][i]);
}
} // namespace ssr
