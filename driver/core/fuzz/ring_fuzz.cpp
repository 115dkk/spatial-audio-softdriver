// SPDX-License-Identifier: MS-PL
// libFuzzer target for record-ring operations and READ completion rules.
#include "ssr_core.h"
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

namespace {

struct SeenState {
    SSR_VOLUME_PAYLOAD volume;
    uint32_t epoch;
    bool format_known;
    bool streaming;
    uint32_t format_bytes;
    uint8_t format[ssr::kMaxFormatBytes];
};

uint32_t RecordBytes(uint32_t payload) {
    return (static_cast<uint32_t>(sizeof(SSR_RECORD_HEADER)) + payload +
            SSR_RECORD_ALIGN - 1U) & ~(SSR_RECORD_ALIGN - 1U);
}

void Consume(const uint8_t* records, uint32_t bytes, SeenState* seen) {
    uint32_t at = 0;
    while (at + sizeof(SSR_RECORD_HEADER) <= bytes) {
        SSR_RECORD_HEADER header{};
        memcpy(&header, records + at, sizeof(header));
        const uint32_t record_bytes = RecordBytes(header.len);
        if (record_bytes > bytes - at || header.endpoint >= SSR_MAX_ENDPOINTS) abort();
        SeenState& state = seen[header.endpoint];
        if (header.kind == SSR_RECORD_KIND_FORMAT_CHANGE) {
            if (header.len > ssr::kMaxFormatBytes) abort();
            state.epoch = header.epoch;
            state.format_known = true;
            state.format_bytes = header.len;
            memcpy(state.format, records + at + sizeof(header), header.len);
        } else if (header.kind == SSR_RECORD_KIND_VOLUME) {
            if (header.len != sizeof(state.volume)) abort();
            memcpy(&state.volume, records + at + sizeof(header), sizeof(state.volume));
            if (state.volume.channel_count == 0 || state.volume.channel_count > SSR_MAX_VOLUME_CHANNELS) abort();
            if (state.volume.mute_mask >> state.volume.channel_count) abort();
        } else if (header.kind == SSR_RECORD_KIND_STREAM_START) {
            state.epoch = header.epoch;
            state.streaming = true;
        } else if (header.kind == SSR_RECORD_KIND_STREAM_STOP) {
            state.epoch = header.epoch;
            state.streaming = false;
        }
        at += record_bytes;
    }
    if (at != bytes) abort();
}

}  // namespace

extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
    alignas(8) uint8_t storage[64U * 1024U];
    uint8_t output[4096];
    ssr::RecordRing ring;
    if (!ring.Init(storage, sizeof(storage))) return 0;
    SeenState seen[SSR_MAX_ENDPOINTS]{};
    for (uint16_t ep = 0; ep < SSR_MAX_ENDPOINTS; ++ep)
        if (!ring.ConfigureVolume(ep, ep == 0 ? 12 : 2)) abort();
    size_t at = 0;
    uint64_t qpc = 0;
    while (at < size) {
        const uint8_t op = data[at++];
        const uint16_t ep = static_cast<uint16_t>((op >> 3U) & 7U);
        qpc += op;
        switch (op & 7U) {
            case 0: (void)ring.StreamStart(ep, qpc); break;
            case 1: (void)ring.StreamStop(ep, qpc); break;
            case 2: {
                const uint32_t remain = static_cast<uint32_t>(size - at);
                const uint32_t bytes = remain < ssr::kMaxFormatBytes ? remain : ssr::kMaxFormatBytes;
                (void)ring.FormatChange(ep, qpc, data + at, bytes);
                at += bytes;
                break;
            }
            case 3:
                (void)ring.SetVolume(ep, (op & 0x80U) ? -1 : 0, -static_cast<int32_t>(op) * 65536, qpc);
                (void)ring.SetMute(ep, (op & 0x40U) ? -1 : 1, (op & 0x80U) != 0, qpc);
                break;
            case 4: {
                const uint32_t remain = static_cast<uint32_t>(size - at);
                const uint32_t bytes = remain < 1024U ? remain : 1024U;
                const uint32_t split = bytes / 2U;
                (void)ring.Data(ep, qpc, qpc + 1U, data + at, split,
                                data + at + split, bytes - split);
                at += bytes;
                break;
            }
            case 5: {
                const uint32_t got = ring.Read(output, static_cast<uint32_t>(op) * 16U, qpc);
                Consume(output, got, seen);
                break;
            }
            case 6:
                (void)ring.SourceGap(ep, qpc, static_cast<uint64_t>(op) + 1U);
                break;
            default:
                if ((op & 0x80U) != 0) {
                    ring.DiscardQueued(qpc);
                    memset(seen, 0, sizeof(seen));
                } else {
                    (void)ssr::ReadGate::ShouldComplete(ring, static_cast<uint32_t>(op) * 32U,
                                                        sizeof(output), qpc, qpc + op);
                }
                break;
        }
    }
    (void)ring.PrepareRead(qpc);
    for (;;) {
        const uint32_t got = ring.Read(output, sizeof(output), qpc);
        if (got == 0) break;
        Consume(output, got, seen);
    }
    for (uint16_t ep = 0; ep < SSR_MAX_ENDPOINTS; ++ep) {
        const ssr::EndpointState& actual = ring.Endpoint(ep);
        if (memcmp(&seen[ep].volume, &actual.volume, sizeof(actual.volume)) != 0) abort();
        if (seen[ep].format_known != (actual.format_bytes != 0)) abort();
        if (seen[ep].streaming != ((actual.state & SSR_ENDPOINT_STATE_STREAMING) != 0)) abort();
        if (seen[ep].format_known && seen[ep].epoch != actual.epoch) abort();
        if (seen[ep].format_known && seen[ep].format_bytes != actual.format_bytes) abort();
        if (seen[ep].format_known &&
            memcmp(seen[ep].format, actual.format, actual.format_bytes) != 0) abort();
    }
    return 0;
}
