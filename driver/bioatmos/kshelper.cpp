// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/kshelper.cpp at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#include "kshelper.h"
NTSTATUS BioPropertySize(PPCPROPERTY_REQUEST request, ULONG size) {
    ULONG supplied = request->ValueSize;
    request->ValueSize = size;
    if (!supplied) return STATUS_BUFFER_OVERFLOW;
    if (supplied < size) return STATUS_BUFFER_TOO_SMALL;
    return STATUS_SUCCESS;
}
NTSTATUS BioBasicSupport(PPCPROPERTY_REQUEST request, ULONG access, ULONG type) {
    ULONG supplied = request->ValueSize;
    if (supplied >= sizeof(KSPROPERTY_DESCRIPTION)) {
        auto description = static_cast<PKSPROPERTY_DESCRIPTION>(request->Value);
        RtlZeroMemory(description, sizeof(*description));
        description->AccessFlags = access;
        description->DescriptionSize = sizeof(*description);
        description->PropTypeSet.Set = KSPROPTYPESETID_General;
        description->PropTypeSet.Id = type;
        request->ValueSize = sizeof(*description);
        return STATUS_SUCCESS;
    }
    NTSTATUS status = BioPropertySize(request, sizeof(ULONG));
    if (NT_SUCCESS(status)) *static_cast<PULONG>(request->Value) = access;
    return status;
}
void BioBuildFormat(const BioFormatEntry& entry, PKSDATAFORMAT_WAVEFORMATEXTENSIBLE output) {
    RtlZeroMemory(output, sizeof(*output));
    output->DataFormat.FormatSize = sizeof(*output);
    output->DataFormat.MajorFormat = KSDATAFORMAT_TYPE_AUDIO;
    output->DataFormat.SubFormat = entry.Subtype;
    output->DataFormat.Specifier = KSDATAFORMAT_SPECIFIER_WAVEFORMATEX;
    auto& wave = output->WaveFormatExt;
    wave.Format.wFormatTag = WAVE_FORMAT_EXTENSIBLE;
    wave.Format.nChannels = entry.Channels;
    wave.Format.nSamplesPerSec = entry.Rate;
    wave.Format.nBlockAlign = entry.Channels * 2;
    wave.Format.nAvgBytesPerSec = entry.Rate * wave.Format.nBlockAlign;
    wave.Format.wBitsPerSample = 16;
    wave.Format.cbSize = sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX);
    wave.Samples.wValidBitsPerSample = 16;
    wave.dwChannelMask = entry.ChannelMask;
    wave.SubFormat = entry.Subtype;
}
NTSTATUS BioValidateFormat(ULONGLONG mask, PKSDATAFORMAT data, const WAVEFORMATEX** wave) {
    if (!data || data->FormatSize < sizeof(KSDATAFORMAT) + sizeof(WAVEFORMATEX) ||
        !IsEqualGUIDAligned(data->MajorFormat, KSDATAFORMAT_TYPE_AUDIO) ||
        !IsEqualGUIDAligned(data->Specifier, KSDATAFORMAT_SPECIFIER_WAVEFORMATEX)) return STATUS_NO_MATCH;
    auto wfx = reinterpret_cast<const WAVEFORMATEX*>(data + 1);
    ULONG bytes = sizeof(WAVEFORMATEX) + wfx->cbSize;
    if (bytes > bio::kMaxFormatBytes || bytes > data->FormatSize - sizeof(KSDATAFORMAT)) return STATUS_NO_MATCH;
    bool allowed = bio::FormatAllowed(mask, wfx, bytes);
    if (!allowed || wfx->wBitsPerSample != 16 || wfx->nBlockAlign != wfx->nChannels * 2 ||
        static_cast<ULONGLONG>(wfx->nSamplesPerSec) * wfx->nBlockAlign != wfx->nAvgBytesPerSec)
        return STATUS_NO_MATCH;
    GUID subtype;
    ULONG channelMask = 0;
    if (wfx->wFormatTag == WAVE_FORMAT_EXTENSIBLE) {
        if (bytes < sizeof(WAVEFORMATEXTENSIBLE)) return STATUS_NO_MATCH;
        auto ext = reinterpret_cast<const WAVEFORMATEXTENSIBLE*>(wfx);
        if (ext->Samples.wValidBitsPerSample != 16) return STATUS_NO_MATCH;
        subtype = ext->SubFormat; channelMask = ext->dwChannelMask;
    } else if (wfx->wFormatTag == WAVE_FORMAT_PCM) subtype = KSDATAFORMAT_SUBTYPE_PCM;
    else if (wfx->wFormatTag == WAVE_FORMAT_DOLBY_AC3_SPDIF) subtype = KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_DIGITAL;
    else return STATUS_NO_MATCH;
    if (!IsEqualGUIDAligned(subtype, data->SubFormat)) return STATUS_NO_MATCH;
    for (const auto& entry : BioFormats) {
        if ((entry.Bit & mask) && IsEqualGUIDAligned(entry.Subtype, subtype) && entry.Channels == wfx->nChannels &&
            entry.Rate == wfx->nSamplesPerSec && (!channelMask || channelMask == entry.ChannelMask)) {
            *wave = wfx; return STATUS_SUCCESS;
        }
    }
    return STATUS_NO_MATCH;
}
bool BioRangeAccepts(PKSDATARANGE range, const BioFormatEntry& format) {
    if (range->FormatSize < sizeof(KSDATARANGE_AUDIO)) return false;
    if (!IsEqualGUIDAligned(range->MajorFormat, KSDATAFORMAT_TYPE_AUDIO) && !IsEqualGUIDAligned(range->MajorFormat, KSDATAFORMAT_TYPE_WILDCARD)) return false;
    if (!IsEqualGUIDAligned(range->SubFormat, format.Subtype) && !IsEqualGUIDAligned(range->SubFormat, KSDATAFORMAT_SUBTYPE_WILDCARD)) return false;
    if (!IsEqualGUIDAligned(range->Specifier, KSDATAFORMAT_SPECIFIER_WAVEFORMATEX) && !IsEqualGUIDAligned(range->Specifier, KSDATAFORMAT_SPECIFIER_WILDCARD)) return false;
    auto audio = reinterpret_cast<PKSDATARANGE_AUDIO>(range);
    return audio->MaximumChannels >= format.Channels && audio->MinimumBitsPerSample <= 16 &&
        audio->MaximumBitsPerSample >= 16 && audio->MinimumSampleFrequency <= format.Rate && audio->MaximumSampleFrequency >= format.Rate;
}
