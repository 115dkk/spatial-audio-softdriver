// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/TabletAudioSample/hdmiwavtable.h at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#pragma once
#include "spatialsoftreceiver.h"
struct SsrFormatEntry {
    ULONGLONG Bit;
    GUID Subtype;
    USHORT Channels;
    ULONG Rate;
    ULONG ChannelMask;
    USHORT Bits;
    USHORT ValidBits;
};
// Bitstreams retain their concrete carrier formats. PCM is rule-based.
constexpr GUID SsrGuid(ULONG a, USHORT b, USHORT c, BYTE d, BYTE e, BYTE f, BYTE g, BYTE h, BYTE i, BYTE j, BYTE k) {
    return {a, b, c, {d, e, f, g, h, i, j, k}};
}
static constexpr auto SsrCarrierTable = [] {
    struct Table { SsrFormatEntry entries[RTL_NUMBER_OF(ssr::kCodecCarriers)]; } table{};
    for (size_t i = 0; i < RTL_NUMBER_OF(ssr::kCodecCarriers); ++i) {
        const auto& carrier = ssr::kCodecCarriers[i];
        table.entries[i] = {carrier.bit,
            SsrGuid(carrier.subtype_id, carrier.subtype_family, 0x0010, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71),
            carrier.channels, carrier.rate, carrier.channel_mask, carrier.bits, carrier.valid_bits};
    }
    return table;
}();
static constexpr const auto& SsrFormats = SsrCarrierTable.entries;
constexpr bool SsrSameGuid(const GUID& a, const GUID& b) {
    if (a.Data1 != b.Data1 || a.Data2 != b.Data2 || a.Data3 != b.Data3) return false;
    for (size_t i = 0; i < sizeof(a.Data4); ++i) if (a.Data4[i] != b.Data4[i]) return false;
    return true;
}
#define SSR_CHECK_CARRIER(index, subtype, speakers) \
    static_assert(SsrSameGuid(SsrFormats[index].Subtype, SsrGuid(STATIC_##subtype)) && \
        SsrFormats[index].ChannelMask == speakers, "core carrier agrees with ksmedia.h")
SSR_CHECK_CARRIER(0, KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_DIGITAL, KSAUDIO_SPEAKER_5POINT1_SURROUND);
SSR_CHECK_CARRIER(1, KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_DIGITAL, KSAUDIO_SPEAKER_5POINT1_SURROUND);
SSR_CHECK_CARRIER(2, KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_MLP, KSAUDIO_SPEAKER_7POINT1);
SSR_CHECK_CARRIER(3, KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_MLP, KSAUDIO_SPEAKER_7POINT1_SURROUND);
SSR_CHECK_CARRIER(4, KSDATAFORMAT_SUBTYPE_IEC61937_DTS, KSAUDIO_SPEAKER_5POINT1_SURROUND);
SSR_CHECK_CARRIER(5, KSDATAFORMAT_SUBTYPE_IEC61937_DTS_HD, KSAUDIO_SPEAKER_7POINT1);
SSR_CHECK_CARRIER(6, KSDATAFORMAT_SUBTYPE_IEC61937_DTSX_E1, KSAUDIO_SPEAKER_7POINT1);
SSR_CHECK_CARRIER(7, KSDATAFORMAT_SUBTYPE_IEC61937_DTSX_E2, KSAUDIO_SPEAKER_7POINT1);
SSR_CHECK_CARRIER(8, KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_MAT20, KSAUDIO_SPEAKER_7POINT1);
SSR_CHECK_CARRIER(9, KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_MAT21, KSAUDIO_SPEAKER_7POINT1);
// Keep the alias literal for older WDKs; SDK 26100 calls it MAT21_PROFILE4.
static_assert(SsrSameGuid(SsrFormats[10].Subtype,
    SsrGuid(0x070c, 0x0cea, 0x0010, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71)) &&
    SsrFormats[10].ChannelMask == KSAUDIO_SPEAKER_7POINT1, "MAT21 alias is unchanged");
#ifdef STATIC_KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_MAT21_PROFILE4
SSR_CHECK_CARRIER(10, KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_MAT21_PROFILE4, KSAUDIO_SPEAKER_7POINT1);
#endif
SSR_CHECK_CARRIER(11, KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_DIGITAL_PLUS, KSAUDIO_SPEAKER_5POINT1_SURROUND);
SSR_CHECK_CARRIER(12, KSDATAFORMAT_SUBTYPE_IEC61937_DOLBY_DIGITAL_PLUS_ATMOS, KSAUDIO_SPEAKER_7POINT1);
#undef SSR_CHECK_CARRIER
constexpr ULONG SsrFormatCount = RTL_NUMBER_OF(SsrFormats) + 2;

inline ULONG SsrVolumeChannels(USHORT endpoint) {
    return endpoint == SSR_ENDPOINT_PRIMARY ? 8UL : 2UL;
}
inline SsrFormatEntry SsrDefaultPcm(USHORT endpoint) {
    return {0, SsrGuid(STATICGUIDOF(KSDATAFORMAT_SUBTYPE_IEEE_FLOAT)),
        static_cast<USHORT>(SsrVolumeChannels(endpoint)), 48000,
        static_cast<ULONG>(endpoint == SSR_ENDPOINT_PRIMARY ? KSAUDIO_SPEAKER_7POINT1_SURROUND : KSAUDIO_SPEAKER_STEREO),
        32, 32};
}
