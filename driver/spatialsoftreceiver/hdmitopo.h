// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/TabletAudioSample/hdmitopo.h at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#pragma once
#include "common.h"
#include "kshelper.h"
#include "basetopo.h"
class CMiniportTopologyHdmi final : public IMiniportTopology {
public:
    CMiniportTopologyHdmi(CAdapterCommon* adapter, USHORT endpoint);
    ~CMiniportTopologyHdmi();
    SSR_UNKNOWN_METHODS;
    IMP_IMiniportTopology;
    static NTSTATUS Property(PPCPROPERTY_REQUEST request);
    static NTSTATUS AudioProperty(PPCPROPERTY_REQUEST request);
private:
    LONG references_ = 1;
    ssr::km::ComRef<CAdapterCommon> adapter_;
    USHORT endpoint_;
    ULONG channels_;
    PCFILTER_DESCRIPTOR descriptor_{};
    PCPIN_DESCRIPTOR pins_[2]{};
    KSDATARANGE ranges_[SsrFormatCount]{};
    PKSDATARANGE pointers_[SsrFormatCount]{};
    PCCONNECTION_DESCRIPTOR connections_[3]{};
};
