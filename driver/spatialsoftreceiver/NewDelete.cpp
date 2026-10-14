// SPDX-License-Identifier: MS-PL
// Copyright (c) Microsoft Corporation. All Rights Reserved.
// Reduced from audio/sysvad/EndpointsCommon/NewDelete.cpp at 2dc3fd3a0cc84a2933f2194e7ec0871584979071.
#include "NewDelete.h"
#include "ssr_kernel.h"
void* __cdecl operator new(size_t bytes, SsrNonPaged) noexcept { return ssr::km::PoolAllocate(bytes); }
void __cdecl operator delete(void* pointer, SsrNonPaged) noexcept { operator delete(pointer); }
void __cdecl operator delete(void* pointer) noexcept { ssr::km::PoolFree(pointer); }
void __cdecl operator delete(void* pointer, size_t) noexcept { operator delete(pointer); }
