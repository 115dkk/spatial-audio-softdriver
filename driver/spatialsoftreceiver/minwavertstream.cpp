// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/EndpointsCommon/minwavertstream.cpp at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#include "minwavertstream.h"
#include "minwavert.h"
CMiniportWaveRTStream::CMiniportWaveRTStream(CMiniportWaveRT* miniport, PPORTWAVERTSTREAM port)
    : miniport_(ssr::km::ComRef<CMiniportWaveRT>::Retain(miniport)), adapter_(miniport->Adapter()),
      port_(ssr::km::ComRef<IPortWaveRTStream>::Retain(port)), endpoint_(miniport->Endpoint()) {
    InitializeListHead(&events_);
}
CMiniportWaveRTStream::~CMiniportWaveRTStream() {
    PAGED_CODE();
    SetState(KSSTATE_STOP);
    if (registered_) adapter_->UnregisterStream(endpoint_, this);
    FreeAudioBuffer(pages_.Mdl(), pages_.Bytes());
    while (!IsListEmpty(&events_)) {
        ssr::km::PoolPtr<Notification> entry(
            CONTAINING_RECORD(RemoveHeadList(&events_), Notification, Link));
    }
}
NTSTATUS CMiniportWaveRTStream::Initialize(PKSDATAFORMAT format) {
    NTSTATUS status = adapter_->RegisterStream(endpoint_, this);
    if (!NT_SUCCESS(status)) return status;
    registered_ = true;
    return SetFormat(format);
}
STDMETHODIMP CMiniportWaveRTStream::QueryInterface(REFIID iid, PVOID* object) {
    if (!object) return STATUS_INVALID_PARAMETER;
    *object = nullptr;
    if (IsEqualGUIDAligned(iid, IID_IUnknown) || IsEqualGUIDAligned(iid, IID_IMiniportWaveRTStream) ||
        IsEqualGUIDAligned(iid, IID_IMiniportWaveRTStreamNotification)) *object = static_cast<IMiniportWaveRTStreamNotification*>(this);
    else if (IsEqualGUIDAligned(iid, IID_IMiniportWaveRTOutputStream) && adapter_->Settings().WritePacket)
        *object = static_cast<IMiniportWaveRTOutputStream*>(this);
    else return STATUS_NOINTERFACE;
    AddRef(); return STATUS_SUCCESS;
}
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::SetFormat(PKSDATAFORMAT data) {
    const WAVEFORMATEX* wave;
    NTSTATUS status = SsrValidateFormat(miniport_->Formats(), data, &wave);
    if (!NT_SUCCESS(status)) return status;
    KIRQL irql; adapter_->Lock(&irql);
    if (!adapter_->Active()) status = STATUS_DEVICE_NOT_READY;
    else if (state_ != KSSTATE_STOP || pages_) status = STATUS_INVALID_DEVICE_STATE;
    else {
        RtlZeroMemory(format_, sizeof(format_));
        RtlCopyMemory(format_, wave, sizeof(WAVEFORMATEX) + wave->cbSize);
        Reset();
        adapter_->Format(endpoint_, SsrQpc(), Format());
    }
    adapter_->Unlock(irql);
    SsrPumpRead(adapter_->State());
    return status;
}
void CMiniportWaveRTStream::Reset() {
    linear_ = transferred_ = packetCount_ = eosPosition_ = 0;
    positionQpc_ = SsrQpc(); eos_ = false;
    if (packets_) RtlZeroMemory(packets_.Get(), sizeof(Packet) * notifications_);
}
void CMiniportWaveRTStream::AdapterStop(ULONGLONG qpc) {
    // CAdapterCommon::Stop has already brought every stream to qpc.
    if (state_ == KSSTATE_RUN) adapter_->StreamState(endpoint_, qpc, false);
    state_ = KSSTATE_STOP;
}
void CMiniportWaveRTStream::PowerResume(ULONGLONG qpc) {
    if (state_ == KSSTATE_RUN) clock_.Start(qpc, adapter_->State()->Frequency, Format()->nAvgBytesPerSec, Format()->nBlockAlign);
    positionQpc_ = qpc;
}
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::SetState(KSSTATE state) {
    if (state < KSSTATE_STOP || state > KSSTATE_RUN) return STATUS_INVALID_PARAMETER;
    KIRQL irql; adapter_->Lock(&irql);
    NTSTATUS status = STATUS_SUCCESS;
    if (state == KSSTATE_RUN && (!adapter_->Active() || !pages_)) status = STATUS_DEVICE_NOT_READY;
    else if (state != state_) {
        ULONGLONG qpc = SsrQpc();
        // Every stream reaches qpc before this one's start or stop record.
        adapter_->UpdatePositions(qpc);
        if (state_ == KSSTATE_RUN) adapter_->StreamState(endpoint_, qpc, false);
        if (state == KSSTATE_STOP) Reset();
        if (state == KSSTATE_RUN) {
            clock_.Start(qpc, adapter_->State()->Frequency, Format()->nAvgBytesPerSec, Format()->nBlockAlign);
            positionQpc_ = qpc;
            adapter_->StreamState(endpoint_, qpc, true);
        }
        state_ = state;
    }
    adapter_->Unlock(irql);
    SsrPumpRead(adapter_->State());
    return status;
}
NTSTATUS CMiniportWaveRTStream::Allocate(ULONG notifications, ULONG requested, PMDL* mdl, ULONG* actual, ULONG* offset, MEMORY_CACHING_TYPE* cache) {
    PAGED_CODE();
    *mdl = nullptr; *actual = 0; *offset = 0; *cache = MmCached;
    ULONG align = Format()->nBlockAlign;
    if (!align || !requested || requested > SsrRingBytes / 2 || notifications > 2) return STATUS_INVALID_PARAMETER;
    if (notifications && (requested % notifications || (requested / notifications) % align)) return STATUS_INVALID_PARAMETER;
    requested -= requested % align;
    if (!requested) return STATUS_INVALID_PARAMETER;
    ssr::km::DmaPages pages;
    NTSTATUS status = pages.Allocate(port_.Get(), requested, MmCached);
    if (!NT_SUCCESS(status)) return status;
    ssr::km::PoolPtr<Packet> packets;
    if (notifications) {
        packets = ssr::km::PoolPtr<Packet>::Allocate(sizeof(Packet) * notifications);
        if (!packets) return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(pages.Buffer(), requested);
    KIRQL irql; adapter_->Lock(&irql);
    if (pages_ || state_ == KSSTATE_RUN || !adapter_->Active()) status = STATUS_INVALID_DEVICE_STATE;
    else {
        *mdl = pages.Mdl(); *actual = requested;
        pages_ = ssr::km::Move(pages);
        packets_ = ssr::km::Move(packets);
        notifications_ = notifications; Reset();
    }
    adapter_->Unlock(irql);
    return status;
}
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::AllocateAudioBuffer(ULONG requested, PMDL* mdl, ULONG* actual, ULONG* offset, MEMORY_CACHING_TYPE* cache) {
    return Allocate(0, requested, mdl, actual, offset, cache);
}
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::AllocateBufferWithNotification(ULONG count, ULONG requested, PMDL* mdl, ULONG* actual, ULONG* offset, MEMORY_CACHING_TYPE* cache) {
    if (!count) return STATUS_INVALID_PARAMETER;
    return Allocate(count, requested, mdl, actual, offset, cache);
}
STDMETHODIMP_(void) CMiniportWaveRTStream::FreeAudioBuffer(PMDL mdl, ULONG) {
    PAGED_CODE();
    ssr::km::DmaPages pages;
    ssr::km::PoolPtr<Packet> packets;
    KIRQL irql; adapter_->Lock(&irql);
    if (mdl && mdl == pages_.Mdl()) {
        if (state_ == KSSTATE_RUN) {
            ULONGLONG qpc = SsrQpc(); adapter_->UpdatePositions(qpc);
            adapter_->StreamState(endpoint_, qpc, false);
        }
        state_ = KSSTATE_STOP;
        pages = ssr::km::Move(pages_);
        packets = ssr::km::Move(packets_);
        notifications_ = 0; Reset();
    }
    adapter_->Unlock(irql);
    pages.Reset();
    packets.Reset();
    SsrPumpRead(adapter_->State());
}
STDMETHODIMP_(void) CMiniportWaveRTStream::FreeBufferWithNotification(PMDL mdl, ULONG bytes) { FreeAudioBuffer(mdl, bytes); }
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::RegisterNotificationEvent(PKEVENT event) {
    if (!event) return STATUS_INVALID_PARAMETER;
    auto entry = ssr::km::PoolPtr<Notification>::Allocate();
    if (!entry) return STATUS_INSUFFICIENT_RESOURCES;
    entry->Event = event;
    NTSTATUS status = STATUS_SUCCESS;
    KIRQL irql; adapter_->Lock(&irql);
    for (auto link = events_.Flink; link != &events_; link = link->Flink) {
        if (CONTAINING_RECORD(link, Notification, Link)->Event == event) { status = STATUS_OBJECT_NAME_COLLISION; break; }
    }
    if (NT_SUCCESS(status)) InsertTailList(&events_, &entry.Release()->Link);
    adapter_->Unlock(irql);
    return status;
}
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::UnregisterNotificationEvent(PKEVENT event) {
    ssr::km::PoolPtr<Notification> removed;
    KIRQL irql; adapter_->Lock(&irql);
    for (auto link = events_.Flink; link != &events_; link = link->Flink) {
        auto entry = CONTAINING_RECORD(link, Notification, Link);
        if (entry->Event == event) {
            RemoveEntryList(link);
            removed = ssr::km::PoolPtr<Notification>(entry);
            break;
        }
    }
    adapter_->Unlock(irql);
    if (!removed) return STATUS_NOT_FOUND;
    return STATUS_SUCCESS;
}
void CMiniportWaveRTStream::WriteBytes(ULONGLONG begin, ULONGLONG bytes, ULONGLONG qpc) {
    while (bytes) {
        ULONG offset = static_cast<ULONG>(begin % pages_.Bytes());
        ULONG length = static_cast<ULONG>(bytes > pages_.Bytes() ? pages_.Bytes() : bytes);
        // One record carries at most kMaxDataPayload (the contract's limit).
        if (length > ssr::kMaxDataPayload) length = ssr::kMaxDataPayload;
        ULONGLONG written = 0;
        if (notifications_ && adapter_->Settings().WritePacket) {
            ULONG packetBytes = pages_.Bytes() / notifications_;
            ULONGLONG number = begin / packetBytes;
            ULONG remaining = packetBytes - static_cast<ULONG>(begin % packetBytes);
            if (length > remaining) length = remaining;
            const auto& packet = packets_.Get()[number % notifications_];
            if (packet.Valid && packet.Number == number) written = packet.Qpc;
        }
        ULONG first = length < pages_.Bytes() - offset ? length : pages_.Bytes() - offset;
        adapter_->DataLocked(endpoint_, qpc, written, pages_.Buffer() + offset, first, length > first ? pages_.Buffer() : nullptr, length - first);
        begin += length; bytes -= length;
    }
}
void CMiniportWaveRTStream::UpdatePosition(ULONGLONG qpc, LONG trim) {
    const ULONG bufferBytes = pages_.Bytes();
    if (state_ != KSSTATE_RUN || !pages_.Buffer() || !bufferBytes || !adapter_->Active()) return;
    clock_.SetTrim(trim);
    ULONGLONG advance = clock_.Advance(qpc);
    linear_ += advance;
    ULONGLONG end = eos_ && linear_ > eosPosition_ ? eosPosition_ : linear_;
    if (end > transferred_) {
        // The DMA mapping only contains one buffer. On extreme scheduler delay,
        // replaying overwritten cycles would fabricate audio. Bound work to one
        // buffer; the ring's byte position still represents bytes actually sent.
        ULONGLONG bytes = end - transferred_;
        ULONGLONG begin = transferred_;
        if (bytes > bufferBytes) {
            adapter_->SourceGapLocked(endpoint_, qpc, bytes - bufferBytes);
            begin = end - bufferBytes; bytes = bufferBytes;
        }
        WriteBytes(begin, bytes, qpc);
        transferred_ = end;
    }
    positionQpc_ = qpc;
    if (notifications_) {
        ULONGLONG completed = linear_ / (bufferBytes / notifications_);
        if (completed != packetCount_) {
            packetCount_ = completed;
            for (auto link = events_.Flink; link != &events_; link = link->Flink)
                KeSetEvent(CONTAINING_RECORD(link, Notification, Link)->Event, IO_NO_INCREMENT, FALSE);
        }
    }
}
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::GetPosition(PKSAUDIO_POSITION position) {
    KIRQL irql; adapter_->Lock(&irql);
    adapter_->UpdatePositions(SsrQpc());
    position->PlayOffset = pages_.Bytes() ? transferred_ % pages_.Bytes() : 0;
    position->WriteOffset = position->PlayOffset;
    adapter_->Unlock(irql);
    SsrPumpRead(adapter_->State()); return STATUS_SUCCESS;
}
STDMETHODIMP_(void) CMiniportWaveRTStream::GetHWLatency(KSRTAUDIO_HWLATENCY* latency) { RtlZeroMemory(latency, sizeof(*latency)); }
// Virtual hardware has no user-mappable hardware position/clock register.
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::GetPositionRegister(KSRTAUDIO_HWREGISTER* reg) { RtlZeroMemory(reg, sizeof(*reg)); return STATUS_NOT_SUPPORTED; }
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::GetClockRegister(KSRTAUDIO_HWREGISTER* reg) { RtlZeroMemory(reg, sizeof(*reg)); return STATUS_NOT_SUPPORTED; }
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::SetWritePacket(ULONG number, DWORD flags, ULONG eosBytes) {
    KIRQL irql; adapter_->Lock(&irql);
    NTSTATUS status = STATUS_SUCCESS;
    ULONGLONG qpc = SsrQpc();
    adapter_->UpdatePositions(qpc);
    if (!adapter_->Settings().WritePacket || !notifications_ || !pages_.Buffer()) status = STATUS_NOT_SUPPORTED;
    else if (!adapter_->Active() || eos_) status = STATUS_INVALID_DEVICE_STATE;
    else if ((flags & ~KSSTREAM_HEADER_OPTIONSF_ENDOFSTREAM)) status = STATUS_INVALID_PARAMETER;
    else {
        ULONG packetBytes = pages_.Bytes() / notifications_;
        ULONGLONG expected = packetCount_ + (state_ == KSSTATE_RUN ? 1 : 0);
        LONG delta = static_cast<LONG>(number - static_cast<ULONG>(expected));
        if (delta < 0) status = STATUS_DATA_LATE_ERROR;
        else if (delta > 0) status = STATUS_DATA_OVERRUN;
        else if ((flags & KSSTREAM_HEADER_OPTIONSF_ENDOFSTREAM) && (eosBytes > packetBytes || eosBytes % Format()->nBlockAlign)) status = STATUS_INVALID_PARAMETER;
        else {
            auto& packet = packets_.Get()[expected % notifications_];
            if (packet.Valid && packet.Number == expected) status = STATUS_INVALID_DEVICE_STATE;
            else {
                packet = {expected, qpc, true};
                if (flags & KSSTREAM_HEADER_OPTIONSF_ENDOFSTREAM) {
                    eos_ = true; eosPosition_ = expected * packetBytes + eosBytes;
                }
            }
        }
    }
    adapter_->Unlock(irql);
    SsrPumpRead(adapter_->State()); return status;
}
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::GetPacketCount(ULONG* count) {
    KIRQL irql; adapter_->Lock(&irql);
    NTSTATUS status = notifications_ ? STATUS_SUCCESS : STATUS_NOT_SUPPORTED;
    adapter_->UpdatePositions(SsrQpc()); *count = static_cast<ULONG>(packetCount_);
    adapter_->Unlock(irql); SsrPumpRead(adapter_->State()); return status;
}
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::GetOutputStreamPresentationPosition(KSAUDIO_PRESENTATION_POSITION* position) {
    KIRQL irql; adapter_->Lock(&irql);
    NTSTATUS status = notifications_ ? STATUS_SUCCESS : STATUS_NOT_SUPPORTED;
    adapter_->UpdatePositions(SsrQpc());
    position->u64PositionInBlocks = Format()->nBlockAlign ? linear_ / Format()->nBlockAlign : 0;
    position->u64QPCPosition = positionQpc_;
    adapter_->Unlock(irql); SsrPumpRead(adapter_->State()); return status;
}
