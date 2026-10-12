// SPDX-License-Identifier: MS-PL
// WAVEFORMATEX and WAVEFORMATEXTENSIBLE format-mask classification.
#include "bio_core.h"
#include <string.h>

namespace bio {
namespace {

constexpr uint16_t kPcm = 0x0001;
constexpr uint16_t kFloat = 0x0003;
constexpr uint16_t kDts = 0x0008;
constexpr uint16_t kAc3 = 0x0092;
constexpr uint16_t kExtensible = 0xfffe;
constexpr uint16_t kWaveFamily = 0x0000;
constexpr uint16_t kIec61937Family = 0x0cea;
constexpr uint32_t kWaveFormatExBytes = 18;
constexpr uint32_t kExtensibleBytes = 40;
constexpr uint8_t kGuidTail[10] = {0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71};

uint16_t U16(const uint8_t* bytes) {
    return static_cast<uint16_t>(bytes[0]) |
           static_cast<uint16_t>(static_cast<uint16_t>(bytes[1]) << 8U);
}

uint32_t U32(const uint8_t* bytes) {
    return static_cast<uint32_t>(bytes[0]) |
           (static_cast<uint32_t>(bytes[1]) << 8U) |
           (static_cast<uint32_t>(bytes[2]) << 16U) |
           (static_cast<uint32_t>(bytes[3]) << 24U);
}

}  // namespace

uint64_t FormatBit(const void* wfx, uint32_t bytes) {
    if (wfx == nullptr || bytes < kWaveFormatExBytes) {
        return 0;
    }
    const auto* data = static_cast<const uint8_t*>(wfx);
    const uint16_t tag = U16(data);
    const uint32_t declared = kWaveFormatExBytes + U16(data + 16);
    if (declared > bytes) {
        return 0;
    }
    if (tag == kPcm || tag == kFloat) {
        return BIO_FORMAT_PCM;
    }
    if (tag == kAc3) {
        return BIO_FORMAT_AC3;
    }
    if (tag == kDts) {
        return BIO_FORMAT_DTS;
    }
    if (tag != kExtensible || bytes < kExtensibleBytes || declared < kExtensibleBytes) {
        return 0;
    }
    const uint8_t* guid = data + 24;
    if (memcmp(guid + 6, kGuidTail, sizeof(kGuidTail)) != 0) {
        return 0;
    }
    const uint32_t id = U32(guid);
    const uint16_t family = U16(guid + 4);
    if (family == kWaveFamily) {
        if (id == kPcm || id == kFloat) {
            return BIO_FORMAT_PCM;
        }
        if (id == kAc3) {
            return BIO_FORMAT_AC3;
        }
        if (id == kDts) {
            return BIO_FORMAT_DTS;
        }
        return 0;
    }
    if (family != kIec61937Family) {
        return 0;
    }
    switch (id) {
        case 0x000a:
            return BIO_FORMAT_EAC3;
        case 0x010a:
            return BIO_FORMAT_EAC3_ATMOS;
        case 0x000b:
        case 0x010b:  // DTS:X E1
        case 0x030b:  // DTS:X E2
            return BIO_FORMAT_DTS_HD;
        case 0x000c:
            return BIO_FORMAT_TRUEHD;
        case 0x010c:
            return BIO_FORMAT_MAT20;
        case 0x030c:
        case 0x070c:
            return BIO_FORMAT_MAT21;
        default:
            return 0;
    }
}

bool FormatAllowed(uint64_t enabled_mask, const void* wfx, uint32_t bytes) {
    const uint64_t bit = FormatBit(wfx, bytes);
    return bit != 0 && (enabled_mask & bit) != 0;
}

}  // namespace bio
