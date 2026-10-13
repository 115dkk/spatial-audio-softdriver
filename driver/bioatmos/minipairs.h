// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/TabletAudioSample/minipairs.h at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#pragma once
struct BioMiniPair { PCWSTR Wave; PCWSTR Topology; USHORT Endpoint; };
static const BioMiniPair BioMiniPairs[] = {
    {L"WaveHdmi0", L"TopologyHdmi0", BIO_ENDPOINT_PRIMARY},
    {L"WaveHdmi1", L"TopologyHdmi1", BIO_ENDPOINT_SECOND},
    {L"WaveHdmi2", L"TopologyHdmi2", BIO_ENDPOINT_PASSTHROUGH},
};
