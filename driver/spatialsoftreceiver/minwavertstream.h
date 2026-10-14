// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/EndpointsCommon/minwavertstream.h at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#pragma once
#include "common.h"
class CMiniportWaveRT;
class CMiniportWaveRTStream final : public IMiniportWaveRTStreamNotification, public IMiniportWaveRTOutputStream {
public:
    CMiniportWaveRTStream(CMiniportWaveRT* miniport, PPORTWAVERTSTREAM port);
    ~CMiniportWaveRTStream();
    SSR_UNKNOWN_METHODS;
    IMP_IMiniportWaveRTStream;
    IMP_IMiniportWaveRTStreamNotification;
    IMP_IMiniportWaveRTOutputStream;
    NTSTATUS Initialize(PKSDATAFORMAT format);
    // All below execute under adapter's stream lock, including destruction.
    // UpdatePosition also needs the control lock: only
    // CAdapterCommon::UpdatePositions calls it, for every stream at once.
    void UpdatePosition(ULONGLONG qpc, LONG trim);
    void PowerResume(ULONGLONG qpc);
    void AdapterStop(ULONGLONG qpc);
private:
    struct Notification { LIST_ENTRY Link; PKEVENT Event; };
    struct Packet { ULONGLONG Number; ULONGLONG Qpc; bool Valid; };
    NTSTATUS Allocate(ULONG notifications, ULONG requested, PMDL* mdl, ULONG* actual, ULONG* offset, MEMORY_CACHING_TYPE* cache);
    void WriteBytes(ULONGLONG begin, ULONGLONG bytes, ULONGLONG qpc);
    void Reset();
    LONG references_ = 1;
    ssr::km::ComRef<CMiniportWaveRT> miniport_;
    // The miniport reference keeps the adapter alive; this pointer does not own it.
    CAdapterCommon* adapter_;
    ssr::km::ComRef<IPortWaveRTStream> port_;
    ssr::km::DmaPages pages_;
    USHORT endpoint_;
    bool registered_ = false;
    KSSTATE state_ = KSSTATE_STOP;
    alignas(8) BYTE format_[ssr::kMaxFormatBytes] = {};
    WAVEFORMATEX* Format() { return reinterpret_cast<WAVEFORMATEX*>(format_); }
    ULONG notifications_ = 0;
    ssr::km::PoolPtr<Packet> packets_;
    LIST_ENTRY events_;
    ssr::DmaClock clock_;
    ULONGLONG linear_ = 0;
    ULONGLONG transferred_ = 0;
    ULONGLONG packetCount_ = 0;
    ULONGLONG positionQpc_ = 0;
    ULONGLONG eosPosition_ = 0;
    bool eos_ = false;
};
