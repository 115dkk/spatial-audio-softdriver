// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/TabletAudioSample/hdmitoptable.h at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#pragma once
static KSJACK_DESCRIPTION SsrHdmiJack = {
    KSAUDIO_SPEAKER_STEREO, 0, eConnTypeOtherDigital, eGeoLocHDMI,
    eGenLocPrimaryBox, ePortConnJack, TRUE
};
static PCPROPERTY_ITEM HdmiProperties[] = {
    {&KSPROPSETID_Jack, KSPROPERTY_JACK_DESCRIPTION, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, CMiniportTopologyHdmi::Property},
    {&KSPROPSETID_Jack, KSPROPERTY_JACK_DESCRIPTION2, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, CMiniportTopologyHdmi::Property},
    {&KSPROPSETID_Jack, KSPROPERTY_JACK_SINK_INFO, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, CMiniportTopologyHdmi::Property},
};
DEFINE_PCAUTOMATION_TABLE_PROP(HdmiAutomation, HdmiProperties);

// Spatial SoftReceiver serial volume/mute nodes; the property handler follows SysVAD kshelper.cpp.
static PCPROPERTY_ITEM SsrVolumeProperties[] = {
    {&KSPROPSETID_Audio, KSPROPERTY_AUDIO_VOLUMELEVEL, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT, CMiniportTopologyHdmi::AudioProperty},
};
static PCPROPERTY_ITEM SsrMuteProperties[] = {
    {&KSPROPSETID_Audio, KSPROPERTY_AUDIO_MUTE, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT, CMiniportTopologyHdmi::AudioProperty},
};
DEFINE_PCAUTOMATION_TABLE_PROP(SsrVolumeAutomation, SsrVolumeProperties);
DEFINE_PCAUTOMATION_TABLE_PROP(SsrMuteAutomation, SsrMuteProperties);
static PCNODE_DESCRIPTOR SsrVolumeNodes[] = {
    {0, &SsrVolumeAutomation, &KSNODETYPE_VOLUME, &KSAUDFNAME_VOLUME_CONTROL},
    {0, &SsrMuteAutomation, &KSNODETYPE_MUTE, &KSAUDFNAME_MASTER_MUTE},
};
