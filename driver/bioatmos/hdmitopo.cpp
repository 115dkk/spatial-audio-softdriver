// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/TabletAudioSample/hdmitopo.cpp at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#include "hdmitopo.h"
#include "hdmitoptable.h"
CMiniportTopologyHdmi::CMiniportTopologyHdmi(CAdapterCommon* adapter, USHORT endpoint) : adapter_(adapter), endpoint_(endpoint) {
    adapter_->AddRef();
    ULONG count = 0;
    for (const auto& format : BioFormats) if (format.Bit & adapter_->Settings().FormatMask[endpoint_]) {
        auto& range = ranges_[count];
        range.FormatSize = sizeof(range);
        range.MajorFormat = KSDATAFORMAT_TYPE_AUDIO;
        range.SubFormat = format.Bit == BIO_FORMAT_PCM ? KSDATAFORMAT_SUBTYPE_ANALOG : format.Subtype;
        range.Specifier = KSDATAFORMAT_SPECIFIER_NONE;
        pointers_[count++] = &range;
    }
    for (ULONG i = 0; i < 2; ++i) {
        pins_[i].KsPinDescriptor.DataRangesCount = count;
        pins_[i].KsPinDescriptor.DataRanges = pointers_;
        pins_[i].KsPinDescriptor.DataFlow = i ? KSPIN_DATAFLOW_OUT : KSPIN_DATAFLOW_IN;
        pins_[i].KsPinDescriptor.Communication = KSPIN_COMMUNICATION_NONE;
        pins_[i].KsPinDescriptor.Category = i ? &KSNODETYPE_HDMI_INTERFACE : &KSCATEGORY_AUDIO;
    }
    connection_ = {PCFILTER_NODE, BioTopologyBridgePin, PCFILTER_NODE, BioTopologyJackPin};
    descriptor_.AutomationTable = &HdmiAutomation;
    descriptor_.PinSize = sizeof(PCPIN_DESCRIPTOR); descriptor_.PinCount = 2; descriptor_.Pins = pins_;
    descriptor_.NodeSize = sizeof(PCNODE_DESCRIPTOR);
    descriptor_.ConnectionCount = 1; descriptor_.Connections = &connection_;
}
CMiniportTopologyHdmi::~CMiniportTopologyHdmi() { adapter_->Release(); }
STDMETHODIMP CMiniportTopologyHdmi::QueryInterface(REFIID iid, PVOID* object) {
    if (!object) return STATUS_INVALID_PARAMETER;
    *object = nullptr;
    if (!IsEqualGUIDAligned(iid, IID_IUnknown) && !IsEqualGUIDAligned(iid, IID_IMiniport) && !IsEqualGUIDAligned(iid, IID_IMiniportTopology)) return STATUS_NOINTERFACE;
    *object = static_cast<IMiniportTopology*>(this); AddRef(); return STATUS_SUCCESS;
}
STDMETHODIMP_(NTSTATUS) CMiniportTopologyHdmi::Init(PUNKNOWN, PRESOURCELIST, PPORTTOPOLOGY) { return STATUS_SUCCESS; }
STDMETHODIMP_(NTSTATUS) CMiniportTopologyHdmi::GetDescription(PPCFILTER_DESCRIPTOR* description) { *description = &descriptor_; return STATUS_SUCCESS; }
STDMETHODIMP_(NTSTATUS) CMiniportTopologyHdmi::DataRangeIntersection(ULONG pin, PKSDATARANGE range, PKSDATARANGE matching, ULONG bytes, PVOID output, PULONG required) {
    if (pin > BioTopologyJackPin) { *required = 0; return STATUS_NO_MATCH; }
    return BioBridgeIntersection(range, matching, bytes, output, required);
}
NTSTATUS CMiniportTopologyHdmi::Property(PPCPROPERTY_REQUEST request) {
    // Only the jack pin has a jack. Any other pin answers "not supported"
    // (STATUS_INVALID_DEVICE_REQUEST), as SysVAD's HDMI topology does; its
    // endpoint turns active on the same runner where ours, answering
    // STATUS_INVALID_PARAMETER here, stopped short of the wave filter.
    if (request->InstanceSize < sizeof(ULONG) || *static_cast<PULONG>(request->Instance) != BioTopologyJackPin) return STATUS_INVALID_DEVICE_REQUEST;
    if (request->Verb & KSPROPERTY_TYPE_BASICSUPPORT) return BioBasicSupport(request, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT);
    if (!(request->Verb & KSPROPERTY_TYPE_GET)) return STATUS_INVALID_DEVICE_REQUEST;
    ULONG id = request->PropertyItem->Id;
    if (id == KSPROPERTY_JACK_SINK_INFO) {
        NTSTATUS status = BioPropertySize(request, sizeof(KSJACK_SINK_INFORMATION));
        if (!NT_SUCCESS(status)) return status;
        auto sink = static_cast<PKSJACK_SINK_INFORMATION>(request->Value);
        RtlZeroMemory(sink, sizeof(*sink));
        sink->ConnType = KSJACK_SINK_CONNECTIONTYPE_HDMI;
        sink->ManufacturerId = 0x115d;
        sink->ProductId = 1;
        sink->AudioLatency = 0;
        sink->HDCPCapable = FALSE; sink->AICapable = TRUE;
        const WCHAR name[] = L"BIO HDMI render";
        sink->SinkDescriptionLength = RTL_NUMBER_OF(name) - 1;
        RtlCopyMemory(sink->SinkDescription, name, sizeof(name));
        return STATUS_SUCCESS;
    }
    ULONG itemBytes;
    if (id == KSPROPERTY_JACK_DESCRIPTION) itemBytes = sizeof(KSJACK_DESCRIPTION);
    else if (id == KSPROPERTY_JACK_DESCRIPTION2) itemBytes = sizeof(KSJACK_DESCRIPTION2);
    else return STATUS_INVALID_DEVICE_REQUEST;
    NTSTATUS status = BioPropertySize(request, sizeof(KSMULTIPLE_ITEM) + itemBytes);
    if (!NT_SUCCESS(status)) return status;
    auto list = static_cast<PKSMULTIPLE_ITEM>(request->Value);
    list->Size = sizeof(*list) + itemBytes; list->Count = 1;
    if (id == KSPROPERTY_JACK_DESCRIPTION) RtlCopyMemory(list + 1, &BioHdmiJack, itemBytes);
    else {
        // The virtual jack is always plugged in, and says so: presence detection
        // makes IsConnected = TRUE mean "connected" rather than "unknown". With
        // JackCapabilities 0 the endpoint builder left both endpoints NOTPRESENT
        // although every KS query on the path answered correctly; SysVAD's HDMI
        // jack sets the same bit.
        auto jack = reinterpret_cast<PKSJACK_DESCRIPTION2>(list + 1);
        RtlZeroMemory(jack, itemBytes);
        jack->JackCapabilities = JACKDESC2_PRESENCE_DETECT_CAPABILITY;
    }
    return STATUS_SUCCESS;
}
