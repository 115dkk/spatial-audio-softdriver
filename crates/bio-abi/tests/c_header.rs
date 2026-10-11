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

use bio_abi::{AbiInfo, BioStats, DumpHeader, OverrunPayload, RateTrim, ReadRequest, RecordHeader};

const RENAMES: [(&str, &str); 7] = [
    ("RecordHeader", "BIO_RECORD_HEADER"),
    ("OverrunPayload", "BIO_OVERRUN_PAYLOAD"),
    ("ReadRequest", "BIO_READ_REQUEST"),
    ("RateTrim", "BIO_RATE_TRIM"),
    ("AbiInfo", "BIO_ABI_INFO"),
    ("BioStats", "BIO_STATS"),
    ("DumpHeader", "BIO_DUMP_HEADER"),
];

macro_rules! assert_layout {
    ($out:ident, $rust:ty, $c:literal, [$($field:ident),* $(,)?]) => {
        writeln!(
            $out,
            "BIO_STATIC_ASSERT(sizeof({c}) == {size}, \"{c} size\");",
            c = $c,
            size = size_of::<$rust>()
        )
        .unwrap();
        $(
            writeln!(
                $out,
                "BIO_STATIC_ASSERT(offsetof({c}, {f}) == {off}, \"{c}.{f} offset\");",
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
    assert_layout!(t, RecordHeader, "BIO_RECORD_HEADER", [
        kind,
        flags,
        epoch,
        byte_pos,
        qpc,
        qpc_written,
        len,
        reserved
    ]);
    assert_layout!(t, OverrunPayload, "BIO_OVERRUN_PAYLOAD", [lost_bytes]);
    assert_layout!(t, ReadRequest, "BIO_READ_REQUEST", [min_bytes, timeout_ms]);
    assert_layout!(t, RateTrim, "BIO_RATE_TRIM", [ppm_q16, reserved]);
    assert_layout!(t, AbiInfo, "BIO_ABI_INFO", [
        abi_version,
        struct_size,
        driver_version,
        ring_bytes,
        compiled_formats,
        enabled_formats,
        features,
        qpc_frequency
    ]);
    assert_layout!(t, BioStats, "BIO_STATS", [
        struct_size,
        open_count,
        total_bytes,
        overruns,
        lost_bytes,
        max_timer_lateness_us,
        reserved
    ]);
    assert_layout!(t, DumpHeader, "BIO_DUMP_HEADER", [
        magic,
        abi_version,
        header_size,
        qpc_frequency,
        reserved
    ]);
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
        trailer: Some(format!("{}\n#endif /* BIOATMOS_ABI_H */", layout_trailer())),
        documentation: true,
        ..Default::default()
    };
    for (rust, c) in RENAMES {
        config.export.include.push(rust.into());
        config.export.rename.insert(rust.into(), c.into());
    }
    for name in constant_names(&src) {
        if !name.starts_with("IOCTL_BIO_") {
            config.export.rename.insert(name.clone(), format!("BIO_{name}"));
        }
    }
    let mut out = Vec::new();
    cbindgen::Builder::new()
        .with_config(config)
        .with_src(wire)
        .generate()
        .expect("cbindgen parses src/wire.rs")
        .write(&mut out);
    String::from_utf8(out).expect("cbindgen writes UTF-8")
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
