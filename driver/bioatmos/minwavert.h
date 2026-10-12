// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/EndpointsCommon/minwavert.h at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#pragma once
#include "common.h"
#include "kshelper.h"
class CMiniportWaveRT final : public IMiniportWaveRT, public IMiniportAudioSignalProcessing {
public:
    CMiniportWaveRT(CAdapterCommon* adapter, USHORT endpoint);
    ~CMiniportWaveRT();
    BIO_UNKNOWN_METHODS;
    IMP_IMiniportWaveRT;
    STDMETHODIMP_(NTSTATUS) GetModes(ULONG pin, GUID* modes, ULONG* count) override;
    static NTSTATUS Property(PPCPROPERTY_REQUEST request);
    CAdapterCommon* Adapter() const { return adapter_; }
    USHORT Endpoint() const { return endpoint_; }
    ULONGLONG Mask() const { return adapter_->Settings().FormatMask[endpoint_]; }
private:
    LONG references_ = 1;
    CAdapterCommon* adapter_;
    USHORT endpoint_;
    PCFILTER_DESCRIPTOR descriptor_{};
    PCPIN_DESCRIPTOR pins_[2]{};
    KSDATARANGE_AUDIO ranges_[BioFormatCount]{};
    PKSDATARANGE rangePointers_[BioFormatCount]{};
    KSDATARANGE bridges_[BioFormatCount]{};
    PKSDATARANGE bridgePointers_[BioFormatCount]{};
    PCCONNECTION_DESCRIPTOR connection_{};
};
