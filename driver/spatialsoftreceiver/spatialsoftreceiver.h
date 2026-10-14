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
#include "ssr_core.h"
#include "ssr_names.h"
#include "NewDelete.h"

constexpr ULONG SsrPoolTag = 'DrsS';
constexpr ULONG SsrRingBytes = 8 * 1024 * 1024;
// constexpr from the header's initializer lists: copying SSR_COMPILED_PCM
// here would need a dynamic initializer, and a kernel image has no CRT to
// run one (LNK4210 in Debug builds).
constexpr SSR_ENDPOINT_FORMATS SsrCompiledFormats = {SSR_COMPILED_CODECS, SSR_COMPILED_PCM_INIT};
constexpr SSR_ENDPOINT_FORMATS SsrDefaultFormats = {SSR_DEFAULT_ENABLED_CODECS, SSR_DEFAULT_PCM_INIT};
constexpr ULONG SsrWaveHostPin = 0;
constexpr ULONG SsrWaveBridgePin = 1;
constexpr ULONG SsrTopologyBridgePin = 0;
constexpr ULONG SsrTopologyJackPin = 1;

class CAdapterCommon;
struct SsrControlState;
struct SsrSettings {
    ULONG EndpointMask;
    SSR_ENDPOINT_FORMATS Formats[SSR_MAX_ENDPOINTS];
    BOOLEAN WritePacket;
    WCHAR EndpointName[SSR_MAX_ENDPOINTS][ssr::kEndpointNameCapacity];
};
// PortCls documents slots 4..7 as adapter-owned (PcAddAdapterDevice).
struct SsrAdapterExtension {
    ULONG_PTR Reserved[4];
    CAdapterCommon* Common;
    PDEVICE_OBJECT ControlDevice;
    NTSTATUS ControlStatus;
};
inline SsrAdapterExtension* SsrExtension(PDEVICE_OBJECT device) {
    return static_cast<SsrAdapterExtension*>(device->DeviceExtension);
}
inline ULONGLONG SsrQpc() {
    return static_cast<ULONGLONG>(KeQueryPerformanceCounter(nullptr).QuadPart);
}
#define SSR_UNKNOWN_METHODS \
    STDMETHODIMP QueryInterface(REFIID iid, PVOID* object) override; \
    STDMETHODIMP_(ULONG) AddRef() override { return static_cast<ULONG>(InterlockedIncrement(&references_)); } \
    STDMETHODIMP_(ULONG) Release() override { LONG n = InterlockedDecrement(&references_); if (!n) delete this; return static_cast<ULONG>(n); }

NTSTATUS SsrComplete(PIRP irp, NTSTATUS status, ULONG_PTR information = 0);
// Completes a request this driver does not handle and that has no lower
// driver, leaving the status its sender set (PnP, power, WMI).
NTSTATUS SsrCompleteUnhandled(PIRP irp);
extern const GUID SsrAdapterInterface;
