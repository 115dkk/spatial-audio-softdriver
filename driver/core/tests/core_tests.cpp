// SPDX-License-Identifier: MS-PL
// Dependency-free unit tests for the driver core.
#include "ssr_core.h"
#include "ssr_names.h"
#include <stdio.h>
#include <string.h>

namespace {

int failures = 0;
#define CHECK(expression) do { if (!(expression)) { \
    fprintf(stderr, "%s:%d: CHECK(%s) failed\n", __FILE__, __LINE__, #expression); \
    ++failures; } } while (false)

alignas(8) uint8_t storage[64U * 1024U];

uint32_t RecordBytes(uint32_t payload) {
    return (static_cast<uint32_t>(sizeof(SSR_RECORD_HEADER)) + payload + 7U) & ~7U;
}

void Put16(uint8_t* out, uint32_t offset, uint16_t value) {
    out[offset] = static_cast<uint8_t>(value);
    out[offset + 1U] = static_cast<uint8_t>(value >> 8U);
}

void Put32(uint8_t* out, uint32_t offset, uint32_t value) {
    out[offset] = static_cast<uint8_t>(value);
    out[offset + 1U] = static_cast<uint8_t>(value >> 8U);
    out[offset + 2U] = static_cast<uint8_t>(value >> 16U);
    out[offset + 3U] = static_cast<uint8_t>(value >> 24U);
}

void Wave(uint8_t* out, uint32_t id, uint16_t family) {
    memset(out, 0, 52);
    Put16(out, 0, 0xfffe);
    Put16(out, 2, 8);
    Put32(out, 4, 192000);
    Put32(out, 8, 3072000);
    Put16(out, 12, 16);
    Put16(out, 14, 16);
    Put16(out, 16, 34);
    Put16(out, 18, 16);
    Put32(out, 24, id);
    Put16(out, 28, family);
    const uint8_t tail[10] = {0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71};
    memcpy(out + 30, tail, sizeof(tail));
}

void EndpointNames() {
    char16_t output[ssr::kEndpointNameCapacity]{};
    const char16_t custom[] = u"\uacf5\uac04 \uc18c\ud504\ud2b8\ub4dc\ub77c\uc774\ubc84 \uc74c\uc545";
    ssr::EndpointName(1, true, custom, sizeof(custom), output);
    CHECK(memcmp(output, custom, sizeof(custom)) == 0);
    char16_t boundary[33]{};
    for (size_t i = 0; i < 31; ++i) boundary[i] = u'x';
    ssr::EndpointName(2, true, boundary, 64, output);
    CHECK(output[30] == u'x' && output[31] == 0);
    boundary[31] = u'x';
    ssr::EndpointName(2, true, boundary, sizeof(boundary), output);
    CHECK(memcmp(output, ssr::kEndpointNames[2], sizeof(output)) == 0);
    const char16_t empty[] = u"";
    const char16_t embedded[] = {u'a', 0, u'b', 0};
    const char16_t unterminated[] = {u'a', u'b'};
    for (size_t ep = 0; ep < 4; ++ep) {
        ssr::EndpointName<char16_t>(ep, false, nullptr, 0, output);
        CHECK(memcmp(output, ssr::kEndpointNames[ep], sizeof(output)) == 0);
        ssr::EndpointName(ep, false, custom, sizeof(custom), output);
        CHECK(memcmp(output, ssr::kEndpointNames[ep], sizeof(output)) == 0);
        ssr::EndpointName(ep, true, empty, sizeof(empty), output);
        CHECK(memcmp(output, ssr::kEndpointNames[ep], sizeof(output)) == 0);
        ssr::EndpointName(ep, true, embedded, sizeof(embedded), output);
        CHECK(memcmp(output, ssr::kEndpointNames[ep], sizeof(output)) == 0);
        ssr::EndpointName(ep, true, unterminated, sizeof(unterminated), output);
        CHECK(memcmp(output, ssr::kEndpointNames[ep], sizeof(output)) == 0);
        ssr::EndpointName(ep, true, custom, sizeof(custom) - 1, output);
        CHECK(memcmp(output, ssr::kEndpointNames[ep], sizeof(output)) == 0);
    }
}

void AlignmentAndRead() {
    ssr::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    const uint8_t data[3] = {1, 2, 3};
    CHECK(ring.Data(0, 7, 8, data, sizeof(data), nullptr, 0));
    CHECK(ring.FirstRecordBytes() == RecordBytes(3));
    uint8_t out[64]{};
    CHECK(ring.Read(out, RecordBytes(3) - 1U) == 0);
    CHECK(ring.FirstRecordBytes() == RecordBytes(3));
    CHECK(ring.Read(out, sizeof(out)) == RecordBytes(3));
    SSR_RECORD_HEADER header{};
    memcpy(&header, out, sizeof(header));
    CHECK(header.kind == SSR_RECORD_KIND_DATA);
    CHECK(header.len == 3);
    CHECK(header.reserved == 0);
    CHECK(memcmp(out + sizeof(header), data, sizeof(data)) == 0);
    for (uint32_t i = static_cast<uint32_t>(sizeof(header)) + 3U; i < RecordBytes(3); ++i) {
        CHECK(out[i] == 0);
    }
}

void PassthroughStateAndIsolation() {
    ssr::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    CHECK(ring.ConfigureVolume(SSR_ENDPOINT_PASSTHROUGH, 8));
    uint8_t format[52];
    Wave(format, 0x030c, 0x0cea);
    const uint8_t data[3] = {3, 4, 5};
    CHECK(ring.FormatChange(SSR_ENDPOINT_PASSTHROUGH, 1, format, sizeof(format)));
    CHECK(ring.SetVolume(SSR_ENDPOINT_PASSTHROUGH, -1, -20 * 65536, 2));
    CHECK(ring.SetMute(SSR_ENDPOINT_PASSTHROUGH, 1, true, 3));
    CHECK(ring.StreamStart(SSR_ENDPOINT_PASSTHROUGH, 4));
    ring.DiscardQueued();
    CHECK(ring.Data(SSR_ENDPOINT_PASSTHROUGH, 5, 0, data, sizeof(data), nullptr, 0));
    CHECK(ring.StreamStop(SSR_ENDPOINT_PASSTHROUGH, 6));
    uint8_t out[1024];
    const uint32_t bytes = ring.Read(out, sizeof(out));
    const uint16_t kinds[] = {SSR_RECORD_KIND_FORMAT_CHANGE, SSR_RECORD_KIND_VOLUME,
        SSR_RECORD_KIND_STREAM_START, SSR_RECORD_KIND_DATA, SSR_RECORD_KIND_STREAM_STOP};
    uint32_t count = 0;
    for (uint32_t at = 0; at < bytes;) {
        SSR_RECORD_HEADER header{};
        memcpy(&header, out + at, sizeof(header));
        CHECK(header.endpoint == SSR_ENDPOINT_PASSTHROUGH);
        CHECK(count < 5);
        if (count < 5) CHECK(header.kind == kinds[count]);
        if (count < 3) CHECK(header.flags == SSR_RECORD_FLAG_REPEAT);
        if (header.kind == SSR_RECORD_KIND_DATA) {
            CHECK(header.byte_pos == 0);
            CHECK(header.flags == SSR_RECORD_FLAG_DISCONTINUITY);
            CHECK(memcmp(out + at + sizeof(header), data, sizeof(data)) == 0);
        }
        ++count;
        at += RecordBytes(header.len);
    }
    CHECK(count == 5);
    SSR_STATS stats{};
    ring.FillStats(&stats, 1, 0);
    CHECK(stats.endpoints[2].total_bytes == sizeof(data));
    CHECK(stats.endpoints[2].epoch == 1);
    CHECK(stats.endpoints[2].state == 0);
    CHECK(stats.endpoints[0].total_bytes == 0 && stats.endpoints[0].epoch == 0);
    CHECK(stats.endpoints[1].total_bytes == 0 && stats.endpoints[1].epoch == 0);
    CHECK(ring.Endpoint(2).volume.mute_mask == 2);
}

void WrapIntegrity() {
    ssr::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    uint8_t payload[997];
    uint8_t out[2048];
    for (uint32_t round = 0; round < 400; ++round) {
        memset(payload, static_cast<int>(round & 0xffU), sizeof(payload));
        CHECK(ring.Data(static_cast<uint16_t>(round & 1U), round, 0, payload, sizeof(payload), nullptr, 0));
        CHECK(ring.Read(out, sizeof(out)) == RecordBytes(sizeof(payload)));
        SSR_RECORD_HEADER header{};
        memcpy(&header, out, sizeof(header));
        CHECK(header.len == sizeof(payload));
        CHECK(memcmp(out + sizeof(header), payload, sizeof(payload)) == 0);
    }
}

void UrgentAndStats() {
    ssr::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    uint8_t format[52];
    Wave(format, 0x030c, 0x0cea);
    CHECK(!ring.UrgentQueued());
    CHECK(ring.StreamStart(0, 1));
    CHECK(!ring.UrgentQueued());
    CHECK(ring.FormatChange(0, 2, format, sizeof(format)));
    CHECK(ring.UrgentQueued());
    uint8_t out[256];
    CHECK(ring.Read(out, sizeof(out)) != 0);
    CHECK(!ring.UrgentQueued());
    CHECK(ring.StreamStop(0, 3));
    CHECK(ring.UrgentQueued());
    SSR_STATS stats{};
    ring.FillStats(&stats, 4, 17);
    CHECK(stats.struct_size == sizeof(SSR_STATS));
    CHECK(stats.open_count == 4);
    CHECK(stats.max_timer_lateness_us == 17);
    CHECK(stats.endpoints[0].epoch == 1);
    CHECK(stats.endpoints[0].state == 0);
}

void OverrunRecovery() {
    ssr::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    uint8_t format[52];
    Wave(format, 0x030c, 0x0cea);
    CHECK(ring.FormatChange(0, 1, format, sizeof(format)));
    CHECK(ring.FormatChange(1, 2, format, sizeof(format)));
    uint8_t block[4096];
    memset(block, 3, sizeof(block));
    for (uint32_t i = 0; i < 24; ++i) {
        CHECK(ring.Data(static_cast<uint16_t>(i & 1U), 10U + i, 0, block, sizeof(block), nullptr, 0));
    }
    const uint64_t lost0 = ring.Endpoint(0).lost_bytes;
    const uint64_t lost1 = ring.Endpoint(1).lost_bytes;
    CHECK(lost0 > 0);
    CHECK(lost1 > 0);
    CHECK(ring.Data(0, 100, 0, block, 32, nullptr, 0));
    uint8_t all[64U * 1024U];
    const uint32_t got = ring.Read(all, sizeof(all));
    bool saw_overrun = false;
    bool saw_repeat = false;
    bool saw_discontinuous = false;
    uint64_t read_data[SSR_MAX_ENDPOINTS]{};
    for (uint32_t at = 0; at < got;) {
        SSR_RECORD_HEADER header{};
        memcpy(&header, all + at, sizeof(header));
        if (header.kind == SSR_RECORD_KIND_OVERRUN && header.endpoint == 0) {
            saw_overrun = true;
        }
        if (header.kind == SSR_RECORD_KIND_FORMAT_CHANGE && header.endpoint == 0 &&
            header.flags == SSR_RECORD_FLAG_REPEAT) {
            saw_repeat = true;
            CHECK(header.epoch == ring.Endpoint(0).epoch);
        }
        if (header.kind == SSR_RECORD_KIND_DATA) {
            read_data[header.endpoint] += header.len;
            if (header.endpoint == 0 && header.len == 32) {
                saw_discontinuous = (header.flags & SSR_RECORD_FLAG_DISCONTINUITY) != 0;
            }
        }
        at += RecordBytes(header.len);
    }
    CHECK(saw_overrun);
    CHECK(saw_repeat);
    CHECK(saw_discontinuous);
    CHECK(ring.Endpoint(0).total_bytes == read_data[0] + ring.Endpoint(0).lost_bytes);
    CHECK(ring.Endpoint(1).total_bytes == read_data[1] + ring.Endpoint(1).lost_bytes);
    CHECK(ring.Endpoint(0).lost_bytes >= lost0);
    CHECK(ring.Endpoint(1).lost_bytes >= lost1);
}

void StreamStartDoesNotRepeatFormat() {
    ssr::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    uint8_t format[52];
    Wave(format, 0x030c, 0x0cea);
    const uint8_t data[3] = {1, 2, 3};
    CHECK(ring.FormatChange(0, 1, format, sizeof(format)));
    CHECK(ring.StreamStart(0, 2));
    CHECK(ring.Data(0, 3, 4, data, sizeof(data), nullptr, 0));

    uint8_t out[256];
    const uint32_t got = ring.Read(out, sizeof(out));
    uint32_t at = 0;
    SSR_RECORD_HEADER header{};
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == SSR_RECORD_KIND_FORMAT_CHANGE);
    CHECK(header.flags == 0);
    at += RecordBytes(header.len);
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == SSR_RECORD_KIND_STREAM_START);
    at += RecordBytes(header.len);
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == SSR_RECORD_KIND_DATA);
    CHECK(header.flags == SSR_RECORD_FLAG_DISCONTINUITY);
    at += RecordBytes(header.len);
    CHECK(at == got);
}

void DiscardQueuedRestatesCurrentState() {
    ssr::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    uint8_t format[52];
    Wave(format, 0x030c, 0x0cea);
    const uint8_t data[2] = {7, 8};
    CHECK(ring.FormatChange(0, 1, format, sizeof(format)));
    CHECK(ring.StreamStart(0, 2));
    CHECK(ring.Data(0, 3, 0, data, sizeof(data), nullptr, 0));
    const uint64_t lost = ring.Endpoint(0).lost_bytes;
    const uint64_t overruns = ring.Endpoint(0).overruns;
    ring.DiscardQueued(4);
    CHECK(ring.QueuedBytes() == RecordBytes(sizeof(format)) + RecordBytes(0));
    CHECK(ring.Data(0, 5, 0, data, sizeof(data), nullptr, 0));

    uint8_t out[256];
    const uint32_t got = ring.Read(out, sizeof(out), 6);
    uint32_t at = 0;
    SSR_RECORD_HEADER header{};
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == SSR_RECORD_KIND_FORMAT_CHANGE);
    CHECK(header.flags == SSR_RECORD_FLAG_REPEAT);
    CHECK(header.reserved == 0);
    at += RecordBytes(header.len);
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == SSR_RECORD_KIND_STREAM_START);
    CHECK(header.flags == SSR_RECORD_FLAG_REPEAT);
    CHECK(header.reserved == 0);
    at += RecordBytes(header.len);
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == SSR_RECORD_KIND_DATA);
    CHECK(header.flags == SSR_RECORD_FLAG_DISCONTINUITY);
    CHECK(header.reserved == 0);
    at += RecordBytes(header.len);
    CHECK(at == got);
    CHECK(ring.Endpoint(0).lost_bytes == lost);
    CHECK(ring.Endpoint(0).overruns == overruns);
}

void DropBeforeFormatChangeRestatesOldState() {
    ssr::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    uint8_t format[52];
    Wave(format, 0x030c, 0x0cea);
    CHECK(ring.FormatChange(0, 1, format, sizeof(format)));
    uint8_t out[64U * 1024U];
    CHECK(ring.Read(out, sizeof(out)) == RecordBytes(sizeof(format)));

    uint8_t block[4096];
    memset(block, 3, sizeof(block));
    for (uint32_t i = 0; i < 15; ++i) {
        CHECK(ring.Data(0, 10U + i, 0, block, sizeof(block), nullptr, 0));
    }
    CHECK(ring.Data(0, 30, 0, block, 3368, nullptr, 0));
    Wave(format, 0x010c, 0x0cea);
    CHECK(ring.FormatChange(0, 31, format, sizeof(format)));
    CHECK(ring.Endpoint(0).lost_bytes == sizeof(block));

    const uint32_t got = ring.Read(out, sizeof(out));
    uint32_t at = 0;
    SSR_RECORD_HEADER previous[4]{};
    while (at < got) {
        previous[0] = previous[1];
        previous[1] = previous[2];
        previous[2] = previous[3];
        memcpy(&previous[3], out + at, sizeof(previous[3]));
        at += RecordBytes(previous[3].len);
    }
    CHECK(at == got);
    CHECK(previous[0].kind == SSR_RECORD_KIND_OVERRUN);
    CHECK(previous[0].flags == 0);
    CHECK(previous[1].kind == SSR_RECORD_KIND_FORMAT_CHANGE);
    CHECK(previous[1].flags == SSR_RECORD_FLAG_REPEAT);
    CHECK(previous[2].kind == SSR_RECORD_KIND_STREAM_STOP);
    CHECK(previous[2].flags == SSR_RECORD_FLAG_REPEAT);
    CHECK(previous[3].kind == SSR_RECORD_KIND_FORMAT_CHANGE);
    CHECK(previous[3].flags == 0);
}

void StoppedEndpointRestatesAfterOverrun() {
    ssr::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    uint8_t format[52];
    Wave(format, 0x030c, 0x0cea);
    CHECK(ring.FormatChange(0, 1, format, sizeof(format)));
    CHECK(ring.StreamStart(0, 2));
    CHECK(ring.StreamStop(0, 3));
    uint8_t out[512];
    CHECK(ring.Read(out, sizeof(out), 3) != 0);
    CHECK(ring.SourceGap(0, 4, 64));
    CHECK(ring.PrepareRead(5));
    const uint32_t got = ring.Read(out, sizeof(out), 5);
    const uint16_t kinds[] = {SSR_RECORD_KIND_OVERRUN, SSR_RECORD_KIND_FORMAT_CHANGE,
                              SSR_RECORD_KIND_STREAM_STOP};
    uint32_t at = 0;
    for (uint32_t i = 0; i < 3; ++i) {
        SSR_RECORD_HEADER header{};
        memcpy(&header, out + at, sizeof(header));
        CHECK(header.kind == kinds[i]);
        CHECK(header.endpoint == 0);
        CHECK(header.reserved == 0);
        if (i != 0) CHECK(header.flags == SSR_RECORD_FLAG_REPEAT);
        at += RecordBytes(header.len);
    }
    CHECK(at == got);
}

void SourceGapRecovery() {
    ssr::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    uint8_t format[52];
    Wave(format, 0x030c, 0x0cea);
    CHECK(ring.FormatChange(0, 1, format, sizeof(format)));
    CHECK(ring.FormatChange(1, 2, format, sizeof(format)));
    uint8_t out[512];
    CHECK(ring.Read(out, sizeof(out)) != 0);

    const uint8_t other[3] = {4, 5, 6};
    const uint8_t resumed[2] = {7, 8};
    CHECK(!ring.SourceGap(SSR_MAX_ENDPOINTS, 3, 10));
    CHECK(!ring.SourceGap(0, 3, 0));
    CHECK(ring.SourceGap(0, 3, 100));
    CHECK(ring.QueuedBytes() == 0);
    CHECK(ring.Data(1, 4, 0, other, sizeof(other), nullptr, 0));
    CHECK(ring.Data(0, 5, 0, resumed, sizeof(resumed), nullptr, 0));

    const uint32_t got = ring.Read(out, sizeof(out));
    uint32_t at = 0;
    SSR_RECORD_HEADER header{};
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == SSR_RECORD_KIND_DATA);
    CHECK(header.endpoint == 1);
    CHECK(header.flags == 0);
    at += RecordBytes(header.len);
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == SSR_RECORD_KIND_OVERRUN);
    CHECK(header.endpoint == 0);
    SSR_OVERRUN_PAYLOAD overrun{};
    memcpy(&overrun, out + at + sizeof(header), sizeof(overrun));
    CHECK(overrun.lost_bytes == 100);
    CHECK(header.byte_pos == 100);
    at += RecordBytes(header.len);
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == SSR_RECORD_KIND_FORMAT_CHANGE);
    CHECK(header.endpoint == 0);
    CHECK(header.flags == SSR_RECORD_FLAG_REPEAT);
    at += RecordBytes(header.len);
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == SSR_RECORD_KIND_STREAM_STOP);
    CHECK(header.endpoint == 0);
    CHECK(header.flags == SSR_RECORD_FLAG_REPEAT);
    at += RecordBytes(header.len);
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == SSR_RECORD_KIND_DATA);
    CHECK(header.endpoint == 0);
    CHECK(header.flags == SSR_RECORD_FLAG_DISCONTINUITY);
    CHECK(header.byte_pos == 100);
    at += RecordBytes(header.len);
    CHECK(at == got);
    CHECK(ring.Endpoint(0).pos == 102);
    CHECK(ring.Endpoint(0).total_bytes == 102);
    CHECK(ring.Endpoint(0).lost_bytes == 100);
    CHECK(ring.Endpoint(0).pending_lost == 0);
    CHECK(ring.Endpoint(1).lost_bytes == 0);
}

void VolumeStateAndRecovery() {
    ssr::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    CHECK(!ring.ConfigureVolume(SSR_MAX_ENDPOINTS, 2));
    CHECK(!ring.ConfigureVolume(0, 0));
    CHECK(!ring.ConfigureVolume(0, 33));
    CHECK(ring.ConfigureVolume(0, 12));
    CHECK(ring.ConfigureVolume(1, 2));
    CHECK(!ring.ConfigureVolume(0, 2));
    CHECK(!ring.SetVolume(0, 12, 0, 1));
    CHECK(!ring.SetVolume(0, -2, 0, 1));
    CHECK(!ring.SetMute(0, 12, true, 1));
    CHECK(!ring.SetMute(3, -1, true, 1));
    ring.DiscardQueued(10);
    uint8_t out[64U * 1024U];
    uint32_t got = ring.Read(out, sizeof(out));
    CHECK(got == 2 * (RecordBytes(sizeof(SSR_VOLUME_PAYLOAD)) + RecordBytes(0)));
    SSR_RECORD_HEADER h{};
    memcpy(&h, out, sizeof(h));
    CHECK(h.kind == SSR_RECORD_KIND_VOLUME && h.flags == SSR_RECORD_FLAG_REPEAT && h.qpc == 10);
    SSR_VOLUME_PAYLOAD value{};
    memcpy(&value, out + sizeof(h), sizeof(value));
    CHECK(value.channel_count == 12 && value.mute_mask == 0);
    for (int32_t level : value.level_q16) CHECK(level == 0);
    CHECK(ring.SetVolume(0, -1, -20 * 65536, 11));
    CHECK(ring.UrgentQueued());
    CHECK(ssr::ReadGate::ShouldComplete(ring, 4096, 4096, 11, UINT64_MAX));
    CHECK(ring.SetMute(0, -1, true, 12));
    CHECK(ring.SetMute(0, 4, false, 13));
    CHECK(ring.Endpoint(0).volume.mute_mask == (0xfffU & ~(1U << 4U)));
    CHECK(ring.Endpoint(1).volume.mute_mask == 0);
    got = ring.Read(out, sizeof(out));
    CHECK(got == 3 * RecordBytes(sizeof(SSR_VOLUME_PAYLOAD)));
    CHECK(!ring.UrgentQueued());
    for (uint32_t at = 0, i = 0; at < got; at += RecordBytes(h.len), ++i) {
        memcpy(&h, out + at, sizeof(h));
        CHECK(h.kind == SSR_RECORD_KIND_VOLUME && h.flags == 0 && h.qpc == 11 + i);
    }
    CHECK(ring.SetVolume(0, -1, -20 * 65536, 14));
    CHECK(ring.QueuedBytes() == 0); // no changed value, no event
    CHECK(ring.SetVolume(0, 0, INT32_MIN, 15));
    CHECK(ring.Endpoint(0).volume.level_q16[0] == SSR_VOLUME_MIN_Q16);
    CHECK(ring.SetVolume(0, 1, INT32_MAX, 15));
    CHECK(ring.Endpoint(0).volume.level_q16[1] == 0);
    CHECK(ring.SetVolume(0, 2, -16384, 15));
    CHECK(ring.Endpoint(0).volume.level_q16[2] == -32768);
    uint8_t format[52]; Wave(format, 0x030c, 0x0cea);
    CHECK(ring.FormatChange(0, 16, format, sizeof(format)));
    CHECK(ring.StreamStart(0, 17));
    ring.DiscardQueued(18);
    got = ring.Read(out, sizeof(out));
    const uint16_t kinds[] = {SSR_RECORD_KIND_FORMAT_CHANGE, SSR_RECORD_KIND_VOLUME,
        SSR_RECORD_KIND_STREAM_START, SSR_RECORD_KIND_VOLUME, SSR_RECORD_KIND_STREAM_STOP};
    uint32_t at = 0;
    for (uint16_t kind : kinds) {
        memcpy(&h, out + at, sizeof(h));
        CHECK(h.kind == kind && h.flags == SSR_RECORD_FLAG_REPEAT);
        at += RecordBytes(h.len);
    }
    CHECK(at == got);
    CHECK(ring.SourceGap(0, 19, 100));
    got = ring.Read(out, sizeof(out), 20);
    const uint16_t recovery[] = {SSR_RECORD_KIND_OVERRUN, SSR_RECORD_KIND_FORMAT_CHANGE,
        SSR_RECORD_KIND_VOLUME, SSR_RECORD_KIND_STREAM_START};
    at = 0;
    for (uint16_t kind : recovery) {
        memcpy(&h, out + at, sizeof(h));
        CHECK(h.kind == kind && h.qpc == 20);
        if (kind != SSR_RECORD_KIND_OVERRUN) CHECK(h.flags == SSR_RECORD_FLAG_REPEAT);
        if (kind == SSR_RECORD_KIND_VOLUME) {
            memcpy(&value, out + at + sizeof(h), sizeof(value));
            CHECK(memcmp(&value, &ring.Endpoint(0).volume, sizeof(value)) == 0);
        }
        at += RecordBytes(h.len);
    }
    CHECK(at == got);
    // Flood one endpoint until the other endpoint's final volume is overwritten.
    CHECK(ring.SetVolume(1, -1, -30 * 65536, 21));
    uint8_t block[4096]{};
    for (unsigned i = 0; i < 40; ++i) CHECK(ring.Data(0, 22 + i, 0, block, sizeof(block), nullptr, 0));
    got = ring.Read(out, sizeof(out), 70);
    bool recovered = false;
    for (at = 0; at < got; at += RecordBytes(h.len)) {
        memcpy(&h, out + at, sizeof(h));
        if (h.kind == SSR_RECORD_KIND_VOLUME && h.endpoint == 1) {
            memcpy(&value, out + at + sizeof(h), sizeof(value));
            CHECK(value.level_q16[0] == -30 * 65536 && value.level_q16[1] == -30 * 65536);
            CHECK(h.flags == SSR_RECORD_FLAG_REPEAT);
            recovered = true;
        }
    }
    CHECK(recovered);
}

void EndpointBounds() {
    ssr::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    const uint8_t data[1] = {1};
    for (uint16_t ep = 2; ep < SSR_MAX_ENDPOINTS; ++ep) {
        CHECK(ring.StreamStart(ep, ep));
        CHECK(ring.Data(ep, ep, ep, data, sizeof(data), nullptr, 0));
        CHECK(ring.Endpoint(ep).total_bytes == 1);
    }
    CHECK(!ring.StreamStart(SSR_MAX_ENDPOINTS, 0));
    CHECK(!ring.Data(SSR_MAX_ENDPOINTS, 0, 0, data, sizeof(data), nullptr, 0));
    CHECK(ring.Endpoint(SSR_MAX_ENDPOINTS).total_bytes == 0);
}

void ReadRules() {
    ssr::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    CHECK(ssr::ReadGate::ShouldComplete(ring, 0, 64, 1, 9));
    CHECK(!ssr::ReadGate::ShouldComplete(ring, 10, 64, 1, 9));
    uint8_t data[16]{};
    CHECK(ring.Data(0, 0, 0, data, sizeof(data), nullptr, 0));
    CHECK(ssr::ReadGate::ShouldComplete(ring, 1000, 48, 1, 9));
    ring.DiscardQueued();
    CHECK(ssr::ReadGate::ShouldComplete(ring, 10, 64, 9, 9));
    CHECK(ssr::ReadGate::Deadline(UINT64_MAX - 1U, 10000000, 1) == UINT64_MAX);
    CHECK(ssr::ReadGate::Deadline(5, 10000000, 2) == 20005);
}

void PayloadAndReadLimits() {
    alignas(8) static uint8_t big[1024U * 1024U];
    static uint8_t data[ssr::kMaxDataPayload + 8U];
    static uint8_t out[ssr::kReadMaxBytes + 4096U];
    ssr::RecordRing ring;
    CHECK(ring.Init(big, sizeof(big)));
    CHECK(!ring.Data(0, 1, 0, data, ssr::kMaxDataPayload + 1U, nullptr, 0));
    CHECK(!ring.Data(0, 1, 0, data, ssr::kMaxDataPayload, data, 1));
    CHECK(ring.QueuedBytes() == 0);
    CHECK(ring.Data(0, 1, 0, data, ssr::kMaxDataPayload - 8U, data, 8));
    const uint32_t largest = ring.FirstRecordBytes();
    CHECK(largest == sizeof(SSR_RECORD_HEADER) + ssr::kMaxDataPayload);
    CHECK(largest <= ssr::ReadGate::Capacity(UINT32_MAX));
    CHECK(ssr::ReadGate::Capacity(100) == 100);
    CHECK(ssr::ReadGate::Capacity(ssr::kReadMaxBytes + 1U) == ssr::kReadMaxBytes);
    // Five largest records queued: one READ with a huge buffer takes three,
    // never more than kReadMaxBytes.
    for (uint32_t i = 0; i < 4U; ++i) CHECK(ring.Data(0, 2U + i, 0, data, ssr::kMaxDataPayload, nullptr, 0));
    const uint32_t capacity = ssr::ReadGate::Capacity(sizeof(out));
    CHECK(ssr::ReadGate::ShouldComplete(ring, UINT32_MAX, capacity, 0, UINT64_MAX));
    const uint32_t moved = ring.Read(out, capacity);
    CHECK(moved == 3U * largest);
    CHECK(moved <= ssr::kReadMaxBytes);
    CHECK(ring.QueuedBytes() == 2U * largest);
    CHECK(!ssr::ReadGate::ShouldComplete(ring, UINT32_MAX, capacity, 0, UINT64_MAX));
}

void ClockRules() {
    ssr::DmaClock clock;
    clock.Start(0, 10000000, 192000, 4);
    CHECK(clock.Advance(10000000) == 192000);
    CHECK(clock.SetTrim(3000 * 65536) == 2000 * 65536);
    clock.Start(0, 10000000, 192000, 4);
    CHECK(clock.SetTrim(2000 * 65536) == 2000 * 65536);
    CHECK(clock.Advance(10000000) == 192384);
    CHECK(clock.SetTrim(-3000 * 65536) == -2000 * 65536);
    clock.Start(0, 10000000, 192000, 4);
    uint64_t total = 0;
    for (uint64_t i = 1; i <= 1000000; ++i) {
        total += clock.Advance(i * 10000U);
    }
    CHECK(total == 192000000);
    CHECK(clock.Advance(1) == 0);

    // Audit regression: the former 64-bit product overflow returned zero and
    // permanently discarded this interval.
    clock.Start(0, 10000000, 176400, 4);
    CHECK(clock.SetTrim(1) == 1);
    CHECK(clock.Advance(1000001) == 17640);
    clock.Start(0, (UINT64_C(1) << 48U) + 1U, 192000, 4);
    CHECK(clock.Advance((UINT64_C(1) << 48U) + 1U) == 0);
}

#if defined(__SIZEOF_INT128__)
__extension__ typedef unsigned __int128 NativeU128;

uint64_t Random64(uint64_t* state) {
    uint64_t value = *state;
    value ^= value << 13U;
    value ^= value >> 7U;
    value ^= value << 17U;
    *state = value;
    return value;
}

NativeU128 Native(ssr::U128 value) {
    return (static_cast<NativeU128>(value.high) << 64U) | value.low;
}

void U128Rules() {
    uint64_t random = UINT64_C(0x9e3779b97f4a7c15);
    for (uint32_t i = 0; i < 10000U; ++i) {
        const uint64_t a = Random64(&random);
        const uint64_t b = Random64(&random);
        CHECK(Native(ssr::U128Multiply(a, b)) ==
              static_cast<NativeU128>(a) * static_cast<NativeU128>(b));
        ssr::U128 numerator{Random64(&random), Random64(&random)};
        ssr::U128 denominator{Random64(&random), Random64(&random)};
        if (denominator.high == 0 && denominator.low == 0) denominator.low = 1;
        ssr::U128 remainder{};
        const ssr::U128 quotient = ssr::U128Divide(numerator, denominator, &remainder);
        CHECK(Native(quotient) == Native(numerator) / Native(denominator));
        CHECK(Native(remainder) == Native(numerator) % Native(denominator));
    }
}

uint64_t ReferenceMulDivCeil(uint64_t a, uint64_t b, uint64_t c) {
    if (c == 0) return UINT64_MAX;
    const NativeU128 product = static_cast<NativeU128>(a) * b;
    NativeU128 quotient = product / c;
    if (product % c != 0) ++quotient;
    return quotient > UINT64_MAX ? UINT64_MAX : static_cast<uint64_t>(quotient);
}

void MulDivRules() {
    const uint64_t values[] = {0, 1, 2, UINT64_MAX - 1U, UINT64_MAX};
    for (uint64_t a : values) for (uint64_t b : values) for (uint64_t c : values)
        CHECK(ssr::MulDivCeil(a, b, c) == ReferenceMulDivCeil(a, b, c));
    uint64_t random = UINT64_C(0xd1b54a32d192ed03);
    for (uint32_t i = 0; i < 10000U; ++i) {
        const uint64_t a = Random64(&random);
        const uint64_t b = Random64(&random);
        const uint64_t c = Random64(&random);
        CHECK(ssr::MulDivCeil(a, b, c) == ReferenceMulDivCeil(a, b, c));
    }
}

void RandomClockRules() {
    const uint64_t frequencies[] = {10000000, 24000000, 3000000000, UINT64_C(1) << 48U};
    const uint32_t rates[] = {1, 192000, 176400, 3072000, UINT32_MAX};
    const uint32_t aligns[] = {1, 4, 8, 32};
    const int32_t trims[] = {-SSR_TRIM_LIMIT_PPM * 65536, 0, 1,
                             SSR_TRIM_LIMIT_PPM * 65536};
    constexpr uint64_t trimDenominator = UINT64_C(1000000) * 65536U;
    uint64_t random = UINT64_C(0x94d049bb133111eb);
    for (uint32_t scenario = 0; scenario < 80U; ++scenario) {
        const uint64_t frequency = frequencies[scenario % 4U];
        const uint32_t rate = rates[(scenario / 4U) % 5U];
        const uint32_t align = aligns[(scenario / 20U) % 4U];
        ssr::DmaClock clock;
        clock.Start(0, frequency, rate, align);
        uint64_t now = 0;
        uint64_t returned = 0;
        uint64_t referenceReturned = 0;
        uint64_t referenceUnaligned = 0;
        NativeU128 remainder = 0;
        const NativeU128 denominator = static_cast<NativeU128>(frequency) * trimDenominator;
        for (uint32_t call = 0; call < 64U; ++call) {
            int32_t trim = trims[(call + scenario) % 4U];
            if ((call & 7U) == 7U) {
                const uint32_t span = static_cast<uint32_t>(SSR_TRIM_LIMIT_PPM * 2 + 1);
                trim = static_cast<int32_t>(Random64(&random) % span) * 65536 -
                       SSR_TRIM_LIMIT_PPM * 65536;
            }
            CHECK(clock.SetTrim(trim) == trim);
            uint64_t ticks;
            switch (call & 7U) {
                case 0: ticks = 0; break;
                case 1: ticks = 1; break;
                case 2: ticks = frequency; break;
                case 3: ticks = frequency * 3U + 1U; break;
                default: ticks = Random64(&random) % (frequency * 5U + 1U); break;
            }
            now += ticks;
            returned += clock.Advance(now);
            const uint64_t rateNumerator = trim >= 0
                ? trimDenominator + static_cast<uint32_t>(trim)
                : trimDenominator - static_cast<uint32_t>(-static_cast<int64_t>(trim));
            const NativeU128 numerator = static_cast<NativeU128>(ticks) * rate * rateNumerator + remainder;
            const uint64_t bytes = static_cast<uint64_t>(numerator / denominator);
            remainder = numerator % denominator;
            referenceUnaligned += bytes;
            const uint64_t aligned = (referenceUnaligned / align) * align;
            referenceUnaligned -= aligned;
            referenceReturned += aligned;
            CHECK(returned == referenceReturned);
        }
        CHECK(referenceUnaligned < align);
    }
}
#else
void U128Rules() {}
void MulDivRules() {}
void RandomClockRules() {}
#endif

void Formats() {
    uint8_t wave[52];
    const uint32_t ids[] = {0x000a, 0x010a, 0x000b, 0x010b, 0x030b, 0x000c, 0x010c, 0x030c, 0x070c};
    const uint64_t bits[] = {SSR_FORMAT_EAC3, SSR_FORMAT_EAC3_JOC, SSR_FORMAT_DTS_HD,
        SSR_FORMAT_DTS_HD, SSR_FORMAT_DTS_HD, SSR_FORMAT_TRUEHD, SSR_FORMAT_MAT20,
        SSR_FORMAT_MAT21, SSR_FORMAT_MAT21};
    for (uint32_t i = 0; i < sizeof(ids) / sizeof(ids[0]); ++i) {
        Wave(wave, ids[i], 0x0cea);
        CHECK(ssr::CodecBit(wave, sizeof(wave)) == bits[i]);
        CHECK(ssr::FormatAllowed(SSR_ENDPOINT_FORMATS{bits[i], {}}, wave, sizeof(wave)));
    }
    CHECK(ssr::CodecBit(wave, 17) == 0);
    Put16(wave, 16, 100);
    CHECK(ssr::CodecBit(wave, sizeof(wave)) == 0);
    uint8_t plain[18]{};
    Put16(plain, 0, 0x0092);
    CHECK(ssr::CodecBit(plain, sizeof(plain)) == SSR_FORMAT_AC3);
    Put16(plain, 0, 0x0008);
    CHECK(ssr::CodecBit(plain, sizeof(plain)) == SSR_FORMAT_DTS);
}

// Overwriting an endpoint that never had a format leaves no format to
// restate; that must not leave a recovery request behind that blocks READ.
void FormatlessOverwriteKeepsReading() {
    ssr::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    uint8_t payload[1024]{};
    for (uint64_t i = 0; i < 80; ++i) CHECK(ring.Data(0, i, i, payload, sizeof(payload), nullptr, 0));
    CHECK(ring.Endpoint(0).lost_bytes != 0);
    CHECK(ring.PrepareRead(100));
    uint8_t out[4096];
    CHECK(ring.Read(out, sizeof(out), 100) != 0);
}

void Volume32() {
    ssr::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    CHECK(ring.ConfigureVolume(0, 32));
    CHECK(ring.SetMute(0, -1, true, 1));
    CHECK(ring.Endpoint(0).volume.mute_mask == UINT32_MAX);
    CHECK(ring.SetMute(0, 31, false, 2));
    CHECK(ring.Endpoint(0).volume.mute_mask == 0x7fffffff);
    CHECK(ring.SetVolume(0, 31, -65536, 3));
    CHECK(ring.Endpoint(0).volume.level_q16[31] == -65536);
}

struct IntersectionRule {
    uint32_t rate;
    uint16_t max_channels;
    uint16_t bits;
    bool floating;
    bool direct_only;
};

bool IntersectionAccept(const ssr::PcmCandidate& value, void* opaque) {
    const auto& rule = *static_cast<IntersectionRule*>(opaque);
    return value.rate == rule.rate && value.channels <= rule.max_channels &&
           value.bits == rule.bits && value.floating == rule.floating &&
           (!rule.direct_only || value.channel_mask == 0);
}

void CandidateWave(uint8_t* wave, const ssr::PcmCandidate& value) {
    Put16(wave, 0, 0xfffe);
    Put16(wave, 2, value.channels);
    Put32(wave, 4, value.rate);
    const uint16_t align = static_cast<uint16_t>(value.channels * (value.bits / 8));
    Put32(wave, 8, value.rate * align);
    Put16(wave, 12, align);
    Put16(wave, 14, value.bits);
    Put16(wave, 16, 22);
    Put16(wave, 18, value.valid_bits);
    Put32(wave, 20, value.channel_mask);
    Put32(wave, 24, value.floating ? 3 : 1);
    Put16(wave, 28, 0);
    const uint8_t tail[10] = {0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71};
    memcpy(wave + 30, tail, sizeof(tail));
}

bool PcmIntersectionAllowed(const ssr::PcmCandidate& value, void* opaque) {
    const auto& enabled = *static_cast<SSR_PCM_FORMATS*>(opaque);
    uint8_t wave[40]{};
    CandidateWave(wave, value);
    CHECK(ssr::PcmCandidateAllowed(enabled, value));
    return ssr::PcmAllowed(enabled, wave, sizeof(wave));
}

void PcmIntersectionOrder() {
    const ssr::PcmCandidate oem{8, 48000, 0x63f, 32, 32, true};
    ssr::PcmCandidate result{};
    IntersectionRule rule{96000, 32, 32, true, false};
    CHECK(ssr::ChoosePcmIntersection(SSR_DEFAULT_PCM, oem, IntersectionAccept, &rule, &result));
    CHECK(result.rate == 96000 && result.channels == 8 && result.channel_mask == 0x63f && result.floating);
    rule = {96000, 2, 32, true, false};
    CHECK(ssr::ChoosePcmIntersection(SSR_DEFAULT_PCM, oem, IntersectionAccept, &rule, &result));
    CHECK(result.channels == 2 && result.channel_mask == 3);
    rule = {44100, 32, 16, false, false};
    CHECK(ssr::ChoosePcmIntersection(SSR_DEFAULT_PCM, oem, IntersectionAccept, &rule, &result));
    CHECK(result.rate == 44100 && result.channels == 8 && result.channel_mask == 0x63f && result.bits == 16);
    auto direct = SSR_DEFAULT_PCM; direct.max_masked_channels = 0;
    rule = {48000, 32, 32, true, true};
    CHECK(ssr::ChoosePcmIntersection(direct, oem, IntersectionAccept, &rule, &result));
    CHECK(result.channels == 2 && result.channel_mask == 0);
    rule = {32000, 32, 32, true, false};
    CHECK(!ssr::ChoosePcmIntersection(SSR_DEFAULT_PCM, oem, IntersectionAccept, &rule, &result));

    auto enabled = SSR_DEFAULT_PCM;
    enabled.rates = 1U;
    CHECK(ssr::ChoosePcmIntersection(enabled, oem, PcmIntersectionAllowed, &enabled, &result));
    CHECK(result.rate == 44100);
    enabled = SSR_DEFAULT_PCM;
    enabled.shapes = SSR_PCM_SHAPE_INT16;
    CHECK(ssr::ChoosePcmIntersection(enabled, oem, PcmIntersectionAllowed, &enabled, &result));
    CHECK(result.bits == 16 && result.valid_bits == 16 && !result.floating);
    enabled = {};
    CHECK(!ssr::ChoosePcmIntersection(enabled, oem, PcmIntersectionAllowed, &enabled, &result));
    enabled = SSR_DEFAULT_PCM;
    enabled.max_masked_channels = 2;
    enabled.direct_channels = 1U << 1U;
    CHECK(ssr::ChoosePcmIntersection(enabled, oem, PcmIntersectionAllowed, &enabled, &result));
    CHECK(result.channels == 2);
}

struct TestShape { uint16_t bits, valid; bool floating; uint32_t bit; };
constexpr TestShape kTestShapes[] = {
    {16, 16, false, SSR_PCM_SHAPE_INT16}, {24, 24, false, SSR_PCM_SHAPE_INT24},
    {32, 24, false, SSR_PCM_SHAPE_INT24_IN_32}, {32, 32, false, SSR_PCM_SHAPE_INT32},
    {32, 32, true, SSR_PCM_SHAPE_FLOAT32},
    {8, 8, false, 0}, {16, 15, false, 0}, {24, 20, false, 0},
    {32, 0, false, 0}, {32, 16, false, 0}, {64, 64, false, 0},
    {16, 16, true, 0}, {24, 24, true, 0}, {32, 24, true, 0}, {64, 64, true, 0},
};

void CheckCandidate(const SSR_PCM_FORMATS& enabled, const ssr::PcmCandidate& candidate, bool expected) {
    uint8_t unaligned[41]{};
    auto* wave = unaligned + 1; // Exercise unaligned WAVE and GUID parsing.
    CandidateWave(wave, candidate);
    CHECK(ssr::PcmCandidateAllowed(enabled, candidate) == expected);
    CHECK(ssr::PcmAllowed(enabled, wave, 40) == expected);
    CHECK(ssr::FormatAccepted({0, enabled}, wave + 24, wave, 40) == expected);
}

void PcmCandidateAgreementGrid() {
    const uint32_t masks[] = {0, 4, 3, 0x33, 0x3f, 0x60f, 0xff, 0x63f,
        0x2d63f, SSR_PCM_SPEAKER_BITS, 0x40000, UINT32_MAX};
    const uint32_t outside[] = {0, 1, 32000, 47999, 48001, 192001, UINT32_MAX};
    const uint32_t limits[] = {0, 2, 8, 18};
    for (uint16_t channels = 1; channels <= 33; ++channels) for (uint32_t mask : masks) {
        uint32_t count = 0;
        for (uint32_t m = mask; m; m &= m - 1U) ++count;
        for (uint32_t limit : limits) {
            auto enabled = SSR_DEFAULT_PCM; enabled.max_masked_channels = limit;
            const bool channelOk = channels <= SSR_MAX_PCM_CHANNELS && (!mask ||
                (!(mask & ~static_cast<uint32_t>(SSR_PCM_SPEAKER_BITS)) && count == channels && channels <= limit));
            for (const auto& shape : kTestShapes) {
                for (uint32_t rate : SSR_PCM_RATES)
                    CheckCandidate(enabled, {channels, rate, mask, shape.bits, shape.valid, shape.floating}, channelOk && shape.bit != 0);
                for (uint32_t rate : outside)
                    CheckCandidate(enabled, {channels, rate, mask, shape.bits, shape.valid, shape.floating}, false);
            }
        }
    }
}

void PcmMaskBits() {
    for (uint32_t bit = 0; bit < 32; ++bit) {
        auto enabled = SSR_DEFAULT_PCM;
        enabled.rates = UINT32_C(1) << bit;
        for (uint32_t r = 0; r < SSR_PCM_RATE_COUNT; ++r)
            CheckCandidate(enabled, {2, SSR_PCM_RATES[r], 3, 16, 16, false}, r == bit);
        CHECK(ssr::PcmTypeEnabled(enabled, false) == (bit < SSR_PCM_RATE_COUNT));
        enabled = SSR_DEFAULT_PCM; enabled.shapes = UINT32_C(1) << bit;
        for (const auto& shape : kTestShapes)
            CheckCandidate(enabled, {2, SSR_PCM_RATES[0], 3, shape.bits, shape.valid, shape.floating},
                (shape.bit & enabled.shapes) != 0);
        enabled = SSR_DEFAULT_PCM; enabled.direct_channels = UINT32_C(1) << bit;
        for (uint16_t c = 1; c <= 33; ++c)
            CheckCandidate(enabled, {c, SSR_PCM_RATES[0], 0, 16, 16, false}, c == bit + 1U);
    }
}

void CarrierWave(uint8_t* wave, const ssr::CodecCarrier& carrier) {
    CandidateWave(wave, {carrier.channels, carrier.rate, carrier.channel_mask,
        carrier.bits, carrier.valid_bits, false});
    Put32(wave, 24, carrier.subtype_id);
    Put16(wave, 28, carrier.subtype_family);
}

void CodecCarrierAcceptance() {
    for (const auto& carrier : ssr::kCodecCarriers) {
        uint8_t wave[40]{}; CarrierWave(wave, carrier);
        const SSR_ENDPOINT_FORMATS enabled{carrier.bit, {}};
        CHECK(ssr::FormatAccepted(enabled, wave + 24, wave, sizeof(wave)));
        CHECK(!ssr::FormatAccepted({SSR_COMPILED_CODECS & ~carrier.bit, {}}, wave + 24, wave, sizeof(wave)));
        CHECK(ssr::CodecBit(wave, sizeof(wave)) == carrier.bit);
        Put32(wave, 20, 0);
        CHECK(ssr::FormatAccepted(enabled, wave + 24, wave, sizeof(wave)));
        // Rebuild between mutations. These values cannot describe another carrier.
        for (uint32_t mutation = 0; mutation < 8; ++mutation) {
            CarrierWave(wave, carrier);
            switch (mutation) {
                case 0: Put32(wave, 24, 0xffffffff); break;
                case 1: Put16(wave, 2, 3); Put16(wave, 12, 6); Put32(wave, 8, carrier.rate * 6U); break;
                case 2: Put32(wave, 4, carrier.rate + 1U); Put32(wave, 8, (carrier.rate + 1U) * carrier.channels * 2U); break;
                case 3: Put16(wave, 14, 24); Put16(wave, 18, 24); Put16(wave, 12, static_cast<uint16_t>(carrier.channels * 3U)); Put32(wave, 8, carrier.rate * carrier.channels * 3U); break;
                case 4: Put16(wave, 18, 15); break;
                case 5: Put32(wave, 20, 1); break;
                case 6: Put16(wave, 12, 1); break;
                default: Put32(wave, 8, 1); break;
            }
            CHECK(!ssr::FormatAccepted(enabled, wave + 24, wave, sizeof(wave)));
        }
    }
}

// Pinned independently of kCodecCarriers: codec bits as numbers, the
// ksmedia.h subtypes and KSAUDIO_SPEAKER masks the driver advertised before
// the table moved into the core.
void CodecCarrierGolden() {
    const ssr::CodecCarrier golden[] = {
        {0x002, 0x0092, 0x0000, 2,  44100, 0x60f, 16, 16},  // AC-3, 5.1 surround
        {0x002, 0x0092, 0x0000, 2,  48000, 0x60f, 16, 16},
        {0x010, 0x000c, 0x0cea, 8, 192000, 0x0ff, 16, 16},  // TrueHD (MLP), 7.1
        {0x010, 0x000c, 0x0cea, 8, 192000, 0x63f, 16, 16},  // TrueHD, 7.1 surround
        {0x080, 0x0008, 0x0000, 2,  48000, 0x60f, 16, 16},  // DTS
        {0x100, 0x000b, 0x0cea, 8, 192000, 0x0ff, 16, 16},  // DTS-HD
        {0x100, 0x010b, 0x0cea, 8, 192000, 0x0ff, 16, 16},  // DTS:X E1
        {0x100, 0x030b, 0x0cea, 8, 192000, 0x0ff, 16, 16},  // DTS:X E2
        {0x020, 0x010c, 0x0cea, 8, 192000, 0x0ff, 16, 16},  // MAT 2.0
        {0x040, 0x030c, 0x0cea, 8, 192000, 0x0ff, 16, 16},  // MAT 2.1
        {0x040, 0x070c, 0x0cea, 8, 192000, 0x0ff, 16, 16},  // MAT 2.1 (PROFILE4 alias)
        {0x004, 0x000a, 0x0cea, 2, 192000, 0x60f, 16, 16},  // E-AC-3
        {0x008, 0x010a, 0x0cea, 2, 192000, 0x0ff, 16, 16},  // E-AC-3 JOC
    };
    static_assert(sizeof(golden) == sizeof(ssr::kCodecCarriers), "carrier count");
    for (size_t i = 0; i < sizeof(golden) / sizeof(golden[0]); ++i) {
        const auto& a = golden[i];
        const auto& b = ssr::kCodecCarriers[i];
        CHECK(a.bit == b.bit && a.subtype_id == b.subtype_id && a.subtype_family == b.subtype_family &&
              a.channels == b.channels && a.rate == b.rate && a.channel_mask == b.channel_mask &&
              a.bits == b.bits && a.valid_bits == b.valid_bits);
        uint8_t wave[40]{}; CarrierWave(wave, a);
        CHECK(ssr::CodecBit(wave, sizeof(wave)) == a.bit);
        CHECK(ssr::FormatAccepted({a.bit, {}}, wave + 24, wave, sizeof(wave)));
    }
}

// Each check of FormatAccepted on its own, with every other field valid.
void FormatAcceptedSingleChecks() {
    const SSR_ENDPOINT_FORMATS all{SSR_COMPILED_CODECS, SSR_COMPILED_PCM};
    // Extensible with a cbSize too small for WAVEFORMATEXTENSIBLE, although
    // the buffer holds a valid one.
    const uint16_t shortSizes[] = {0, 21};
    for (uint16_t cb : shortSizes) {
        uint8_t wave[40]{}; CandidateWave(wave, {2, SSR_PCM_RATES[1], 3, 16, 16, false});
        CHECK(ssr::FormatAccepted(all, wave + 24, wave, sizeof(wave)));
        Put16(wave, 16, cb);
        CHECK(!ssr::FormatAccepted(all, wave + 24, wave, sizeof(wave)));
    }
    // A plain WAVEFORMATEX must agree with the KS subtype too.
    {
        uint8_t wave[40]{}; CandidateWave(wave, {2, SSR_PCM_RATES[1], 3, 16, 16, false});
        Put16(wave, 0, 1); Put16(wave, 16, 0);
        uint8_t pcm[16]; memcpy(pcm, wave + 24, sizeof(pcm));
        uint8_t floating[16]; memcpy(floating, pcm, sizeof(floating)); Put32(floating, 0, 3);
        CHECK(ssr::FormatAccepted(all, pcm, wave, 18));
        CHECK(!ssr::FormatAccepted(all, floating, wave, 18));
    }
    // A carrier's container width on its own (valid bits unchanged).
    for (const auto& carrier : ssr::kCodecCarriers) {
        uint8_t wave[40]{}; CarrierWave(wave, carrier);
        Put16(wave, 14, 24);
        Put16(wave, 12, static_cast<uint16_t>(carrier.channels * 3U));
        Put32(wave, 8, carrier.rate * carrier.channels * 3U);
        CHECK(!ssr::FormatAccepted({carrier.bit, {}}, wave + 24, wave, sizeof(wave)));
    }
    // The ring keeps formats up to kMaxFormatBytes; a longer declared format
    // is refused even when the buffer holds it.
    {
        uint8_t wave[ssr::kMaxFormatBytes + 1]{};
        CandidateWave(wave, {2, SSR_PCM_RATES[1], 3, 16, 16, false});
        Put16(wave, 16, static_cast<uint16_t>(ssr::kMaxFormatBytes - 18U));
        CHECK(ssr::FormatAccepted(all, wave + 24, wave, sizeof(wave)));
        Put16(wave, 16, static_cast<uint16_t>(ssr::kMaxFormatBytes - 17U));
        CHECK(!ssr::FormatAccepted(all, wave + 24, wave, sizeof(wave)));
    }
}

void FormatSubtypeValidation() {
    const SSR_ENDPOINT_FORMATS all{SSR_COMPILED_CODECS, SSR_COMPILED_PCM};
    uint8_t wave[40]{}; CandidateWave(wave, {2, SSR_PCM_RATES[1], 3, 32, 32, true});
    uint8_t guid[16]; memcpy(guid, wave + 24, sizeof(guid));
    Put32(guid, 0, 1); // Both subtypes are supported, but KS and WAVE must agree.
    CHECK(!ssr::FormatAccepted(all, guid, wave, sizeof(wave)));
    Put32(guid, 0, 3);
    for (uint32_t i = 0; i < sizeof(guid); ++i) {
        guid[i] ^= 0x80;
        CHECK(!ssr::FormatAccepted(all, guid, wave, sizeof(wave)));
        wave[24 + i] ^= 0x80;
        CHECK(!ssr::FormatAccepted(all, guid, wave, sizeof(wave)));
        guid[i] ^= 0x80; wave[24 + i] ^= 0x80;
    }
    for (uint32_t n = 0; n < sizeof(wave); ++n) CHECK(!ssr::FormatAccepted(all, guid, wave, n));
    CHECK(!ssr::FormatAccepted(all, nullptr, wave, sizeof(wave)));
    CHECK(!ssr::FormatAccepted(all, guid, nullptr, sizeof(wave)));
    Put16(wave, 16, UINT16_MAX); CHECK(!ssr::FormatAccepted(all, guid, wave, sizeof(wave)));
    const uint16_t tags[] = {1, 3, 0x92, 8};
    for (uint16_t tag : tags) {
        if (tag == 0x92 || tag == 8) CarrierWave(wave, ssr::kCodecCarriers[tag == 8 ? 4 : 1]);
        else CandidateWave(wave, {2, SSR_PCM_RATES[1], 3, 32, 32, tag == 3});
        Put16(wave, 0, tag); Put16(wave, 16, 0);
        CHECK(ssr::FormatAccepted(all, wave + 24, wave, 18) == (tag != 8));
        if (tag == 8) CHECK(ssr::CodecBit(wave, 18) == SSR_FORMAT_DTS);
    }
    CandidateWave(wave, {32, UINT32_MAX, 0, 32, 32, true});
    CHECK(!ssr::FormatAccepted(all, wave + 24, wave, sizeof(wave)));
}

void FormatTypeRules() {
    const SSR_ENDPOINT_FORMATS all{SSR_COMPILED_CODECS, SSR_COMPILED_PCM};
    uint8_t wave[40]{};
    const bool types[] = {false, true};
    for (bool floating : types) {
        CandidateWave(wave, {2, SSR_PCM_RATES[1], 3, 32, 32, floating});
        CHECK(ssr::FormatTypeAccepted(all, wave + 24));
        CHECK(!ssr::FormatTypeAccepted({}, wave + 24));
        auto enabled = all; enabled.pcm.rates = UINT32_C(1) << 31U;
        CHECK(!ssr::FormatTypeAccepted(enabled, wave + 24));
        enabled = all; enabled.pcm.shapes = UINT32_C(1) << 31U;
        CHECK(!ssr::FormatTypeAccepted(enabled, wave + 24));
        enabled = all; enabled.pcm.max_masked_channels = 0; enabled.pcm.direct_channels = 0;
        CHECK(!ssr::FormatTypeAccepted(enabled, wave + 24));
    }
    for (const auto& carrier : ssr::kCodecCarriers) {
        CarrierWave(wave, carrier);
        CHECK(ssr::FormatTypeAccepted({carrier.bit, {}}, wave + 24));
        CHECK(!ssr::FormatTypeAccepted({all.codecs & ~carrier.bit, all.pcm}, wave + 24));
        for (uint32_t i = 24; i < 40; ++i) {
            wave[i] ^= 0x80;
            CHECK(!ssr::FormatTypeAccepted(all, wave + 24));
            wave[i] ^= 0x80;
        }
    }
    Put32(wave, 24, UINT32_MAX); CHECK(!ssr::FormatTypeAccepted(all, wave + 24));
    CHECK(!ssr::FormatTypeAccepted(all, nullptr));
}

void PcmRangeRules() {
    ssr::PcmRangeBounds bounds{};
    CHECK(ssr::PcmRange(SSR_DEFAULT_PCM, false, &bounds));
    CHECK(bounds.max_channels == 32 && bounds.min_bits == 16 && bounds.max_bits == 32 &&
        bounds.min_rate == SSR_PCM_RATES[0] && bounds.max_rate == SSR_PCM_RATES[SSR_PCM_RATE_COUNT - 1] );
    CHECK(ssr::PcmRange(SSR_DEFAULT_PCM, true, &bounds));
    CHECK(bounds.min_bits == 32 && bounds.max_bits == 32);
    auto enabled = SSR_DEFAULT_PCM; enabled.shapes = SSR_PCM_SHAPES_INTEGER;
    CHECK(ssr::PcmRange(enabled, false, &bounds)); CHECK(!ssr::PcmRange(enabled, true, &bounds));
    enabled.shapes = SSR_PCM_SHAPE_FLOAT32;
    CHECK(ssr::PcmRange(enabled, true, &bounds)); CHECK(!ssr::PcmRange(enabled, false, &bounds));
    enabled.shapes = SSR_PCM_SHAPE_INT16;
    CHECK(ssr::PcmRange(enabled, false, &bounds)); CHECK(bounds.min_bits == 16 && bounds.max_bits == 16);
    enabled = SSR_DEFAULT_PCM; enabled.max_masked_channels = 0; enabled.direct_channels = 1U << 11U;
    CHECK(ssr::PcmRange(enabled, false, &bounds)); CHECK(bounds.max_channels == 12);
    enabled = SSR_DEFAULT_PCM; enabled.direct_channels = 0; enabled.max_masked_channels = UINT32_MAX;
    CHECK(ssr::PcmRange(enabled, false, &bounds)); CHECK(bounds.max_channels == 18);
    enabled = SSR_DEFAULT_PCM; enabled.rates = UINT32_C(1) << 31U;
    CHECK(!ssr::PcmRange(enabled, false, &bounds));
    enabled = SSR_DEFAULT_PCM; enabled.shapes = UINT32_C(1) << 31U;
    CHECK(!ssr::PcmRange(enabled, false, &bounds));
    enabled = SSR_DEFAULT_PCM; enabled.rates = 0;
    CHECK(!ssr::PcmRange(enabled, false, &bounds)); CHECK(!ssr::PcmRange(enabled, true, &bounds));
    enabled = SSR_DEFAULT_PCM; enabled.shapes = 0;
    CHECK(!ssr::PcmRange(enabled, false, &bounds)); CHECK(!ssr::PcmRange(enabled, true, &bounds));
    enabled = SSR_DEFAULT_PCM; enabled.max_masked_channels = 0; enabled.direct_channels = 0;
    CHECK(!ssr::PcmRange(enabled, false, &bounds)); CHECK(!ssr::PcmRange(enabled, true, &bounds));
    const auto saved = bounds;
    CHECK(!ssr::PcmRange({}, false, &bounds));
    CHECK(bounds.max_channels == saved.max_channels && bounds.min_bits == saved.min_bits &&
        bounds.max_bits == saved.max_bits && bounds.min_rate == saved.min_rate && bounds.max_rate == saved.max_rate);
    CHECK(!ssr::PcmRange(SSR_DEFAULT_PCM, false, nullptr));
    for (uint32_t bit = 0; bit < 32; ++bit) {
        enabled = SSR_DEFAULT_PCM; enabled.rates = UINT32_C(1) << bit;
        const bool found = ssr::PcmRange(enabled, false, &bounds);
        CHECK(found == (bit < SSR_PCM_RATE_COUNT));
        if (found) CHECK(bounds.min_rate == SSR_PCM_RATES[bit] && bounds.max_rate == SSR_PCM_RATES[bit]);
        enabled = SSR_DEFAULT_PCM; enabled.shapes = UINT32_C(1) << bit;
        for (const auto& shape : kTestShapes) if (shape.bit == enabled.shapes) {
            CHECK(ssr::PcmRange(enabled, shape.floating, &bounds));
            CHECK(bounds.min_bits == shape.bits && bounds.max_bits == shape.bits);
        }
        enabled = SSR_DEFAULT_PCM; enabled.max_masked_channels = 0; enabled.direct_channels = UINT32_C(1) << bit;
        CHECK(ssr::PcmRange(enabled, false, &bounds)); CHECK(bounds.max_channels == bit + 1U);
    }
}

void PcmIntersectionCandidates() {
    const ssr::PcmCandidate oem{8, SSR_PCM_RATES[1], 0x63f, 32, 32, true};
    auto accepts = [](const ssr::PcmCandidate& candidate, void* opaque) {
        CHECK(ssr::PcmCandidateAllowed(*static_cast<SSR_PCM_FORMATS*>(opaque), candidate));
        return true;
    };
    for (uint32_t bit = 0; bit < 32; ++bit) for (uint32_t field = 0; field < 3; ++field) {
        auto enabled = SSR_DEFAULT_PCM;
        if (field == 0) enabled.rates = UINT32_C(1) << bit;
        else if (field == 1) enabled.shapes = UINT32_C(1) << bit;
        else { enabled.direct_channels = UINT32_C(1) << bit; enabled.max_masked_channels = 0; }
        ssr::PcmCandidate result{};
        const bool found = ssr::ChoosePcmIntersection(enabled, oem, accepts, &enabled, &result);
        const bool expected = field == 0 ? bit < SSR_PCM_RATE_COUNT : field == 1 ?
            ((UINT32_C(1) << bit) & (SSR_PCM_SHAPES_INTEGER | SSR_PCM_SHAPE_FLOAT32)) != 0 :
            ((UINT32_C(1) << bit) & SSR_DIRECT_DEFAULT_CHANNELS) != 0;
        CHECK(found == expected);
        if (found) CHECK(ssr::PcmCandidateAllowed(enabled, result));
    }
}

void PcmRules() {
    uint8_t wave[52];
    auto make = [&](uint16_t channels, uint32_t mask, uint32_t rate = 48000,
                    uint16_t bits = 32, uint16_t valid = 32, uint32_t id = 3) {
        Wave(wave, id, 0);
        Put16(wave, 2, channels); Put32(wave, 4, rate);
        Put16(wave, 12, static_cast<uint16_t>(channels * (bits / 8)));
        Put32(wave, 8, rate * channels * (bits / 8));
        Put16(wave, 14, bits); Put16(wave, 16, 22);
        Put16(wave, 18, valid); Put32(wave, 20, mask);
    };
    const uint32_t masks[] = {0x33, 0x3f, 0x60f, 0x70f, 0xff, 0x503f, 0x563f, 0x2d03f, 0x2d63f, 0x3ffff};
    for (auto mask : masks) {
        uint16_t channels = 0;
        for (auto m = mask; m; m &= m - 1) ++channels;
        make(channels, mask);
        CHECK(ssr::PcmAllowed(SSR_DEFAULT_PCM, wave, 40));
        // Keep byte-rate/alignment valid so this specifically checks popcount.
        make(static_cast<uint16_t>(channels + 1), mask);
        CHECK(!ssr::PcmAllowed(SSR_DEFAULT_PCM, wave, 40));
    }
    const uint16_t direct[] = {0, 1, 2, 16, 24, 32, 33};
    for (uint16_t channels : direct) {
        make(channels, 0);
        CHECK(ssr::PcmAllowed(SSR_DEFAULT_PCM, wave, 40) == (channels >= 1 && channels <= 32));
    }
    make(1, 0x40000); CHECK(!ssr::PcmAllowed(SSR_DEFAULT_PCM, wave, 40));
    for (uint32_t rate : SSR_PCM_RATES) {
        make(2, 3, rate); CHECK(ssr::PcmAllowed(SSR_DEFAULT_PCM, wave, 40));
    }
    make(2, 3, 32000); CHECK(!ssr::PcmAllowed(SSR_DEFAULT_PCM, wave, 40));
    for (const auto& shape : kTestShapes) if (shape.bit) {
        make(2, 3, SSR_PCM_RATES[1], shape.bits, shape.valid, shape.floating ? 3U : 1U);
        CHECK(ssr::PcmAllowed(SSR_DEFAULT_PCM, wave, 40));
        auto enabled = SSR_DEFAULT_PCM; enabled.shapes &= ~shape.bit;
        CHECK(!ssr::PcmAllowed(enabled, wave, 40));
        Put16(wave, 18, 20); CHECK(!ssr::PcmAllowed(SSR_DEFAULT_PCM, wave, 40));
    }
    make(2, 3);
    auto enabled = SSR_DEFAULT_PCM; enabled.rates &= ~2U;
    CHECK(!ssr::PcmAllowed(enabled, wave, 40));
    enabled = SSR_DEFAULT_PCM; enabled.max_masked_channels = 1;
    CHECK(!ssr::PcmAllowed(enabled, wave, 40));
    make(32, 0); enabled = SSR_DEFAULT_PCM; enabled.direct_channels &= 0x7fffffff;
    CHECK(!ssr::PcmAllowed(enabled, wave, 40));
    const uint16_t plainChannels[] = {1, 2, 3};
    for (uint16_t channels : plainChannels) {
        make(channels, 0, 48000, 16, 16, 1);
        Put16(wave, 0, 1); Put16(wave, 16, 0);
        CHECK(ssr::PcmAllowed(SSR_DEFAULT_PCM, wave, 18) == (channels <= 2));
        Put16(wave, 0, 3); Put16(wave, 14, 32);
        Put16(wave, 12, static_cast<uint16_t>(channels * 4)); Put32(wave, 8, 48000U * channels * 4);
        CHECK(ssr::PcmAllowed(SSR_DEFAULT_PCM, wave, 18) == (channels <= 2));
    }
    make(2, 3);
    for (uint32_t n = 0; n < 40; ++n) CHECK(!ssr::PcmAllowed(SSR_DEFAULT_PCM, wave, n));
    CHECK(!ssr::PcmAllowed(SSR_DEFAULT_PCM, nullptr, 40));
    Put16(wave, 16, 65535); CHECK(!ssr::PcmAllowed(SSR_DEFAULT_PCM, wave, 40));
    make(2, 3); wave[35] ^= 1; CHECK(!ssr::PcmAllowed(SSR_DEFAULT_PCM, wave, 40));
    make(2, 3); Put16(wave, 12, 7); CHECK(!ssr::PcmAllowed(SSR_DEFAULT_PCM, wave, 40));
    make(2, 3); Put32(wave, 8, 1); CHECK(!ssr::PcmAllowed(SSR_DEFAULT_PCM, wave, 40));
    make(32, 0, UINT32_MAX); CHECK(!ssr::PcmAllowed(SSR_DEFAULT_PCM, wave, 40));
}

}  // namespace

int main() {
    EndpointNames();
    AlignmentAndRead();
    PassthroughStateAndIsolation();
    WrapIntegrity();
    UrgentAndStats();
    OverrunRecovery();
    StreamStartDoesNotRepeatFormat();
    DiscardQueuedRestatesCurrentState();
    DropBeforeFormatChangeRestatesOldState();
    StoppedEndpointRestatesAfterOverrun();
    SourceGapRecovery();
    VolumeStateAndRecovery();
    EndpointBounds();
    ReadRules();
    PayloadAndReadLimits();
    ClockRules();
    U128Rules();
    MulDivRules();
    RandomClockRules();
    Formats();
    PcmIntersectionOrder();
    PcmRules();
    PcmCandidateAgreementGrid();
    PcmMaskBits();
    CodecCarrierAcceptance();
    CodecCarrierGolden();
    FormatAcceptedSingleChecks();
    FormatSubtypeValidation();
    FormatTypeRules();
    PcmRangeRules();
    PcmIntersectionCandidates();
    Volume32();
    FormatlessOverwriteKeepsReading();
    if (failures != 0) {
        fprintf(stderr, "%d checks failed\n", failures);
        return 1;
    }
    printf("core_tests: all checks passed\n");
    return 0;
}
