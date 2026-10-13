// SPDX-License-Identifier: MS-PL
// Emits a deterministic core record stream and a text expectation file.
#include "bio_core.h"
#include <stdio.h>
#include <string.h>

namespace {

alignas(8) uint8_t storage[64U * 1024U];
uint8_t stream[64U * 1024U];

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

void Mat21(uint8_t* out) {
    memset(out, 0, 52);
    Put16(out, 0, 0xfffe);
    Put16(out, 2, 8);
    Put32(out, 4, 192000);
    Put32(out, 8, 3072000);
    Put16(out, 12, 16);
    Put16(out, 14, 16);
    Put16(out, 16, 34);
    Put16(out, 18, 16);
    Put32(out, 24, 0x030c);
    Put16(out, 28, 0x0cea);
    const uint8_t tail[10] = {0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71};
    memcpy(out + 30, tail, sizeof(tail));
    Put32(out, 40, 48000);
}

bool WriteAll(FILE* file, const void* data, size_t bytes) {
    return fwrite(data, 1, bytes, file) == bytes;
}

}  // namespace

int main(int argc, char** argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: write_dump PATH\n");
        return 2;
    }
    bio::RecordRing ring;
    if (!ring.Init(storage, sizeof(storage))) {
        return 1;
    }
    if (!ring.ConfigureVolume(0, 12) || !ring.ConfigureVolume(1, 2) || !ring.ConfigureVolume(2, 8) ||
        !ring.SetVolume(0, -1, -20 * 65536, 1) || !ring.SetMute(1, -1, true, 1)) return 1;
    uint8_t format[52];
    Mat21(format);
    uint8_t data0[4096];
    uint8_t data1[4096];
    uint8_t data2[4096];
    memset(data0, 1, sizeof(data0));
    memset(data1, 2, sizeof(data1));
    memset(data2, 3, sizeof(data2));
    if (!ring.StreamStart(0, 1) || !ring.StreamStart(1, 2) || !ring.StreamStart(2, 2) ||
        !ring.FormatChange(0, 3, format, sizeof(format)) ||
        !ring.FormatChange(1, 4, format, sizeof(format)) ||
        !ring.FormatChange(2, 5, format, sizeof(format))) {
        return 1;
    }
    ring.DiscardQueued();
    for (uint32_t i = 0; i < 30; ++i) {
        if (!ring.Data(static_cast<uint16_t>(i % 3U), 10U + i, 100U + i,
                       i % 3U == 0 ? data0 : i % 3U == 1 ? data1 : data2, sizeof(data0), nullptr, 0)) {
            return 1;
        }
    }
    if (!ring.Data(0, 100, 200, data0, 19, nullptr, 0) ||
        !ring.SourceGap(1, 101, 777) ||
        !ring.Data(1, 102, 201, data1, 23, nullptr, 0) ||
        !ring.StreamStop(1, 103) ||
        !ring.SourceGap(2, 104, 123) ||
        !ring.Data(2, 105, 202, data2, 31, nullptr, 0) ||
        !ring.StreamStop(2, 106)) {
        return 1;
    }
    const uint32_t bytes = ring.Read(stream, sizeof(stream));

    BIO_DUMP_HEADER dump{};
    memcpy(dump.magic, "BIODUMP\0", 8);
    dump.abi_version = BIO_ABI_VERSION;
    dump.header_size = static_cast<uint32_t>(sizeof(dump));
    dump.qpc_frequency = 10000000;
    dump.format_version = BIO_DUMP_FORMAT_VERSION;
    FILE* file = fopen(argv[1], "wb");
    if (file == nullptr || !WriteAll(file, &dump, sizeof(dump)) || !WriteAll(file, stream, bytes) ||
        fclose(file) != 0) {
        return 1;
    }

    char expect_path[4096];
    const int count = snprintf(expect_path, sizeof(expect_path), "%s.expect", argv[1]);
    if (count < 0 || static_cast<size_t>(count) >= sizeof(expect_path)) {
        return 1;
    }
    FILE* expect = fopen(expect_path, "wb");
    if (expect == nullptr) {
        return 1;
    }
    for (uint32_t at = 0; at < bytes;) {
        BIO_RECORD_HEADER header{};
        memcpy(&header, stream + at, sizeof(header));
        uint64_t sum = 0;
        if (header.kind == BIO_RECORD_KIND_DATA) {
            for (uint32_t i = 0; i < header.len; ++i) {
                sum += stream[at + sizeof(header) + i];
            }
        }
        if (fprintf(expect, "%u %u %u %u %llu %u %llu\n",
                    static_cast<unsigned>(header.endpoint), static_cast<unsigned>(header.kind),
                    static_cast<unsigned>(header.epoch), static_cast<unsigned>(header.flags),
                    static_cast<unsigned long long>(header.byte_pos),
                    static_cast<unsigned>(header.len), static_cast<unsigned long long>(sum)) < 0) {
            fclose(expect);
            return 1;
        }
        at += RecordBytes(header.len);
    }
    if (fclose(expect) != 0) {
        return 1;
    }
    printf("write_dump: wrote %u record bytes\n", static_cast<unsigned>(bytes));
    return 0;
}
