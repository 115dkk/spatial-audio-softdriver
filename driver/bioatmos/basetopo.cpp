// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/basetopo.cpp at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#include "basetopo.h"
NTSTATUS BioBridgeIntersection(PKSDATARANGE range, PKSDATARANGE matching, ULONG bytes, PVOID output, PULONG required) {
    *required = 0;
    if (range->FormatSize < sizeof(KSDATARANGE) || matching->FormatSize < sizeof(KSDATARANGE) ||
        (!IsEqualGUIDAligned(range->MajorFormat, matching->MajorFormat) && !IsEqualGUIDAligned(range->MajorFormat, KSDATAFORMAT_TYPE_WILDCARD)) ||
        (!IsEqualGUIDAligned(range->SubFormat, matching->SubFormat) && !IsEqualGUIDAligned(range->SubFormat, KSDATAFORMAT_SUBTYPE_WILDCARD)) ||
        (!IsEqualGUIDAligned(range->Specifier, matching->Specifier) && !IsEqualGUIDAligned(range->Specifier, KSDATAFORMAT_SPECIFIER_WILDCARD))) return STATUS_NO_MATCH;
    *required = sizeof(KSDATAFORMAT);
    if (!bytes) return STATUS_BUFFER_OVERFLOW;
    if (bytes < *required) return STATUS_BUFFER_TOO_SMALL;
    if (!output) return STATUS_INVALID_PARAMETER;
    RtlCopyMemory(output, matching, sizeof(KSDATAFORMAT));
    auto format = static_cast<PKSDATAFORMAT>(output);
    format->FormatSize = sizeof(*format); format->Flags = 0;
    return STATUS_SUCCESS;
}
