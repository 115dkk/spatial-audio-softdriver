// SPDX-License-Identifier: MS-PL
#include "ssr_core.h"
#include <string.h>

namespace {
// The oracles must not depend on assert(): NDEBUG would remove them.
void Require(bool condition) { if (!condition) __builtin_trap(); }

void CheckAccepted(const SSR_ENDPOINT_FORMATS& enabled, const uint8_t* guid,
                   const uint8_t* wave, uint32_t bytes) {
    (void)ssr::FormatTypeAccepted(enabled, guid);
    if (!ssr::FormatAccepted(enabled, guid, wave, bytes)) return;
    const uint8_t pcm[16] = {1, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71};
    uint8_t floating[16]; memcpy(floating, pcm, sizeof(floating)); floating[0] = 3;
    if (memcmp(guid, pcm, sizeof(pcm)) == 0 || memcmp(guid, floating, sizeof(floating)) == 0)
        Require(ssr::PcmAllowed(enabled.pcm, wave, bytes));
    else {
        const uint64_t bit = ssr::CodecBit(wave, bytes);
        Require(bit != 0 && (enabled.codecs & bit) != 0);
    }
}
}

extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
    if (size > UINT32_MAX) return 0;
    const SSR_ENDPOINT_FORMATS compiled{SSR_COMPILED_CODECS, SSR_COMPILED_PCM};
    (void)ssr::PcmAllowed(compiled.pcm, data, static_cast<uint32_t>(size));
    (void)ssr::FormatAllowed(compiled, data, static_cast<uint32_t>(size));
    if (size >= 16) CheckAccepted(compiled, data, data + 16, static_cast<uint32_t>(size - 16));
    if (size >= 40) CheckAccepted(compiled, data + 24, data, static_cast<uint32_t>(size));
    if (size >= sizeof(SSR_ENDPOINT_FORMATS)) {
        SSR_ENDPOINT_FORMATS enabled;
        memcpy(&enabled, data, sizeof(enabled));
        data += sizeof(enabled);
        size -= sizeof(enabled);
        (void)ssr::PcmAllowed(enabled.pcm, data, static_cast<uint32_t>(size));
        (void)ssr::FormatAllowed(enabled, data, static_cast<uint32_t>(size));
        if (size >= 16) CheckAccepted(enabled, data, data + 16, static_cast<uint32_t>(size - 16));
        if (size >= 40) CheckAccepted(enabled, data + 24, data, static_cast<uint32_t>(size));
    }
    return 0;
}
