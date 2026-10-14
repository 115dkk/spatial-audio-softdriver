// SPDX-License-Identifier: MS-PL
// Copyright (c) 2026 115dkk
#include "kstrace.h"
#include <TraceLoggingProvider.h>

// {a63636ab-03aa-4a58-9dcd-86c273eb95fd}
TRACELOGGING_DEFINE_PROVIDER(SsrKsProvider, "SpatialSoftReceiver.Driver.Ks",
    (0xa63636ab, 0x03aa, 0x4a58, 0x9d, 0xcd, 0x86, 0xc2, 0x73, 0xeb, 0x95, 0xfd));

static bool Registered;

void SsrTraceRegister() { Registered = NT_SUCCESS(TraceLoggingRegister(SsrKsProvider)); }
void SsrTraceUnregister() {
    if (Registered) TraceLoggingUnregister(SsrKsProvider);
    Registered = false;
}

// Copies up to `capacity` bytes of a request buffer. A user-mode caller's
// METHOD_NEITHER buffer is probed first; a fault leaves nothing copied.
static USHORT Capture(PVOID source, ULONG length, KPROCESSOR_MODE mode, BYTE* target, ULONG capacity) {
    ULONG bytes = length < capacity ? length : capacity;
    if (!source || !bytes) return 0;
    __try {
        if (mode == UserMode) ProbeForRead(source, bytes, 1);
        RtlCopyMemory(target, source, bytes);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        bytes = 0;
    }
    return static_cast<USHORT>(bytes);
}

void SsrTraceBefore(PIRP irp, SsrIrpNote* note) {
    note->Active = false;
    if (!Registered || !TraceLoggingProviderEnabled(SsrKsProvider, 0, 0)) return;
    auto stack = IoGetCurrentIrpStackLocation(irp);
    if (stack->MajorFunction != IRP_MJ_CREATE && stack->MajorFunction != IRP_MJ_DEVICE_CONTROL) return;
    note->Active = true;
    note->Major = stack->MajorFunction;
    note->Code = 0;
    note->InputLength = note->OutputLength = 0;
    note->File = stack->FileObject;
    note->Related = stack->FileObject ? stack->FileObject->RelatedFileObject : nullptr;
    note->Process = HandleToULong(PsGetCurrentProcessId());
    note->InputBytes = note->ValueBytes = note->NameBytes = 0;
    if (stack->MajorFunction == IRP_MJ_CREATE) {
        // A filter's reference string, or KSSTRING_Pin with the KSPIN_CONNECT
        // and data format after it: the I/O manager's copy, kernel memory.
        if (stack->FileObject && stack->FileObject->FileName.Buffer)
            note->NameBytes = Capture(stack->FileObject->FileName.Buffer, stack->FileObject->FileName.Length,
                                      KernelMode, note->Name, sizeof(note->Name));
        return;
    }
    auto& control = stack->Parameters.DeviceIoControl;
    note->Code = control.IoControlCode;
    note->InputLength = control.InputBufferLength;
    note->OutputLength = control.OutputBufferLength;
    if (METHOD_FROM_CTL_CODE(control.IoControlCode) != METHOD_NEITHER) return;
    // KSPROPERTY or KSP_PIN, then for DATAINTERSECTION the client's ranges.
    note->InputBytes = Capture(control.Type3InputBuffer, control.InputBufferLength, irp->RequestorMode,
                               note->Input, sizeof(note->Input));
    if (control.IoControlCode == IOCTL_KS_PROPERTY && note->InputBytes >= sizeof(KSPROPERTY) &&
        (reinterpret_cast<PKSPROPERTY>(note->Input)->Flags & KSPROPERTY_TYPE_SET))
        note->ValueBytes = Capture(irp->UserBuffer, control.OutputBufferLength, irp->RequestorMode,
                                   note->Value, sizeof(note->Value));
}

void SsrTraceAfter(const SsrIrpNote& note, NTSTATUS status) {
    if (!note.Active) return;
    if (note.Major == IRP_MJ_CREATE) {
        TraceLoggingWrite(SsrKsProvider, "KsCreate",
            TraceLoggingUInt32(note.Process, "Process"),
            TraceLoggingPointer(note.File, "File"),
            TraceLoggingPointer(note.Related, "Related"),
            TraceLoggingBinary(note.Name, note.NameBytes, "Name"),
            TraceLoggingHexInt32(status, "Status"));
        return;
    }
    TraceLoggingWrite(SsrKsProvider, "KsIoctl",
        TraceLoggingUInt32(note.Process, "Process"),
        TraceLoggingPointer(note.File, "File"),
        TraceLoggingHexUInt32(note.Code, "Code"),
        TraceLoggingUInt32(note.InputLength, "InputLength"),
        TraceLoggingUInt32(note.OutputLength, "OutputLength"),
        TraceLoggingBinary(note.Input, note.InputBytes, "Input"),
        TraceLoggingBinary(note.Value, note.ValueBytes, "Value"),
        TraceLoggingHexInt32(status, "Status"));
}
