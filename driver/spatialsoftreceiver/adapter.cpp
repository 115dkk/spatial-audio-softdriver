// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/adapter.cpp at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#include <initguid.h>
#include <ntifs.h>
#include "spatialsoftreceiver.h"
#include "common.h"
#include "minwavert.h"
#include "hdmitopo.h"
#include "minipairs.h"
#include "kstrace.h"

static SsrSettings Settings;
static LONG AdapterPresent;
static PDRIVER_UNLOAD PortUnload;
static PDRIVER_DISPATCH PortDispatch[IRP_MJ_MAXIMUM_FUNCTION + 1];
static HANDLE ServiceKey;
extern "C" DRIVER_INITIALIZE DriverEntry;
static DRIVER_ADD_DEVICE AddDevice;
// SsrDispatch takes the control device's major functions and passes PortCls's
// on (PcInitializeAdapterDriver and KS set these); PnpHandler takes PnP. Each
// major is assigned by name so the dispatch annotations cover every slot.
static _Dispatch_type_(IRP_MJ_CREATE) _Dispatch_type_(IRP_MJ_CLOSE) _Dispatch_type_(IRP_MJ_CLEANUP)
    _Dispatch_type_(IRP_MJ_DEVICE_CONTROL) _Dispatch_type_(IRP_MJ_READ) _Dispatch_type_(IRP_MJ_WRITE)
    _Dispatch_type_(IRP_MJ_FLUSH_BUFFERS) _Dispatch_type_(IRP_MJ_QUERY_SECURITY) _Dispatch_type_(IRP_MJ_SET_SECURITY)
    _Dispatch_type_(IRP_MJ_POWER) _Dispatch_type_(IRP_MJ_SYSTEM_CONTROL)
    DRIVER_DISPATCH SsrDispatch;
static _Dispatch_type_(IRP_MJ_PNP) DRIVER_DISPATCH PnpHandler;
static DRIVER_UNLOAD DriverUnload;
static NTSTATUS StartDevice(PDEVICE_OBJECT device, PIRP irp, PRESOURCELIST resources);

static void RegistryValue(HANDLE key, PCWSTR name, ULONG type, PVOID output, ULONG bytes) {
    UNICODE_STRING value; RtlInitUnicodeString(&value, name);
    alignas(8) BYTE storage[sizeof(KEY_VALUE_PARTIAL_INFORMATION) + sizeof(ULONGLONG)]{};
    ULONG needed = 0;
    NTSTATUS status = ZwQueryValueKey(key, &value, KeyValuePartialInformation, storage, sizeof(storage), &needed);
    if (NT_SUCCESS(status)) {
        auto information = reinterpret_cast<PKEY_VALUE_PARTIAL_INFORMATION>(storage);
        if (information->Type == type && information->DataLength == bytes) RtlCopyMemory(output, information->Data, bytes);
    }
}
static void RegistryName(HANDLE key, ULONG endpoint) {
    WCHAR name[] = L"EndpointName0";
    name[ARRAYSIZE(name) - 2] = static_cast<WCHAR>(L'0' + endpoint);
    UNICODE_STRING value; RtlInitUnicodeString(&value, name);
    alignas(8) BYTE storage[FIELD_OFFSET(KEY_VALUE_PARTIAL_INFORMATION, Data) +
        ssr::kEndpointNameCapacity * sizeof(WCHAR)]{};
    ULONG needed = 0;
    NTSTATUS status = ZwQueryValueKey(key, &value, KeyValuePartialInformation, storage, sizeof(storage), &needed);
    if (!NT_SUCCESS(status)) return; // Missing/oversized values keep the default.
    auto information = reinterpret_cast<PKEY_VALUE_PARTIAL_INFORMATION>(storage);
    if (information->DataLength > sizeof(storage) - FIELD_OFFSET(KEY_VALUE_PARTIAL_INFORMATION, Data)) return;
    ssr::EndpointName(endpoint, information->Type == REG_SZ,
        reinterpret_cast<const WCHAR*>(information->Data), information->DataLength, Settings.EndpointName[endpoint]);
}
static void LoadSettings() {
    PAGED_CODE();
    Settings = {0x7, {SsrDefaultFormats, SsrDefaultFormats, SsrDefaultFormats, SsrDefaultFormats}, FALSE, {}};
    for (ULONG i = 0; i < SSR_MAX_ENDPOINTS; ++i)
        ssr::EndpointName<WCHAR>(i, false, nullptr, 0, Settings.EndpointName[i]);
    UNICODE_STRING parameters; RtlInitUnicodeString(&parameters, L"Parameters");
    OBJECT_ATTRIBUTES attributes;
    InitializeObjectAttributes(&attributes, &parameters, OBJ_KERNEL_HANDLE | OBJ_CASE_INSENSITIVE, ServiceKey, nullptr);
    HANDLE key;
    ULONG packet = 0;
    if (NT_SUCCESS(ZwOpenKey(&key, KEY_QUERY_VALUE, &attributes))) {
        for (ULONG i = 0; i < SSR_MAX_ENDPOINTS; ++i) RegistryName(key, i);
        RegistryValue(key, L"EndpointMask", REG_DWORD, &Settings.EndpointMask, sizeof(Settings.EndpointMask));
        RegistryValue(key, L"WritePacket", REG_DWORD, &packet, sizeof(packet));
        for (ULONG i = 0; i < SSR_MAX_ENDPOINTS; ++i) {
            WCHAR codec[] = L"CodecMask0", rates[] = L"PcmRateMask0", shapes[] = L"PcmShapeMask0";
            WCHAR masked[] = L"PcmMaxMaskedChannels0", direct[] = L"PcmDirectChannels0";
            codec[ARRAYSIZE(codec) - 2] += static_cast<WCHAR>(i);
            rates[ARRAYSIZE(rates) - 2] += static_cast<WCHAR>(i);
            shapes[ARRAYSIZE(shapes) - 2] += static_cast<WCHAR>(i);
            masked[ARRAYSIZE(masked) - 2] += static_cast<WCHAR>(i);
            direct[ARRAYSIZE(direct) - 2] += static_cast<WCHAR>(i);
            auto& f = Settings.Formats[i];
            RegistryValue(key, codec, REG_QWORD, &f.codecs, sizeof(f.codecs));
            RegistryValue(key, rates, REG_DWORD, &f.pcm.rates, sizeof(f.pcm.rates));
            RegistryValue(key, shapes, REG_DWORD, &f.pcm.shapes, sizeof(f.pcm.shapes));
            RegistryValue(key, masked, REG_DWORD, &f.pcm.max_masked_channels, sizeof(f.pcm.max_masked_channels));
            RegistryValue(key, direct, REG_DWORD, &f.pcm.direct_channels, sizeof(f.pcm.direct_channels));
        }
        ZwClose(key);
    }
    // Normalize defaults too: the absent reserved endpoint must report mask 0
    // even when the Parameters key could not be opened.
    Settings.WritePacket = packet != 0;
    constexpr ULONG available = (1UL << ARRAYSIZE(SsrMiniPairs)) - 1UL;
    Settings.EndpointMask &= available;
    if (!Settings.EndpointMask) Settings.EndpointMask = 0x7 & available;
    for (ULONG i = 0; i < SSR_MAX_ENDPOINTS; ++i) {
        auto& f = Settings.Formats[i];
        if (!(Settings.EndpointMask & (1UL << i))) { f = {}; continue; }
        f.codecs &= SSR_COMPILED_CODECS;
        f.pcm.rates &= SSR_COMPILED_PCM.rates;
        f.pcm.shapes &= SSR_COMPILED_PCM.shapes;
        // This field is a ceiling, not a bit mask: intersect by minimum.
        if (f.pcm.max_masked_channels > SSR_COMPILED_PCM.max_masked_channels)
            f.pcm.max_masked_channels = SSR_COMPILED_PCM.max_masked_channels;
        f.pcm.direct_channels &= SSR_COMPILED_PCM.direct_channels;
    }
}
extern "C" NTSTATUS DriverEntry(PDRIVER_OBJECT driver, PUNICODE_STRING registryPath) {
    OBJECT_ATTRIBUTES attributes;
    InitializeObjectAttributes(&attributes, registryPath, OBJ_KERNEL_HANDLE | OBJ_CASE_INSENSITIVE, nullptr, nullptr);
    NTSTATUS status = ZwOpenKey(&ServiceKey, KEY_READ, &attributes);
    if (!NT_SUCCESS(status)) return status;
    SsrTraceRegister();
    status = PcInitializeAdapterDriver(driver, registryPath, AddDevice);
    if (!NT_SUCCESS(status)) { SsrTraceUnregister(); ZwClose(ServiceKey); ServiceKey = nullptr; return status; }
    PortUnload = driver->DriverUnload;
    driver->DriverUnload = DriverUnload;
    for (ULONG major = 0; major <= IRP_MJ_MAXIMUM_FUNCTION; ++major) PortDispatch[major] = driver->MajorFunction[major];
    driver->MajorFunction[IRP_MJ_CREATE] = SsrDispatch;
    driver->MajorFunction[IRP_MJ_CLOSE] = SsrDispatch;
    driver->MajorFunction[IRP_MJ_CLEANUP] = SsrDispatch;
    driver->MajorFunction[IRP_MJ_DEVICE_CONTROL] = SsrDispatch;
    driver->MajorFunction[IRP_MJ_READ] = SsrDispatch;
    driver->MajorFunction[IRP_MJ_WRITE] = SsrDispatch;
    driver->MajorFunction[IRP_MJ_FLUSH_BUFFERS] = SsrDispatch;
    driver->MajorFunction[IRP_MJ_QUERY_SECURITY] = SsrDispatch;
    driver->MajorFunction[IRP_MJ_SET_SECURITY] = SsrDispatch;
    driver->MajorFunction[IRP_MJ_POWER] = SsrDispatch;
    driver->MajorFunction[IRP_MJ_SYSTEM_CONTROL] = SsrDispatch;
    driver->MajorFunction[IRP_MJ_PNP] = PnpHandler;
    return STATUS_SUCCESS;
}
static NTSTATUS SsrDispatch(PDEVICE_OBJECT device, PIRP irp) {
    const UCHAR major = IoGetCurrentIrpStackLocation(irp)->MajorFunction;
    if (device->DeviceType == FILE_DEVICE_UNKNOWN) {
        if (major == IRP_MJ_CREATE || major == IRP_MJ_CLOSE || major == IRP_MJ_CLEANUP ||
            major == IRP_MJ_DEVICE_CONTROL)
            return SsrControlDispatch(device, irp);
        return SsrComplete(irp, STATUS_INVALID_DEVICE_REQUEST);
    }
    if (major > IRP_MJ_MAXIMUM_FUNCTION || !PortDispatch[major])
        return SsrComplete(irp, STATUS_INVALID_DEVICE_REQUEST);
    SsrIrpNote note;
    SsrTraceBefore(irp, &note);
    NTSTATUS status = PortDispatch[major](device, irp);
    SsrTraceAfter(note, status);
    return status;
}
static VOID DriverUnload(PDRIVER_OBJECT driver) {
    PAGED_CODE();
    if (ServiceKey) { ZwClose(ServiceKey); ServiceKey = nullptr; }
    if (PortUnload) PortUnload(driver);
    SsrTraceUnregister();
}
static NTSTATUS AddDevice(PDRIVER_OBJECT driver, PDEVICE_OBJECT pdo) {
    PAGED_CODE();
    if (InterlockedCompareExchange(&AdapterPresent, 1, 0)) return STATUS_DEVICE_BUSY;
    LoadSettings();
    NTSTATUS status = PcAddAdapterDevice(driver, pdo, StartDevice, 2 * ARRAYSIZE(SsrMiniPairs), 0);
    if (!NT_SUCCESS(status)) { InterlockedExchange(&AdapterPresent, 0); return status; }
    PDEVICE_OBJECT fdo = IoGetAttachedDeviceReference(pdo);
    while (fdo && fdo->DriverObject != driver) {
        PDEVICE_OBJECT lower = IoGetLowerDeviceObject(fdo);
        ObDereferenceObject(fdo); fdo = lower;
    }
    if (!fdo) return STATUS_SUCCESS; // START detects absent control state.
    // PcAddAdapterDevice already cleared it; stated here for the driver rules.
    fdo->Flags &= ~DO_DEVICE_INITIALIZING;
    auto extension = SsrExtension(fdo);
    extension->Common = nullptr;
    extension->ControlDevice = nullptr;
    extension->ControlStatus = SsrCreateControl(driver, Settings, &extension->ControlDevice);
    ObDereferenceObject(fdo);
    // PortCls has already attached its FDO. A failed control-device creation
    // fails START, not ADD, so PnP/PortCls retain responsibility for teardown.
    return STATUS_SUCCESS;
}
static NTSTATUS Install(PDEVICE_OBJECT device, PIRP irp, PRESOURCELIST resources,
                        CAdapterCommon* adapter, const SsrMiniPair& pair) {
    PPORT topology = nullptr, wave = nullptr;
    auto miniTopology = new (SsrNonPaged{}) CMiniportTopologyHdmi(adapter, pair.Endpoint);
    auto miniWave = new (SsrNonPaged{}) CMiniportWaveRT(adapter, pair.Endpoint);
    NTSTATUS status = miniTopology && miniWave ? STATUS_SUCCESS : STATUS_INSUFFICIENT_RESOURCES;
    if (NT_SUCCESS(status)) status = PcNewPort(&topology, CLSID_PortTopology);
    if (NT_SUCCESS(status)) status = topology->Init(device, irp, miniTopology, adapter, resources);
    if (NT_SUCCESS(status)) status = PcRegisterSubdevice(device, const_cast<PWSTR>(pair.Topology), topology);
    if (NT_SUCCESS(status)) status = PcNewPort(&wave, CLSID_PortWaveRT);
    if (NT_SUCCESS(status)) status = wave->Init(device, irp, static_cast<IMiniportWaveRT*>(miniWave), adapter, resources);
    if (NT_SUCCESS(status)) status = PcRegisterSubdevice(device, const_cast<PWSTR>(pair.Wave), wave);
    if (NT_SUCCESS(status)) status = PcRegisterPhysicalConnection(device, wave, SsrWaveBridgePin, topology, SsrTopologyBridgePin);
    if (wave) wave->Release();
    if (topology) topology->Release();
    if (miniWave) miniWave->Release();
    if (miniTopology) miniTopology->Release();
    return status;
}
static NTSTATUS StartDevice(PDEVICE_OBJECT device, PIRP irp, PRESOURCELIST resources) {
    PAGED_CODE();
    auto extension = SsrExtension(device);
    if (!NT_SUCCESS(extension->ControlStatus)) return extension->ControlStatus;
    if (!extension->ControlDevice) return STATUS_DEVICE_NOT_READY;
    LoadSettings();
    auto control = SsrControl(extension->ControlDevice);
    KIRQL settingsIrql; KeAcquireSpinLock(&control->Lock, &settingsIrql);
    control->Settings = Settings;
    KeReleaseSpinLock(&control->Lock, settingsIrql);
    if (extension->Common) return STATUS_INVALID_DEVICE_STATE;
    auto adapter = new (SsrNonPaged{}) CAdapterCommon(extension->ControlDevice);
    if (!adapter) return STATUS_INSUFFICIENT_RESOURCES;
    NTSTATUS status = adapter->Start();
    if (NT_SUCCESS(status)) status = PcRegisterAdapterPowerManagement(adapter, device);
    for (ULONG i = 0; NT_SUCCESS(status) && i < SSR_MAX_ENDPOINTS; ++i) {
        if ((adapter->Settings().EndpointMask & (1UL << i)) && i < ARRAYSIZE(SsrMiniPairs))
            status = Install(device, irp, resources, adapter, SsrMiniPairs[i]);
    }
    if (NT_SUCCESS(status)) extension->Common = adapter;
    else { adapter->Stop(); adapter->Release(); }
    return status;
}
static NTSTATUS PnpHandler(PDEVICE_OBJECT device, PIRP irp) {
    PAGED_CODE();
    if (device->DeviceType == FILE_DEVICE_UNKNOWN) return SsrComplete(irp, STATUS_INVALID_DEVICE_REQUEST);
    auto extension = SsrExtension(device);
    UCHAR minor = IoGetCurrentIrpStackLocation(irp)->MinorFunction;
    if (minor == IRP_MN_STOP_DEVICE || minor == IRP_MN_SURPRISE_REMOVAL || minor == IRP_MN_REMOVE_DEVICE) {
        if (extension->Common) {
            extension->Common->Stop(); extension->Common->Release(); extension->Common = nullptr;
        }
        if (minor == IRP_MN_REMOVE_DEVICE) {
            PDEVICE_OBJECT control = extension->ControlDevice; extension->ControlDevice = nullptr;
            if (control) SsrDeleteControl(control);
            InterlockedExchange(&AdapterPresent, 0);
        }
    }
    return PortDispatch[IRP_MJ_PNP](device, irp);
}
