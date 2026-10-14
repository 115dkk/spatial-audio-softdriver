// SPDX-License-Identifier: MS-PL
#include "ssr_core.h"
#include <string.h>

extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
    if (size > UINT32_MAX) return 0;
    const SSR_ENDPOINT_FORMATS compiled{SSR_COMPILED_CODECS, SSR_COMPILED_PCM};
    (void)ssr::PcmAllowed(compiled.pcm, data, static_cast<uint32_t>(size));
    (void)ssr::FormatAllowed(compiled, data, static_cast<uint32_t>(size));
    if (size >= sizeof(SSR_ENDPOINT_FORMATS)) {
        SSR_ENDPOINT_FORMATS enabled;
        memcpy(&enabled, data, sizeof(enabled));
        data += sizeof(enabled);
        size -= sizeof(enabled);
        (void)ssr::PcmAllowed(enabled.pcm, data, static_cast<uint32_t>(size));
        (void)ssr::FormatAllowed(enabled, data, static_cast<uint32_t>(size));
    }
    return 0;
}
