// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/common.cpp at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#include "common.h"
#include "minwavertstream.h"
constinit const GUID SsrAdapterInterface = {0x534664d2,0x3ca6,0x4fbd,{0x84,0x75,0x3a,0x0b,0x41,0xb9,0x4c,0x11}};
CAdapterCommon::CAdapterCommon(PDEVICE_OBJECT control)
    : control_(ssr::km::ObjectRef<DEVICE_OBJECT>::Retain(control)), state_(SsrControl(control)) {
    settings_ = state_->Settings;
    KeInitializeSpinLock(&streamLock_);
}
CAdapterCommon::~CAdapterCommon() { Stop(); }
STDMETHODIMP CAdapterCommon::QueryInterface(REFIID iid, PVOID* object) {
    if (!object) return STATUS_INVALID_PARAMETER;
    *object = nullptr;
    if (IsEqualGUIDAligned(iid, IID_IUnknown) || IsEqualGUIDAligned(iid, IID_IAdapterPowerManagement) ||
        IsEqualGUIDAligned(iid, SsrAdapterInterface)) *object = static_cast<IAdapterPowerManagement*>(this);
    else return STATUS_NOINTERFACE;
    AddRef(); return STATUS_SUCCESS;
}
NTSTATUS CAdapterCommon::Start() {
    PAGED_CODE();
    NTSTATUS status = SsrStartControl(state_);
    if (!NT_SUCCESS(status)) return status;
    timer_ = ssr::km::ExTimer::Allocate(TimerTick, this, EX_TIMER_HIGH_RESOLUTION);
    if (!timer_) { SsrStopControl(state_); return STATUS_INSUFFICIENT_RESOURCES; }
    // The DMA timer starts with the first stream in RUN (StreamState).
    active_ = true;
    return STATUS_SUCCESS;
}
void CAdapterCommon::Stop() {
    PAGED_CODE();
    ssr::km::ExTimer timer;
    KIRQL irql; Lock(&irql);
    bool wasActive = active_;
    if (wasActive) {
        ULONGLONG qpc = SsrQpc();
        UpdatePositions(qpc);
        for (auto stream : streams_) if (stream) stream->AdapterStop(qpc);
    }
    active_ = false;
    SyncTimer(SsrQpc());
    timer = ssr::km::Move(timer_);
    Unlock(irql);
    timer.Reset();
    if (wasActive) SsrStopControl(state_);
}
NTSTATUS CAdapterCommon::RegisterStream(USHORT endpoint, CMiniportWaveRTStream* stream) {
    KIRQL irql; Lock(&irql);
    NTSTATUS status = STATUS_SUCCESS;
    if (endpoint >= SSR_MAX_ENDPOINTS) status = STATUS_INVALID_PARAMETER;
    else if (!active_) status = STATUS_DEVICE_NOT_READY;
    else if (streams_[endpoint]) status = STATUS_DEVICE_BUSY;
    else streams_[endpoint] = stream;
    Unlock(irql); return status;
}
void CAdapterCommon::UnregisterStream(USHORT endpoint, CMiniportWaveRTStream* stream) {
    KIRQL irql; Lock(&irql);
    if (endpoint < SSR_MAX_ENDPOINTS && streams_[endpoint] == stream) streams_[endpoint] = nullptr;
    Unlock(irql);
}
void CAdapterCommon::UpdatePositions(ULONGLONG qpc) {
    KIRQL irql; KeAcquireSpinLock(&state_->Lock, &irql);
    const LONG trim = state_->Trim;
    for (auto stream : streams_) if (stream) stream->UpdatePosition(qpc, trim);
    KeReleaseSpinLock(&state_->Lock, irql);
}
void CAdapterCommon::SourceGapLocked(USHORT ep, ULONGLONG qpc, ULONGLONG bytes) {
    if (active_ && state_->Started) state_->Ring.SourceGap(ep, qpc, bytes);
}
void CAdapterCommon::DataLocked(USHORT ep, ULONGLONG qpc, ULONGLONG written, const BYTE* a, ULONG an, const BYTE* b, ULONG bn) {
    if (active_ && state_->Started) state_->Ring.Data(ep, qpc, written, a, an, b, bn);
}
void CAdapterCommon::Format(USHORT ep, ULONGLONG qpc, const WAVEFORMATEX* format) {
    KIRQL irql; KeAcquireSpinLock(&state_->Lock, &irql);
    if (active_ && state_->Started) state_->Ring.FormatChange(ep, qpc, format, sizeof(WAVEFORMATEX) + format->cbSize);
    KeReleaseSpinLock(&state_->Lock, irql);
}
void CAdapterCommon::SyncTimer(ULONGLONG qpc) {
    const bool shouldRun = timer_ && active_ && power_ == PowerDeviceD0 && running_ > 0;
    if (shouldRun && !timerArmed_) {
        lastTick_ = qpc;
        ExSetTimer(timer_.Get(), -10000, 10000, nullptr);
        timerArmed_ = true;
    } else if (!shouldRun && timerArmed_) {
        ExCancelTimer(timer_.Get(), nullptr);
        timerArmed_ = false;
    }
}
void CAdapterCommon::StreamState(USHORT ep, ULONGLONG qpc, bool running) {
    // Every entry to and exit from KSSTATE_RUN comes here, so the count stays balanced.
    if (running) ++running_;
    else if (running_) --running_;
    SyncTimer(qpc);
    KIRQL irql; KeAcquireSpinLock(&state_->Lock, &irql);
    if (active_ && state_->Started) {
        if (running) state_->Ring.StreamStart(ep, qpc);
        else state_->Ring.StreamStop(ep, qpc);
    }
    KeReleaseSpinLock(&state_->Lock, irql);
}
VOID CAdapterCommon::TimerTick(PEX_TIMER, PVOID context) {
    auto adapter = static_cast<CAdapterCommon*>(context);
    KIRQL irql; adapter->Lock(&irql);
    ULONGLONG qpc = SsrQpc();
    if (adapter->Active()) {
        auto state = adapter->state_;
        ULONGLONG elapsed = qpc - adapter->lastTick_;
        ULONGLONG expected = state->Frequency / 1000;
        if (elapsed > expected) {
            ULONGLONG late = (elapsed - expected) * 1000000 / state->Frequency;
            KIRQL stateIrql; KeAcquireSpinLock(&state->Lock, &stateIrql);
            ULONG value = static_cast<ULONG>(late > MAXULONG ? MAXULONG : late);
            if (value > state->MaxLatenessUs) state->MaxLatenessUs = value;
            KeReleaseSpinLock(&state->Lock, stateIrql);
        }
        adapter->UpdatePositions(qpc);
    }
    adapter->lastTick_ = qpc;
    adapter->Unlock(irql);
    SsrPumpRead(adapter->state_);
}
STDMETHODIMP_(void) CAdapterCommon::PowerChangeState(POWER_STATE state) {
    KIRQL irql; Lock(&irql);
    ULONGLONG qpc = SsrQpc();
    if (Active()) UpdatePositions(qpc);
    if (state.DeviceState == PowerDeviceD0 && power_ != PowerDeviceD0)
        for (auto stream : streams_) if (stream) stream->PowerResume(qpc);
    power_ = state.DeviceState;
    SyncTimer(qpc);
    Unlock(irql);
    SsrPumpRead(state_);
}
STDMETHODIMP_(NTSTATUS) CAdapterCommon::QueryPowerChangeState(POWER_STATE state) {
    return state.DeviceState >= PowerDeviceD0 && state.DeviceState <= PowerDeviceD3 ? STATUS_SUCCESS : STATUS_INVALID_PARAMETER;
}
STDMETHODIMP_(NTSTATUS) CAdapterCommon::QueryDeviceCapabilities(PDEVICE_CAPABILITIES caps) {
    if (!caps || caps->Size < sizeof(*caps)) return STATUS_INVALID_PARAMETER;
    caps->DeviceD1 = FALSE; caps->DeviceD2 = FALSE;
    for (ULONG i = 0; i < PowerSystemMaximum; ++i)
        caps->DeviceState[i] = i == PowerSystemWorking ? PowerDeviceD0 : PowerDeviceD3;
    return STATUS_SUCCESS;
}
