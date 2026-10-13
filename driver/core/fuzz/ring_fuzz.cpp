// SPDX-License-Identifier: MS-PL
// libFuzzer target for record-ring operations and READ completion rules.
#include "bio_core.h"
#include <stddef.h>
#include <stdint.h>

extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
    alignas(8) uint8_t storage[64U * 1024U];
    uint8_t output[4096];
    bio::RecordRing ring;
    if (!ring.Init(storage, sizeof(storage))) {
        return 0;
    }
    size_t at = 0;
    uint64_t qpc = 0;
    while (at < size) {
        const uint8_t op = data[at++];
        const uint16_t ep = static_cast<uint16_t>((op >> 3U) & 3U);
        qpc += op;
        switch (op & 7U) {
            case 0:
                (void)ring.StreamStart(ep, qpc);
                break;
            case 1:
                (void)ring.StreamStop(ep, qpc);
                break;
            case 2: {
                const uint32_t remain = static_cast<uint32_t>(size - at);
                const uint32_t bytes = remain < bio::kMaxFormatBytes ? remain : bio::kMaxFormatBytes;
                (void)ring.FormatChange(ep, qpc, data + at, bytes);
                at += bytes;
                break;
            }
            case 3:
            case 4: {
                const uint32_t remain = static_cast<uint32_t>(size - at);
                const uint32_t bytes = remain < 1024U ? remain : 1024U;
                const uint32_t split = bytes / 2U;
                (void)ring.Data(ep, qpc, qpc + 1U, data + at, split,
                                data + at + split, bytes - split);
                at += bytes;
                break;
            }
            case 5:
                (void)ring.Read(output, static_cast<uint32_t>(op) * 16U);
                break;
            case 6:
                (void)ring.SourceGap(ep, qpc, static_cast<uint64_t>(op) + 1U);
                break;
            default:
                if ((op & 0x80U) != 0) {
                    ring.DiscardQueued();
                } else {
                    (void)bio::ReadGate::ShouldComplete(ring, static_cast<uint32_t>(op) * 32U,
                                                        sizeof(output), qpc, qpc + op);
                }
                break;
        }
    }
    return 0;
}
