// SPDX-License-Identifier: MS-PL
#include "bio_core.h"
#include <string.h>

extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
    if (size > UINT32_MAX) return 0;
    const BIO_ENDPOINT_FORMATS compiled{BIO_COMPILED_CODECS, BIO_COMPILED_PCM};
    (void)bio::PcmAllowed(compiled.pcm, data, static_cast<uint32_t>(size));
    (void)bio::FormatAllowed(compiled, data, static_cast<uint32_t>(size));
    if (size >= sizeof(BIO_ENDPOINT_FORMATS)) {
        BIO_ENDPOINT_FORMATS enabled;
        memcpy(&enabled, data, sizeof(enabled));
        data += sizeof(enabled);
        size -= sizeof(enabled);
        (void)bio::PcmAllowed(enabled.pcm, data, static_cast<uint32_t>(size));
        (void)bio::FormatAllowed(enabled, data, static_cast<uint32_t>(size));
    }
    return 0;
}
