# Generated from crates/ssr-abi/src/wire.rs by crates/ssr-abi/tests/py_module.rs.
# Do not edit: change the Rust contract and run the test with SSR_ABI_BLESS=1.

import struct

ABI_VERSION = 1
CONTROL_DEVICE_PATH = "\\\\.\\SpatialSoftReceiver"
IOCTL_SSR_QUERY_INFO = 2253824
IOCTL_SSR_READ = 2253830
IOCTL_SSR_SET_RATE_TRIM = 2270216
IOCTL_SSR_GET_STATS = 2253836
RECORD_KIND_DATA = 1
RECORD_KIND_FORMAT_CHANGE = 2
RECORD_KIND_STREAM_START = 3
RECORD_KIND_STREAM_STOP = 4
RECORD_KIND_OVERRUN = 5
RECORD_KIND_VOLUME = 6
MAX_VOLUME_CHANNELS = 32
VOLUME_MIN_Q16 = -6291456
VOLUME_MAX_Q16 = 0
VOLUME_STEP_Q16 = 32768
RECORD_KIND_RECORDER_MIN = 32768
RECORD_KIND_READ_END = 32769
RECORD_KIND_RECORDER_GAP = 32770
RECORD_ENDPOINT_RECORDER = 65535
ENDPOINT_PRIMARY = 0
ENDPOINT_SECOND = 1
ENDPOINT_PASSTHROUGH = 2
MAX_ENDPOINTS = 4
DEFAULT_ENDPOINT_MASK = 7
RECORD_FLAG_DISCONTINUITY = 1
RECORD_FLAG_REPEAT = 2
RECORD_ALIGN = 8
MAX_DATA_PAYLOAD = 65536
READ_MAX_BYTES = 262144
TRIM_LIMIT_PPM = 2000
PCM_RATE_COUNT = 6
PCM_SHAPE_INT16 = 1
PCM_SHAPE_INT24 = 2
PCM_SHAPE_INT24_IN_32 = 4
PCM_SHAPE_INT32 = 8
PCM_SHAPE_FLOAT32 = 16
PCM_SHAPES_INTEGER = 15
PCM_SPEAKER_BITS = 262143
MAX_PCM_CHANNELS = 32
DIRECT_DEFAULT_CHANNELS = 43711
FORMAT_AC3 = 2
FORMAT_EAC3 = 4
FORMAT_EAC3_JOC = 8
FORMAT_TRUEHD = 16
FORMAT_MAT20 = 32
FORMAT_MAT21 = 64
FORMAT_DTS = 128
FORMAT_DTS_HD = 256
COMPILED_CODECS = 510
DEFAULT_ENABLED_CODECS = 108
FEATURE_PENDING_READ = 1
FEATURE_RATE_TRIM = 2
FEATURE_WRITE_PACKET = 4
ENDPOINT_STATE_STREAMING = 1
DUMP_FORMAT_VERSION = 3
WAVE_FORMAT_PCM = 1
WAVE_FORMAT_IEEE_FLOAT = 3
WAVE_FORMAT_DTS = 8
WAVE_FORMAT_DOLBY_AC3_SPDIF = 146
WAVE_FORMAT_EXTENSIBLE = 65534
WAVE_FAMILY = 0
IEC61937_FAMILY = 3306
WAVEFORMATEX_BYTES = 18
WAVEFORMATEXTENSIBLE_BYTES = 40
IEC61937_FORMAT_BYTES = 52
FILE_DEVICE_UNKNOWN = 34
METHOD_BUFFERED = 0
METHOD_IN_DIRECT = 1
METHOD_OUT_DIRECT = 2
METHOD_NEITHER = 3
FILE_ANY_ACCESS = 0
FILE_READ_ACCESS = 1
FILE_WRITE_ACCESS = 2
PCM_RATES = [44100, 48000, 88200, 96000, 176400, 192000]
DUMP_MAGIC = b"SSRDUMP\0"
LEGACY_DUMP_MAGIC = b"BIODUMP\0"
WAVE_GUID_TAIL = bytes([16, 0, 128, 0, 0, 170, 0, 56, 155, 113])
IEC61937_NAMES = {
    10: "IEC 61937 E-AC-3",
    266: "IEC 61937 E-AC-3 JOC",
    11: "IEC 61937 DTS-HD",
    267: "IEC 61937 DTS:X E1",
    779: "IEC 61937 DTS:X E2",
    12: "IEC 61937 TrueHD",
    268: "IEC 61937 MAT 2.0",
    780: "IEC 61937 MAT 2.1",
    1804: "IEC 61937 MAT 2.1",
}

VOLUME_PAYLOAD_FIELDS = (
    "channel_count",
    "mute_mask",
    "level_q16[0]",
    "level_q16[1]",
    "level_q16[2]",
    "level_q16[3]",
    "level_q16[4]",
    "level_q16[5]",
    "level_q16[6]",
    "level_q16[7]",
    "level_q16[8]",
    "level_q16[9]",
    "level_q16[10]",
    "level_q16[11]",
    "level_q16[12]",
    "level_q16[13]",
    "level_q16[14]",
    "level_q16[15]",
    "level_q16[16]",
    "level_q16[17]",
    "level_q16[18]",
    "level_q16[19]",
    "level_q16[20]",
    "level_q16[21]",
    "level_q16[22]",
    "level_q16[23]",
    "level_q16[24]",
    "level_q16[25]",
    "level_q16[26]",
    "level_q16[27]",
    "level_q16[28]",
    "level_q16[29]",
    "level_q16[30]",
    "level_q16[31]",
)
VOLUME_PAYLOAD = struct.Struct("<II32i")

LEGACY_VOLUME_PAYLOAD_FIELDS = (
    "channel_count",
    "mute_mask",
    "level_q16[0]",
    "level_q16[1]",
    "level_q16[2]",
    "level_q16[3]",
    "level_q16[4]",
    "level_q16[5]",
    "level_q16[6]",
    "level_q16[7]",
    "level_q16[8]",
    "level_q16[9]",
    "level_q16[10]",
    "level_q16[11]",
    "level_q16[12]",
    "level_q16[13]",
    "level_q16[14]",
    "level_q16[15]",
)
LEGACY_VOLUME_PAYLOAD = struct.Struct("<II16i")

RECORD_HEADER_FIELDS = (
    "kind",
    "flags",
    "epoch",
    "byte_pos",
    "qpc",
    "qpc_written",
    "len",
    "endpoint",
    "reserved",
)
RECORD_HEADER = struct.Struct("<HHIQQQIHH")

OVERRUN_PAYLOAD_FIELDS = (
    "lost_bytes",
)
OVERRUN_PAYLOAD = struct.Struct("<Q")

READ_REQUEST_FIELDS = (
    "min_bytes",
    "timeout_ms",
)
READ_REQUEST = struct.Struct("<II")

RATE_TRIM_FIELDS = (
    "ppm_q16",
    "reserved",
)
RATE_TRIM = struct.Struct("<iI")

PCM_FORMATS_FIELDS = (
    "rates",
    "shapes",
    "max_masked_channels",
    "direct_channels",
)
PCM_FORMATS = struct.Struct("<IIII")

ENDPOINT_FORMATS_FIELDS = (
    "codecs",
    "pcm.rates",
    "pcm.shapes",
    "pcm.max_masked_channels",
    "pcm.direct_channels",
)
ENDPOINT_FORMATS = struct.Struct("<QIIII")

ABI_INFO_FIELDS = (
    "abi_version",
    "struct_size",
    "driver_version",
    "ring_bytes",
    "compiled.codecs",
    "compiled.pcm.rates",
    "compiled.pcm.shapes",
    "compiled.pcm.max_masked_channels",
    "compiled.pcm.direct_channels",
    "features",
    "qpc_frequency",
    "endpoint_mask",
    "reserved",
    "endpoints[0].codecs",
    "endpoints[0].pcm.rates",
    "endpoints[0].pcm.shapes",
    "endpoints[0].pcm.max_masked_channels",
    "endpoints[0].pcm.direct_channels",
    "endpoints[1].codecs",
    "endpoints[1].pcm.rates",
    "endpoints[1].pcm.shapes",
    "endpoints[1].pcm.max_masked_channels",
    "endpoints[1].pcm.direct_channels",
    "endpoints[2].codecs",
    "endpoints[2].pcm.rates",
    "endpoints[2].pcm.shapes",
    "endpoints[2].pcm.max_masked_channels",
    "endpoints[2].pcm.direct_channels",
    "endpoints[3].codecs",
    "endpoints[3].pcm.rates",
    "endpoints[3].pcm.shapes",
    "endpoints[3].pcm.max_masked_channels",
    "endpoints[3].pcm.direct_channels",
)
ABI_INFO = struct.Struct("<IIIIQIIIIQQIIQIIIIQIIIIQIIIIQIIII")

ENDPOINT_STATS_FIELDS = (
    "total_bytes",
    "overruns",
    "lost_bytes",
    "epoch",
    "state",
)
ENDPOINT_STATS = struct.Struct("<QQQII")

DRIVER_STATS_FIELDS = (
    "struct_size",
    "open_count",
    "max_timer_lateness_us",
    "reserved",
    "endpoints[0].total_bytes",
    "endpoints[0].overruns",
    "endpoints[0].lost_bytes",
    "endpoints[0].epoch",
    "endpoints[0].state",
    "endpoints[1].total_bytes",
    "endpoints[1].overruns",
    "endpoints[1].lost_bytes",
    "endpoints[1].epoch",
    "endpoints[1].state",
    "endpoints[2].total_bytes",
    "endpoints[2].overruns",
    "endpoints[2].lost_bytes",
    "endpoints[2].epoch",
    "endpoints[2].state",
    "endpoints[3].total_bytes",
    "endpoints[3].overruns",
    "endpoints[3].lost_bytes",
    "endpoints[3].epoch",
    "endpoints[3].state",
)
DRIVER_STATS = struct.Struct("<IIIIQQQIIQQQIIQQQIIQQQII")

DUMP_HEADER_FIELDS = (
    "magic",
    "abi_version",
    "header_size",
    "qpc_frequency",
    "format_version",
    "reserved",
)
DUMP_HEADER = struct.Struct("<8sIIQII")

WAVEFORMATEX_FIELDS = (
    "format_tag",
    "channels",
    "samples_per_sec",
    "avg_bytes_per_sec",
    "block_align",
    "bits_per_sample",
    "extra_size",
)
WAVEFORMATEX = struct.Struct("<HHIIHHH")

WAVEFORMATEXTENSIBLE_FIELDS = (
    "format_tag",
    "channels",
    "samples_per_sec",
    "avg_bytes_per_sec",
    "block_align",
    "bits_per_sample",
    "extra_size",
    "valid_bits_per_sample",
    "channel_mask",
    "subformat",
)
WAVEFORMATEXTENSIBLE = struct.Struct("<HHIIHHHHI16s")

ABI_INFO_PREFIX_FIELDS = (
    "abi_version",
    "struct_size",
)
ABI_INFO_PREFIX = struct.Struct("<II")

DRIVER_STATS_PREFIX_FIELDS = (
    "struct_size",
)
DRIVER_STATS_PREFIX = struct.Struct("<I")


def records(stream):
    """Yield complete ABI records as dictionaries; stop at a cut record."""
    offset = 0
    while offset + RECORD_HEADER.size <= len(stream):
        values = RECORD_HEADER.unpack_from(stream, offset)
        record = dict(zip(RECORD_HEADER_FIELDS, values))
        end = offset + RECORD_HEADER.size + record["len"]
        if end > len(stream):
            break
        record["payload"] = bytes(stream[offset + RECORD_HEADER.size:end])
        record["offset"] = offset
        total = RECORD_HEADER.size + record["len"]
        record["size"] = min((total + RECORD_ALIGN - 1) // RECORD_ALIGN * RECORD_ALIGN, len(stream) - offset)
        yield record
        offset += record["size"]

def ctl_code(device_type, function, method, access):
    return (device_type << 16) | (access << 14) | (function << 2) | method

def wave_subformat(data1, family=WAVE_FAMILY):
    return struct.pack("<IH", data1, family) + WAVE_GUID_TAIL
