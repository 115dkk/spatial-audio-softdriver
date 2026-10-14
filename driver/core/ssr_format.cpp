// SPDX-License-Identifier: MS-PL
// WAVEFORMATEX and WAVEFORMATEXTENSIBLE format-mask classification.
#include "ssr_core.h"
#include <string.h>

namespace ssr {
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

uint64_t CodecBit(const void* wfx, uint32_t bytes) {
    if (wfx == nullptr || bytes < kWaveFormatExBytes) {
        return 0;
    }
    const auto* data = static_cast<const uint8_t*>(wfx);
    const uint16_t tag = U16(data);
    const uint32_t declared = kWaveFormatExBytes + U16(data + 16);
    if (declared > bytes) {
        return 0;
    }
    if (tag == kAc3) {
        return SSR_FORMAT_AC3;
    }
    if (tag == kDts) {
        return SSR_FORMAT_DTS;
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
        if (id == kAc3) {
            return SSR_FORMAT_AC3;
        }
        if (id == kDts) {
            return SSR_FORMAT_DTS;
        }
        return 0;
    }
    if (family != kIec61937Family) {
        return 0;
    }
    switch (id) {
        case 0x000a:
            return SSR_FORMAT_EAC3;
        case 0x010a:
            return SSR_FORMAT_EAC3_JOC;
        case 0x000b:
        case 0x010b:  // DTS:X E1
        case 0x030b:  // DTS:X E2
            return SSR_FORMAT_DTS_HD;
        case 0x000c:
            return SSR_FORMAT_TRUEHD;
        case 0x010c:
            return SSR_FORMAT_MAT20;
        case 0x030c:
        case 0x070c:
            return SSR_FORMAT_MAT21;
        default:
            return 0;
    }
}

bool PcmAllowed(const SSR_PCM_FORMATS& enabled, const void* wfx, uint32_t bytes) {
    if (!wfx || bytes < kWaveFormatExBytes) return false;
    const auto* data = static_cast<const uint8_t*>(wfx);
    const uint32_t declared = kWaveFormatExBytes + U16(data + 16);
    if (declared > bytes) return false;
    const uint16_t channels = U16(data + 2);
    if (!channels || channels > SSR_MAX_PCM_CHANNELS) return false;
    const uint16_t bits = U16(data + 14);
    uint16_t valid = bits;
    uint32_t tag = U16(data);
    uint32_t mask;
    if (tag == kExtensible) {
        if (declared < kExtensibleBytes) return false;
        if (U16(data + 28) != kWaveFamily ||
            memcmp(data + 30, kGuidTail, sizeof(kGuidTail)) != 0) return false;
        tag = U32(data + 24);
        valid = U16(data + 18);
        mask = U32(data + 20);
    } else {
        if (channels > 2) return false;
        mask = channels == 1 ? 4U : 3U;
    }
    uint32_t shape;
    if (tag == kFloat && bits == 32 && valid == 32) shape = 4;
    else if (tag != kPcm) return false;
    else if (bits == 16 && valid == 16) shape = 0;
    else if (bits == 24 && valid == 24) shape = 1;
    else if (bits == 32 && valid == 24) shape = 2;
    else if (bits == 32 && valid == 32) shape = 3;
    else return false;
    if (!(enabled.shapes & (1U << shape))) return false;
    const uint32_t align = static_cast<uint32_t>(channels) * (bits / 8U);
    const uint32_t rate = U32(data + 4);
    if (U16(data + 12) != align ||
        static_cast<uint64_t>(rate) * align != U32(data + 8)) return false;
    bool rateAllowed = false;
    for (uint32_t i = 0; i < 6; ++i)
        if (rate == SSR_PCM_RATES[i] && (enabled.rates & (1U << i))) rateAllowed = true;
    if (!rateAllowed) return false;
    if (!mask) return (enabled.direct_channels & (UINT32_C(1) << (channels - 1U))) != 0;
    if ((mask & ~static_cast<uint32_t>(SSR_PCM_SPEAKER_BITS)) || channels > enabled.max_masked_channels) return false;
    uint32_t count = 0;
    for (; mask; mask &= mask - 1U) ++count;
    return count == channels;
}

bool FormatAllowed(const SSR_ENDPOINT_FORMATS& enabled, const void* wfx, uint32_t bytes) {
    const uint64_t bit = CodecBit(wfx, bytes);
    return (bit != 0 && (enabled.codecs & bit) != 0) || PcmAllowed(enabled.pcm, wfx, bytes);
}

bool ChoosePcmIntersection(const SSR_PCM_FORMATS& pcm, const PcmCandidate& oem,
                           PcmCandidateAccept accepts, void* context, PcmCandidate* result) {
    if (!accepts || !result) return false;
    const uint32_t rates[] = {48000, 44100, 96000, 88200, 192000, 176400};
    struct Shape { uint16_t bits; uint16_t valid; bool floating; };
    const Shape shapes[] = {{32, 32, true}, {32, 32, false}, {32, 24, false},
                            {24, 24, false}, {16, 16, false}};
    const struct Layout { uint16_t channels; uint32_t mask; } layouts[] = {
        {8, 0x63f}, {6, 0x3f}, {6, 0x60f}, {12, 0x2d63f},
        {8, 0xff}, {4, 0x33}, {2, 0x3}, {1, 0x4}};
    const uint16_t direct[] = {2, 8, 6, 12, 16, 10, 14, 1, 3, 4, 5};
    auto enabled_candidate = [&](const PcmCandidate& candidate) {
        uint32_t rateBit = 6;
        for (uint32_t i = 0; i < 6; ++i)
            if (candidate.rate == SSR_PCM_RATES[i]) rateBit = i;
        if (rateBit == 6 || !(pcm.rates & (1U << rateBit))) return false;
        uint32_t shapeBit = 5;
        if (candidate.floating && candidate.bits == 32 && candidate.valid_bits == 32) shapeBit = 4;
        else if (!candidate.floating && candidate.bits == 16 && candidate.valid_bits == 16) shapeBit = 0;
        else if (!candidate.floating && candidate.bits == 24 && candidate.valid_bits == 24) shapeBit = 1;
        else if (!candidate.floating && candidate.bits == 32 && candidate.valid_bits == 24) shapeBit = 2;
        else if (!candidate.floating && candidate.bits == 32 && candidate.valid_bits == 32) shapeBit = 3;
        if (shapeBit == 5 || !(pcm.shapes & (1U << shapeBit))) return false;
        if (!candidate.channels || candidate.channels > SSR_MAX_PCM_CHANNELS) return false;
        if (!candidate.channel_mask)
            return (pcm.direct_channels & (UINT32_C(1) << (candidate.channels - 1U))) != 0;
        if ((candidate.channel_mask & ~static_cast<uint32_t>(SSR_PCM_SPEAKER_BITS)) ||
            candidate.channels > pcm.max_masked_channels) return false;
        uint32_t count = 0;
        for (uint32_t mask = candidate.channel_mask; mask; mask &= mask - 1U) ++count;
        return count == candidate.channels;
    };
    auto try_candidate = [&](const PcmCandidate& candidate) {
        if (enabled_candidate(candidate) && accepts(candidate, context)) {
            *result = candidate;
            return true;
        }
        return false;
    };
    if (try_candidate(oem)) return true;
    for (const auto& shape : shapes) {
        PcmCandidate candidate{oem.channels, oem.rate, oem.channel_mask,
                               shape.bits, shape.valid, shape.floating};
        if (candidate.bits == oem.bits && candidate.valid_bits == oem.valid_bits &&
            candidate.floating == oem.floating) continue;
        if (try_candidate(candidate)) return true;
    }
    for (uint32_t rate : rates) {
        if (rate == oem.rate) continue;
        for (const auto& shape : shapes)
            if (try_candidate({oem.channels, rate, oem.channel_mask,
                               shape.bits, shape.valid, shape.floating})) return true;
    }
    for (const auto& layout : layouts) {
        if (layout.channels > pcm.max_masked_channels) continue;
        for (uint32_t rate : rates) for (const auto& shape : shapes)
            if (try_candidate({layout.channels, rate, layout.mask,
                               shape.bits, shape.valid, shape.floating})) return true;
    }
    for (uint16_t channels : direct) {
        if (!(pcm.direct_channels & (UINT32_C(1) << (channels - 1U)))) continue;
        for (uint32_t rate : rates) for (const auto& shape : shapes)
            if (try_candidate({channels, rate, 0, shape.bits, shape.valid, shape.floating})) return true;
    }
    return false;
}

}  // namespace ssr
