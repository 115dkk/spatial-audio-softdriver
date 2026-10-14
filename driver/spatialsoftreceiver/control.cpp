// SPDX-License-Identifier: MS-PL
// Copyright (c) 2026 115dkk
#include "control.h"
#include "hdmiwavtable.h"
#include <wdmsec.h>

static constinit const GUID SsrControlClass = {0x26be775d,0x821e,0x41ad,{0xbc,0x82,0x74,0x51,0x29,0xc1,0x29,0x42}};
static constinit UNICODE_STRING SsrLink{};

NTSTATUS SsrComplete(PIRP irp, NTSTATUS status, ULONG_PTR information) {
    irp->IoStatus.Status = status;
    irp->IoStatus.Information = information;
    IoCompleteRequest(irp, IO_NO_INCREMENT);
    return status;
}
static SsrControlState* StateFromQueue(PIO_CSQ queue) {
    return CONTAINING_RECORD(queue, SsrControlState, Csq);
}
static SsrReadContext* ReadContext(PIRP irp) {
    return static_cast<SsrReadContext*>(irp->Tail.Overlay.DriverContext[0]);
}
static void FinishRead(SsrControlState* state, PIRP irp, NTSTATUS status, ULONG bytes = 0) {
    auto read = ssr::km::PoolPtr<SsrReadContext>(ReadContext(irp));
    if (read->Detached) {
        KIRQL irql;
        KeAcquireSpinLock(&state->Lock, &irql);
        --state->DetachedReads;
        KeReleaseSpinLock(&state->Lock, irql);
    }
    irp->Tail.Overlay.DriverContext[0] = nullptr;
    read.Reset();
    // No state accesses after releasing this reference: removal can finish.
    IoReleaseRemoveLock(&state->RemoveLock, irp);
    SsrComplete(irp, status, bytes);
}
static IO_CSQ_ACQUIRE_LOCK CsqAcquire;
static IO_CSQ_RELEASE_LOCK CsqRelease;
_IRQL_raises_(DISPATCH_LEVEL)
static VOID CsqAcquire(PIO_CSQ queue, _Out_ _At_(*irql, _IRQL_saves_) PKIRQL irql) {
    KeAcquireSpinLock(&StateFromQueue(queue)->Lock, irql);
}
_IRQL_requires_(DISPATCH_LEVEL)
static VOID CsqRelease(PIO_CSQ queue, _IRQL_restores_ KIRQL irql) {
    KeReleaseSpinLock(&StateFromQueue(queue)->Lock, irql);
}
// Under state->Lock. One shot at the READ's deadline, so no timer runs while
// a READ waits on data alone; data arriving completes it through SsrPumpRead.
static void ArmDeadline(SsrControlState* state, ULONGLONG deadline) {
    if (!state->DeadlineTimer || deadline == MAXULONGLONG) return;
    ULONGLONG now = SsrQpc();
    ULONGLONG left = deadline > now ? deadline - now : 0;
    // Rounded up: firing before the QPC deadline would leave the READ waiting.
    // Clamp the converted duration, not Frequency * 60, so neither product can
    // overflow even with a pathological performance-counter frequency.
    ULONGLONG due = ssr::MulDivCeil(left, 10000000, state->Frequency);
    constexpr ULONGLONG oneMinute = 600000000;
    if (due > oneMinute) due = oneMinute;
    ExSetTimer(state->DeadlineTimer.Get(), -static_cast<LONGLONG>(due > 0 ? due : 1), 0, nullptr);
}
static NTSTATUS CsqInsert(PIO_CSQ queue, PIRP irp, PVOID) {
    auto state = StateFromQueue(queue);
    if (state->Removed || !state->Started) return STATUS_DEVICE_NOT_READY;
    if (ReadContext(irp)->Generation != state->Generation) return STATUS_DEVICE_NOT_READY;
    if (ReadContext(irp)->File->Cleaned) return STATUS_FILE_CLOSED;
    if (state->Pending || state->DetachedReads) return STATUS_DEVICE_BUSY;
    state->Pending = irp;
    // Timer ownership and rearming are protected by the same lock as detach.
    ArmDeadline(state, ReadContext(irp)->Deadline);
    return STATUS_SUCCESS;
}
static VOID CsqRemove(PIO_CSQ queue, PIRP irp) {
    auto state = StateFromQueue(queue);
    NT_ASSERT(state->Pending == irp);
    UNREFERENCED_PARAMETER(irp);
    state->Pending = nullptr;
    // Preserve READ ownership across the CSQ-unlock/consumer-lock gap.
    ReadContext(irp)->Detached = TRUE;
    ++state->DetachedReads;
    if (state->DeadlineTimer) ExCancelTimer(state->DeadlineTimer.Get(), nullptr);
}
struct SsrPeek { BOOLEAN ReadyOnly; SsrFileContext* File; };
static PIRP CsqPeek(PIO_CSQ queue, PIRP previous, PVOID context) {
    auto state = StateFromQueue(queue);
    if (previous || !state->Pending) return nullptr;
    auto peek = static_cast<SsrPeek*>(context);
    auto read = ReadContext(state->Pending);
    if (peek && peek->File && read->File != peek->File) return nullptr;
    if (peek && peek->ReadyOnly && state->Started && !state->Removed && !read->File->Cleaned &&
        state->Ring.FirstRecordBytes() <= read->Capacity &&
        !ssr::ReadGate::ShouldComplete(state->Ring, read->Minimum, read->Capacity, SsrQpc(), read->Deadline))
        return nullptr;
    return state->Pending;
}
static VOID CsqCancel(PIO_CSQ queue, PIRP irp) {
    FinishRead(StateFromQueue(queue), irp, STATUS_CANCELLED);
}
void SsrPumpRead(SsrControlState* state) {
    // Caller holds an adapter, dispatch, or timer lifetime reference.
    SsrPeek peek{TRUE, nullptr};
    PIRP irp = IoCsqRemoveNextIrp(&state->Csq, &peek);
    if (!irp) return;
    auto read = ReadContext(irp);
    KIRQL irql;
    KeAcquireSpinLock(&state->Lock, &irql);
    NTSTATUS status = STATUS_SUCCESS;
    ULONG bytes = 0;
    if (state->Removed || !state->Started || read->Generation != state->Generation) status = STATUS_DEVICE_NOT_READY;
    else if (read->File->Cleaned) status = STATUS_CANCELLED;
    else {
        const ULONGLONG qpc = SsrQpc();
        state->Ring.PrepareRead(qpc);
        if (state->Ring.FirstRecordBytes() > read->Capacity) status = STATUS_BUFFER_TOO_SMALL;
        else bytes = state->Ring.Read(read->Output, read->Capacity, qpc);
    }
    KeReleaseSpinLock(&state->Lock, irql);
    FinishRead(state, irp, status, bytes);
}
static VOID DeadlineTick(PEX_TIMER, PVOID context) {
    auto state = static_cast<SsrControlState*>(context);
    SsrPumpRead(state);
    // Still pending: the deadline is further than one shot, or the timer and
    // QPC disagreed by a tick. Arm again for what is left.
    KIRQL irql;
    KeAcquireSpinLock(&state->Lock, &irql);
    if (state->Pending) ArmDeadline(state, ReadContext(state->Pending)->Deadline);
    KeReleaseSpinLock(&state->Lock, irql);
}
NTSTATUS SsrCreateControl(PDRIVER_OBJECT driver, const SsrSettings& settings, PDEVICE_OBJECT* device) {
    PAGED_CODE();
    *device = nullptr;
    UNICODE_STRING name; RtlInitUnicodeString(&name, L"\\Device\\SpatialSoftReceiver");
    RtlInitUnicodeString(&SsrLink, L"\\DosDevices\\SpatialSoftReceiver");
    UNICODE_STRING sddl; RtlInitUnicodeString(&sddl, L"D:P(A;;GA;;;SY)(A;;GRGW;;;BA)(A;;GRGW;;;IU)");
    PDEVICE_OBJECT created = nullptr;
    NTSTATUS status = ssr::km::fault::Hit() ? STATUS_INSUFFICIENT_RESOURCES :
        IoCreateDeviceSecure(driver, sizeof(SsrControlState), &name,
            FILE_DEVICE_UNKNOWN, FILE_DEVICE_SECURE_OPEN, FALSE, &sddl, &SsrControlClass, &created);
    if (!NT_SUCCESS(status)) return status;
    auto state = new (created->DeviceExtension) SsrControlState{};
    KeInitializeSpinLock(&state->Lock);
    IoInitializeRemoveLock(&state->RemoveLock, SsrPoolTag, 0, 0);
    state->Settings = settings;
    LARGE_INTEGER frequency;
    KeQueryPerformanceCounter(&frequency);
    state->Frequency = static_cast<ULONGLONG>(frequency.QuadPart);
    status = IoCsqInitializeEx(&state->Csq, CsqInsert, CsqRemove, CsqPeek, CsqAcquire, CsqRelease, CsqCancel);
    if (NT_SUCCESS(status)) status = IoCreateSymbolicLink(&SsrLink, &name);
    if (!NT_SUCCESS(status)) {
        state->~SsrControlState();
        IoDeleteDevice(created);
        return status;
    }
    created->Flags &= ~DO_DEVICE_INITIALIZING;
    *device = created;
    return STATUS_SUCCESS;
}
NTSTATUS SsrStartControl(SsrControlState* state) {
    PAGED_CODE();
    auto storage = ssr::km::PoolPtr<UCHAR>::Allocate(SsrRingBytes);
    if (!storage) return STATUS_INSUFFICIENT_RESOURCES;
    auto timer = ssr::km::ExTimer::Allocate(DeadlineTick, state, EX_TIMER_HIGH_RESOLUTION);
    if (!timer) return STATUS_INSUFFICIENT_RESOURCES;
    KIRQL irql;
    KeAcquireSpinLock(&state->Lock, &irql);
    NTSTATUS status = STATUS_SUCCESS;
    if (state->Removed || state->Started) status = STATUS_INVALID_DEVICE_STATE;
    else if (!state->Ring.Init(storage.Get(), SsrRingBytes)) status = STATUS_INVALID_PARAMETER;
    else {
        for (USHORT ep = 0; ep < SSR_MAX_ENDPOINTS; ++ep)
            if (state->Settings.EndpointMask & (1UL << ep))
                state->Ring.ConfigureVolume(ep, SsrVolumeChannels(ep));
        state->Storage = ssr::km::Move(storage);
        state->DeadlineTimer = ssr::km::Move(timer);
        state->Trim = 0;
        state->MaxLatenessUs = 0;
        ++state->Generation;
        state->Started = TRUE;
    }
    KeReleaseSpinLock(&state->Lock, irql);
    return status;
}
void SsrStopControl(SsrControlState* state) {
    PAGED_CODE();
    ssr::km::ExTimer timer;
    ssr::km::PoolPtr<UCHAR> storage;
    KIRQL irql;
    KeAcquireSpinLock(&state->Lock, &irql);
    state->Started = FALSE;
    state->Trim = 0;
    timer = ssr::km::Move(state->DeadlineTimer);
    storage = ssr::km::Move(state->Storage);
    KeReleaseSpinLock(&state->Lock, irql);
    timer.Reset();
    PIRP pending;
    while ((pending = IoCsqRemoveNextIrp(&state->Csq, nullptr)) != nullptr)
        FinishRead(state, pending, STATUS_DEVICE_NOT_READY);
    storage.Reset();
}
void SsrDeleteControl(PDEVICE_OBJECT device) {
    PAGED_CODE();
    auto state = SsrControl(device);
    // This reference lets RemoveLockAndWait account for the remove operation.
    NTSTATUS status = IoAcquireRemoveLock(&state->RemoveLock, device);
    NT_ASSERT(NT_SUCCESS(status));
    UNREFERENCED_PARAMETER(status);
    KIRQL irql;
    KeAcquireSpinLock(&state->Lock, &irql);
    state->Removed = TRUE;
    KeReleaseSpinLock(&state->Lock, irql);
    IoDeleteSymbolicLink(&SsrLink);
    SsrStopControl(state);
    IoReleaseRemoveLockAndWait(&state->RemoveLock, device);
    // Open FILE_OBJECT references retain the device extension until CLOSE,
    // and CLEANUP/CLOSE still use this state after IoDeleteDevice, so its
    // lifetime does not end here. SsrStopControl has already released the
    // only members that own anything; the I/O manager frees the memory.
    NT_ASSERT(!state->DeadlineTimer && !state->Storage);
    IoDeleteDevice(device);
}
static void CleanFile(SsrControlState* state, SsrFileContext* file) {
    KIRQL irql;
    KeAcquireSpinLock(&state->Lock, &irql);
    file->Cleaned = TRUE;
    state->Trim = 0;
    if (state->Started) state->Ring.DiscardQueued(SsrQpc());
    KeReleaseSpinLock(&state->Lock, irql);
    SsrPeek peek{FALSE, file};
    PIRP pending;
    while ((pending = IoCsqRemoveNextIrp(&state->Csq, &peek)) != nullptr)
        FinishRead(state, pending, STATUS_CANCELLED);
}
NTSTATUS SsrControlDispatch(PDEVICE_OBJECT device, PIRP irp) {
    auto state = SsrControl(device);
    auto stack = IoGetCurrentIrpStackLocation(irp);
    if (!stack->FileObject) return SsrComplete(irp, STATUS_INVALID_PARAMETER);
    auto file = static_cast<SsrFileContext*>(stack->FileObject->FsContext);
    // CLOSE/CLEANUP must work after remove-lock rundown. The file pins the DO.
    if (stack->MajorFunction == IRP_MJ_CLEANUP || stack->MajorFunction == IRP_MJ_CLOSE) {
        if (file) {
            CleanFile(state, file);
            if (stack->MajorFunction == IRP_MJ_CLOSE) {
                KIRQL irql;
                KeAcquireSpinLock(&state->Lock, &irql);
                --state->OpenCount;
                KeReleaseSpinLock(&state->Lock, irql);
                stack->FileObject->FsContext = nullptr;
                ssr::km::PoolPtr<SsrFileContext> closeFile(file);
            }
        }
        return SsrComplete(irp, STATUS_SUCCESS);
    }
    NTSTATUS status = IoAcquireRemoveLock(&state->RemoveLock, irp);
    if (!NT_SUCCESS(status)) return SsrComplete(irp, status);
    ULONG_PTR information = 0;
    KIRQL irql;
    if (stack->MajorFunction == IRP_MJ_CREATE) {
        ssr::km::PoolPtr<SsrFileContext> newFile;
        if (stack->FileObject->FileName.Length != 0) status = STATUS_OBJECT_NAME_NOT_FOUND;
        else newFile = ssr::km::PoolPtr<SsrFileContext>::Allocate();
        if (NT_SUCCESS(status) && !newFile) status = STATUS_INSUFFICIENT_RESOURCES;
        else if (newFile) {
            KeAcquireSpinLock(&state->Lock, &irql);
            if (state->Removed) status = STATUS_DELETE_PENDING;
            else if (state->OpenCount) status = STATUS_SHARING_VIOLATION;
            else {
                state->OpenCount = 1;
                stack->FileObject->FsContext = newFile.Release();
                if (state->Started) state->Ring.DiscardQueued(SsrQpc());
            }
            KeReleaseSpinLock(&state->Lock, irql);
        }
    } else if (stack->MajorFunction != IRP_MJ_DEVICE_CONTROL || !file) {
        status = STATUS_INVALID_DEVICE_REQUEST;
    } else {
        ULONG code = stack->Parameters.DeviceIoControl.IoControlCode;
        ULONG input = stack->Parameters.DeviceIoControl.InputBufferLength;
        ULONG output = stack->Parameters.DeviceIoControl.OutputBufferLength;
        PVOID buffer = irp->AssociatedIrp.SystemBuffer;
        ssr::km::PoolPtr<SsrReadContext> read;
        status = STATUS_SUCCESS;
        if (code == IOCTL_SSR_READ) {
            if (input < sizeof(SSR_READ_REQUEST)) status = STATUS_INVALID_PARAMETER;
            else if (!output || !irp->MdlAddress) status = STATUS_BUFFER_TOO_SMALL;
            else if (output > MmGetMdlByteCount(irp->MdlAddress)) status = STATUS_INVALID_BUFFER_SIZE;
            else {
                auto mapped = static_cast<PUCHAR>(MmGetSystemAddressForMdlSafe(irp->MdlAddress,
                    NormalPagePriority | MdlMappingNoExecute));
                if (!mapped) status = STATUS_INSUFFICIENT_RESOURCES;
                else {
                    read = ssr::km::PoolPtr<SsrReadContext>::Allocate();
                    if (!read) status = STATUS_INSUFFICIENT_RESOURCES;
                    else {
                        const auto request = static_cast<SSR_READ_REQUEST*>(buffer);
                        *read = {mapped, ssr::ReadGate::Capacity(output), request->min_bytes,
                            ssr::ReadGate::Deadline(SsrQpc(), state->Frequency, request->timeout_ms), file, FALSE, 0};
                    }
                }
            }
        }
        if (NT_SUCCESS(status)) {
            KeAcquireSpinLock(&state->Lock, &irql);
            if (state->Removed || !state->Started) status = STATUS_DEVICE_NOT_READY;
            else if (file->Cleaned) status = STATUS_FILE_CLOSED;
            else switch (code) {
            case IOCTL_SSR_QUERY_INFO:
                if (output < sizeof(SSR_ABI_INFO)) { status = STATUS_BUFFER_TOO_SMALL; break; }
                {
                    // Filled on the stack and copied once: the output is the
                    // METHOD_BUFFERED system buffer, never the caller's memory.
                    SSR_ABI_INFO info = {};
                    info.abi_version = SSR_ABI_VERSION;
                    info.struct_size = sizeof(info);
                    info.driver_version = 1u << 16;
                    info.ring_bytes = SsrRingBytes;
                    info.compiled = SsrCompiledFormats;
                    info.features = SSR_FEATURE_PENDING_READ | SSR_FEATURE_RATE_TRIM |
                        (state->Settings.WritePacket ? SSR_FEATURE_WRITE_PACKET : 0);
                    info.qpc_frequency = state->Frequency;
                    info.endpoint_mask = state->Settings.EndpointMask;
                    for (ULONG i = 0; i < SSR_MAX_ENDPOINTS; ++i) {
                        if (info.endpoint_mask & (1UL << i))
                            info.endpoints[i] = state->Settings.Formats[i];
                    }
                    RtlCopyMemory(buffer, &info, sizeof(info));
                    information = sizeof(info);
                }
                break;
            case IOCTL_SSR_GET_STATS:
                if (output < sizeof(SSR_STATS)) status = STATUS_BUFFER_TOO_SMALL;
                else {
                    SSR_STATS stats;
                    state->Ring.FillStats(&stats, state->OpenCount, state->MaxLatenessUs);
                    RtlCopyMemory(buffer, &stats, sizeof(stats));
                    information = sizeof(stats);
                }
                break;
            case IOCTL_SSR_SET_RATE_TRIM:
                if (input < sizeof(SSR_RATE_TRIM) || static_cast<SSR_RATE_TRIM*>(buffer)->reserved) status = STATUS_INVALID_PARAMETER;
                else {
                    LONG trim = static_cast<SSR_RATE_TRIM*>(buffer)->ppm_q16;
                    constexpr LONG limit = SSR_TRIM_LIMIT_PPM * 65536;
                    state->Trim = trim < -limit ? -limit : (trim > limit ? limit : trim);
                }
                break;
            case IOCTL_SSR_READ: {
                read->Generation = state->Generation;
                const ULONGLONG qpc = SsrQpc();
                state->Ring.PrepareRead(qpc);
                if (state->Pending || state->DetachedReads) status = STATUS_DEVICE_BUSY;
                else if (state->Ring.FirstRecordBytes() > read->Capacity) status = STATUS_BUFFER_TOO_SMALL;
                else if (ssr::ReadGate::ShouldComplete(state->Ring, read->Minimum, read->Capacity, qpc, read->Deadline))
                    information = state->Ring.Read(read->Output, read->Capacity, qpc);
                else status = STATUS_PENDING;
                break;
            }
            default: status = STATUS_INVALID_DEVICE_REQUEST; break;
            }
            KeReleaseSpinLock(&state->Lock, irql);
        }
        if (status == STATUS_PENDING) {
            // Keep a separate dispatch reference: CSQ can complete before insertion returns.
            UCHAR dispatchTag = 0;
            ssr::km::RemoveLockHold dispatchHold;
            NTSTATUS held = dispatchHold.Acquire(&state->RemoveLock, &dispatchTag);
            if (NT_SUCCESS(held)) {
                irp->Tail.Overlay.DriverContext[0] = read.Release();
                IoMarkIrpPending(irp);
                NTSTATUS inserted = IoCsqInsertIrpEx(&state->Csq, irp, nullptr, nullptr);
                if (!NT_SUCCESS(inserted)) FinishRead(state, irp, inserted);
                // Catch a write/cleanup between the first gate check and insertion.
                SsrPumpRead(state);
                return STATUS_PENDING;
            }
            status = held;
        }
        read.Reset();
    }
    IoReleaseRemoveLock(&state->RemoveLock, irp);
    return SsrComplete(irp, status, information);
}
