// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/sysvad.h at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#pragma once
#include <ntddk.h>
#ifndef _NEW_DELETE_OPERATORS_
#define _NEW_DELETE_OPERATORS_
#endif
#include "NewDelete.h"
#include <portcls.h>
#include <ksdebug.h>
#include <ksmedia.h>
#include <ntstrsafe.h>
#include "bio_core.h"
#include "NewDelete.h"

constexpr ULONG BioPoolTag = 'oiBA';
constexpr ULONG BioRingBytes = 8 * 1024 * 1024;
constexpr ULONGLONG BioCompiledFormats = 0x1ff;
constexpr ULONGLONG BioDefaultFormats = 0x6f;
constexpr ULONG BioWaveHostPin = 0;
constexpr ULONG BioWaveBridgePin = 1;
constexpr ULONG BioTopologyBridgePin = 0;
constexpr ULONG BioTopologyJackPin = 1;

class CAdapterCommon;
struct BioControlState;
struct BioSettings {
    ULONG EndpointCount;
    ULONGLONG FormatMask[BIO_MAX_ENDPOINTS];
    BOOLEAN WritePacket;
};
// PortCls documents slots 4..7 as adapter-owned (PcAddAdapterDevice).
struct BioAdapterExtension {
    ULONG_PTR Reserved[4];
    CAdapterCommon* Common;
    PDEVICE_OBJECT ControlDevice;
    NTSTATUS ControlStatus;
};
inline BioAdapterExtension* BioExtension(PDEVICE_OBJECT device) {
    return static_cast<BioAdapterExtension*>(device->DeviceExtension);
}
inline ULONGLONG BioQpc() {
    return static_cast<ULONGLONG>(KeQueryPerformanceCounter(nullptr).QuadPart);
}
#define BIO_UNKNOWN_METHODS \
    STDMETHODIMP QueryInterface(REFIID iid, PVOID* object) override; \
    STDMETHODIMP_(ULONG) AddRef() override { return static_cast<ULONG>(InterlockedIncrement(&references_)); } \
    STDMETHODIMP_(ULONG) Release() override { LONG n = InterlockedDecrement(&references_); if (!n) delete this; return static_cast<ULONG>(n); }

NTSTATUS BioComplete(PIRP irp, NTSTATUS status, ULONG_PTR information = 0);
extern const GUID BioAdapterInterface;
