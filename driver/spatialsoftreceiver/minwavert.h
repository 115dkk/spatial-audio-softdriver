// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/EndpointsCommon/minwavert.h at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#pragma once
#include "common.h"
#include "kshelper.h"
// The host pin announces RAW, as SysVAD's HDMI host pin does, and so every
// host data range carries the signal-processing-mode attribute list: no APO
// runs, so the stream reaches the ring untouched.
class CMiniportWaveRT final : public IMiniportWaveRT, public IMiniportAudioSignalProcessing {
public:
    CMiniportWaveRT(CAdapterCommon* adapter, USHORT endpoint);
    ~CMiniportWaveRT();
    SSR_UNKNOWN_METHODS;
    IMP_IMiniportWaveRT;
    STDMETHODIMP_(NTSTATUS) GetModes(ULONG pin, GUID* modes, ULONG* count) override;
    static NTSTATUS Property(PPCPROPERTY_REQUEST request);
    CAdapterCommon* Adapter() const { return adapter_.Get(); }
    USHORT Endpoint() const { return endpoint_; }
    const SSR_ENDPOINT_FORMATS& Formats() const { return adapter_->Settings().Formats[endpoint_]; }
private:
    LONG references_ = 1;
    ssr::km::ComRef<CAdapterCommon> adapter_;
    USHORT endpoint_;
    PCFILTER_DESCRIPTOR descriptor_{};
    PCPIN_DESCRIPTOR pins_[2]{};
    KSDATARANGE_AUDIO ranges_[SsrFormatCount]{};
    // Each range followed by its attribute list (KSDATARANGE_ATTRIBUTES).
    PKSDATARANGE rangePointers_[2 * SsrFormatCount]{};
    KSDATARANGE bridges_[SsrFormatCount]{};
    PKSDATARANGE bridgePointers_[SsrFormatCount]{};
    PCNODE_DESCRIPTOR node_{};
    PCCONNECTION_DESCRIPTOR connections_[2]{};
};
