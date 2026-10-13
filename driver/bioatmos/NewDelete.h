// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/EndpointsCommon/NewDelete.h at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#pragma once
#include <ntddk.h>
struct BioNonPaged {};
void* __cdecl operator new(size_t bytes, BioNonPaged) noexcept;
void __cdecl operator delete(void* pointer, BioNonPaged) noexcept;
void __cdecl operator delete(void* pointer) noexcept;
void __cdecl operator delete(void* pointer, size_t bytes) noexcept;
inline void* __cdecl operator new(size_t, void* pointer) noexcept { return pointer; }
inline void __cdecl operator delete(void*, void*) noexcept {}
