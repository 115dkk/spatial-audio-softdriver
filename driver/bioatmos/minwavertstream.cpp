// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/EndpointsCommon/minwavertstream.cpp at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#include "minwavertstream.h"
#include "minwavert.h"
CMiniportWaveRTStream::CMiniportWaveRTStream(CMiniportWaveRT* miniport, PPORTWAVERTSTREAM port)
    : miniport_(miniport), adapter_(miniport->Adapter()), port_(port), endpoint_(miniport->Endpoint()) {
    miniport_->AddRef();
    port_->AddRef();
    InitializeListHead(&events_);
}
CMiniportWaveRTStream::~CMiniportWaveRTStream() {
    PAGED_CODE();
    SetState(KSSTATE_STOP);
    if (registered_) adapter_->UnregisterStream(endpoint_, this);
    FreeAudioBuffer(mdl_, bufferBytes_);
    while (!IsListEmpty(&events_)) {
        auto entry = CONTAINING_RECORD(RemoveHeadList(&events_), Notification, Link);
        ExFreePoolWithTag(entry, BioPoolTag);
    }
    port_->Release(); miniport_->Release();
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
    NTSTATUS status = BioValidateFormat(miniport_->Formats(), data, &wave);
    if (!NT_SUCCESS(status)) return status;
    KIRQL irql; adapter_->Lock(&irql);
    if (!adapter_->Active()) status = STATUS_DEVICE_NOT_READY;
    else if (state_ != KSSTATE_STOP || mdl_) status = STATUS_INVALID_DEVICE_STATE;
    else {
        RtlZeroMemory(format_, sizeof(format_));
        RtlCopyMemory(format_, wave, sizeof(WAVEFORMATEX) + wave->cbSize);
        Reset();
        adapter_->Format(endpoint_, BioQpc(), Format());
    }
    adapter_->Unlock(irql);
    BioPumpRead(adapter_->State());
    return status;
}
void CMiniportWaveRTStream::Reset() {
    linear_ = transferred_ = packetCount_ = eosPosition_ = 0;
    positionQpc_ = BioQpc(); eos_ = false;
    if (packets_) RtlZeroMemory(packets_, sizeof(Packet) * notifications_);
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
    if (state == KSSTATE_RUN && (!adapter_->Active() || !dma_ || !bufferBytes_)) status = STATUS_DEVICE_NOT_READY;
    else if (state != state_) {
        ULONGLONG qpc = BioQpc();
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
    BioPumpRead(adapter_->State());
    return status;
}
NTSTATUS CMiniportWaveRTStream::Allocate(ULONG notifications, ULONG requested, PMDL* mdl, ULONG* actual, ULONG* offset, MEMORY_CACHING_TYPE* cache) {
    PAGED_CODE();
    *mdl = nullptr; *actual = 0; *offset = 0; *cache = MmCached;
    ULONG align = Format()->nBlockAlign;
    if (!align || !requested || requested > BioRingBytes / 2 || notifications > 2) return STATUS_INVALID_PARAMETER;
    if (notifications && (requested % notifications || (requested / notifications) % align)) return STATUS_INVALID_PARAMETER;
    requested -= requested % align;
    if (!requested) return STATUS_INVALID_PARAMETER;
    PHYSICAL_ADDRESS high; high.QuadPart = MAXLONGLONG;
    PMDL allocated = port_->AllocatePagesForMdl(high, requested);
    if (!allocated) return STATUS_INSUFFICIENT_RESOURCES;
    if (MmGetMdlByteCount(allocated) < requested) {
        port_->FreePagesFromMdl(allocated);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    auto buffer = static_cast<BYTE*>(port_->MapAllocatedPages(allocated, MmCached));
    if (!buffer) { port_->FreePagesFromMdl(allocated); return STATUS_INSUFFICIENT_RESOURCES; }
    Packet* packets = nullptr;
    if (notifications) {
        packets = static_cast<Packet*>(ExAllocatePool2(POOL_FLAG_NON_PAGED, sizeof(Packet) * notifications, BioPoolTag));
        if (!packets) {
            port_->UnmapAllocatedPages(buffer, allocated); port_->FreePagesFromMdl(allocated);
            return STATUS_INSUFFICIENT_RESOURCES;
        }
    }
    RtlZeroMemory(buffer, requested);
    KIRQL irql; adapter_->Lock(&irql);
    NTSTATUS status = STATUS_SUCCESS;
    if (mdl_ || state_ == KSSTATE_RUN || !adapter_->Active()) status = STATUS_INVALID_DEVICE_STATE;
    else {
        mdl_ = allocated; dma_ = buffer; bufferBytes_ = requested;
        packets_ = packets; notifications_ = notifications; Reset();
        *mdl = allocated; *actual = requested;
    }
    adapter_->Unlock(irql);
    if (!NT_SUCCESS(status)) {
        if (packets) ExFreePoolWithTag(packets, BioPoolTag);
        port_->UnmapAllocatedPages(buffer, allocated); port_->FreePagesFromMdl(allocated);
    }
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
    KIRQL irql; adapter_->Lock(&irql);
    BYTE* buffer = nullptr; Packet* packets = nullptr;
    if (mdl && mdl == mdl_) {
        if (state_ == KSSTATE_RUN) {
            ULONGLONG qpc = BioQpc(); adapter_->UpdatePositions(qpc);
            adapter_->StreamState(endpoint_, qpc, false);
        }
        state_ = KSSTATE_STOP;
        buffer = dma_; packets = packets_;
        dma_ = nullptr; packets_ = nullptr; mdl_ = nullptr;
        bufferBytes_ = notifications_ = 0; Reset();
    }
    adapter_->Unlock(irql);
    if (buffer) { port_->UnmapAllocatedPages(buffer, mdl); port_->FreePagesFromMdl(mdl); }
    if (packets) ExFreePoolWithTag(packets, BioPoolTag);
    BioPumpRead(adapter_->State());
}
STDMETHODIMP_(void) CMiniportWaveRTStream::FreeBufferWithNotification(PMDL mdl, ULONG bytes) { FreeAudioBuffer(mdl, bytes); }
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::RegisterNotificationEvent(PKEVENT event) {
    if (!event) return STATUS_INVALID_PARAMETER;
    auto entry = static_cast<Notification*>(ExAllocatePool2(POOL_FLAG_NON_PAGED, sizeof(Notification), BioPoolTag));
    if (!entry) return STATUS_INSUFFICIENT_RESOURCES;
    entry->Event = event;
    NTSTATUS status = STATUS_SUCCESS;
    KIRQL irql; adapter_->Lock(&irql);
    for (auto link = events_.Flink; link != &events_; link = link->Flink) {
        if (CONTAINING_RECORD(link, Notification, Link)->Event == event) { status = STATUS_OBJECT_NAME_COLLISION; break; }
    }
    if (NT_SUCCESS(status)) InsertTailList(&events_, &entry->Link);
    adapter_->Unlock(irql);
    if (!NT_SUCCESS(status)) ExFreePoolWithTag(entry, BioPoolTag);
    return status;
}
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::UnregisterNotificationEvent(PKEVENT event) {
    Notification* removed = nullptr;
    KIRQL irql; adapter_->Lock(&irql);
    for (auto link = events_.Flink; link != &events_; link = link->Flink) {
        auto entry = CONTAINING_RECORD(link, Notification, Link);
        if (entry->Event == event) { RemoveEntryList(link); removed = entry; break; }
    }
    adapter_->Unlock(irql);
    if (!removed) return STATUS_NOT_FOUND;
    ExFreePoolWithTag(removed, BioPoolTag); return STATUS_SUCCESS;
}
void CMiniportWaveRTStream::WriteBytes(ULONGLONG begin, ULONGLONG bytes, ULONGLONG qpc) {
    while (bytes) {
        ULONG offset = static_cast<ULONG>(begin % bufferBytes_);
        ULONG length = static_cast<ULONG>(bytes > bufferBytes_ ? bufferBytes_ : bytes);
        // One record carries at most kMaxDataPayload (the contract's limit).
        if (length > bio::kMaxDataPayload) length = bio::kMaxDataPayload;
        ULONGLONG written = 0;
        if (notifications_ && adapter_->Settings().WritePacket) {
            ULONG packetBytes = bufferBytes_ / notifications_;
            ULONGLONG number = begin / packetBytes;
            ULONG remaining = packetBytes - static_cast<ULONG>(begin % packetBytes);
            if (length > remaining) length = remaining;
            const auto& packet = packets_[number % notifications_];
            if (packet.Valid && packet.Number == number) written = packet.Qpc;
        }
        ULONG first = length < bufferBytes_ - offset ? length : bufferBytes_ - offset;
        adapter_->DataLocked(endpoint_, qpc, written, dma_ + offset, first, length > first ? dma_ : nullptr, length - first);
        begin += length; bytes -= length;
    }
}
void CMiniportWaveRTStream::UpdatePosition(ULONGLONG qpc, LONG trim) {
    if (state_ != KSSTATE_RUN || !dma_ || !adapter_->Active()) return;
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
        if (bytes > bufferBytes_) {
            adapter_->SourceGapLocked(endpoint_, qpc, bytes - bufferBytes_);
            begin = end - bufferBytes_; bytes = bufferBytes_;
        }
        WriteBytes(begin, bytes, qpc);
        transferred_ = end;
    }
    positionQpc_ = qpc;
    if (notifications_) {
        ULONGLONG completed = linear_ / (bufferBytes_ / notifications_);
        if (completed != packetCount_) {
            packetCount_ = completed;
            for (auto link = events_.Flink; link != &events_; link = link->Flink)
                KeSetEvent(CONTAINING_RECORD(link, Notification, Link)->Event, IO_NO_INCREMENT, FALSE);
        }
    }
}
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::GetPosition(PKSAUDIO_POSITION position) {
    KIRQL irql; adapter_->Lock(&irql);
    adapter_->UpdatePositions(BioQpc());
    position->PlayOffset = bufferBytes_ ? transferred_ % bufferBytes_ : 0;
    position->WriteOffset = position->PlayOffset;
    adapter_->Unlock(irql);
    BioPumpRead(adapter_->State()); return STATUS_SUCCESS;
}
STDMETHODIMP_(void) CMiniportWaveRTStream::GetHWLatency(KSRTAUDIO_HWLATENCY* latency) { RtlZeroMemory(latency, sizeof(*latency)); }
// Virtual hardware has no user-mappable hardware position/clock register.
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::GetPositionRegister(KSRTAUDIO_HWREGISTER* reg) { RtlZeroMemory(reg, sizeof(*reg)); return STATUS_NOT_SUPPORTED; }
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::GetClockRegister(KSRTAUDIO_HWREGISTER* reg) { RtlZeroMemory(reg, sizeof(*reg)); return STATUS_NOT_SUPPORTED; }
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::SetWritePacket(ULONG number, DWORD flags, ULONG eosBytes) {
    KIRQL irql; adapter_->Lock(&irql);
    NTSTATUS status = STATUS_SUCCESS;
    ULONGLONG qpc = BioQpc();
    adapter_->UpdatePositions(qpc);
    if (!adapter_->Settings().WritePacket || !notifications_ || !dma_) status = STATUS_NOT_SUPPORTED;
    else if (!adapter_->Active() || eos_) status = STATUS_INVALID_DEVICE_STATE;
    else if ((flags & ~KSSTREAM_HEADER_OPTIONSF_ENDOFSTREAM)) status = STATUS_INVALID_PARAMETER;
    else {
        ULONG packetBytes = bufferBytes_ / notifications_;
        ULONGLONG expected = packetCount_ + (state_ == KSSTATE_RUN ? 1 : 0);
        LONG delta = static_cast<LONG>(number - static_cast<ULONG>(expected));
        if (delta < 0) status = STATUS_DATA_LATE_ERROR;
        else if (delta > 0) status = STATUS_DATA_OVERRUN;
        else if ((flags & KSSTREAM_HEADER_OPTIONSF_ENDOFSTREAM) && (eosBytes > packetBytes || eosBytes % Format()->nBlockAlign)) status = STATUS_INVALID_PARAMETER;
        else {
            auto& packet = packets_[expected % notifications_];
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
    BioPumpRead(adapter_->State()); return status;
}
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::GetPacketCount(ULONG* count) {
    KIRQL irql; adapter_->Lock(&irql);
    NTSTATUS status = notifications_ ? STATUS_SUCCESS : STATUS_NOT_SUPPORTED;
    adapter_->UpdatePositions(BioQpc()); *count = static_cast<ULONG>(packetCount_);
    adapter_->Unlock(irql); BioPumpRead(adapter_->State()); return status;
}
STDMETHODIMP_(NTSTATUS) CMiniportWaveRTStream::GetOutputStreamPresentationPosition(KSAUDIO_PRESENTATION_POSITION* position) {
    KIRQL irql; adapter_->Lock(&irql);
    NTSTATUS status = notifications_ ? STATUS_SUCCESS : STATUS_NOT_SUPPORTED;
    adapter_->UpdatePositions(BioQpc());
    position->u64PositionInBlocks = Format()->nBlockAlign ? linear_ / Format()->nBlockAlign : 0;
    position->u64QPCPosition = positionQpc_;
    adapter_->Unlock(irql); BioPumpRead(adapter_->State()); return status;
}
