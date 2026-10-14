// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/kshelper.cpp at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#include "kshelper.h"
NTSTATUS SsrPropertySize(PPCPROPERTY_REQUEST request, ULONG size) {
    ULONG supplied = request->ValueSize;
    request->ValueSize = size;
    if (!supplied) return STATUS_BUFFER_OVERFLOW;
    if (supplied < size) return STATUS_BUFFER_TOO_SMALL;
    return STATUS_SUCCESS;
}
NTSTATUS SsrBasicSupport(PPCPROPERTY_REQUEST request, ULONG access, ULONG type) {
    ULONG supplied = request->ValueSize;
    if (supplied >= sizeof(KSPROPERTY_DESCRIPTION)) {
        auto description = static_cast<PKSPROPERTY_DESCRIPTION>(request->Value);
        RtlZeroMemory(description, sizeof(*description));
        description->AccessFlags = access;
        description->DescriptionSize = sizeof(*description);
        // No value type: GUID_NULL and 0, as the zeroing left them.
        if (type != VT_ILLEGAL) {
            description->PropTypeSet.Set = KSPROPTYPESETID_General;
            description->PropTypeSet.Id = type;
        }
        request->ValueSize = sizeof(*description);
        return STATUS_SUCCESS;
    }
    if (supplied >= sizeof(ULONG)) {
        *static_cast<PULONG>(request->Value) = access;
        request->ValueSize = sizeof(ULONG);
        return STATUS_SUCCESS;
    }
    request->ValueSize = 0;
    return STATUS_BUFFER_TOO_SMALL;
}
void SsrBuildFormat(const SsrFormatEntry& entry, PKSDATAFORMAT_WAVEFORMATEXTENSIBLE output) {
    RtlZeroMemory(output, sizeof(*output));
    output->DataFormat.FormatSize = sizeof(*output);
    output->DataFormat.MajorFormat = KSDATAFORMAT_TYPE_AUDIO;
    output->DataFormat.SubFormat = entry.Subtype;
    output->DataFormat.Specifier = KSDATAFORMAT_SPECIFIER_WAVEFORMATEX;
    auto& wave = output->WaveFormatExt;
    wave.Format.wFormatTag = WAVE_FORMAT_EXTENSIBLE;
    wave.Format.nChannels = entry.Channels;
    wave.Format.nSamplesPerSec = entry.Rate;
    wave.Format.nBlockAlign = entry.Channels * (entry.Bits / 8);
    wave.Format.nAvgBytesPerSec = entry.Rate * wave.Format.nBlockAlign;
    wave.Format.wBitsPerSample = entry.Bits;
    wave.Format.cbSize = sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX);
    wave.Samples.wValidBitsPerSample = entry.ValidBits;
    wave.dwChannelMask = entry.ChannelMask;
    wave.SubFormat = entry.Subtype;
    // A fixed-size sample: 0 here means a variable sample size. PortCls's own
    // intersection handler sets the frame size, and the endpoint builder
    // found no usable host pin while ours left it 0.
    output->DataFormat.SampleSize = wave.Format.nBlockAlign;
}
// A bare KSDATAFORMAT, with no WAVEFORMATEX after it, asks whether the pin
// takes that format type at all: the endpoint builder proposes PCM, AC3 and
// WMA Pro this way on a render pin before it lists the endpoint's formats.
bool SsrFormatTypeAllowed(const SSR_ENDPOINT_FORMATS& formats, const KSDATAFORMAT* data) {
    if (!IsEqualGUIDAligned(data->MajorFormat, KSDATAFORMAT_TYPE_AUDIO) ||
        !IsEqualGUIDAligned(data->Specifier, KSDATAFORMAT_SPECIFIER_WAVEFORMATEX)) return false;
    if (IsEqualGUIDAligned(data->SubFormat, KSDATAFORMAT_SUBTYPE_PCM))
        return SsrPcmEnabled(formats.pcm, false);
    if (IsEqualGUIDAligned(data->SubFormat, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT))
        return SsrPcmEnabled(formats.pcm, true);
    for (const auto& entry : SsrFormats)
        if ((entry.Bit & formats.codecs) && IsEqualGUIDAligned(entry.Subtype, data->SubFormat)) return true;
    return false;
}
NTSTATUS SsrValidateFormat(const SSR_ENDPOINT_FORMATS& formats, PKSDATAFORMAT data, const WAVEFORMATEX** wave) {
    if (!data || data->FormatSize < sizeof(KSDATAFORMAT) + sizeof(WAVEFORMATEX) ||
        !IsEqualGUIDAligned(data->MajorFormat, KSDATAFORMAT_TYPE_AUDIO) ||
        !IsEqualGUIDAligned(data->Specifier, KSDATAFORMAT_SPECIFIER_WAVEFORMATEX)) return STATUS_NO_MATCH;
    auto wfx = reinterpret_cast<const WAVEFORMATEX*>(data + 1);
    ULONG bytes = sizeof(WAVEFORMATEX) + wfx->cbSize;
    if (bytes > ssr::kMaxFormatBytes || bytes > data->FormatSize - sizeof(KSDATAFORMAT)) return STATUS_NO_MATCH;
    bool allowed = ssr::FormatAllowed(formats, wfx, bytes);
    if (!allowed || (wfx->wBitsPerSample != 16 && wfx->wBitsPerSample != 24 && wfx->wBitsPerSample != 32) ||
        wfx->nBlockAlign != wfx->nChannels * (wfx->wBitsPerSample / 8) ||
        static_cast<ULONGLONG>(wfx->nSamplesPerSec) * wfx->nBlockAlign != wfx->nAvgBytesPerSec)
        return STATUS_NO_MATCH;
    GUID subtype;
    ULONG channelMask = 0;
    USHORT validBits = wfx->wBitsPerSample;
    if (wfx->wFormatTag == WAVE_FORMAT_EXTENSIBLE) {
        if (bytes < sizeof(WAVEFORMATEXTENSIBLE)) return STATUS_NO_MATCH;
        auto ext = reinterpret_cast<const WAVEFORMATEXTENSIBLE*>(wfx);
        validBits = ext->Samples.wValidBitsPerSample;
        subtype = ext->SubFormat; channelMask = ext->dwChannelMask;
    } else if (wfx->wFormatTag == WAVE_FORMAT_PCM) subtype = KSDATAFORMAT_SUBTYPE_PCM;
    else if (wfx->wFormatTag == WAVE_FORMAT_IEEE_FLOAT) subtype = KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
    else if (wfx->wFormatTag == WAVE_FORMAT_DOLBY_AC3_SPDIF) subtype = KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_DIGITAL;
    else return STATUS_NO_MATCH;
    if (!IsEqualGUIDAligned(subtype, data->SubFormat)) return STATUS_NO_MATCH;
    if (ssr::PcmAllowed(formats.pcm, wfx, bytes)) {
        *wave = wfx; return STATUS_SUCCESS;
    }
    for (const auto& entry : SsrFormats) {
        if ((entry.Bit & formats.codecs) && IsEqualGUIDAligned(entry.Subtype, subtype) && entry.Channels == wfx->nChannels &&
            entry.Rate == wfx->nSamplesPerSec && entry.Bits == wfx->wBitsPerSample &&
            entry.ValidBits == validBits && (!channelMask || channelMask == entry.ChannelMask)) {
            *wave = wfx; return STATUS_SUCCESS;
        }
    }
    return STATUS_NO_MATCH;
}
bool SsrRangeAccepts(PKSDATARANGE range, const SsrFormatEntry& format) {
    if (range->FormatSize < sizeof(KSDATARANGE_AUDIO)) return false;
    if (!IsEqualGUIDAligned(range->MajorFormat, KSDATAFORMAT_TYPE_AUDIO) && !IsEqualGUIDAligned(range->MajorFormat, KSDATAFORMAT_TYPE_WILDCARD)) return false;
    if (!IsEqualGUIDAligned(range->SubFormat, format.Subtype) && !IsEqualGUIDAligned(range->SubFormat, KSDATAFORMAT_SUBTYPE_WILDCARD)) return false;
    if (!IsEqualGUIDAligned(range->Specifier, KSDATAFORMAT_SPECIFIER_WAVEFORMATEX) && !IsEqualGUIDAligned(range->Specifier, KSDATAFORMAT_SPECIFIER_WILDCARD)) return false;
    auto audio = reinterpret_cast<PKSDATARANGE_AUDIO>(range);
    return audio->MaximumChannels >= format.Channels && audio->MinimumBitsPerSample <= format.Bits &&
        audio->MaximumBitsPerSample >= format.Bits && audio->MinimumSampleFrequency <= format.Rate &&
        audio->MaximumSampleFrequency >= format.Rate;
}

// IMiniport::DataRangeIntersection permits a concrete WAVEFORMATEXTENSIBLE.
// Prefer the OEM device format (stable shared mix); if it is outside either
// range, choose a legal product member, not the holes in a sparse range.
// https://learn.microsoft.com/windows-hardware/drivers/ddi/portcls/nf-portcls-iminiport-datarangeintersection
bool SsrPcmIntersection(const SSR_PCM_FORMATS& pcm, USHORT endpoint,
                       PKSDATARANGE range, PKSDATARANGE matching, SsrFormatEntry* result) {
    struct Context { const SSR_PCM_FORMATS* pcm; PKSDATARANGE range; PKSDATARANGE matching; } context{&pcm, range, matching};
    auto accepts = [](const ssr::PcmCandidate& candidate, void* opaque) {
        auto& c = *static_cast<Context*>(opaque);
        SsrFormatEntry entry{0, candidate.floating ? KSDATAFORMAT_SUBTYPE_IEEE_FLOAT : KSDATAFORMAT_SUBTYPE_PCM,
            candidate.channels, candidate.rate, candidate.channel_mask, candidate.bits, candidate.valid_bits};
        if (!SsrRangeAccepts(c.range, entry) || !SsrRangeAccepts(c.matching, entry)) return false;
        KSDATAFORMAT_WAVEFORMATEXTENSIBLE format;
        SsrBuildFormat(entry, &format);
        return ssr::PcmAllowed(*c.pcm, &format.WaveFormatExt, sizeof(format.WaveFormatExt));
    };
    const auto oem = SsrDefaultPcm(endpoint);
    ssr::PcmCandidate chosen{};
    if (!ssr::ChoosePcmIntersection(pcm,
        {oem.Channels, oem.Rate, oem.ChannelMask, oem.Bits, oem.ValidBits, true},
        accepts, &context, &chosen)) return false;
    *result = {0, chosen.floating ? KSDATAFORMAT_SUBTYPE_IEEE_FLOAT : KSDATAFORMAT_SUBTYPE_PCM,
        chosen.channels, chosen.rate, chosen.channel_mask, chosen.bits, chosen.valid_bits};
    return true;
}
