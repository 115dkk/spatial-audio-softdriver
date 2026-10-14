// SPDX-License-Identifier: MS-PL
// Copyright (c) 2026 115dkk
#pragma once
#include "spatialsoftreceiver.h"

void SsrTraceFault(ULONG call);

namespace ssr::km {
namespace fault {
struct Counts {
    ULONG calls;
    ULONG injectedAt;
    LONG pool;
    LONG timers;
    LONG dmaPages;
};

inline constinit volatile LONG FailAt{};
inline constinit volatile LONG Calls{};
inline constinit volatile LONG InjectedAt{};
inline constinit volatile LONG OutstandingPool{};
inline constinit volatile LONG OutstandingTimers{};
inline constinit volatile LONG OutstandingDmaPages{};

inline void Configure(ULONG failAt) {
    InterlockedExchange(&FailAt, static_cast<LONG>(failAt));
}

[[nodiscard]] inline bool Hit() {
    const ULONG call = static_cast<ULONG>(InterlockedIncrement(&Calls));
    const ULONG failAt = static_cast<ULONG>(InterlockedCompareExchange(&FailAt, 0, 0));
    if (failAt < 1 || call != failAt) return false;
    InterlockedExchange(&InjectedAt, static_cast<LONG>(call));
    SsrTraceFault(call);
    return true;
}

inline Counts Read() {
    return {
        static_cast<ULONG>(InterlockedCompareExchange(&Calls, 0, 0)),
        static_cast<ULONG>(InterlockedCompareExchange(&InjectedAt, 0, 0)),
        InterlockedCompareExchange(&OutstandingPool, 0, 0),
        InterlockedCompareExchange(&OutstandingTimers, 0, 0),
        InterlockedCompareExchange(&OutstandingDmaPages, 0, 0),
    };
}
}

[[nodiscard]] inline void* PoolAllocate(SIZE_T bytes) {
    if (fault::Hit()) return nullptr;
    void* block = ExAllocatePool2(POOL_FLAG_NON_PAGED, bytes, SsrPoolTag);
    if (block) InterlockedIncrement(&fault::OutstandingPool);
    return block;
}

inline void PoolFree(void* block) {
    if (!block) return;
    ExFreePoolWithTag(block, SsrPoolTag);
    InterlockedDecrement(&fault::OutstandingPool);
}

template <class T> constexpr T&& Move(T& value) { return static_cast<T&&>(value); }

template <class T> class PoolPtr {
public:
    PoolPtr() = default;
    explicit PoolPtr(T* adopt) : pointer_(adopt) {}
    PoolPtr(const PoolPtr&) = delete;
    PoolPtr& operator=(const PoolPtr&) = delete;
    PoolPtr(PoolPtr&& other) : pointer_(other.Release()) {}
    PoolPtr& operator=(PoolPtr&& other) {
        if (this != &other) Reset(other.Release());
        return *this;
    }
    [[nodiscard]] static PoolPtr Allocate(SIZE_T bytes = sizeof(T)) {
        return PoolPtr(static_cast<T*>(PoolAllocate(bytes)));
    }
    ~PoolPtr() { Reset(); }
    T* Get() const { return pointer_; }
    T* operator->() const { return pointer_; }
    T& operator*() const { return *pointer_; }
    explicit operator bool() const { return pointer_ != nullptr; }
    [[nodiscard]] T* Release() {
        T* pointer = pointer_;
        pointer_ = nullptr;
        return pointer;
    }
    void Reset(T* adopt = nullptr) {
        T* old = pointer_;
        pointer_ = adopt;
        PoolFree(old);
    }
private:
    T* pointer_ = nullptr;
};

class KernelHandle {
public:
    KernelHandle() = default;
    explicit KernelHandle(HANDLE adopt) : handle_(adopt) {}
    KernelHandle(const KernelHandle&) = delete;
    KernelHandle& operator=(const KernelHandle&) = delete;
    KernelHandle(KernelHandle&& other) : handle_(other.Release()) {}
    KernelHandle& operator=(KernelHandle&& other) {
        if (this != &other) Reset(other.Release());
        return *this;
    }
    ~KernelHandle() { Reset(); }
    HANDLE Get() const { return handle_; }
    HANDLE* Put() { Reset(); return &handle_; }
    [[nodiscard]] HANDLE Release() {
        HANDLE handle = handle_;
        handle_ = nullptr;
        return handle;
    }
    void Reset(HANDLE adopt = nullptr) {
        HANDLE old = handle_;
        handle_ = adopt;
        if (old) ZwClose(old);
    }
    explicit operator bool() const { return handle_ != nullptr; }
private:
    HANDLE handle_ = nullptr;
};

template <class T> class ObjectRef {
public:
    ObjectRef() = default;
    ObjectRef(const ObjectRef&) = delete;
    ObjectRef& operator=(const ObjectRef&) = delete;
    ObjectRef(ObjectRef&& other) : pointer_(other.Release()) {}
    ObjectRef& operator=(ObjectRef&& other) {
        if (this != &other) {
            Reset();
            pointer_ = other.Release();
        }
        return *this;
    }
    [[nodiscard]] static ObjectRef Adopt(T* referenced) { return ObjectRef(referenced); }
    [[nodiscard]] static ObjectRef Retain(T* object) {
        if (object) ObReferenceObject(object);
        return ObjectRef(object);
    }
    ~ObjectRef() { Reset(); }
    T* Get() const { return pointer_; }
    T* operator->() const { return pointer_; }
    explicit operator bool() const { return pointer_ != nullptr; }
    [[nodiscard]] T* Release() {
        T* pointer = pointer_;
        pointer_ = nullptr;
        return pointer;
    }
    void Reset() {
        T* old = pointer_;
        pointer_ = nullptr;
        if (old) ObDereferenceObject(old);
    }
private:
    explicit ObjectRef(T* pointer) : pointer_(pointer) {}
    T* pointer_ = nullptr;
};

template <class T> class ComRef {
public:
    ComRef() = default;
    ComRef(const ComRef&) = delete;
    ComRef& operator=(const ComRef&) = delete;
    ComRef(ComRef&& other) : pointer_(other.Detach()) {}
    ComRef& operator=(ComRef&& other) {
        if (this != &other) {
            Reset();
            pointer_ = other.Detach();
        }
        return *this;
    }
    [[nodiscard]] static ComRef Adopt(T* referenced) { return ComRef(referenced); }
    [[nodiscard]] static ComRef Retain(T* object) {
        if (object) object->AddRef();
        return ComRef(object);
    }
    ~ComRef() { Reset(); }
    T* Get() const { return pointer_; }
    T* operator->() const { return pointer_; }
    explicit operator bool() const { return pointer_ != nullptr; }
    T** Put() { Reset(); return &pointer_; }
    [[nodiscard]] T* Detach() {
        T* pointer = pointer_;
        pointer_ = nullptr;
        return pointer;
    }
    void Reset() {
        T* old = pointer_;
        pointer_ = nullptr;
        if (old) old->Release();
    }
private:
    explicit ComRef(T* pointer) : pointer_(pointer) {}
    T* pointer_ = nullptr;
};

class ExTimer {
public:
    ExTimer() = default;
    ExTimer(const ExTimer&) = delete;
    ExTimer& operator=(const ExTimer&) = delete;
    ExTimer(ExTimer&& other) : timer_(other.Detach()) {}
    ExTimer& operator=(ExTimer&& other) {
        if (this != &other) {
            Reset();
            timer_ = other.Detach();
        }
        return *this;
    }
    [[nodiscard]] static ExTimer Allocate(PEXT_CALLBACK callback, PVOID context, ULONG attributes) {
        if (fault::Hit()) return ExTimer();
        PEX_TIMER timer = ExAllocateTimer(callback, context, attributes);
        if (timer) InterlockedIncrement(&fault::OutstandingTimers);
        return ExTimer(timer);
    }
    ~ExTimer() { Reset(); }
    PEX_TIMER Get() const { return timer_; }
    explicit operator bool() const { return timer_ != nullptr; }
    void Reset() {
        PEX_TIMER old = timer_;
        timer_ = nullptr;
        if (old) {
            ExDeleteTimer(old, TRUE, TRUE, nullptr);
            InterlockedDecrement(&fault::OutstandingTimers);
        }
    }
private:
    explicit ExTimer(PEX_TIMER timer) : timer_(timer) {}
    [[nodiscard]] PEX_TIMER Detach() {
        PEX_TIMER timer = timer_;
        timer_ = nullptr;
        return timer;
    }
    PEX_TIMER timer_ = nullptr;
};

class RemoveLockHold {
public:
    RemoveLockHold() = default;
    RemoveLockHold(const RemoveLockHold&) = delete;
    RemoveLockHold& operator=(const RemoveLockHold&) = delete;
    RemoveLockHold(RemoveLockHold&& other) : lock_(other.lock_), tag_(other.tag_) {
        other.lock_ = nullptr;
        other.tag_ = nullptr;
    }
    RemoveLockHold& operator=(RemoveLockHold&& other) {
        if (this != &other) {
            Reset();
            lock_ = other.lock_;
            tag_ = other.tag_;
            other.lock_ = nullptr;
            other.tag_ = nullptr;
        }
        return *this;
    }
    [[nodiscard]] NTSTATUS Acquire(PIO_REMOVE_LOCK lock, PVOID tag) {
        Reset();
        NTSTATUS status = IoAcquireRemoveLock(lock, tag);
        if (NT_SUCCESS(status)) {
            lock_ = lock;
            tag_ = tag;
        }
        return status;
    }
    ~RemoveLockHold() { Reset(); }
    void Transfer() { lock_ = nullptr; tag_ = nullptr; }
    bool Held() const { return lock_ != nullptr; }
private:
    void Reset() {
        PIO_REMOVE_LOCK lock = lock_;
        PVOID tag = tag_;
        lock_ = nullptr;
        tag_ = nullptr;
        if (lock) IoReleaseRemoveLock(lock, tag);
    }
    PIO_REMOVE_LOCK lock_ = nullptr;
    PVOID tag_ = nullptr;
};

class DmaPages {
public:
    DmaPages() = default;
    DmaPages(const DmaPages&) = delete;
    DmaPages& operator=(const DmaPages&) = delete;
    DmaPages(DmaPages&& other)
        : port_(other.port_), mdl_(other.mdl_), buffer_(other.buffer_), bytes_(other.bytes_) {
        other.port_ = nullptr;
        other.mdl_ = nullptr;
        other.buffer_ = nullptr;
        other.bytes_ = 0;
    }
    DmaPages& operator=(DmaPages&& other) {
        if (this != &other) {
            Reset();
            port_ = other.port_;
            mdl_ = other.mdl_;
            buffer_ = other.buffer_;
            bytes_ = other.bytes_;
            other.port_ = nullptr;
            other.mdl_ = nullptr;
            other.buffer_ = nullptr;
            other.bytes_ = 0;
        }
        return *this;
    }
    [[nodiscard]] NTSTATUS Allocate(PPORTWAVERTSTREAM port, ULONG bytes, MEMORY_CACHING_TYPE cache) {
        Reset();
        PHYSICAL_ADDRESS high;
        high.QuadPart = MAXLONGLONG;
        if (fault::Hit()) return STATUS_INSUFFICIENT_RESOURCES;
        PMDL mdl = port->AllocatePagesForMdl(high, bytes);
        if (!mdl) return STATUS_INSUFFICIENT_RESOURCES;
        InterlockedIncrement(&fault::OutstandingDmaPages);
        if (MmGetMdlByteCount(mdl) < bytes) {
            port->FreePagesFromMdl(mdl);
            InterlockedDecrement(&fault::OutstandingDmaPages);
            return STATUS_INSUFFICIENT_RESOURCES;
        }
        BYTE* buffer = static_cast<BYTE*>(port->MapAllocatedPages(mdl, cache));
        if (!buffer) {
            port->FreePagesFromMdl(mdl);
            InterlockedDecrement(&fault::OutstandingDmaPages);
            return STATUS_INSUFFICIENT_RESOURCES;
        }
        port_ = port;
        mdl_ = mdl;
        buffer_ = buffer;
        bytes_ = bytes;
        return STATUS_SUCCESS;
    }
    ~DmaPages() { Reset(); }
    PMDL Mdl() const { return mdl_; }
    BYTE* Buffer() const { return buffer_; }
    ULONG Bytes() const { return bytes_; }
    explicit operator bool() const { return mdl_ != nullptr; }
    void Reset() {
        PPORTWAVERTSTREAM port = port_;
        PMDL mdl = mdl_;
        BYTE* buffer = buffer_;
        port_ = nullptr;
        mdl_ = nullptr;
        buffer_ = nullptr;
        bytes_ = 0;
        if (buffer) port->UnmapAllocatedPages(buffer, mdl);
        if (mdl) {
            port->FreePagesFromMdl(mdl);
            InterlockedDecrement(&fault::OutstandingDmaPages);
        }
    }
private:
    PPORTWAVERTSTREAM port_ = nullptr;
    PMDL mdl_ = nullptr;
    BYTE* buffer_ = nullptr;
    ULONG bytes_ = 0;
};
}
