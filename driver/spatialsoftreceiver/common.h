// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/common.h at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#pragma once
#include "control.h"
class CMiniportWaveRTStream;
class CAdapterCommon final : public IAdapterPowerManagement {
public:
    CAdapterCommon(PDEVICE_OBJECT control);
    ~CAdapterCommon();
    SSR_UNKNOWN_METHODS;
    IMP_IAdapterPowerManagement;
    NTSTATUS Start();
    void Stop();
    NTSTATUS RegisterStream(USHORT endpoint, CMiniportWaveRTStream* stream);
    void UnregisterStream(USHORT endpoint, CMiniportWaveRTStream* stream);
    SsrControlState* State() const { return state_; }
    const SsrSettings& Settings() const { return settings_; }
    void Lock(PKIRQL irql) { KeAcquireSpinLock(&streamLock_, irql); }
    void Unlock(KIRQL irql) { KeReleaseSpinLock(&streamLock_, irql); }
    bool Active() const { return active_ && power_ == PowerDeviceD0; }
    // Called with streamLock held; lock order is streamLock -> control Lock.
    // Every position update comes here: it brings every running stream to
    // `qpc` under one hold of the control Lock, so no READ holds one
    // endpoint's audio up to qpc and another's only up to an earlier tick
    // (the mixer counts such a gap as a lane underrun once it passes the
    // arrival slack).
    void UpdatePositions(ULONGLONG qpc);
    // Called with streamLock and the control Lock held (UpdatePositions).
    void SourceGapLocked(USHORT ep, ULONGLONG qpc, ULONGLONG bytes);
    void DataLocked(USHORT ep, ULONGLONG qpc, ULONGLONG written, const BYTE* a, ULONG an, const BYTE* b, ULONG bn);
    // Called with streamLock held; lock order is streamLock -> control Lock.
    void Format(USHORT ep, ULONGLONG qpc, const WAVEFORMATEX* format);
    void StreamState(USHORT ep, ULONGLONG qpc, bool running);
private:
    static VOID TimerTick(PEX_TIMER timer, PVOID context);
    // Called with streamLock_ held.
    void SyncTimer(ULONGLONG qpc);
    LONG references_ = 1;
    PDEVICE_OBJECT control_;
    SsrControlState* state_;
    SsrSettings settings_;
    KSPIN_LOCK streamLock_;
    PEX_TIMER timer_ = nullptr;
    CMiniportWaveRTStream* streams_[SSR_MAX_ENDPOINTS] = {};
    bool active_ = false;
    ULONG running_ = 0;  // streams in KSSTATE_RUN
    bool timerArmed_ = false;
    DEVICE_POWER_STATE power_ = PowerDeviceD0;
    ULONGLONG lastTick_ = 0;
};
