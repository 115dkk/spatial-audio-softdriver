// SPDX-License-Identifier: MS-PL
// Copyright (c) 2026 115dkk
// Licensed under the Microsoft Public License (MS-PL).
//
// <stdint.h> for the kernel build (ADR 0027). The WDK's km\crt has none, so
// MSBuild found Visual Studio's user-mode header, whose vcruntime.h redefines
// the kernel CRT's macros (C4005 and C4083, errors under /WX). Only the driver
// project and tools/kcheck.py put this folder on the include path; the core's
// own tests build against the platform header.
#pragma once

#ifndef _KERNEL_MODE
#error "driver/kcrt is for the kernel build only"
#endif

// Keeps Visual Studio's <stdint.h> out should another header name it.
#define _STDINT

typedef signed char int8_t;
typedef short int16_t;
typedef int int32_t;
typedef long long int64_t;
typedef unsigned char uint8_t;
typedef unsigned short uint16_t;
typedef unsigned int uint32_t;
typedef unsigned long long uint64_t;

#ifndef _INTPTR_T_DEFINED
#define _INTPTR_T_DEFINED
#ifdef _WIN64
typedef long long intptr_t;
#else
typedef int intptr_t;
#endif
#endif

#ifndef _UINTPTR_T_DEFINED
#define _UINTPTR_T_DEFINED
#ifdef _WIN64
typedef unsigned long long uintptr_t;
#else
typedef unsigned int uintptr_t;
#endif
#endif

#define INT8_MIN (-127 - 1)
#define INT16_MIN (-32767 - 1)
#define INT32_MIN (-2147483647 - 1)
#define INT64_MIN (-9223372036854775807LL - 1)
#define INT8_MAX 127
#define INT16_MAX 32767
#define INT32_MAX 2147483647
#define INT64_MAX 9223372036854775807LL
#define UINT8_MAX 0xffU
#define UINT16_MAX 0xffffU
#define UINT32_MAX 0xffffffffU
#define UINT64_MAX 0xffffffffffffffffULL

#ifdef _WIN64
#define INTPTR_MIN INT64_MIN
#define INTPTR_MAX INT64_MAX
#define UINTPTR_MAX UINT64_MAX
#else
#define INTPTR_MIN INT32_MIN
#define INTPTR_MAX INT32_MAX
#define UINTPTR_MAX UINT32_MAX
#endif

#define INT32_C(x) (x)
#define INT64_C(x) (x##LL)
#define UINT32_C(x) (x##U)
#define UINT64_C(x) (x##ULL)
