// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/EndpointsCommon/minwavert.cpp at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#include "minwavert.h"
#include "minwavertstream.h"
#include "basetopo.h"
static PCPROPERTY_ITEM WaveProperties[] = {
    {&KSPROPSETID_Pin, KSPROPERTY_PIN_PROPOSEDATAFORMAT, KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT, CMiniportWaveRT::Property},
};
DEFINE_PCAUTOMATION_TABLE_PROP(WaveAutomation, WaveProperties);
// The optional signal-processing-mode attribute every host data range lists,
// as SysVAD's HDMI ranges do: a pin that announces modes is opened with a
// mode attribute after the format, and KS matches an attributed format only
// against a range that lists the attribute. With bare ranges every pin the
// audio service tried to create failed with STATUS_NO_MATCH (driver KS trace).
static KSATTRIBUTE ModeAttribute = {sizeof(KSATTRIBUTE), 0, BioGuid(STATICGUIDOF(KSATTRIBUTEID_AUDIOSIGNALPROCESSING_MODE))};
static PKSATTRIBUTE ModeAttributes[] = {&ModeAttribute};
static KSATTRIBUTE_LIST ModeAttributeList = {RTL_NUMBER_OF(ModeAttributes), ModeAttributes};
CMiniportWaveRT::CMiniportWaveRT(CAdapterCommon* adapter, USHORT endpoint) : adapter_(adapter), endpoint_(endpoint) {
    adapter_->AddRef();
    ULONG count = 0;
    for (const auto& format : BioFormats) if (format.Bit & Mask()) {
        auto& range = ranges_[count];
        range.DataRange.FormatSize = sizeof(range);
        range.DataRange.Flags = KSDATARANGE_ATTRIBUTES;
        range.DataRange.MajorFormat = KSDATAFORMAT_TYPE_AUDIO;
        range.DataRange.SubFormat = format.Subtype;
        range.DataRange.Specifier = KSDATAFORMAT_SPECIFIER_WAVEFORMATEX;
        range.MaximumChannels = format.Channels;
        range.MinimumBitsPerSample = range.MaximumBitsPerSample = 16;
        range.MinimumSampleFrequency = range.MaximumSampleFrequency = format.Rate;
        rangePointers_[2 * count] = &range.DataRange;
        rangePointers_[2 * count + 1] = reinterpret_cast<PKSDATARANGE>(&ModeAttributeList);
        auto& bridge = bridges_[count];
        bridge.FormatSize = sizeof(bridge);
        bridge.MajorFormat = KSDATAFORMAT_TYPE_AUDIO;
        bridge.SubFormat = format.Bit == BIO_FORMAT_PCM ? KSDATAFORMAT_SUBTYPE_ANALOG : format.Subtype;
        bridge.Specifier = KSDATAFORMAT_SPECIFIER_NONE;
        bridgePointers_[count++] = &bridge;
    }
    pins_[0].MaxGlobalInstanceCount = 1;
    pins_[0].MaxFilterInstanceCount = 1;
    pins_[0].KsPinDescriptor.DataRangesCount = 2 * count;
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
    else if (IsEqualGUIDAligned(iid, IID_IMiniportAudioSignalProcessing)) *object = static_cast<IMiniportAudioSignalProcessing*>(this);
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
// IMiniportAudioSignalProcessing::GetModes with SysVAD's contract: a pin
// outside the filter is STATUS_INVALID_PARAMETER, a pin without modes is
// STATUS_NOT_SUPPORTED, a NULL array asks for the count and succeeds, a short
// array is STATUS_BUFFER_TOO_SMALL with the count set.
STDMETHODIMP_(NTSTATUS) CMiniportWaveRT::GetModes(ULONG pin, GUID* modes, ULONG* count) {
    PAGED_CODE();
    if (!count || pin >= RTL_NUMBER_OF(pins_)) return STATUS_INVALID_PARAMETER;
    if (pin != BioWaveHostPin) return STATUS_NOT_SUPPORTED;
    if (modes) {
        if (*count < 1) { *count = 1; return STATUS_BUFFER_TOO_SMALL; }
        modes[0] = AUDIO_SIGNALPROCESSINGMODE_RAW;
    }
    *count = 1;
    return STATUS_SUCCESS;
}
NTSTATUS CMiniportWaveRT::Property(PPCPROPERTY_REQUEST request) {
    auto miniport = static_cast<CMiniportWaveRT*>(reinterpret_cast<IMiniportWaveRT*>(request->MajorTarget));
    if (request->Verb & KSPROPERTY_TYPE_BASICSUPPORT) return BioBasicSupport(request, KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT);
    // A pin that takes no stream has no format to propose: "not supported",
    // as for the topology's pins, not a bad parameter.
    if (!(request->Verb & KSPROPERTY_TYPE_SET) || request->InstanceSize < sizeof(ULONG) ||
        *static_cast<PULONG>(request->Instance) != BioWaveHostPin) return STATUS_INVALID_DEVICE_REQUEST;
    if (request->ValueSize < sizeof(KSDATAFORMAT)) return STATUS_INVALID_PARAMETER;
    auto data = static_cast<PKSDATAFORMAT>(request->Value);
    if (data->FormatSize > request->ValueSize) return STATUS_INVALID_PARAMETER;
    // The endpoint builder's question "does this pin take PCM (AC3, WMA Pro)
    // at all?" comes as a bare KSDATAFORMAT. The full-format check answered
    // STATUS_NO_MATCH to PCM, and the builder left both endpoints NOTPRESENT
    // (driver KS trace, tools/ci/ks-trace.ps1).
    if (data->FormatSize == sizeof(KSDATAFORMAT))
        return BioFormatTypeAllowed(miniport->Mask(), data) ? STATUS_SUCCESS : STATUS_NO_MATCH;
    const WAVEFORMATEX* wave;
    return BioValidateFormat(miniport->Mask(), data, &wave);
}
