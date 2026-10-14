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

bool GuidIs(const uint8_t* guid, uint32_t id, uint16_t family) {
    return U32(guid) == id && U16(guid + 4) == family &&
        memcmp(guid + 6, kGuidTail, sizeof(kGuidTail)) == 0;
}

struct Shape { uint16_t bits; uint16_t valid; bool floating; uint32_t bit; };
constexpr Shape kShapes[] = {
    {32, 32, true, SSR_PCM_SHAPE_FLOAT32},
    {32, 32, false, SSR_PCM_SHAPE_INT32},
    {32, 24, false, SSR_PCM_SHAPE_INT24_IN_32},
    {24, 24, false, SSR_PCM_SHAPE_INT24},
    {16, 16, false, SSR_PCM_SHAPE_INT16},
};
constexpr uint16_t kDirect[] = {2, 8, 6, 12, 16, 10, 14, 1, 3, 4, 5};
constexpr uint32_t DirectSet() {
    uint32_t mask = 0;
    for (uint16_t channels : kDirect) mask |= UINT32_C(1) << (channels - 1U);
    return mask;
}
static_assert(DirectSet() == SSR_DIRECT_DEFAULT_CHANNELS, "direct intersection layouts match engine defaults");

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
    for (const auto& carrier : kCodecCarriers)
        if (GuidIs(data + 24, carrier.subtype_id, carrier.subtype_family)) return carrier.bit;
    return 0;
}

bool PcmAllowed(const SSR_PCM_FORMATS& enabled, const void* wfx, uint32_t bytes) {
    if (!wfx || bytes < kWaveFormatExBytes) return false;
    const auto* data = static_cast<const uint8_t*>(wfx);
    const uint32_t declared = kWaveFormatExBytes + U16(data + 16);
    if (declared > bytes) return false;
    const uint16_t channels = U16(data + 2);
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
    if (tag != kPcm && tag != kFloat) return false;
    const uint32_t align = static_cast<uint32_t>(channels) * (bits / 8U);
    const uint32_t rate = U32(data + 4);
    if (U16(data + 12) != align ||
        static_cast<uint64_t>(rate) * align != U32(data + 8)) return false;
    return PcmCandidateAllowed(enabled, {channels, rate, mask, bits, valid, tag == kFloat});
}

bool PcmCandidateAllowed(const SSR_PCM_FORMATS& enabled, const PcmCandidate& candidate) {
    if (!candidate.channels || candidate.channels > SSR_MAX_PCM_CHANNELS) return false;
    bool shapeAllowed = false;
    for (const auto& shape : kShapes)
        if (candidate.bits == shape.bits && candidate.valid_bits == shape.valid &&
            candidate.floating == shape.floating && (enabled.shapes & shape.bit)) shapeAllowed = true;
    if (!shapeAllowed) return false;
    bool rateAllowed = false;
    for (uint32_t i = 0; i < SSR_PCM_RATE_COUNT; ++i)
        if (candidate.rate == SSR_PCM_RATES[i] && (enabled.rates & (1U << i))) rateAllowed = true;
    if (!rateAllowed) return false;
    uint32_t mask = candidate.channel_mask;
    if (!mask) return (enabled.direct_channels & (UINT32_C(1) << (candidate.channels - 1U))) != 0;
    if ((mask & ~static_cast<uint32_t>(SSR_PCM_SPEAKER_BITS)) ||
        candidate.channels > enabled.max_masked_channels) return false;
    uint32_t count = 0;
    for (; mask; mask &= mask - 1U) ++count;
    return count == candidate.channels;
}

bool PcmTypeEnabled(const SSR_PCM_FORMATS& enabled, bool floating) {
    const uint32_t shapes = floating ? SSR_PCM_SHAPE_FLOAT32 : SSR_PCM_SHAPES_INTEGER;
    const uint32_t rates = (UINT32_C(1) << SSR_PCM_RATE_COUNT) - 1U;
    return (enabled.rates & rates) && (enabled.shapes & shapes) &&
        (enabled.max_masked_channels || enabled.direct_channels);
}

bool PcmRange(const SSR_PCM_FORMATS& enabled, bool floating, PcmRangeBounds* out) {
    if (!out || !PcmTypeEnabled(enabled, floating)) return false;
    uint32_t speakerCount = 0;
    for (uint32_t mask = SSR_PCM_SPEAKER_BITS; mask; mask &= mask - 1U) ++speakerCount;
    PcmRangeBounds bounds{enabled.max_masked_channels < speakerCount ?
        enabled.max_masked_channels : speakerCount, UINT32_MAX, 0, UINT32_MAX, 0};
    for (uint32_t c = 1; c <= SSR_MAX_PCM_CHANNELS; ++c)
        if ((enabled.direct_channels & (UINT32_C(1) << (c - 1U))) && c > bounds.max_channels)
            bounds.max_channels = c;
    for (uint32_t r = 0; r < SSR_PCM_RATE_COUNT; ++r) if (enabled.rates & (1U << r)) {
        if (SSR_PCM_RATES[r] < bounds.min_rate) bounds.min_rate = SSR_PCM_RATES[r];
        if (SSR_PCM_RATES[r] > bounds.max_rate) bounds.max_rate = SSR_PCM_RATES[r];
    }
    for (const auto& shape : kShapes) if (shape.floating == floating && (enabled.shapes & shape.bit)) {
        if (shape.bits < bounds.min_bits) bounds.min_bits = shape.bits;
        if (shape.bits > bounds.max_bits) bounds.max_bits = shape.bits;
    }
    *out = bounds;
    return true;
}

bool FormatAllowed(const SSR_ENDPOINT_FORMATS& enabled, const void* wfx, uint32_t bytes) {
    const uint64_t bit = CodecBit(wfx, bytes);
    return (bit != 0 && (enabled.codecs & bit) != 0) || PcmAllowed(enabled.pcm, wfx, bytes);
}

bool FormatTypeAccepted(const SSR_ENDPOINT_FORMATS& enabled, const void* subformat) {
    if (!subformat) return false;
    const auto* guid = static_cast<const uint8_t*>(subformat);
    if (GuidIs(guid, kPcm, kWaveFamily)) return PcmTypeEnabled(enabled.pcm, false);
    if (GuidIs(guid, kFloat, kWaveFamily)) return PcmTypeEnabled(enabled.pcm, true);
    for (const auto& carrier : kCodecCarriers)
        if ((enabled.codecs & carrier.bit) && GuidIs(guid, carrier.subtype_id, carrier.subtype_family)) return true;
    return false;
}

bool FormatAccepted(const SSR_ENDPOINT_FORMATS& enabled, const void* subformat,
                    const void* wfx, uint32_t bytes) {
    if (!subformat || !wfx || bytes < kWaveFormatExBytes) return false;
    const auto* guid = static_cast<const uint8_t*>(subformat);
    const auto* data = static_cast<const uint8_t*>(wfx);
    const uint32_t declared = kWaveFormatExBytes + U16(data + 16);
    if (declared > bytes) return false;
    const uint16_t tag = U16(data);
    const uint16_t channels = U16(data + 2);
    const uint32_t rate = U32(data + 4);
    const uint16_t bits = U16(data + 14);
    if (bits != 16 && bits != 24 && bits != 32) return false;
    const uint32_t align = static_cast<uint32_t>(channels) * (bits / 8U);
    if (U16(data + 12) != align || static_cast<uint64_t>(rate) * align != U32(data + 8)) return false;
    uint32_t mask = 0;
    uint16_t valid = bits;
    if (tag == kExtensible) {
        if (declared < kExtensibleBytes || memcmp(guid, data + 24, 16) != 0) return false;
        valid = U16(data + 18);
        mask = U32(data + 20);
    } else {
        // CodecBit also classifies legacy DTS, but KS never accepted that tag.
        if (tag != kPcm && tag != kFloat && tag != kAc3) return false;
        if (!GuidIs(guid, tag, kWaveFamily)) return false;
    }
    if (GuidIs(guid, kPcm, kWaveFamily) || GuidIs(guid, kFloat, kWaveFamily))
        return PcmAllowed(enabled.pcm, wfx, bytes);
    for (const auto& carrier : kCodecCarriers) {
        if ((enabled.codecs & carrier.bit) && GuidIs(guid, carrier.subtype_id, carrier.subtype_family) &&
            channels == carrier.channels && rate == carrier.rate && bits == carrier.bits &&
            valid == carrier.valid_bits && (!mask || mask == carrier.channel_mask)) return true;
    }
    return false;
}

bool ChoosePcmIntersection(const SSR_PCM_FORMATS& pcm, const PcmCandidate& oem,
                           PcmCandidateAccept accepts, void* context, PcmCandidate* result) {
    if (!accepts || !result) return false;
    const uint32_t rates[] = {SSR_PCM_RATES[1], SSR_PCM_RATES[0], SSR_PCM_RATES[3],
                              SSR_PCM_RATES[2], SSR_PCM_RATES[5], SSR_PCM_RATES[4]};
    static_assert(sizeof(rates) / sizeof(rates[0]) == SSR_PCM_RATE_COUNT, "rate preference covers all rates");
    const struct Layout { uint16_t channels; uint32_t mask; } layouts[] = {
        {8, 0x63f}, {6, 0x3f}, {6, 0x60f}, {12, 0x2d63f},
        {8, 0xff}, {4, 0x33}, {2, 0x3}, {1, 0x4}};
    auto try_candidate = [&](const PcmCandidate& candidate) {
        if (PcmCandidateAllowed(pcm, candidate) && accepts(candidate, context)) {
            *result = candidate;
            return true;
        }
        return false;
    };
    if (try_candidate(oem)) return true;
    for (const auto& shape : kShapes) {
        PcmCandidate candidate{oem.channels, oem.rate, oem.channel_mask,
                               shape.bits, shape.valid, shape.floating};
        if (candidate.bits == oem.bits && candidate.valid_bits == oem.valid_bits &&
            candidate.floating == oem.floating) continue;
        if (try_candidate(candidate)) return true;
    }
    for (uint32_t rate : rates) {
        if (rate == oem.rate) continue;
        for (const auto& shape : kShapes)
            if (try_candidate({oem.channels, rate, oem.channel_mask,
                               shape.bits, shape.valid, shape.floating})) return true;
    }
    for (const auto& layout : layouts) {
        if (layout.channels > pcm.max_masked_channels) continue;
        for (uint32_t rate : rates) for (const auto& shape : kShapes)
            if (try_candidate({layout.channels, rate, layout.mask,
                               shape.bits, shape.valid, shape.floating})) return true;
    }
    for (uint16_t channels : kDirect) {
        if (!(pcm.direct_channels & (UINT32_C(1) << (channels - 1U)))) continue;
        for (uint32_t rate : rates) for (const auto& shape : kShapes)
            if (try_candidate({channels, rate, 0, shape.bits, shape.valid, shape.floating})) return true;
    }
    return false;
}

}  // namespace ssr
