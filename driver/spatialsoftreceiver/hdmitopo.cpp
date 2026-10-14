// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/TabletAudioSample/hdmitopo.cpp at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
// Volume/mute properties adapted from audio/sysvad/kshelper.cpp at the same commit.
#include "hdmitopo.h"
#include "hdmitoptable.h"
CMiniportTopologyHdmi::CMiniportTopologyHdmi(CAdapterCommon* adapter, USHORT endpoint) : adapter_(adapter), endpoint_(endpoint) {
    adapter_->AddRef();
    channels_ = SsrVolumeChannels(endpoint_);
    ULONG count = 0;
    const auto& pcm = adapter_->Settings().Formats[endpoint_].pcm;
    if (SsrPcmEnabled(pcm, false) || SsrPcmEnabled(pcm, true)) {
        auto& range = ranges_[count];
        range.FormatSize = sizeof(range);
        range.MajorFormat = KSDATAFORMAT_TYPE_AUDIO;
        range.SubFormat = KSDATAFORMAT_SUBTYPE_ANALOG;
        range.Specifier = KSDATAFORMAT_SPECIFIER_NONE;
        pointers_[count++] = &range;
    }
    for (const auto& format : SsrFormats) if (format.Bit & adapter_->Settings().Formats[endpoint_].codecs) {
        auto& range = ranges_[count];
        range.FormatSize = sizeof(range);
        range.MajorFormat = KSDATAFORMAT_TYPE_AUDIO;
        range.SubFormat = format.Subtype;
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
    connections_[0] = {PCFILTER_NODE, SsrTopologyBridgePin, 0, 1};
    connections_[1] = {0, 0, 1, 1};
    connections_[2] = {1, 0, PCFILTER_NODE, SsrTopologyJackPin};
    descriptor_.AutomationTable = &HdmiAutomation;
    descriptor_.PinSize = sizeof(PCPIN_DESCRIPTOR); descriptor_.PinCount = 2; descriptor_.Pins = pins_;
    descriptor_.NodeSize = sizeof(PCNODE_DESCRIPTOR);
    descriptor_.NodeCount = ARRAYSIZE(SsrVolumeNodes); descriptor_.Nodes = SsrVolumeNodes;
    descriptor_.ConnectionCount = ARRAYSIZE(connections_); descriptor_.Connections = connections_;
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
    if (pin > SsrTopologyJackPin) { *required = 0; return STATUS_NO_MATCH; }
    return SsrBridgeIntersection(range, matching, bytes, output, required);
}
NTSTATUS CMiniportTopologyHdmi::Property(PPCPROPERTY_REQUEST request) {
    // Only the jack pin has a jack. Any other pin answers "not supported"
    // (STATUS_INVALID_DEVICE_REQUEST), as SysVAD's HDMI topology does; its
    // endpoint turns active on the same runner where ours, answering
    // STATUS_INVALID_PARAMETER here, stopped short of the wave filter.
    if (request->InstanceSize < sizeof(ULONG) || *static_cast<PULONG>(request->Instance) != SsrTopologyJackPin) return STATUS_INVALID_DEVICE_REQUEST;
    if (request->Verb & KSPROPERTY_TYPE_BASICSUPPORT) return SsrBasicSupport(request, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT);
    if (!(request->Verb & KSPROPERTY_TYPE_GET)) return STATUS_INVALID_DEVICE_REQUEST;
    ULONG id = request->PropertyItem->Id;
    if (id == KSPROPERTY_JACK_SINK_INFO) {
        NTSTATUS status = SsrPropertySize(request, sizeof(KSJACK_SINK_INFORMATION));
        if (!NT_SUCCESS(status)) return status;
        auto sink = static_cast<PKSJACK_SINK_INFORMATION>(request->Value);
        RtlZeroMemory(sink, sizeof(*sink));
        sink->ConnType = KSJACK_SINK_CONNECTIONTYPE_HDMI;
        sink->ManufacturerId = 0x115d;
        sink->ProductId = 1;
        sink->AudioLatency = 0;
        sink->HDCPCapable = FALSE; sink->AICapable = TRUE;
        // Names are validated once at PASSIVE_LEVEL and copied into adapter
        // state. The endpoint builder names an HDMI endpoint after this sink.
        static_assert(ssr::kEndpointNameCapacity * sizeof(WCHAR) == sizeof(sink->SinkDescription), "sink name capacity");
        auto miniport = static_cast<CMiniportTopologyHdmi*>(reinterpret_cast<IMiniportTopology*>(request->MajorTarget));
        if (miniport->endpoint_ >= SSR_MAX_ENDPOINTS) return STATUS_INVALID_PARAMETER;
        const WCHAR* name = miniport->adapter_->Settings().EndpointName[miniport->endpoint_];
        const size_t characters = wcslen(name);
        sink->SinkDescriptionLength = static_cast<UCHAR>(characters);
        RtlCopyMemory(sink->SinkDescription, name, (characters + 1) * sizeof(WCHAR));
        return STATUS_SUCCESS;
    }
    ULONG itemBytes;
    if (id == KSPROPERTY_JACK_DESCRIPTION) itemBytes = sizeof(KSJACK_DESCRIPTION);
    else if (id == KSPROPERTY_JACK_DESCRIPTION2) itemBytes = sizeof(KSJACK_DESCRIPTION2);
    else return STATUS_INVALID_DEVICE_REQUEST;
    NTSTATUS status = SsrPropertySize(request, sizeof(KSMULTIPLE_ITEM) + itemBytes);
    if (!NT_SUCCESS(status)) return status;
    auto list = static_cast<PKSMULTIPLE_ITEM>(request->Value);
    list->Size = sizeof(*list) + itemBytes; list->Count = 1;
    if (id == KSPROPERTY_JACK_DESCRIPTION) RtlCopyMemory(list + 1, &SsrHdmiJack, itemBytes);
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

// Adapted from PropertyHandler_BasicSupportVolume / PropertyHandler_BasicSupportMute
// in audio/sysvad/kshelper.cpp at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
static NTSTATUS SsrVolumeSupport(PPCPROPERTY_REQUEST request, ULONG channels, bool mute) {
    const ULONG access = KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT;
    const ULONG full = sizeof(KSPROPERTY_DESCRIPTION) + sizeof(KSPROPERTY_MEMBERSHEADER) +
        sizeof(KSPROPERTY_STEPPING_LONG) * channels;
    if (!request->ValueSize) { request->ValueSize = full; return STATUS_BUFFER_OVERFLOW; }
    if (!request->Value) { request->ValueSize = 0; return STATUS_INVALID_PARAMETER; }
    if (request->ValueSize >= sizeof(KSPROPERTY_DESCRIPTION)) {
        auto description = static_cast<PKSPROPERTY_DESCRIPTION>(request->Value);
        RtlZeroMemory(description, sizeof(*description));
        description->AccessFlags = access;
        description->DescriptionSize = full;
        description->PropTypeSet.Set = KSPROPTYPESETID_General;
        description->PropTypeSet.Id = mute ? VT_BOOL : VT_I4;
        description->MembersListCount = 1;
        if (request->ValueSize >= full) {
            auto members = reinterpret_cast<PKSPROPERTY_MEMBERSHEADER>(description + 1);
            members->MembersFlags = KSPROPERTY_MEMBER_STEPPEDRANGES;
            members->MembersSize = sizeof(KSPROPERTY_STEPPING_LONG);
            members->MembersCount = channels;
            members->Flags = KSPROPERTY_MEMBER_FLAG_BASICSUPPORT_MULTICHANNEL;
            auto range = reinterpret_cast<PKSPROPERTY_STEPPING_LONG>(members + 1);
            for (ULONG i = 0; i < channels; ++i) {
                range[i].Bounds.SignedMinimum = mute ? 0 : SSR_VOLUME_MIN_Q16;
                range[i].Bounds.SignedMaximum = mute ? 1 : SSR_VOLUME_MAX_Q16;
                range[i].SteppingDelta = mute ? 1 : SSR_VOLUME_STEP_Q16;
                range[i].Reserved = 0;
            }
            request->ValueSize = full;
        } else request->ValueSize = sizeof(*description);
        return STATUS_SUCCESS;
    }
    if (request->ValueSize >= sizeof(ULONG)) {
        *static_cast<PULONG>(request->Value) = access;
        request->ValueSize = sizeof(ULONG);
        return STATUS_SUCCESS;
    }
    request->ValueSize = 0;
    return STATUS_BUFFER_TOO_SMALL;
}

// Adapted from PropertyHandler_Volume / PropertyHandler_Mute in
// audio/sysvad/kshelper.cpp at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
// ALL_CHANNELS GET reads channel zero; SET changes every supported channel,
// bounded by channel_count (not the upstream ALL_CHANNELS_ID loop bound).
NTSTATUS CMiniportTopologyHdmi::AudioProperty(PPCPROPERTY_REQUEST request) {
    if (!request || !request->MajorTarget || !request->PropertyItem) return STATUS_INVALID_PARAMETER;
    auto miniport = static_cast<CMiniportTopologyHdmi*>(reinterpret_cast<IMiniportTopology*>(request->MajorTarget));
    const ULONG id = request->PropertyItem->Id;
    const bool mute = id == KSPROPERTY_AUDIO_MUTE;
    if ((!mute && id != KSPROPERTY_AUDIO_VOLUMELEVEL) ||
        request->Node != (mute ? 1UL : 0UL) || miniport->channels_ == 0 ||
        miniport->channels_ > SSR_MAX_VOLUME_CHANNELS) return STATUS_INVALID_DEVICE_REQUEST;
    if (request->Verb & KSPROPERTY_TYPE_BASICSUPPORT)
        return SsrVolumeSupport(request, miniport->channels_, mute);
    if (!(request->Verb & (KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET))) return STATUS_INVALID_DEVICE_REQUEST;
    if (!request->Instance || request->InstanceSize < sizeof(LONG)) return STATUS_INVALID_PARAMETER;
    const LONG channel = *static_cast<PLONG>(request->Instance);
    if (channel < -1 || (channel != -1 && static_cast<ULONG>(channel) >= miniport->channels_)) return STATUS_INVALID_PARAMETER;
    NTSTATUS status = SsrPropertySize(request, sizeof(LONG));
    if (!NT_SUCCESS(status)) return status;
    if (!request->Value) return STATUS_INVALID_PARAMETER;
    auto value = static_cast<PLONG>(request->Value);
    auto state = miniport->adapter_->State();
    KIRQL irql;
    KeAcquireSpinLock(&state->Lock, &irql);
    if (!state->Started || state->Removed) status = STATUS_DEVICE_NOT_READY;
    else if (request->Verb & KSPROPERTY_TYPE_GET) {
        const auto& volume = state->Ring.Endpoint(miniport->endpoint_).volume;
        const ULONG index = channel == -1 ? 0 : static_cast<ULONG>(channel);
        *value = mute ? ((volume.mute_mask & (1UL << index)) != 0) : volume.level_q16[index];
    } else {
        const bool ok = mute ? state->Ring.SetMute(miniport->endpoint_, channel, *value != 0, SsrQpc())
                             : state->Ring.SetVolume(miniport->endpoint_, channel, *value, SsrQpc());
        if (!ok) status = STATUS_INVALID_PARAMETER;
    }
    KeReleaseSpinLock(&state->Lock, irql);
    // The stream timer may be idle. A changed volume is urgent and completes a
    // waiting READ without needing playback; never pump while holding Lock.
    if (NT_SUCCESS(status) && (request->Verb & KSPROPERTY_TYPE_SET)) SsrPumpRead(state);
    return status;
}
