// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/TabletAudioSample/hdmitoptable.h at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#pragma once
static KSJACK_DESCRIPTION BioHdmiJack = {
    KSAUDIO_SPEAKER_STEREO, 0, eConnTypeOtherDigital, eGeoLocHDMI,
    eGenLocPrimaryBox, ePortConnJack, TRUE
};
static PCPROPERTY_ITEM HdmiProperties[] = {
    {&KSPROPSETID_Jack, KSPROPERTY_JACK_DESCRIPTION, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, CMiniportTopologyHdmi::Property},
    {&KSPROPSETID_Jack, KSPROPERTY_JACK_DESCRIPTION2, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, CMiniportTopologyHdmi::Property},
    {&KSPROPSETID_Jack, KSPROPERTY_JACK_SINK_INFO, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, CMiniportTopologyHdmi::Property},
};
DEFINE_PCAUTOMATION_TABLE_PROP(HdmiAutomation, HdmiProperties);
