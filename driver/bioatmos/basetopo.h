// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/basetopo.h at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#pragma once
#include "bioatmos.h"
NTSTATUS BioBridgeIntersection(PKSDATARANGE range, PKSDATARANGE matching, ULONG bytes, PVOID output, PULONG required);
