// SPDX-License-Identifier: MS-PL
// Fixed-storage record ring and endpoint loss accounting.
#include "bio_core.h"
#include <string.h>

namespace bio {
namespace {

constexpr uint32_t kHeaderBytes = static_cast<uint32_t>(sizeof(BIO_RECORD_HEADER));

bool ValidSpan(const uint8_t* data, uint32_t bytes) {
    return bytes == 0 || data != nullptr;
}

}  // namespace

uint32_t RecordRing::RecordBytes(uint32_t payload_bytes) {
    const uint64_t raw = static_cast<uint64_t>(kHeaderBytes) + payload_bytes;
    const uint64_t aligned = (raw + BIO_RECORD_ALIGN - 1U) & ~(static_cast<uint64_t>(BIO_RECORD_ALIGN) - 1U);
    return aligned > UINT32_MAX ? 0U : static_cast<uint32_t>(aligned);
}

bool RecordRing::Init(uint8_t* storage, uint32_t bytes) {
    if (storage == nullptr || (reinterpret_cast<uintptr_t>(storage) % BIO_RECORD_ALIGN) != 0U ||
        bytes < 64U * 1024U || (bytes % BIO_RECORD_ALIGN) != 0U) {
        return false;
    }
    storage_ = storage;
    capacity_ = bytes;
    head_ = 0;
    tail_ = 0;
    queued_ = 0;
    urgent_ = 0;
    memset(endpoints_, 0, sizeof(endpoints_));
    memset(needs_repeat_, 0, sizeof(needs_repeat_));
    memset(recovery_prepared_, 0, sizeof(recovery_prepared_));
    memset(storage_, 0, capacity_);
    return true;
}

void RecordRing::CopyIn(uint32_t offset, const void* source, uint32_t bytes) {
    if (bytes == 0) {
        return;
    }
    const uint32_t first = bytes < capacity_ - offset ? bytes : capacity_ - offset;
    memcpy(storage_ + offset, source, first);
    if (bytes > first) {
        memcpy(storage_, static_cast<const uint8_t*>(source) + first, bytes - first);
    }
}

void RecordRing::CopyOut(uint32_t offset, void* destination, uint32_t bytes) const {
    if (bytes == 0) {
        return;
    }
    const uint32_t first = bytes < capacity_ - offset ? bytes : capacity_ - offset;
    memcpy(destination, storage_ + offset, first);
    if (bytes > first) {
        memcpy(static_cast<uint8_t*>(destination) + first, storage_, bytes - first);
    }
}

BIO_RECORD_HEADER RecordRing::HeaderAt(uint32_t offset) const {
    BIO_RECORD_HEADER header{};
    CopyOut(offset, &header, kHeaderBytes);
    return header;
}

uint32_t RecordRing::RecordBytesAt(uint32_t offset) const {
    return RecordBytes(HeaderAt(offset).len);
}

void RecordRing::DropFirst() {
    const BIO_RECORD_HEADER header = HeaderAt(head_);
    const uint32_t bytes = RecordBytes(header.len);
    if (header.endpoint < BIO_MAX_ENDPOINTS) {
        EndpointState& endpoint = endpoints_[header.endpoint];
        if (header.kind == BIO_RECORD_KIND_DATA) {
            endpoint.lost_bytes += header.len;
            endpoint.pending_lost += header.len;
        }
        endpoint.discontinuity = true;
        needs_repeat_[header.endpoint] = true;
        recovery_prepared_[header.endpoint] = false;
    }
    if (header.kind == BIO_RECORD_KIND_FORMAT_CHANGE || header.kind == BIO_RECORD_KIND_STREAM_STOP) {
        --urgent_;
    }
    head_ = (head_ + bytes) % capacity_;
    queued_ -= bytes;
}

bool RecordRing::MakeRoom(uint32_t bytes) {
    if (bytes == 0 || bytes > capacity_) {
        return false;
    }
    while (capacity_ - queued_ < bytes) {
        DropFirst();
    }
    return true;
}

bool RecordRing::Emit(uint16_t ep, uint16_t kind, uint16_t flags, uint32_t epoch,
                      uint64_t pos, uint64_t qpc, uint64_t qpc_written,
                      const uint8_t* a, uint32_t a_len, const uint8_t* b, uint32_t b_len) {
    if (ep >= BIO_MAX_ENDPOINTS || !ValidSpan(a, a_len) || !ValidSpan(b, b_len) ||
        a_len > UINT32_MAX - b_len) {
        return false;
    }
    const uint32_t payload_bytes = a_len + b_len;
    const uint32_t record_bytes = RecordBytes(payload_bytes);
    if (!MakeRoom(record_bytes)) {
        return false;
    }
    const BIO_RECORD_HEADER header{kind, flags, epoch, pos, qpc, qpc_written, payload_bytes, ep, 0};
    CopyIn(tail_, &header, kHeaderBytes);
    uint32_t cursor = (tail_ + kHeaderBytes) % capacity_;
    CopyIn(cursor, a, a_len);
    cursor = (cursor + a_len) % capacity_;
    CopyIn(cursor, b, b_len);
    cursor = (cursor + b_len) % capacity_;
    const uint32_t padding = record_bytes - kHeaderBytes - payload_bytes;
    const uint64_t zero = 0;
    CopyIn(cursor, &zero, padding);
    tail_ = (tail_ + record_bytes) % capacity_;
    queued_ += record_bytes;
    if (kind == BIO_RECORD_KIND_FORMAT_CHANGE || kind == BIO_RECORD_KIND_STREAM_STOP) {
        ++urgent_;
    }
    return true;
}

bool RecordRing::PrepareRecovery(uint16_t ep, uint64_t qpc, uint32_t main_bytes,
                                 bool main_is_format_change) {
    EndpointState& endpoint = endpoints_[ep];
    bool recovery = false;
    bool repeat = false;
    for (;;) {
        recovery = endpoint.pending_lost != 0 || needs_repeat_[ep];
        repeat = needs_repeat_[ep] && endpoint.format_bytes != 0 && !main_is_format_change;
        const uint32_t overrun_bytes = endpoint.pending_lost == 0
            ? 0U : RecordBytes(static_cast<uint32_t>(sizeof(BIO_OVERRUN_PAYLOAD)));
        const uint32_t repeat_bytes = repeat ? RecordBytes(endpoint.format_bytes) : 0U;
        const uint64_t required = static_cast<uint64_t>(main_bytes) + overrun_bytes + repeat_bytes;
        if (required > capacity_ || !MakeRoom(static_cast<uint32_t>(required))) {
            return false;
        }
        const bool recovery_after_room = endpoint.pending_lost != 0 || needs_repeat_[ep];
        const bool repeat_after_room = needs_repeat_[ep] && endpoint.format_bytes != 0 &&
                                       !main_is_format_change;
        if (recovery == recovery_after_room && repeat == repeat_after_room) {
            break;
        }
        // Making room caused this endpoint's first loss. Recompute the atomic
        // recovery group before emitting any of it.
    }
    if (!recovery) {
        return true;
    }
    if (endpoint.pending_lost != 0) {
        const BIO_OVERRUN_PAYLOAD payload{endpoint.pending_lost};
        if (!Emit(ep, BIO_RECORD_KIND_OVERRUN, 0, 0, endpoint.pos, qpc, 0,
                  reinterpret_cast<const uint8_t*>(&payload), static_cast<uint32_t>(sizeof(payload)),
                  nullptr, 0)) {
            return false;
        }
        endpoint.pending_lost = 0;
        ++endpoint.overruns;
    }
    if (repeat) {
        if (!Emit(ep, BIO_RECORD_KIND_FORMAT_CHANGE, BIO_RECORD_FLAG_FORMAT_REPEAT,
                  endpoint.epoch, endpoint.pos, qpc, 0, endpoint.format,
                  endpoint.format_bytes, nullptr, 0)) {
            return false;
        }
        needs_repeat_[ep] = false;
    }
    recovery_prepared_[ep] = endpoint.discontinuity;
    return true;
}

bool RecordRing::StreamStart(uint16_t ep, uint64_t qpc) {
    if (ep >= BIO_MAX_ENDPOINTS || capacity_ == 0) {
        return false;
    }
    const uint32_t bytes = RecordBytes(0);
    if (!PrepareRecovery(ep, qpc, bytes)) {
        return false;
    }
    EndpointState& endpoint = endpoints_[ep];
    endpoint.pos = 0;
    endpoint.state |= BIO_ENDPOINT_STATE_STREAMING;
    endpoint.discontinuity = true;
    return Emit(ep, BIO_RECORD_KIND_STREAM_START, 0, endpoint.epoch, 0, qpc, 0,
                nullptr, 0, nullptr, 0);
}

bool RecordRing::StreamStop(uint16_t ep, uint64_t qpc) {
    if (ep >= BIO_MAX_ENDPOINTS || capacity_ == 0) {
        return false;
    }
    const uint32_t bytes = RecordBytes(0);
    if (!PrepareRecovery(ep, qpc, bytes)) {
        return false;
    }
    EndpointState& endpoint = endpoints_[ep];
    endpoint.state &= ~static_cast<uint32_t>(BIO_ENDPOINT_STATE_STREAMING);
    return Emit(ep, BIO_RECORD_KIND_STREAM_STOP, 0, endpoint.epoch, endpoint.pos, qpc, 0,
                nullptr, 0, nullptr, 0);
}

bool RecordRing::FormatChange(uint16_t ep, uint64_t qpc, const void* wfx, uint32_t wfx_bytes) {
    if (ep >= BIO_MAX_ENDPOINTS || capacity_ == 0 || wfx == nullptr ||
        wfx_bytes == 0 || wfx_bytes > kMaxFormatBytes) {
        return false;
    }
    const uint32_t bytes = RecordBytes(wfx_bytes);
    if (!PrepareRecovery(ep, qpc, bytes, true)) {
        return false;
    }
    EndpointState& endpoint = endpoints_[ep];
    ++endpoint.epoch;
    endpoint.pos = 0;
    endpoint.format_bytes = wfx_bytes;
    memcpy(endpoint.format, wfx, wfx_bytes);
    if (!Emit(ep, BIO_RECORD_KIND_FORMAT_CHANGE, 0, endpoint.epoch, 0, qpc, 0,
              endpoint.format, endpoint.format_bytes, nullptr, 0)) {
        return false;
    }
    needs_repeat_[ep] = false;
    return true;
}

bool RecordRing::Data(uint16_t ep, uint64_t qpc, uint64_t qpc_written,
                      const uint8_t* a, uint32_t a_len, const uint8_t* b, uint32_t b_len) {
    if (ep >= BIO_MAX_ENDPOINTS || capacity_ == 0 || !ValidSpan(a, a_len) ||
        !ValidSpan(b, b_len) || a_len > UINT32_MAX - b_len) {
        return false;
    }
    const uint32_t payload_bytes = a_len + b_len;
    const uint32_t bytes = RecordBytes(payload_bytes);
    if (payload_bytes > kMaxDataPayload || bytes == 0 || bytes > capacity_) {
        return false;
    }
    EndpointState& endpoint = endpoints_[ep];
    if ((!recovery_prepared_[ep] || endpoint.pending_lost != 0) &&
        !PrepareRecovery(ep, qpc, bytes)) {
        return false;
    }
    const uint16_t flags = endpoint.discontinuity ? BIO_RECORD_FLAG_DISCONTINUITY : 0U;
    const uint64_t pos = endpoint.pos;
    if (!Emit(ep, BIO_RECORD_KIND_DATA, flags, endpoint.epoch, pos, qpc, qpc_written,
              a, a_len, b, b_len)) {
        return false;
    }
    endpoint.pos += payload_bytes;
    endpoint.total_bytes += payload_bytes;
    endpoint.discontinuity = false;
    recovery_prepared_[ep] = false;
    return true;
}

bool RecordRing::SourceGap(uint16_t ep, uint64_t qpc, uint64_t lost_bytes) {
    (void)qpc;
    if (ep >= BIO_MAX_ENDPOINTS || lost_bytes == 0) {
        return false;
    }
    EndpointState& endpoint = endpoints_[ep];
    endpoint.pos += lost_bytes;
    endpoint.total_bytes += lost_bytes;
    endpoint.lost_bytes += lost_bytes;
    endpoint.pending_lost += lost_bytes;
    endpoint.discontinuity = true;
    needs_repeat_[ep] = true;
    recovery_prepared_[ep] = false;
    return true;
}

uint32_t RecordRing::QueuedBytes() const {
    return queued_;
}

bool RecordRing::UrgentQueued() const {
    return urgent_ != 0;
}

uint32_t RecordRing::Read(uint8_t* out, uint32_t capacity) {
    if ((out == nullptr && capacity != 0) || queued_ == 0) {
        return 0;
    }
    uint32_t moved = 0;
    while (queued_ != 0) {
        const BIO_RECORD_HEADER header = HeaderAt(head_);
        const uint32_t bytes = RecordBytes(header.len);
        if (bytes > capacity - moved) {
            break;
        }
        CopyOut(head_, out + moved, bytes);
        if (header.kind == BIO_RECORD_KIND_FORMAT_CHANGE || header.kind == BIO_RECORD_KIND_STREAM_STOP) {
            --urgent_;
        }
        head_ = (head_ + bytes) % capacity_;
        queued_ -= bytes;
        moved += bytes;
    }
    return moved;
}

uint32_t RecordRing::FirstRecordBytes() const {
    return queued_ == 0 ? 0U : RecordBytesAt(head_);
}

void RecordRing::DiscardQueued() {
    head_ = tail_;
    queued_ = 0;
    urgent_ = 0;
    memset(recovery_prepared_, 0, sizeof(recovery_prepared_));
    for (uint32_t ep = 0; ep < BIO_MAX_ENDPOINTS; ++ep) {
        EndpointState& endpoint = endpoints_[ep];
        if (endpoint.format_bytes != 0) {
            needs_repeat_[ep] = true;
            endpoint.discontinuity = true;
        }
    }
}

const EndpointState& RecordRing::Endpoint(uint16_t ep) const {
    static const EndpointState empty{};
    return ep < BIO_MAX_ENDPOINTS ? endpoints_[ep] : empty;
}

uint32_t RecordRing::CapacityBytes() const {
    return capacity_;
}

void RecordRing::FillStats(BIO_STATS* out, uint32_t open_count, uint32_t max_timer_lateness_us) const {
    if (out == nullptr) {
        return;
    }
    memset(out, 0, sizeof(*out));
    out->struct_size = static_cast<uint32_t>(sizeof(*out));
    out->open_count = open_count;
    out->max_timer_lateness_us = max_timer_lateness_us;
    for (uint32_t ep = 0; ep < BIO_MAX_ENDPOINTS; ++ep) {
        out->endpoints[ep].total_bytes = endpoints_[ep].total_bytes;
        out->endpoints[ep].overruns = endpoints_[ep].overruns;
        out->endpoints[ep].lost_bytes = endpoints_[ep].lost_bytes;
        out->endpoints[ep].epoch = endpoints_[ep].epoch;
        out->endpoints[ep].state = endpoints_[ep].state;
    }
}

}  // namespace bio
