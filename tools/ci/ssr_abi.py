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
PCM_RATES = [44100, 48000, 88200, 96000, 176400, 192000]
DUMP_MAGIC = b"SSRDUMP\0"
LEGACY_DUMP_MAGIC = b"BIODUMP\0"

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
