// SPDX-License-Identifier: MS-PL
// Copyright (c) 2026 115dkk
// Diagnostics: the KS requests that reach PortCls through SsrDispatch, as
// TraceLogging events (provider "SpatialSoftReceiver.Driver.Ks"). Nothing is copied
// unless an ETW session has the provider enabled. CI reads them to see what
// the endpoint builder asked of our filters (tools/ci/ks-trace.ps1).
#pragma once
#include "spatialsoftreceiver.h"

struct SsrIrpNote {
    bool Active;
    UCHAR Major;
    ULONG Code;
    ULONG InputLength;
    ULONG OutputLength;
    PFILE_OBJECT File;
    PFILE_OBJECT Related;
    ULONG Process;
    USHORT InputBytes;
    USHORT ValueBytes;
    USHORT NameBytes;
    BYTE Input[160];
    BYTE Value[128];
    BYTE Name[256];
};

void SsrTraceRegister();
void SsrTraceUnregister();
// Before PortCls sees the IRP: afterwards it may already be completed and freed.
void SsrTraceBefore(PIRP irp, SsrIrpNote* note);
void SsrTraceAfter(const SsrIrpNote& note, NTSTATUS status);
