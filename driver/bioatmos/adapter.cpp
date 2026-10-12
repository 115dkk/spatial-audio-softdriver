// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/adapter.cpp at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#include <initguid.h>
#include <ntifs.h>
#include "bioatmos.h"
#include "common.h"
#include "minwavert.h"
#include "hdmitopo.h"
#include "minipairs.h"

static BioSettings Settings;
static LONG AdapterPresent;
static PDRIVER_UNLOAD PortUnload;
static PDRIVER_DISPATCH PortDispatch[IRP_MJ_MAXIMUM_FUNCTION + 1];
static HANDLE ServiceKey;
extern "C" DRIVER_INITIALIZE DriverEntry;
static DRIVER_ADD_DEVICE AddDevice;
static DRIVER_DISPATCH BioDispatch;
static DRIVER_DISPATCH PnpHandler;
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
static void LoadSettings() {
    Settings = {2, {BioDefaultFormats, BioDefaultFormats}, FALSE};
    UNICODE_STRING parameters; RtlInitUnicodeString(&parameters, L"Parameters");
    OBJECT_ATTRIBUTES attributes;
    InitializeObjectAttributes(&attributes, &parameters, OBJ_KERNEL_HANDLE | OBJ_CASE_INSENSITIVE, ServiceKey, nullptr);
    HANDLE key;
    if (!NT_SUCCESS(ZwOpenKey(&key, KEY_QUERY_VALUE, &attributes))) return;
    ULONG second = 1, packet = 0;
    RegistryValue(key, L"SecondEndpoint", REG_DWORD, &second, sizeof(second));
    RegistryValue(key, L"WritePacket", REG_DWORD, &packet, sizeof(packet));
    RegistryValue(key, L"FormatMask0", REG_QWORD, &Settings.FormatMask[0], sizeof(ULONGLONG));
    RegistryValue(key, L"FormatMask1", REG_QWORD, &Settings.FormatMask[1], sizeof(ULONGLONG));
    ZwClose(key);
    Settings.EndpointCount = second ? 2 : 1;
    Settings.WritePacket = packet != 0;
    Settings.FormatMask[0] &= BioCompiledFormats;
    Settings.FormatMask[1] = second ? Settings.FormatMask[1] & BioCompiledFormats : 0;
}
extern "C" NTSTATUS DriverEntry(PDRIVER_OBJECT driver, PUNICODE_STRING registryPath) {
    OBJECT_ATTRIBUTES attributes;
    InitializeObjectAttributes(&attributes, registryPath, OBJ_KERNEL_HANDLE | OBJ_CASE_INSENSITIVE, nullptr, nullptr);
    NTSTATUS status = ZwOpenKey(&ServiceKey, KEY_READ, &attributes);
    if (!NT_SUCCESS(status)) return status;
    status = PcInitializeAdapterDriver(driver, registryPath, AddDevice);
    if (!NT_SUCCESS(status)) { ZwClose(ServiceKey); ServiceKey = nullptr; return status; }
    PortUnload = driver->DriverUnload;
    driver->DriverUnload = DriverUnload;
    for (ULONG major = 0; major <= IRP_MJ_MAXIMUM_FUNCTION; ++major) {
        PortDispatch[major] = driver->MajorFunction[major];
        driver->MajorFunction[major] = BioDispatch;
    }
    driver->MajorFunction[IRP_MJ_PNP] = PnpHandler;
    return STATUS_SUCCESS;
}
static NTSTATUS BioDispatch(PDEVICE_OBJECT device, PIRP irp) {
    const UCHAR major = IoGetCurrentIrpStackLocation(irp)->MajorFunction;
    if (device->DeviceType == FILE_DEVICE_UNKNOWN) {
        if (major == IRP_MJ_CREATE || major == IRP_MJ_CLOSE || major == IRP_MJ_CLEANUP ||
            major == IRP_MJ_DEVICE_CONTROL)
            return BioControlDispatch(device, irp);
        return BioComplete(irp, STATUS_INVALID_DEVICE_REQUEST);
    }
    if (major > IRP_MJ_MAXIMUM_FUNCTION || !PortDispatch[major])
        return BioComplete(irp, STATUS_INVALID_DEVICE_REQUEST);
    return PortDispatch[major](device, irp);
}
static VOID DriverUnload(PDRIVER_OBJECT driver) {
    PAGED_CODE();
    if (ServiceKey) { ZwClose(ServiceKey); ServiceKey = nullptr; }
    if (PortUnload) PortUnload(driver);
}
static NTSTATUS AddDevice(PDRIVER_OBJECT driver, PDEVICE_OBJECT pdo) {
    PAGED_CODE();
    if (InterlockedCompareExchange(&AdapterPresent, 1, 0)) return STATUS_DEVICE_BUSY;
    LoadSettings();
    NTSTATUS status = PcAddAdapterDevice(driver, pdo, StartDevice, 4, 0);
    if (!NT_SUCCESS(status)) { InterlockedExchange(&AdapterPresent, 0); return status; }
    PDEVICE_OBJECT fdo = IoGetAttachedDeviceReference(pdo);
    while (fdo && fdo->DriverObject != driver) {
        PDEVICE_OBJECT lower = IoGetLowerDeviceObject(fdo);
        ObDereferenceObject(fdo); fdo = lower;
    }
    if (!fdo) return STATUS_SUCCESS; // START detects absent control state.
    auto extension = BioExtension(fdo);
    extension->Common = nullptr;
    extension->ControlDevice = nullptr;
    extension->ControlStatus = BioCreateControl(driver, Settings, &extension->ControlDevice);
    ObDereferenceObject(fdo);
    // PortCls has already attached its FDO. A failed control-device creation
    // fails START, not ADD, so PnP/PortCls retain responsibility for teardown.
    return STATUS_SUCCESS;
}
static NTSTATUS Install(PDEVICE_OBJECT device, PIRP irp, PRESOURCELIST resources,
                        CAdapterCommon* adapter, const BioMiniPair& pair) {
    PPORT topology = nullptr, wave = nullptr;
    auto miniTopology = new (BioNonPaged{}) CMiniportTopologyHdmi(adapter, pair.Endpoint);
    auto miniWave = new (BioNonPaged{}) CMiniportWaveRT(adapter, pair.Endpoint);
    NTSTATUS status = miniTopology && miniWave ? STATUS_SUCCESS : STATUS_INSUFFICIENT_RESOURCES;
    if (NT_SUCCESS(status)) status = PcNewPort(&topology, CLSID_PortTopology);
    if (NT_SUCCESS(status)) status = topology->Init(device, irp, miniTopology, adapter, resources);
    if (NT_SUCCESS(status)) status = PcRegisterSubdevice(device, const_cast<PWSTR>(pair.Topology), topology);
    if (NT_SUCCESS(status)) status = PcNewPort(&wave, CLSID_PortWaveRT);
    if (NT_SUCCESS(status)) status = wave->Init(device, irp, static_cast<IMiniportWaveRT*>(miniWave), adapter, resources);
    if (NT_SUCCESS(status)) status = PcRegisterSubdevice(device, const_cast<PWSTR>(pair.Wave), wave);
    if (NT_SUCCESS(status)) status = PcRegisterPhysicalConnection(device, wave, BioWaveBridgePin, topology, BioTopologyBridgePin);
    if (wave) wave->Release();
    if (topology) topology->Release();
    if (miniWave) miniWave->Release();
    if (miniTopology) miniTopology->Release();
    return status;
}
static NTSTATUS StartDevice(PDEVICE_OBJECT device, PIRP irp, PRESOURCELIST resources) {
    PAGED_CODE();
    auto extension = BioExtension(device);
    if (!NT_SUCCESS(extension->ControlStatus)) return extension->ControlStatus;
    if (!extension->ControlDevice) return STATUS_DEVICE_NOT_READY;
    LoadSettings();
    auto control = BioControl(extension->ControlDevice);
    KIRQL settingsIrql; KeAcquireSpinLock(&control->Lock, &settingsIrql);
    control->Settings = Settings;
    KeReleaseSpinLock(&control->Lock, settingsIrql);
    if (extension->Common) return STATUS_INVALID_DEVICE_STATE;
    auto adapter = new (BioNonPaged{}) CAdapterCommon(extension->ControlDevice);
    if (!adapter) return STATUS_INSUFFICIENT_RESOURCES;
    NTSTATUS status = adapter->Start();
    if (NT_SUCCESS(status)) status = PcRegisterAdapterPowerManagement(adapter, device);
    for (ULONG i = 0; NT_SUCCESS(status) && i < adapter->Settings().EndpointCount; ++i)
        status = Install(device, irp, resources, adapter, BioMiniPairs[i]);
    if (NT_SUCCESS(status)) extension->Common = adapter;
    else { adapter->Stop(); adapter->Release(); }
    return status;
}
static NTSTATUS PnpHandler(PDEVICE_OBJECT device, PIRP irp) {
    PAGED_CODE();
    if (device->DeviceType == FILE_DEVICE_UNKNOWN) return BioComplete(irp, STATUS_INVALID_DEVICE_REQUEST);
    auto extension = BioExtension(device);
    UCHAR minor = IoGetCurrentIrpStackLocation(irp)->MinorFunction;
    if (minor == IRP_MN_STOP_DEVICE || minor == IRP_MN_SURPRISE_REMOVAL || minor == IRP_MN_REMOVE_DEVICE) {
        if (extension->Common) {
            extension->Common->Stop(); extension->Common->Release(); extension->Common = nullptr;
        }
        if (minor == IRP_MN_REMOVE_DEVICE) {
            PDEVICE_OBJECT control = extension->ControlDevice; extension->ControlDevice = nullptr;
            if (control) BioDeleteControl(control);
            InterlockedExchange(&AdapterPresent, 0);
        }
    }
    return PortDispatch[IRP_MJ_PNP](device, irp);
}
