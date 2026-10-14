// SPDX-License-Identifier: MS-PL
// Copyright (c) 2026 115dkk
#pragma once
#include "spatialsoftreceiver.h"
#include "ssr_kernel.h"

struct SsrFileContext { BOOLEAN Cleaned; };
struct SsrReadContext {
    PUCHAR Output;
    ULONG Capacity;  // the buffer clamped to SSR_READ_MAX_BYTES
    ULONG Minimum;
    ULONGLONG Deadline;
    SsrFileContext* File;
    BOOLEAN Detached;
    ULONGLONG Generation;
};
struct SsrControlState {
    KSPIN_LOCK Lock;
    IO_REMOVE_LOCK RemoveLock;
    IO_CSQ Csq;
    PIRP Pending;
    ULONG DetachedReads;
    SsrReadContext ReadSlot;
    BOOLEAN ReadSlotBusy;
    ULONGLONG Generation;
    ssr::km::ExTimer DeadlineTimer;
    BOOLEAN Removed;
    BOOLEAN Started;
    ULONG OpenCount;
    LONG Trim;
    ULONG MaxLatenessUs;
    ULONGLONG Frequency;
    SsrSettings Settings;
    ssr::RecordRing Ring;
    ssr::km::PoolPtr<UCHAR> Storage;
};
NTSTATUS SsrCreateControl(PDRIVER_OBJECT driver, const SsrSettings& settings, PDEVICE_OBJECT* device);
NTSTATUS SsrStartControl(SsrControlState* state);
void SsrStopControl(SsrControlState* state);
void SsrDeleteControl(PDEVICE_OBJECT device);
void SsrPumpRead(SsrControlState* state);
DRIVER_DISPATCH SsrControlDispatch;
inline SsrControlState* SsrControl(PDEVICE_OBJECT device) {
    return static_cast<SsrControlState*>(device->DeviceExtension);
}
