#![forbid(unsafe_code)]

//! Generates the C header the driver includes from `src/wire.rs` and checks it
//! against the committed `include/bioatmos_abi.h`.
//!
//! The header ends with `BIO_STATIC_ASSERT` lines whose numbers come from the
//! Rust layout (`size_of`, `offset_of`) at test time, so a field that moves on
//! either side breaks the driver build or this test. Run with
//! `BIO_ABI_BLESS=1` to rewrite the committed header after a deliberate change
//! (which, after v1 is frozen, needs a new ABI version and an ADR).

use std::fmt::Write as _;
use std::mem::{offset_of, size_of};
use std::path::Path;

use bio_abi::{
    AbiInfo, BioStats, DEFAULT_ENABLED_CODECS, DumpHeader, EndpointFormats, EndpointStats,
    OverrunPayload, PcmFormats, RateTrim, ReadRequest, RecordHeader, VolumePayload,
};

const RENAMES: [(&str, &str); 11] = [
    ("PcmFormats", "BIO_PCM_FORMATS"),
    ("EndpointFormats", "BIO_ENDPOINT_FORMATS"),
    ("RecordHeader", "BIO_RECORD_HEADER"),
    ("OverrunPayload", "BIO_OVERRUN_PAYLOAD"),
    ("VolumePayload", "BIO_VOLUME_PAYLOAD"),
    ("ReadRequest", "BIO_READ_REQUEST"),
    ("RateTrim", "BIO_RATE_TRIM"),
    ("AbiInfo", "BIO_ABI_INFO"),
    ("EndpointStats", "BIO_ENDPOINT_STATS"),
    ("BioStats", "BIO_STATS"),
    ("DumpHeader", "BIO_DUMP_HEADER"),
];

macro_rules! assert_layout {
    ($out:ident, $rust:ty, $c:literal, [$($field:ident),* $(,)?]) => {
        writeln!(
            $out,
            r#"BIO_STATIC_ASSERT(sizeof({c}) == {size}, "{c} size");"#,
            c = $c,
            size = size_of::<$rust>()
        )
        .unwrap();
        $(
            writeln!(
                $out,
                r#"BIO_STATIC_ASSERT(offsetof({c}, {f}) == {off}, "{c}.{f} offset");"#,
                c = $c,
                f = stringify!($field),
                off = offset_of!($rust, $field)
            )
            .unwrap();
        )*
    };
}

fn layout_trailer() -> String {
    // MSVC compiles C as C89 unless told /std:c11, so the last branch is the
    // negative-array-size form every C compiler rejects.
    let mut t = String::from(
        "\n#if defined(__cplusplus)\n\
         #define BIO_STATIC_ASSERT(e, m) static_assert(e, m)\n\
         #elif defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L\n\
         #define BIO_STATIC_ASSERT(e, m) _Static_assert(e, m)\n\
         #else\n\
         #define BIO_ASSERT_CAT_(a, b) a##b\n\
         #define BIO_ASSERT_CAT(a, b) BIO_ASSERT_CAT_(a, b)\n\
         #define BIO_STATIC_ASSERT(e, m) typedef char BIO_ASSERT_CAT(bio_static_assert_, __LINE__)[(e) ? 1 : -1]\n\
         #endif\n\n",
    );
    assert_layout!(
        t,
        RecordHeader,
        "BIO_RECORD_HEADER",
        [
            kind,
            flags,
            epoch,
            byte_pos,
            qpc,
            qpc_written,
            len,
            endpoint,
            reserved
        ]
    );
    assert_layout!(
        t,
        VolumePayload,
        "BIO_VOLUME_PAYLOAD",
        [channel_count, mute_mask, level_q16]
    );
    writeln!(
        t,
        r#"BIO_STATIC_ASSERT(BIO_MAX_VOLUME_CHANNELS == 32, "volume channels");"#
    )
    .unwrap();
    assert_layout!(t, OverrunPayload, "BIO_OVERRUN_PAYLOAD", [lost_bytes]);
    assert_layout!(t, ReadRequest, "BIO_READ_REQUEST", [min_bytes, timeout_ms]);
    assert_layout!(t, RateTrim, "BIO_RATE_TRIM", [ppm_q16, reserved]);
    assert_layout!(
        t,
        PcmFormats,
        "BIO_PCM_FORMATS",
        [rates, shapes, max_masked_channels, direct_channels]
    );
    assert_layout!(t, EndpointFormats, "BIO_ENDPOINT_FORMATS", [codecs, pcm]);
    assert_layout!(
        t,
        AbiInfo,
        "BIO_ABI_INFO",
        [
            abi_version,
            struct_size,
            driver_version,
            ring_bytes,
            compiled,
            features,
            qpc_frequency,
            endpoint_mask,
            reserved,
            endpoints
        ]
    );
    assert_layout!(
        t,
        EndpointStats,
        "BIO_ENDPOINT_STATS",
        [total_bytes, overruns, lost_bytes, epoch, state]
    );
    assert_layout!(
        t,
        BioStats,
        "BIO_STATS",
        [
            struct_size,
            open_count,
            max_timer_lateness_us,
            reserved,
            endpoints
        ]
    );
    assert_layout!(
        t,
        DumpHeader,
        "BIO_DUMP_HEADER",
        [
            magic,
            abi_version,
            header_size,
            qpc_frequency,
            format_version,
            reserved
        ]
    );
    writeln!(
        t,
        r#"BIO_STATIC_ASSERT(BIO_MAX_ENDPOINTS == 4, "BIO_MAX_ENDPOINTS value");"#
    )
    .unwrap();
    t
}

/// Names of the `pub const` items in wire.rs, so every constant gets a `BIO_`
/// prefix in C (the driver includes this header next to WDK headers).
fn constant_names(src: &str) -> Vec<String> {
    src.lines()
        .filter_map(|line| line.trim_start().strip_prefix("pub const "))
        .filter_map(|rest| rest.split(':').next())
        .filter(|name| !name.contains(' ') && !name.contains('('))
        .map(str::to_owned)
        .collect()
}

fn generate() -> String {
    let wire = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("wire.rs");
    let src = std::fs::read_to_string(&wire).unwrap();
    // cbindgen's own include guard would close before the trailer, leaving the
    // layout assertions outside it, so the guard is part of header and trailer.
    let mut config = cbindgen::Config {
        language: cbindgen::Language::C,
        header: Some(
            "/* Generated from crates/bio-abi/src/wire.rs by crates/bio-abi/tests/c_header.rs.\n * Do not edit: change the Rust side and run the test with BIO_ABI_BLESS=1. */\n\n#ifndef BIOATMOS_ABI_H\n#define BIOATMOS_ABI_H"
                .into(),
        ),
        no_includes: true,
        sys_includes: vec!["stddef.h".into(), "stdint.h".into()],
        style: cbindgen::Style::Both,
        // cbindgen omits &str constants; derive the C literal from the Rust
        // value rather than keeping a second spelling of the device path.
        trailer: Some(format!(
            "#define BIO_CONTROL_DEVICE_PATH {:?}\n{}\n#endif /* BIOATMOS_ABI_H */",
            bio_abi::CONTROL_DEVICE_PATH,
            layout_trailer()
        )),
        documentation: true,
        ..Default::default()
    };
    for (rust, c) in RENAMES {
        config.export.include.push(rust.into());
        config.export.rename.insert(rust.into(), c.into());
    }
    for name in constant_names(&src) {
        if !name.starts_with("IOCTL_BIO_") {
            config
                .export
                .rename
                .insert(name.clone(), format!("BIO_{name}"));
        }
    }
    let mut out = Vec::new();
    cbindgen::Builder::new()
        .with_config(config)
        .with_src(wire)
        .generate()
        .expect("cbindgen parses src/wire.rs")
        .write(&mut out);
    let generated = String::from_utf8(out).expect("cbindgen writes UTF-8");
    // C89 and C++17 share aggregate initialization, not C99 compound literals.
    // The PCM structs also get an initializer-list macro: a `static const`
    // struct is not a constant expression in C++, and a kernel image has no
    // CRT to run the dynamic initializer that copying it at namespace scope
    // would need.
    generated
        .lines()
        .map(|line| {
            if let Some(values) = line.strip_prefix("#define BIO_PCM_RATES ") {
                format!("static const uint32_t BIO_PCM_RATES[6] = {values};")
            } else if line.starts_with("#define BIO_DEFAULT_PCM ")
                || line.starts_with("#define BIO_COMPILED_PCM ")
            {
                let (name, value) = if line.starts_with("#define BIO_DEFAULT_PCM ") {
                    ("BIO_DEFAULT_PCM", bio_abi::DEFAULT_PCM)
                } else {
                    ("BIO_COMPILED_PCM", bio_abi::COMPILED_PCM)
                };
                format!(
                    "#define {name}_INIT {{ {}, {}, {}, {}U }}\n\n\
                     static const BIO_PCM_FORMATS {name} = {name}_INIT;",
                    value.rates, value.shapes, value.max_masked_channels, value.direct_channels
                )
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn parse_c_decimal_define(header: &str, name: &str) -> u64 {
    let prefix = format!("#define {name} ");
    header
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("header lacks {name}"))
        .parse()
        .unwrap_or_else(|e| panic!("invalid {name}: {e}"))
}

fn parse_inf_qword(inf: &str, name: &str) -> u64 {
    let prefix = format!("HKR,Parameters,{name},0x000B0001,");
    let bytes = inf
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("INF lacks {name}"))
        .split(',')
        .map(|byte| u8::from_str_radix(byte.trim(), 16).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(bytes.len(), 8, "{name} must contain one QWORD");
    u64::from_le_bytes(bytes.try_into().unwrap())
}

#[test]
fn default_capabilities_match_header_and_inf() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let header = generate();
    assert_eq!(
        parse_c_decimal_define(&header, "BIO_DEFAULT_ENABLED_CODECS"),
        DEFAULT_ENABLED_CODECS
    );
    let bytes = std::fs::read(root.join("driver/bioatmos/spatialsoftreceiver.inx")).unwrap();
    assert!(
        bytes.starts_with(&[0xff, 0xfe]),
        "INF requires UTF-16 LE BOM"
    );
    assert_eq!(bytes.len() % 2, 0);
    let units: Vec<_> = bytes[2..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    let inf = String::from_utf16(&units).unwrap();
    for index in 0..4 {
        let name = format!("CodecMask{index}");
        assert_eq!(parse_inf_qword(&inf, &name), DEFAULT_ENABLED_CODECS);
        for (field, expected) in [
            ("PcmRateMask", bio_abi::DEFAULT_PCM.rates),
            ("PcmShapeMask", bio_abi::DEFAULT_PCM.shapes),
            (
                "PcmMaxMaskedChannels",
                bio_abi::DEFAULT_PCM.max_masked_channels,
            ),
            ("PcmDirectChannels", bio_abi::DEFAULT_PCM.direct_channels),
        ] {
            let prefix = format!("HKR,Parameters,{field}{index},0x00010001,");
            let raw = inf
                .lines()
                .find_map(|line| line.strip_prefix(&prefix))
                .unwrap_or_else(|| panic!("INF lacks {field}{index}"));
            let actual = if let Some(hex) = raw.strip_prefix("0x") {
                u32::from_str_radix(hex, 16).unwrap()
            } else {
                raw.parse().unwrap()
            };
            assert_eq!(actual, expected, "{field}{index}");
        }
    }
}

#[test]
fn control_device_path_matches_generated_header() {
    assert_eq!(bio_abi::DEVICE_PATH, bio_abi::CONTROL_DEVICE_PATH);
    let header = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("include/bioatmos_abi.h"),
    )
    .unwrap();
    assert!(header.contains(&format!(
        "#define BIO_CONTROL_DEVICE_PATH {:?}",
        bio_abi::CONTROL_DEVICE_PATH
    )));
}

#[test]
fn c_header_matches_the_rust_layout() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("include")
        .join("bioatmos_abi.h");
    let generated = generate();
    if std::env::var_os("BIO_ABI_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &generated).unwrap();
    }
    for (_, c) in RENAMES {
        assert!(
            generated.contains(&format!("}} {c};")),
            "generated header lacks {c}"
        );
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}; run with BIO_ABI_BLESS=1", path.display()))
        .replace("\r\n", "\n");
    assert!(
        committed == generated,
        "{} is stale: the Rust contract changed. Review the change, then run \
         `BIO_ABI_BLESS=1 cargo test -p bio-abi --test c_header`.",
        path.display()
    );
}
