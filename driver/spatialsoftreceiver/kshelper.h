// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/kshelper.h at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#pragma once
#include "hdmiwavtable.h"
NTSTATUS SsrPropertySize(PPCPROPERTY_REQUEST request, ULONG size);
NTSTATUS SsrBasicSupport(PPCPROPERTY_REQUEST request, ULONG access, ULONG type = VT_ILLEGAL);
void SsrBuildFormat(const SsrFormatEntry& entry, PKSDATAFORMAT_WAVEFORMATEXTENSIBLE output);
bool SsrFormatTypeAllowed(const SSR_ENDPOINT_FORMATS& formats, const KSDATAFORMAT* data);
NTSTATUS SsrValidateFormat(const SSR_ENDPOINT_FORMATS& formats, PKSDATAFORMAT data, const WAVEFORMATEX** wave);
bool SsrRangeAccepts(PKSDATARANGE range, const SsrFormatEntry& format);
bool SsrPcmIntersection(const SSR_PCM_FORMATS& pcm, USHORT endpoint, PKSDATARANGE range, PKSDATARANGE matching, SsrFormatEntry* result);
