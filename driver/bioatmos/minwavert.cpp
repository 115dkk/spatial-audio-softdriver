// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/EndpointsCommon/minwavert.cpp at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#include "minwavert.h"
#include "minwavertstream.h"
#include "basetopo.h"
static PCPROPERTY_ITEM WaveProperties[] = {
    {&KSPROPSETID_Pin, KSPROPERTY_PIN_PROPOSEDATAFORMAT, KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT, CMiniportWaveRT::Property},
    {&KSPROPSETID_General, KSPROPERTY_GENERAL_COMPONENTID, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, CMiniportWaveRT::Property},
};
DEFINE_PCAUTOMATION_TABLE_PROP(WaveAutomation, WaveProperties);
CMiniportWaveRT::CMiniportWaveRT(CAdapterCommon* adapter, USHORT endpoint) : adapter_(adapter), endpoint_(endpoint) {
    adapter_->AddRef();
    ULONG count = 0;
    for (const auto& format : BioFormats) if (format.Bit & Mask()) {
        auto& range = ranges_[count];
        range.DataRange.FormatSize = sizeof(range);
        range.DataRange.MajorFormat = KSDATAFORMAT_TYPE_AUDIO;
        range.DataRange.SubFormat = format.Subtype;
        range.DataRange.Specifier = KSDATAFORMAT_SPECIFIER_WAVEFORMATEX;
        range.MaximumChannels = format.Channels;
        range.MinimumBitsPerSample = range.MaximumBitsPerSample = 16;
        range.MinimumSampleFrequency = range.MaximumSampleFrequency = format.Rate;
        rangePointers_[count] = &range.DataRange;
        auto& bridge = bridges_[count];
        bridge.FormatSize = sizeof(bridge);
        bridge.MajorFormat = KSDATAFORMAT_TYPE_AUDIO;
        bridge.SubFormat = format.Bit == BIO_FORMAT_PCM ? KSDATAFORMAT_SUBTYPE_ANALOG : format.Subtype;
        bridge.Specifier = KSDATAFORMAT_SPECIFIER_NONE;
        bridgePointers_[count++] = &bridge;
    }
    pins_[0].MaxGlobalInstanceCount = 1;
    pins_[0].MaxFilterInstanceCount = 1;
    pins_[0].KsPinDescriptor.DataRangesCount = count;
    pins_[0].KsPinDescriptor.DataRanges = rangePointers_;
    pins_[0].KsPinDescriptor.DataFlow = KSPIN_DATAFLOW_IN;
    pins_[0].KsPinDescriptor.Communication = KSPIN_COMMUNICATION_SINK;
    pins_[0].KsPinDescriptor.Category = &KSCATEGORY_AUDIO;
    pins_[1].KsPinDescriptor.DataRangesCount = count;
    pins_[1].KsPinDescriptor.DataRanges = bridgePointers_;
    pins_[1].KsPinDescriptor.DataFlow = KSPIN_DATAFLOW_OUT;
    pins_[1].KsPinDescriptor.Communication = KSPIN_COMMUNICATION_NONE;
    pins_[1].KsPinDescriptor.Category = &KSCATEGORY_AUDIO;
    // Host pin -> SUM node -> bridge pin, as SysVAD's HDMI wave filter starts
    // its chain: with the host pin wired straight to the bridge pin the
    // endpoint builder found no path to it and left the endpoints NOTPRESENT.
    node_.Type = &KSNODETYPE_SUM;
    connections_[0] = {PCFILTER_NODE, BioWaveHostPin, 0, KSNODEPIN_STANDARD_IN};
    connections_[1] = {0, KSNODEPIN_STANDARD_OUT, PCFILTER_NODE, BioWaveBridgePin};
    descriptor_.AutomationTable = &WaveAutomation;
    descriptor_.PinSize = sizeof(PCPIN_DESCRIPTOR);
    descriptor_.PinCount = RTL_NUMBER_OF(pins_);
    descriptor_.Pins = pins_;
    descriptor_.NodeSize = sizeof(PCNODE_DESCRIPTOR);
    descriptor_.NodeCount = 1;
    descriptor_.Nodes = &node_;
    descriptor_.ConnectionCount = RTL_NUMBER_OF(connections_);
    descriptor_.Connections = connections_;
}
CMiniportWaveRT::~CMiniportWaveRT() { adapter_->Release(); }
STDMETHODIMP CMiniportWaveRT::QueryInterface(REFIID iid, PVOID* object) {
    if (!object) return STATUS_INVALID_PARAMETER;
    *object = nullptr;
    if (IsEqualGUIDAligned(iid, IID_IUnknown) || IsEqualGUIDAligned(iid, IID_IMiniport) || IsEqualGUIDAligned(iid, IID_IMiniportWaveRT))
        *object = static_cast<IMiniportWaveRT*>(this);
    else return STATUS_NOINTERFACE;
    AddRef(); return STATUS_SUCCESS;
}
STDMETHODIMP_(NTSTATUS) CMiniportWaveRT::Init(PUNKNOWN, PRESOURCELIST, PPORTWAVERT) { return STATUS_SUCCESS; }
STDMETHODIMP_(NTSTATUS) CMiniportWaveRT::GetDescription(PPCFILTER_DESCRIPTOR* description) {
    *description = &descriptor_; return STATUS_SUCCESS;
}
STDMETHODIMP_(NTSTATUS) CMiniportWaveRT::GetDeviceDescription(PDEVICE_DESCRIPTION description) {
    RtlZeroMemory(description, sizeof(*description));
    description->Version = DEVICE_DESCRIPTION_VERSION;
    description->Master = TRUE;
    description->ScatterGather = TRUE;
    description->Dma32BitAddresses = TRUE;
    description->InterfaceType = PCIBus;
    description->MaximumLength = MAXULONG;
    return STATUS_SUCCESS;
}
STDMETHODIMP_(NTSTATUS) CMiniportWaveRT::NewStream(PMINIPORTWAVERTSTREAM* output, PPORTWAVERTSTREAM port, ULONG pin, BOOLEAN capture, PKSDATAFORMAT format) {
    PAGED_CODE();
    *output = nullptr;
    if (pin != BioWaveHostPin || capture) return STATUS_INVALID_PARAMETER;
    const WAVEFORMATEX* wave;
    NTSTATUS status = BioValidateFormat(Mask(), format, &wave);
    if (!NT_SUCCESS(status)) return status;
    auto stream = new (BioNonPaged{}) CMiniportWaveRTStream(this, port);
    if (!stream) return STATUS_INSUFFICIENT_RESOURCES;
    status = stream->Initialize(format);
    if (NT_SUCCESS(status)) *output = static_cast<IMiniportWaveRTStreamNotification*>(stream);
    else stream->Release();
    return status;
}
STDMETHODIMP_(NTSTATUS) CMiniportWaveRT::DataRangeIntersection(ULONG pin, PKSDATARANGE range, PKSDATARANGE matching, ULONG size, PVOID output, PULONG required) {
    *required = 0;
    if (pin == BioWaveBridgePin) return BioBridgeIntersection(range, matching, size, output, required);
    if (pin != BioWaveHostPin) return STATUS_NO_MATCH;
    for (const auto& entry : BioFormats) if ((Mask() & entry.Bit) && BioRangeAccepts(range, entry) && BioRangeAccepts(matching, entry)) {
        *required = sizeof(KSDATAFORMAT_WAVEFORMATEXTENSIBLE);
        if (!size) return STATUS_BUFFER_OVERFLOW;
        if (size < *required) return STATUS_BUFFER_TOO_SMALL;
        if (!output) return STATUS_INVALID_PARAMETER;
        BioBuildFormat(entry, static_cast<PKSDATAFORMAT_WAVEFORMATEXTENSIBLE>(output));
        return STATUS_SUCCESS;
    }
    return STATUS_NO_MATCH;
}
NTSTATUS CMiniportWaveRT::Property(PPCPROPERTY_REQUEST request) {
    auto miniport = static_cast<CMiniportWaveRT*>(reinterpret_cast<IMiniportWaveRT*>(request->MajorTarget));
    if (IsEqualGUIDAligned(*request->PropertyItem->Set, KSPROPSETID_General)) {
        if (request->Verb & KSPROPERTY_TYPE_BASICSUPPORT) return BioBasicSupport(request, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT);
        NTSTATUS status = BioPropertySize(request, sizeof(KSCOMPONENTID));
        if (NT_SUCCESS(status)) {
            auto id = static_cast<PKSCOMPONENTID>(request->Value);
            RtlZeroMemory(id, sizeof(*id)); id->Component = BioAdapterInterface; id->Version = 1;
        }
        return status;
    }
    if (request->Verb & KSPROPERTY_TYPE_BASICSUPPORT) return BioBasicSupport(request, KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT);
    if (!(request->Verb & KSPROPERTY_TYPE_SET) || request->InstanceSize < sizeof(ULONG) ||
        *static_cast<PULONG>(request->Instance) != BioWaveHostPin || request->ValueSize < sizeof(KSDATAFORMAT)) return STATUS_INVALID_PARAMETER;
    auto data = static_cast<PKSDATAFORMAT>(request->Value);
    if (data->FormatSize > request->ValueSize) return STATUS_INVALID_PARAMETER;
    const WAVEFORMATEX* wave;
    return BioValidateFormat(miniport->Mask(), data, &wave);
}
