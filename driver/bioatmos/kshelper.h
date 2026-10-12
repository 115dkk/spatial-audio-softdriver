// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/kshelper.h at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#pragma once
#include "hdmiwavtable.h"
NTSTATUS BioPropertySize(PPCPROPERTY_REQUEST request, ULONG size);
NTSTATUS BioBasicSupport(PPCPROPERTY_REQUEST request, ULONG access, ULONG type = VT_ILLEGAL);
void BioBuildFormat(const BioFormatEntry& entry, PKSDATAFORMAT_WAVEFORMATEXTENSIBLE output);
NTSTATUS BioValidateFormat(ULONGLONG mask, PKSDATAFORMAT data, const WAVEFORMATEX** wave);
bool BioRangeAccepts(PKSDATARANGE range, const BioFormatEntry& format);
