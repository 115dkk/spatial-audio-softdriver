// SPDX-License-Identifier: MS-PL
// Dependency-free unit tests for the driver core.
#include "bio_core.h"
#include <stdio.h>
#include <string.h>

namespace {

int failures = 0;
#define CHECK(expression) do { if (!(expression)) { \
    fprintf(stderr, "%s:%d: CHECK(%s) failed\n", __FILE__, __LINE__, #expression); \
    ++failures; } } while (false)

alignas(8) uint8_t storage[64U * 1024U];

uint32_t RecordBytes(uint32_t payload) {
    return (static_cast<uint32_t>(sizeof(BIO_RECORD_HEADER)) + payload + 7U) & ~7U;
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

void AlignmentAndRead() {
    bio::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    const uint8_t data[3] = {1, 2, 3};
    CHECK(ring.Data(0, 7, 8, data, sizeof(data), nullptr, 0));
    CHECK(ring.FirstRecordBytes() == RecordBytes(3));
    uint8_t out[64]{};
    CHECK(ring.Read(out, RecordBytes(3) - 1U) == 0);
    CHECK(ring.FirstRecordBytes() == RecordBytes(3));
    CHECK(ring.Read(out, sizeof(out)) == RecordBytes(3));
    BIO_RECORD_HEADER header{};
    memcpy(&header, out, sizeof(header));
    CHECK(header.kind == BIO_RECORD_KIND_DATA);
    CHECK(header.len == 3);
    CHECK(memcmp(out + sizeof(header), data, sizeof(data)) == 0);
    for (uint32_t i = static_cast<uint32_t>(sizeof(header)) + 3U; i < RecordBytes(3); ++i) {
        CHECK(out[i] == 0);
    }
}

void WrapIntegrity() {
    bio::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    uint8_t payload[997];
    uint8_t out[2048];
    for (uint32_t round = 0; round < 400; ++round) {
        memset(payload, static_cast<int>(round & 0xffU), sizeof(payload));
        CHECK(ring.Data(static_cast<uint16_t>(round & 1U), round, 0, payload, sizeof(payload), nullptr, 0));
        CHECK(ring.Read(out, sizeof(out)) == RecordBytes(sizeof(payload)));
        BIO_RECORD_HEADER header{};
        memcpy(&header, out, sizeof(header));
        CHECK(header.len == sizeof(payload));
        CHECK(memcmp(out + sizeof(header), payload, sizeof(payload)) == 0);
    }
}

void UrgentAndStats() {
    bio::RecordRing ring;
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
    BIO_STATS stats{};
    ring.FillStats(&stats, 4, 17);
    CHECK(stats.struct_size == sizeof(BIO_STATS));
    CHECK(stats.open_count == 4);
    CHECK(stats.max_timer_lateness_us == 17);
    CHECK(stats.endpoints[0].epoch == 1);
    CHECK(stats.endpoints[0].state == 0);
}

void OverrunRecovery() {
    bio::RecordRing ring;
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
    uint64_t read_data[2] = {0, 0};
    for (uint32_t at = 0; at < got;) {
        BIO_RECORD_HEADER header{};
        memcpy(&header, all + at, sizeof(header));
        if (header.kind == BIO_RECORD_KIND_OVERRUN && header.endpoint == 0) {
            saw_overrun = true;
        }
        if (header.kind == BIO_RECORD_KIND_FORMAT_CHANGE && header.endpoint == 0 &&
            header.flags == BIO_RECORD_FLAG_FORMAT_REPEAT) {
            saw_repeat = true;
            CHECK(header.epoch == ring.Endpoint(0).epoch);
        }
        if (header.kind == BIO_RECORD_KIND_DATA) {
            read_data[header.endpoint] += header.len;
            if (header.endpoint == 0 && header.len == 32) {
                saw_discontinuous = (header.flags & BIO_RECORD_FLAG_DISCONTINUITY) != 0;
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
    bio::RecordRing ring;
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
    BIO_RECORD_HEADER header{};
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == BIO_RECORD_KIND_FORMAT_CHANGE);
    CHECK(header.flags == 0);
    at += RecordBytes(header.len);
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == BIO_RECORD_KIND_STREAM_START);
    at += RecordBytes(header.len);
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == BIO_RECORD_KIND_DATA);
    CHECK(header.flags == BIO_RECORD_FLAG_DISCONTINUITY);
    at += RecordBytes(header.len);
    CHECK(at == got);
}

void DiscardQueuedRepeatsFormat() {
    bio::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    uint8_t format[52];
    Wave(format, 0x030c, 0x0cea);
    const uint8_t data[2] = {7, 8};
    CHECK(ring.FormatChange(0, 1, format, sizeof(format)));
    CHECK(ring.Data(0, 2, 0, data, sizeof(data), nullptr, 0));
    const uint64_t lost = ring.Endpoint(0).lost_bytes;
    const uint64_t overruns = ring.Endpoint(0).overruns;
    ring.DiscardQueued();
    CHECK(ring.QueuedBytes() == 0);
    CHECK(ring.Data(0, 3, 0, data, sizeof(data), nullptr, 0));

    uint8_t out[256];
    const uint32_t got = ring.Read(out, sizeof(out));
    uint32_t at = 0;
    BIO_RECORD_HEADER header{};
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == BIO_RECORD_KIND_FORMAT_CHANGE);
    CHECK(header.flags == BIO_RECORD_FLAG_FORMAT_REPEAT);
    at += RecordBytes(header.len);
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == BIO_RECORD_KIND_DATA);
    CHECK(header.flags == BIO_RECORD_FLAG_DISCONTINUITY);
    at += RecordBytes(header.len);
    CHECK(at == got);
    CHECK(ring.Endpoint(0).lost_bytes == lost);
    CHECK(ring.Endpoint(0).overruns == overruns);
}

void DropBeforeFormatChangeSkipsRepeat() {
    bio::RecordRing ring;
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
    BIO_RECORD_HEADER previous{};
    BIO_RECORD_HEADER header{};
    while (at < got) {
        previous = header;
        memcpy(&header, out + at, sizeof(header));
        CHECK((header.flags & BIO_RECORD_FLAG_FORMAT_REPEAT) == 0);
        at += RecordBytes(header.len);
    }
    CHECK(at == got);
    CHECK(previous.kind == BIO_RECORD_KIND_OVERRUN);
    CHECK(header.kind == BIO_RECORD_KIND_FORMAT_CHANGE);
    CHECK(header.flags == 0);
}

void SourceGapRecovery() {
    bio::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    uint8_t format[52];
    Wave(format, 0x030c, 0x0cea);
    CHECK(ring.FormatChange(0, 1, format, sizeof(format)));
    CHECK(ring.FormatChange(1, 2, format, sizeof(format)));
    uint8_t out[512];
    CHECK(ring.Read(out, sizeof(out)) != 0);

    const uint8_t other[3] = {4, 5, 6};
    const uint8_t resumed[2] = {7, 8};
    CHECK(!ring.SourceGap(BIO_MAX_ENDPOINTS, 3, 10));
    CHECK(!ring.SourceGap(0, 3, 0));
    CHECK(ring.SourceGap(0, 3, 100));
    CHECK(ring.QueuedBytes() == 0);
    CHECK(ring.Data(1, 4, 0, other, sizeof(other), nullptr, 0));
    CHECK(ring.Data(0, 5, 0, resumed, sizeof(resumed), nullptr, 0));

    const uint32_t got = ring.Read(out, sizeof(out));
    uint32_t at = 0;
    BIO_RECORD_HEADER header{};
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == BIO_RECORD_KIND_DATA);
    CHECK(header.endpoint == 1);
    CHECK(header.flags == 0);
    at += RecordBytes(header.len);
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == BIO_RECORD_KIND_OVERRUN);
    CHECK(header.endpoint == 0);
    BIO_OVERRUN_PAYLOAD overrun{};
    memcpy(&overrun, out + at + sizeof(header), sizeof(overrun));
    CHECK(overrun.lost_bytes == 100);
    CHECK(header.byte_pos == 100);
    at += RecordBytes(header.len);
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == BIO_RECORD_KIND_FORMAT_CHANGE);
    CHECK(header.endpoint == 0);
    CHECK(header.flags == BIO_RECORD_FLAG_FORMAT_REPEAT);
    at += RecordBytes(header.len);
    memcpy(&header, out + at, sizeof(header));
    CHECK(header.kind == BIO_RECORD_KIND_DATA);
    CHECK(header.endpoint == 0);
    CHECK(header.flags == BIO_RECORD_FLAG_DISCONTINUITY);
    CHECK(header.byte_pos == 100);
    at += RecordBytes(header.len);
    CHECK(at == got);
    CHECK(ring.Endpoint(0).pos == 102);
    CHECK(ring.Endpoint(0).total_bytes == 102);
    CHECK(ring.Endpoint(0).lost_bytes == 100);
    CHECK(ring.Endpoint(0).pending_lost == 0);
    CHECK(ring.Endpoint(1).lost_bytes == 0);
}

void ReadRules() {
    bio::RecordRing ring;
    CHECK(ring.Init(storage, sizeof(storage)));
    CHECK(bio::ReadGate::ShouldComplete(ring, 0, 64, 1, 9));
    CHECK(!bio::ReadGate::ShouldComplete(ring, 10, 64, 1, 9));
    uint8_t data[16]{};
    CHECK(ring.Data(0, 0, 0, data, sizeof(data), nullptr, 0));
    CHECK(bio::ReadGate::ShouldComplete(ring, 1000, 48, 1, 9));
    ring.DiscardQueued();
    CHECK(bio::ReadGate::ShouldComplete(ring, 10, 64, 9, 9));
    CHECK(bio::ReadGate::Deadline(UINT64_MAX - 1U, 10000000, 1) == UINT64_MAX);
    CHECK(bio::ReadGate::Deadline(5, 10000000, 2) == 20005);
}

void PayloadAndReadLimits() {
    alignas(8) static uint8_t big[1024U * 1024U];
    static uint8_t data[bio::kMaxDataPayload + 8U];
    static uint8_t out[bio::kReadMaxBytes + 4096U];
    bio::RecordRing ring;
    CHECK(ring.Init(big, sizeof(big)));
    CHECK(!ring.Data(0, 1, 0, data, bio::kMaxDataPayload + 1U, nullptr, 0));
    CHECK(!ring.Data(0, 1, 0, data, bio::kMaxDataPayload, data, 1));
    CHECK(ring.QueuedBytes() == 0);
    CHECK(ring.Data(0, 1, 0, data, bio::kMaxDataPayload - 8U, data, 8));
    const uint32_t largest = ring.FirstRecordBytes();
    CHECK(largest == sizeof(BIO_RECORD_HEADER) + bio::kMaxDataPayload);
    CHECK(largest <= bio::ReadGate::Capacity(UINT32_MAX));
    CHECK(bio::ReadGate::Capacity(100) == 100);
    CHECK(bio::ReadGate::Capacity(bio::kReadMaxBytes + 1U) == bio::kReadMaxBytes);
    // Five largest records queued: one READ with a huge buffer takes three,
    // never more than kReadMaxBytes.
    for (uint32_t i = 0; i < 4U; ++i) CHECK(ring.Data(0, 2U + i, 0, data, bio::kMaxDataPayload, nullptr, 0));
    const uint32_t capacity = bio::ReadGate::Capacity(sizeof(out));
    CHECK(bio::ReadGate::ShouldComplete(ring, UINT32_MAX, capacity, 0, UINT64_MAX));
    const uint32_t moved = ring.Read(out, capacity);
    CHECK(moved == 3U * largest);
    CHECK(moved <= bio::kReadMaxBytes);
    CHECK(ring.QueuedBytes() == 2U * largest);
    CHECK(!bio::ReadGate::ShouldComplete(ring, UINT32_MAX, capacity, 0, UINT64_MAX));
}

void ClockRules() {
    bio::DmaClock clock;
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

NativeU128 Native(bio::U128 value) {
    return (static_cast<NativeU128>(value.high) << 64U) | value.low;
}

void U128Rules() {
    uint64_t random = UINT64_C(0x9e3779b97f4a7c15);
    for (uint32_t i = 0; i < 10000U; ++i) {
        const uint64_t a = Random64(&random);
        const uint64_t b = Random64(&random);
        CHECK(Native(bio::U128Multiply(a, b)) ==
              static_cast<NativeU128>(a) * static_cast<NativeU128>(b));
        bio::U128 numerator{Random64(&random), Random64(&random)};
        bio::U128 denominator{Random64(&random), Random64(&random)};
        if (denominator.high == 0 && denominator.low == 0) denominator.low = 1;
        bio::U128 remainder{};
        const bio::U128 quotient = bio::U128Divide(numerator, denominator, &remainder);
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
        CHECK(bio::MulDivCeil(a, b, c) == ReferenceMulDivCeil(a, b, c));
    uint64_t random = UINT64_C(0xd1b54a32d192ed03);
    for (uint32_t i = 0; i < 10000U; ++i) {
        const uint64_t a = Random64(&random);
        const uint64_t b = Random64(&random);
        const uint64_t c = Random64(&random);
        CHECK(bio::MulDivCeil(a, b, c) == ReferenceMulDivCeil(a, b, c));
    }
}

void RandomClockRules() {
    const uint64_t frequencies[] = {10000000, 24000000, 3000000000, UINT64_C(1) << 48U};
    const uint32_t rates[] = {1, 192000, 176400, 3072000, UINT32_MAX};
    const uint32_t aligns[] = {1, 4, 8, 32};
    const int32_t trims[] = {-BIO_TRIM_LIMIT_PPM * 65536, 0, 1,
                             BIO_TRIM_LIMIT_PPM * 65536};
    constexpr uint64_t trimDenominator = UINT64_C(1000000) * 65536U;
    uint64_t random = UINT64_C(0x94d049bb133111eb);
    for (uint32_t scenario = 0; scenario < 80U; ++scenario) {
        const uint64_t frequency = frequencies[scenario % 4U];
        const uint32_t rate = rates[(scenario / 4U) % 5U];
        const uint32_t align = aligns[(scenario / 20U) % 4U];
        bio::DmaClock clock;
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
                const uint32_t span = static_cast<uint32_t>(BIO_TRIM_LIMIT_PPM * 2 + 1);
                trim = static_cast<int32_t>(Random64(&random) % span) * 65536 -
                       BIO_TRIM_LIMIT_PPM * 65536;
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
    Wave(wave, 1, 0);
    Put16(wave, 16, 22);
    CHECK(bio::FormatBit(wave, 40) == BIO_FORMAT_PCM);
    Wave(wave, 3, 0);
    Put16(wave, 16, 22);
    CHECK(bio::FormatBit(wave, 40) == BIO_FORMAT_PCM);
    const uint32_t ids[] = {0x000a, 0x010a, 0x000b, 0x010b, 0x030b, 0x000c, 0x010c, 0x030c, 0x070c};
    const uint64_t bits[] = {BIO_FORMAT_EAC3, BIO_FORMAT_EAC3_ATMOS, BIO_FORMAT_DTS_HD,
        BIO_FORMAT_DTS_HD, BIO_FORMAT_DTS_HD, BIO_FORMAT_TRUEHD, BIO_FORMAT_MAT20,
        BIO_FORMAT_MAT21, BIO_FORMAT_MAT21};
    for (uint32_t i = 0; i < sizeof(ids) / sizeof(ids[0]); ++i) {
        Wave(wave, ids[i], 0x0cea);
        CHECK(bio::FormatBit(wave, sizeof(wave)) == bits[i]);
        CHECK(bio::FormatAllowed(bits[i], wave, sizeof(wave)));
    }
    CHECK(bio::FormatBit(wave, 17) == 0);
    Put16(wave, 16, 100);
    CHECK(bio::FormatBit(wave, sizeof(wave)) == 0);
    uint8_t plain[18]{};
    Put16(plain, 0, 0x0092);
    CHECK(bio::FormatBit(plain, sizeof(plain)) == BIO_FORMAT_AC3);
    Put16(plain, 0, 0x0008);
    CHECK(bio::FormatBit(plain, sizeof(plain)) == BIO_FORMAT_DTS);
}

}  // namespace

int main() {
    AlignmentAndRead();
    WrapIntegrity();
    UrgentAndStats();
    OverrunRecovery();
    StreamStartDoesNotRepeatFormat();
    DiscardQueuedRepeatsFormat();
    DropBeforeFormatChangeSkipsRepeat();
    SourceGapRecovery();
    ReadRules();
    PayloadAndReadLimits();
    ClockRules();
    U128Rules();
    MulDivRules();
    RandomClockRules();
    Formats();
    if (failures != 0) {
        fprintf(stderr, "%d checks failed\n", failures);
        return 1;
    }
    printf("core_tests: all checks passed\n");
    return 0;
}
