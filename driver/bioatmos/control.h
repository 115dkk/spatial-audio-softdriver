// SPDX-License-Identifier: MS-PL
// Copyright (c) 2026 115dkk
#pragma once
#include "bioatmos.h"

struct BioFileContext { BOOLEAN Cleaned; };
struct BioReadContext {
    PUCHAR Output;
    ULONG Capacity;  // the buffer clamped to BIO_READ_MAX_BYTES
    ULONG Minimum;
    ULONGLONG Deadline;
    BioFileContext* File;
    BOOLEAN Detached;
    ULONGLONG Generation;
};
struct BioControlState {
    KSPIN_LOCK Lock;
    IO_REMOVE_LOCK RemoveLock;
    IO_CSQ Csq;
    PIRP Pending;
    ULONG DetachedReads;
    ULONGLONG Generation;
    PEX_TIMER DeadlineTimer;
    BOOLEAN Removed;
    BOOLEAN Started;
    ULONG OpenCount;
    LONG Trim;
    ULONG MaxLatenessUs;
    ULONGLONG Frequency;
    BioSettings Settings;
    bio::RecordRing Ring;
    PUCHAR Storage;
};
NTSTATUS BioCreateControl(PDRIVER_OBJECT driver, const BioSettings& settings, PDEVICE_OBJECT* device);
NTSTATUS BioStartControl(BioControlState* state);
void BioStopControl(BioControlState* state);
void BioDeleteControl(PDEVICE_OBJECT device);
void BioPumpRead(BioControlState* state);
DRIVER_DISPATCH BioControlDispatch;
inline BioControlState* BioControl(PDEVICE_OBJECT device) {
    return static_cast<BioControlState*>(device->DeviceExtension);
}
