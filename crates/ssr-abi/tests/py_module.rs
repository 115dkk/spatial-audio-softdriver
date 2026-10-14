#![forbid(unsafe_code)]

//! Generates the Python ABI module used by the diagnostic tools and checks it
//! against the committed `tools/ci/ssr_abi.py`. Run with `SSR_ABI_BLESS=1` to
//! rewrite that module after a deliberate Rust contract change.

use std::fmt::Write as _;
use std::mem::{offset_of, size_of};
use std::path::Path;

use ssr_abi::{
    AbiInfo, DriverStats, DumpHeader, EndpointFormats, EndpointStats, OverrunPayload, PcmFormats,
    RateTrim, ReadRequest, RecordHeader, VolumePayload,
};

struct Layout {
    python: &'static str,
    format: &'static str,
    size: usize,
    fields: Vec<(&'static str, usize)>,
}

macro_rules! fields {
    ($ty:ty, $($field:ident),+ $(,)?) => {
        vec![$((stringify!($field), offset_of!($ty, $field))),+]
    };
}

fn layouts() -> Vec<Layout> {
    let mut volume = vec![
        ("channel_count", offset_of!(VolumePayload, channel_count)),
        ("mute_mask", offset_of!(VolumePayload, mute_mask)),
    ];
    for index in 0..ssr_abi::MAX_VOLUME_CHANNELS {
        volume.push((
            Box::leak(format!("level_q16[{index}]").into_boxed_str()),
            offset_of!(VolumePayload, level_q16) + index * size_of::<i32>(),
        ));
    }
    let mut abi = vec![
        ("abi_version", offset_of!(AbiInfo, abi_version)),
        ("struct_size", offset_of!(AbiInfo, struct_size)),
        ("driver_version", offset_of!(AbiInfo, driver_version)),
        ("ring_bytes", offset_of!(AbiInfo, ring_bytes)),
        (
            "compiled.codecs",
            offset_of!(AbiInfo, compiled) + offset_of!(EndpointFormats, codecs),
        ),
        (
            "compiled.pcm.rates",
            offset_of!(AbiInfo, compiled)
                + offset_of!(EndpointFormats, pcm)
                + offset_of!(PcmFormats, rates),
        ),
        (
            "compiled.pcm.shapes",
            offset_of!(AbiInfo, compiled)
                + offset_of!(EndpointFormats, pcm)
                + offset_of!(PcmFormats, shapes),
        ),
        (
            "compiled.pcm.max_masked_channels",
            offset_of!(AbiInfo, compiled)
                + offset_of!(EndpointFormats, pcm)
                + offset_of!(PcmFormats, max_masked_channels),
        ),
        (
            "compiled.pcm.direct_channels",
            offset_of!(AbiInfo, compiled)
                + offset_of!(EndpointFormats, pcm)
                + offset_of!(PcmFormats, direct_channels),
        ),
        ("features", offset_of!(AbiInfo, features)),
        ("qpc_frequency", offset_of!(AbiInfo, qpc_frequency)),
        ("endpoint_mask", offset_of!(AbiInfo, endpoint_mask)),
        ("reserved", offset_of!(AbiInfo, reserved)),
    ];
    for endpoint in 0..ssr_abi::MAX_ENDPOINTS {
        let base = offset_of!(AbiInfo, endpoints) + endpoint * size_of::<EndpointFormats>();
        for (name, offset) in [
            ("codecs", offset_of!(EndpointFormats, codecs)),
            (
                "pcm.rates",
                offset_of!(EndpointFormats, pcm) + offset_of!(PcmFormats, rates),
            ),
            (
                "pcm.shapes",
                offset_of!(EndpointFormats, pcm) + offset_of!(PcmFormats, shapes),
            ),
            (
                "pcm.max_masked_channels",
                offset_of!(EndpointFormats, pcm) + offset_of!(PcmFormats, max_masked_channels),
            ),
            (
                "pcm.direct_channels",
                offset_of!(EndpointFormats, pcm) + offset_of!(PcmFormats, direct_channels),
            ),
        ] {
            abi.push((
                Box::leak(format!("endpoints[{endpoint}].{name}").into_boxed_str()),
                base + offset,
            ));
        }
    }
    let mut stats = fields!(
        DriverStats,
        struct_size,
        open_count,
        max_timer_lateness_us,
        reserved
    );
    for endpoint in 0..ssr_abi::MAX_ENDPOINTS {
        let base = offset_of!(DriverStats, endpoints) + endpoint * size_of::<EndpointStats>();
        for (name, offset) in [
            ("total_bytes", offset_of!(EndpointStats, total_bytes)),
            ("overruns", offset_of!(EndpointStats, overruns)),
            ("lost_bytes", offset_of!(EndpointStats, lost_bytes)),
            ("epoch", offset_of!(EndpointStats, epoch)),
            ("state", offset_of!(EndpointStats, state)),
        ] {
            stats.push((
                Box::leak(format!("endpoints[{endpoint}].{name}").into_boxed_str()),
                base + offset,
            ));
        }
    }
    vec![
        Layout {
            python: "VOLUME_PAYLOAD",
            format: "<II32i",
            size: size_of::<VolumePayload>(),
            fields: volume,
        },
        Layout {
            python: "RECORD_HEADER",
            format: "<HHIQQQIHH",
            size: size_of::<RecordHeader>(),
            fields: fields!(
                RecordHeader,
                kind,
                flags,
                epoch,
                byte_pos,
                qpc,
                qpc_written,
                len,
                endpoint,
                reserved
            ),
        },
        Layout {
            python: "OVERRUN_PAYLOAD",
            format: "<Q",
            size: size_of::<OverrunPayload>(),
            fields: fields!(OverrunPayload, lost_bytes),
        },
        Layout {
            python: "READ_REQUEST",
            format: "<II",
            size: size_of::<ReadRequest>(),
            fields: fields!(ReadRequest, min_bytes, timeout_ms),
        },
        Layout {
            python: "RATE_TRIM",
            format: "<iI",
            size: size_of::<RateTrim>(),
            fields: fields!(RateTrim, ppm_q16, reserved),
        },
        Layout {
            python: "PCM_FORMATS",
            format: "<IIII",
            size: size_of::<PcmFormats>(),
            fields: fields!(
                PcmFormats,
                rates,
                shapes,
                max_masked_channels,
                direct_channels
            ),
        },
        Layout {
            python: "ENDPOINT_FORMATS",
            format: "<QIIII",
            size: size_of::<EndpointFormats>(),
            fields: vec![
                ("codecs", 0),
                ("pcm.rates", 8),
                ("pcm.shapes", 12),
                ("pcm.max_masked_channels", 16),
                ("pcm.direct_channels", 20),
            ],
        },
        Layout {
            python: "ABI_INFO",
            format: "<IIIIQIIIIQQIIQIIIIQIIIIQIIIIQIIII",
            size: size_of::<AbiInfo>(),
            fields: abi,
        },
        Layout {
            python: "ENDPOINT_STATS",
            format: "<QQQII",
            size: size_of::<EndpointStats>(),
            fields: fields!(
                EndpointStats,
                total_bytes,
                overruns,
                lost_bytes,
                epoch,
                state
            ),
        },
        Layout {
            python: "DRIVER_STATS",
            format: "<IIIIQQQIIQQQIIQQQIIQQQII",
            size: size_of::<DriverStats>(),
            fields: stats,
        },
        Layout {
            python: "DUMP_HEADER",
            format: "<8sIIQII",
            size: size_of::<DumpHeader>(),
            fields: fields!(
                DumpHeader,
                magic,
                abi_version,
                header_size,
                qpc_frequency,
                format_version,
                reserved
            ),
        },
    ]
}

fn constants(out: &mut String) {
    macro_rules! emit {
        ($($name:ident),+ $(,)?) => {$({ writeln!(out, "{} = {:?}", stringify!($name), ssr_abi::$name).unwrap(); })+};
    }
    emit!(
        ABI_VERSION,
        CONTROL_DEVICE_PATH,
        IOCTL_SSR_QUERY_INFO,
        IOCTL_SSR_READ,
        IOCTL_SSR_SET_RATE_TRIM,
        IOCTL_SSR_GET_STATS,
        RECORD_KIND_DATA,
        RECORD_KIND_FORMAT_CHANGE,
        RECORD_KIND_STREAM_START,
        RECORD_KIND_STREAM_STOP,
        RECORD_KIND_OVERRUN,
        RECORD_KIND_VOLUME,
        MAX_VOLUME_CHANNELS,
        VOLUME_MIN_Q16,
        VOLUME_MAX_Q16,
        VOLUME_STEP_Q16,
        RECORD_KIND_RECORDER_MIN,
        RECORD_KIND_READ_END,
        RECORD_KIND_RECORDER_GAP,
        RECORD_ENDPOINT_RECORDER,
        ENDPOINT_PRIMARY,
        ENDPOINT_SECOND,
        ENDPOINT_PASSTHROUGH,
        MAX_ENDPOINTS,
        DEFAULT_ENDPOINT_MASK,
        RECORD_FLAG_DISCONTINUITY,
        RECORD_FLAG_REPEAT,
        RECORD_ALIGN,
        MAX_DATA_PAYLOAD,
        READ_MAX_BYTES,
        TRIM_LIMIT_PPM,
        PCM_RATE_COUNT,
        PCM_SHAPE_INT16,
        PCM_SHAPE_INT24,
        PCM_SHAPE_INT24_IN_32,
        PCM_SHAPE_INT32,
        PCM_SHAPE_FLOAT32,
        PCM_SHAPES_INTEGER,
        PCM_SPEAKER_BITS,
        MAX_PCM_CHANNELS,
        DIRECT_DEFAULT_CHANNELS,
        FORMAT_AC3,
        FORMAT_EAC3,
        FORMAT_EAC3_JOC,
        FORMAT_TRUEHD,
        FORMAT_MAT20,
        FORMAT_MAT21,
        FORMAT_DTS,
        FORMAT_DTS_HD,
        COMPILED_CODECS,
        DEFAULT_ENABLED_CODECS,
        FEATURE_PENDING_READ,
        FEATURE_RATE_TRIM,
        FEATURE_WRITE_PACKET,
        ENDPOINT_STATE_STREAMING,
        DUMP_FORMAT_VERSION,
    );
    writeln!(out, "PCM_RATES = {:?}", ssr_abi::PCM_RATES).unwrap();
    writeln!(
        out,
        "DUMP_MAGIC = b{:?}",
        String::from_utf8_lossy(&ssr_abi::DUMP_MAGIC)
    )
    .unwrap();
    writeln!(
        out,
        "LEGACY_DUMP_MAGIC = b{:?}",
        String::from_utf8_lossy(&ssr_abi::LEGACY_DUMP_MAGIC)
    )
    .unwrap();
}

fn generate() -> String {
    let mut out = String::from(
        "# Generated from crates/ssr-abi/src/wire.rs by crates/ssr-abi/tests/py_module.rs.\n\
         # Do not edit: change the Rust contract and run the test with SSR_ABI_BLESS=1.\n\n\
         import struct\n\n",
    );
    constants(&mut out);
    for layout in layouts() {
        writeln!(out, "\n{}_FIELDS = (", layout.python).unwrap();
        for (field, _) in &layout.fields {
            writeln!(out, "    {:?},", field).unwrap();
        }
        writeln!(
            out,
            ")\n{} = struct.Struct({:?})",
            layout.python, layout.format
        )
        .unwrap();
    }
    out
}

fn parse_format(format: &str) -> Vec<usize> {
    assert!(
        format.starts_with('<'),
        "Python ABI format must use explicit little endian: {format}"
    );
    let mut offsets = Vec::new();
    let mut offset = 0;
    let mut count = 0usize;
    for ch in format[1..].chars() {
        if let Some(digit) = ch.to_digit(10) {
            count = count * 10 + digit as usize;
            continue;
        }
        let repeat = if count == 0 { 1 } else { count };
        count = 0;
        let size = match ch {
            'b' | 'B' => 1,
            'h' | 'H' => 2,
            'i' | 'I' | 'l' | 'L' | 'f' => 4,
            'q' | 'Q' | 'd' => 8,
            's' => {
                offsets.push(offset);
                offset += repeat;
                continue;
            }
            'x' => {
                offset += repeat;
                continue;
            }
            _ => panic!("unsupported Python ABI format code {ch:?} in {format}"),
        };
        for _ in 0..repeat {
            offsets.push(offset);
            offset += size;
        }
    }
    assert_eq!(count, 0, "format ends in a repeat count: {format}");
    offsets.push(offset); // final size
    offsets
}

#[test]
fn python_formats_match_rust_layouts() {
    let generated = generate();
    for layout in layouts() {
        let needle = format!("{} = struct.Struct(\"", layout.python);
        let rest = generated
            .split_once(&needle)
            .unwrap_or_else(|| panic!("generated module lacks {}", layout.python))
            .1;
        let format = rest.split_once('"').unwrap().0;
        let mut offsets = parse_format(format);
        let parsed_size = offsets.pop().unwrap();
        assert_eq!(parsed_size, layout.size, "{} size", layout.python);
        assert_eq!(
            offsets.len(),
            layout.fields.len(),
            "{} field count",
            layout.python
        );
        for ((name, expected), actual) in layout.fields.iter().zip(offsets) {
            assert_eq!(actual, *expected, "{}.{} offset", layout.python, name);
        }
    }
}

#[test]
fn python_module_matches_the_rust_layout() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tools/ci/ssr_abi.py");
    let generated = generate();
    if std::env::var_os("SSR_ABI_BLESS").is_some() {
        std::fs::write(&path, &generated).unwrap();
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}; run with SSR_ABI_BLESS=1", path.display()))
        .replace("\r\n", "\n");
    assert!(
        committed == generated,
        "{} is stale: the Rust contract changed. Review the change, then run \
         `SSR_ABI_BLESS=1 cargo test -p ssr-abi --test py_module`.",
        path.display()
    );
}
