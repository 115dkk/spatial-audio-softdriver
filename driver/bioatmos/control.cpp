// SPDX-License-Identifier: MS-PL
// Copyright (c) 2026 115dkk
#include "control.h"
#include <wdmsec.h>

static const GUID BioControlClass = {0x26be775d,0x821e,0x41ad,{0xbc,0x82,0x74,0x51,0x29,0xc1,0x29,0x42}};
static UNICODE_STRING BioLink;

NTSTATUS BioComplete(PIRP irp, NTSTATUS status, ULONG_PTR information) {
    irp->IoStatus.Status = status;
    irp->IoStatus.Information = information;
    IoCompleteRequest(irp, IO_NO_INCREMENT);
    return status;
}
static BioControlState* StateFromQueue(PIO_CSQ queue) {
    return CONTAINING_RECORD(queue, BioControlState, Csq);
}
static BioReadContext* ReadContext(PIRP irp) {
    return static_cast<BioReadContext*>(irp->Tail.Overlay.DriverContext[0]);
}
static void FinishRead(BioControlState* state, PIRP irp, NTSTATUS status, ULONG bytes = 0) {
    auto read = ReadContext(irp);
    if (read->Detached) {
        KIRQL irql;
        KeAcquireSpinLock(&state->Lock, &irql);
        --state->DetachedReads;
        KeReleaseSpinLock(&state->Lock, irql);
    }
    ExFreePoolWithTag(read, BioPoolTag);
    irp->Tail.Overlay.DriverContext[0] = nullptr;
    // No state accesses after releasing this reference: removal can finish.
    IoReleaseRemoveLock(&state->RemoveLock, irp);
    BioComplete(irp, status, bytes);
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
// a READ waits on data alone; data arriving completes it through BioPumpRead.
static void ArmDeadline(BioControlState* state, ULONGLONG deadline) {
    if (!state->DeadlineTimer || deadline == MAXULONGLONG) return;
    ULONGLONG now = BioQpc();
    ULONGLONG left = deadline > now ? deadline - now : 0;
    // Rounded up: firing before the QPC deadline would leave the READ waiting.
    // Clamp the converted duration, not Frequency * 60, so neither product can
    // overflow even with a pathological performance-counter frequency.
    ULONGLONG due = bio::MulDivCeil(left, 10000000, state->Frequency);
    constexpr ULONGLONG oneMinute = 600000000;
    if (due > oneMinute) due = oneMinute;
    ExSetTimer(state->DeadlineTimer, -static_cast<LONGLONG>(due > 0 ? due : 1), 0, nullptr);
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
    if (state->DeadlineTimer) ExCancelTimer(state->DeadlineTimer, nullptr);
}
struct BioPeek { BOOLEAN ReadyOnly; BioFileContext* File; };
static PIRP CsqPeek(PIO_CSQ queue, PIRP previous, PVOID context) {
    auto state = StateFromQueue(queue);
    if (previous || !state->Pending) return nullptr;
    auto peek = static_cast<BioPeek*>(context);
    auto read = ReadContext(state->Pending);
    if (peek && peek->File && read->File != peek->File) return nullptr;
    if (peek && peek->ReadyOnly && state->Started && !state->Removed && !read->File->Cleaned &&
        state->Ring.FirstRecordBytes() <= read->Capacity &&
        !bio::ReadGate::ShouldComplete(state->Ring, read->Minimum, read->Capacity, BioQpc(), read->Deadline))
        return nullptr;
    return state->Pending;
}
static VOID CsqCancel(PIO_CSQ queue, PIRP irp) {
    FinishRead(StateFromQueue(queue), irp, STATUS_CANCELLED);
}
void BioPumpRead(BioControlState* state) {
    // Caller holds an adapter, dispatch, or timer lifetime reference.
    BioPeek peek{TRUE, nullptr};
    PIRP irp = IoCsqRemoveNextIrp(&state->Csq, &peek);
    if (!irp) return;
    auto read = ReadContext(irp);
    KIRQL irql;
    KeAcquireSpinLock(&state->Lock, &irql);
    NTSTATUS status = STATUS_SUCCESS;
    ULONG bytes = 0;
    if (state->Removed || !state->Started || read->Generation != state->Generation) status = STATUS_DEVICE_NOT_READY;
    else if (read->File->Cleaned) status = STATUS_CANCELLED;
    else if (state->Ring.FirstRecordBytes() > read->Capacity) status = STATUS_BUFFER_TOO_SMALL;
    else bytes = state->Ring.Read(read->Output, read->Capacity);
    KeReleaseSpinLock(&state->Lock, irql);
    FinishRead(state, irp, status, bytes);
}
static VOID DeadlineTick(PEX_TIMER, PVOID context) {
    auto state = static_cast<BioControlState*>(context);
    BioPumpRead(state);
    // Still pending: the deadline is further than one shot, or the timer and
    // QPC disagreed by a tick. Arm again for what is left.
    KIRQL irql;
    KeAcquireSpinLock(&state->Lock, &irql);
    if (state->Pending) ArmDeadline(state, ReadContext(state->Pending)->Deadline);
    KeReleaseSpinLock(&state->Lock, irql);
}
NTSTATUS BioCreateControl(PDRIVER_OBJECT driver, const BioSettings& settings, PDEVICE_OBJECT* device) {
    PAGED_CODE();
    *device = nullptr;
    UNICODE_STRING name; RtlInitUnicodeString(&name, L"\\Device\\BioAtmos");
    RtlInitUnicodeString(&BioLink, L"\\DosDevices\\BioAtmos");
    UNICODE_STRING sddl; RtlInitUnicodeString(&sddl, L"D:P(A;;GA;;;SY)(A;;GRGW;;;BA)(A;;GRGW;;;IU)");
    PDEVICE_OBJECT created = nullptr;
    NTSTATUS status = IoCreateDeviceSecure(driver, sizeof(BioControlState), &name,
        FILE_DEVICE_UNKNOWN, FILE_DEVICE_SECURE_OPEN, FALSE, &sddl, &BioControlClass, &created);
    if (!NT_SUCCESS(status)) return status;
    auto state = new (created->DeviceExtension) BioControlState{};
    KeInitializeSpinLock(&state->Lock);
    IoInitializeRemoveLock(&state->RemoveLock, BioPoolTag, 0, 0);
    state->Settings = settings;
    LARGE_INTEGER frequency;
    KeQueryPerformanceCounter(&frequency);
    state->Frequency = static_cast<ULONGLONG>(frequency.QuadPart);
    status = IoCsqInitializeEx(&state->Csq, CsqInsert, CsqRemove, CsqPeek, CsqAcquire, CsqRelease, CsqCancel);
    if (NT_SUCCESS(status)) status = IoCreateSymbolicLink(&BioLink, &name);
    if (!NT_SUCCESS(status)) { IoDeleteDevice(created); return status; }
    created->Flags &= ~DO_DEVICE_INITIALIZING;
    *device = created;
    return STATUS_SUCCESS;
}
NTSTATUS BioStartControl(BioControlState* state) {
    PAGED_CODE();
    auto storage = static_cast<PUCHAR>(ExAllocatePool2(POOL_FLAG_NON_PAGED, BioRingBytes, BioPoolTag));
    if (!storage) return STATUS_INSUFFICIENT_RESOURCES;
    PEX_TIMER timer = ExAllocateTimer(DeadlineTick, state, EX_TIMER_HIGH_RESOLUTION);
    if (!timer) { ExFreePoolWithTag(storage, BioPoolTag); return STATUS_INSUFFICIENT_RESOURCES; }
    KIRQL irql;
    KeAcquireSpinLock(&state->Lock, &irql);
    NTSTATUS status = STATUS_SUCCESS;
    if (state->Removed || state->Started) status = STATUS_INVALID_DEVICE_STATE;
    else if (!state->Ring.Init(storage, BioRingBytes)) status = STATUS_INVALID_PARAMETER;
    else {
        state->Storage = storage;
        state->DeadlineTimer = timer;
        state->Trim = 0;
        state->MaxLatenessUs = 0;
        ++state->Generation;
        state->Started = TRUE;
    }
    KeReleaseSpinLock(&state->Lock, irql);
    if (!NT_SUCCESS(status)) {
        ExDeleteTimer(timer, TRUE, TRUE, nullptr);
        ExFreePoolWithTag(storage, BioPoolTag);
    }
    return status;
}
void BioStopControl(BioControlState* state) {
    PAGED_CODE();
    KIRQL irql;
    KeAcquireSpinLock(&state->Lock, &irql);
    state->Started = FALSE;
    state->Trim = 0;
    PEX_TIMER timer = state->DeadlineTimer;
    state->DeadlineTimer = nullptr;
    PUCHAR storage = state->Storage;
    state->Storage = nullptr;
    KeReleaseSpinLock(&state->Lock, irql);
    if (timer) ExDeleteTimer(timer, TRUE, TRUE, nullptr);
    PIRP pending;
    while ((pending = IoCsqRemoveNextIrp(&state->Csq, nullptr)) != nullptr)
        FinishRead(state, pending, STATUS_DEVICE_NOT_READY);
    if (storage) ExFreePoolWithTag(storage, BioPoolTag);
}
void BioDeleteControl(PDEVICE_OBJECT device) {
    PAGED_CODE();
    auto state = BioControl(device);
    // This reference lets RemoveLockAndWait account for the remove operation.
    NTSTATUS status = IoAcquireRemoveLock(&state->RemoveLock, device);
    NT_ASSERT(NT_SUCCESS(status));
    UNREFERENCED_PARAMETER(status);
    KIRQL irql;
    KeAcquireSpinLock(&state->Lock, &irql);
    state->Removed = TRUE;
    KeReleaseSpinLock(&state->Lock, irql);
    IoDeleteSymbolicLink(&BioLink);
    BioStopControl(state);
    IoReleaseRemoveLockAndWait(&state->RemoveLock, device);
    // Open FILE_OBJECT references retain the device extension until CLOSE.
    IoDeleteDevice(device);
}
static void CleanFile(BioControlState* state, BioFileContext* file) {
    KIRQL irql;
    KeAcquireSpinLock(&state->Lock, &irql);
    file->Cleaned = TRUE;
    state->Trim = 0;
    if (state->Started) state->Ring.DiscardQueued();
    KeReleaseSpinLock(&state->Lock, irql);
    BioPeek peek{FALSE, file};
    PIRP pending;
    while ((pending = IoCsqRemoveNextIrp(&state->Csq, &peek)) != nullptr)
        FinishRead(state, pending, STATUS_CANCELLED);
}
NTSTATUS BioControlDispatch(PDEVICE_OBJECT device, PIRP irp) {
    auto state = BioControl(device);
    auto stack = IoGetCurrentIrpStackLocation(irp);
    if (!stack->FileObject) return BioComplete(irp, STATUS_INVALID_PARAMETER);
    auto file = static_cast<BioFileContext*>(stack->FileObject->FsContext);
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
                ExFreePoolWithTag(file, BioPoolTag);
            }
        }
        return BioComplete(irp, STATUS_SUCCESS);
    }
    NTSTATUS status = IoAcquireRemoveLock(&state->RemoveLock, irp);
    if (!NT_SUCCESS(status)) return BioComplete(irp, status);
    ULONG_PTR information = 0;
    KIRQL irql;
    if (stack->MajorFunction == IRP_MJ_CREATE) {
        if (stack->FileObject->FileName.Length != 0) status = STATUS_OBJECT_NAME_NOT_FOUND;
        else file = static_cast<BioFileContext*>(ExAllocatePool2(POOL_FLAG_NON_PAGED, sizeof(*file), BioPoolTag));
        if (NT_SUCCESS(status) && !file) status = STATUS_INSUFFICIENT_RESOURCES;
        else if (file) {
            KeAcquireSpinLock(&state->Lock, &irql);
            if (state->Removed) status = STATUS_DELETE_PENDING;
            else if (state->OpenCount) status = STATUS_SHARING_VIOLATION;
            else {
                state->OpenCount = 1; stack->FileObject->FsContext = file;
                if (state->Started) state->Ring.DiscardQueued();
            }
            KeReleaseSpinLock(&state->Lock, irql);
            if (!NT_SUCCESS(status)) ExFreePoolWithTag(file, BioPoolTag);
        }
    } else if (stack->MajorFunction != IRP_MJ_DEVICE_CONTROL || !file) {
        status = STATUS_INVALID_DEVICE_REQUEST;
    } else {
        ULONG code = stack->Parameters.DeviceIoControl.IoControlCode;
        ULONG input = stack->Parameters.DeviceIoControl.InputBufferLength;
        ULONG output = stack->Parameters.DeviceIoControl.OutputBufferLength;
        PVOID buffer = irp->AssociatedIrp.SystemBuffer;
        BioReadContext* read = nullptr;
        status = STATUS_SUCCESS;
        if (code == IOCTL_BIO_READ) {
            if (input < sizeof(BIO_READ_REQUEST)) status = STATUS_INVALID_PARAMETER;
            else if (!output || !irp->MdlAddress) status = STATUS_BUFFER_TOO_SMALL;
            else if (output > MmGetMdlByteCount(irp->MdlAddress)) status = STATUS_INVALID_BUFFER_SIZE;
            else {
                auto mapped = static_cast<PUCHAR>(MmGetSystemAddressForMdlSafe(irp->MdlAddress,
                    NormalPagePriority | MdlMappingNoExecute));
                if (!mapped) status = STATUS_INSUFFICIENT_RESOURCES;
                else {
                    read = static_cast<BioReadContext*>(ExAllocatePool2(POOL_FLAG_NON_PAGED, sizeof(*read), BioPoolTag));
                    if (!read) status = STATUS_INSUFFICIENT_RESOURCES;
                    else {
                        const auto request = static_cast<BIO_READ_REQUEST*>(buffer);
                        *read = {mapped, bio::ReadGate::Capacity(output), request->min_bytes,
                            bio::ReadGate::Deadline(BioQpc(), state->Frequency, request->timeout_ms), file, FALSE, 0};
                    }
                }
            }
        }
        if (NT_SUCCESS(status)) {
            KeAcquireSpinLock(&state->Lock, &irql);
            if (state->Removed || !state->Started) status = STATUS_DEVICE_NOT_READY;
            else if (file->Cleaned) status = STATUS_FILE_CLOSED;
            else switch (code) {
            case IOCTL_BIO_QUERY_INFO:
                if (output < sizeof(BIO_ABI_INFO)) { status = STATUS_BUFFER_TOO_SMALL; break; }
                {
                    // Filled on the stack and copied once: the output is the
                    // METHOD_BUFFERED system buffer, never the caller's memory.
                    BIO_ABI_INFO info = {};
                    info.abi_version = BIO_ABI_VERSION;
                    info.struct_size = sizeof(info);
                    info.driver_version = 1u << 16;
                    info.ring_bytes = BioRingBytes;
                    info.compiled_formats = BioCompiledFormats;
                    info.features = BIO_FEATURE_PENDING_READ | BIO_FEATURE_RATE_TRIM |
                        (state->Settings.WritePacket ? BIO_FEATURE_WRITE_PACKET : 0);
                    info.qpc_frequency = state->Frequency;
                    info.endpoint_count = state->Settings.EndpointCount;
                    for (ULONG i = 0; i < info.endpoint_count && i < BIO_MAX_ENDPOINTS; ++i)
                        info.enabled_formats[i] = state->Settings.FormatMask[i];
                    RtlCopyMemory(buffer, &info, sizeof(info));
                    information = sizeof(info);
                }
                break;
            case IOCTL_BIO_GET_STATS:
                if (output < sizeof(BIO_STATS)) status = STATUS_BUFFER_TOO_SMALL;
                else {
                    BIO_STATS stats;
                    state->Ring.FillStats(&stats, state->OpenCount, state->MaxLatenessUs);
                    RtlCopyMemory(buffer, &stats, sizeof(stats));
                    information = sizeof(stats);
                }
                break;
            case IOCTL_BIO_SET_RATE_TRIM:
                if (input < sizeof(BIO_RATE_TRIM) || static_cast<BIO_RATE_TRIM*>(buffer)->reserved) status = STATUS_INVALID_PARAMETER;
                else {
                    LONG trim = static_cast<BIO_RATE_TRIM*>(buffer)->ppm_q16;
                    constexpr LONG limit = BIO_TRIM_LIMIT_PPM * 65536;
                    state->Trim = trim < -limit ? -limit : (trim > limit ? limit : trim);
                }
                break;
            case IOCTL_BIO_READ:
                read->Generation = state->Generation;
                if (state->Pending || state->DetachedReads) status = STATUS_DEVICE_BUSY;
                else if (state->Ring.FirstRecordBytes() > read->Capacity) status = STATUS_BUFFER_TOO_SMALL;
                else if (bio::ReadGate::ShouldComplete(state->Ring, read->Minimum, read->Capacity, BioQpc(), read->Deadline))
                    information = state->Ring.Read(read->Output, read->Capacity);
                else status = STATUS_PENDING;
                break;
            default: status = STATUS_INVALID_DEVICE_REQUEST; break;
            }
            KeReleaseSpinLock(&state->Lock, irql);
        }
        if (status == STATUS_PENDING) {
            // Keep a separate dispatch reference: CSQ can complete before insertion returns.
            UCHAR dispatchTag = 0;
            NTSTATUS held = IoAcquireRemoveLock(&state->RemoveLock, &dispatchTag);
            if (NT_SUCCESS(held)) {
                irp->Tail.Overlay.DriverContext[0] = read;
                IoMarkIrpPending(irp);
                NTSTATUS inserted = IoCsqInsertIrpEx(&state->Csq, irp, nullptr, nullptr);
                if (!NT_SUCCESS(inserted)) FinishRead(state, irp, inserted);
                // Catch a write/cleanup between the first gate check and insertion.
                BioPumpRead(state);
                IoReleaseRemoveLock(&state->RemoveLock, &dispatchTag);
                return STATUS_PENDING;
            }
            status = held;
        }
        if (read) ExFreePoolWithTag(read, BioPoolTag);
    }
    IoReleaseRemoveLock(&state->RemoveLock, irp);
    return BioComplete(irp, status, information);
}
