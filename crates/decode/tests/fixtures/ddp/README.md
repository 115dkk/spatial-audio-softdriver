# Synthetic DD+ 5.1 fixture

`tones-5.1.eac3` is synthetic audio generated for these tests, not captured
commercial audio. It contains no JOC metadata. It may be redistributed under
the repository's MIT license.

Generated on 2026-10-11 using the local Windows executable
`C:\Users\32170336\AppData\Local\Programs\Python\Python313\Scripts\ffmpeg.exe`.
Version: `N-55702-g920046a`, built 2013-08-21 with GCC 4.7.3;
libavcodec 55.29.100, libavformat 55.14.101. `ffmpeg -encoders` lists `eac3`.

Exact command (run from the repository root; the quoted expression is one argument):

```text
ffmpeg.exe -y -f lavfi -i "aevalsrc=0.1*sin(2*PI*375*t)|0.1*sin(2*PI*562.5*t)|0.1*sin(2*PI*750*t)|0.1*sin(2*PI*62.5*t)|0.1*sin(2*PI*937.5*t)|0.1*sin(2*PI*1125*t):s=48000:d=1:c=5.1" -c:a eac3 -b:a 640k -f eac3 crates/decode/tests/fixtures/ddp/tones-5.1.eac3
```

The source lasts one second, with amplitude 0.1 per channel. WAV/mask input
order is FL 375 Hz, FR 562.5 Hz, FC 750 Hz, LFE 62.5 Hz, BL 937.5 Hz,
BR 1125 Hz. E-AC-3's 3/2 surround pair is decoded as Ls/Rs at +/-110 degrees;
it does not preserve the WAV back-versus-side distinction.
The file contains 32 independent 2560-byte syncframes, each with 1536 samples
at 48 kHz (49152 decoded samples including encoder delay/padding), 81920 bytes
in total. The tests skip the initial and final padded frames for tone checks.
Frequencies are integer multiples of 31.25 Hz, the 1536-sample DFT resolution.

SHA-256:
`8cb8efb7e99d5dd698a5b7f79a010b0ba5ac43aeca8f49d56f077af7d6ecda54`

## JOC coverage

No JOC vector is redistributed here. Upstream harletty-bridge contains
`eac3/tests/data/short_packet_independent_joc.bin`, described as a captured
Atmos frame, but its recording provenance and redistribution rights were not
established. The crate's Apache-2.0 license alone does not establish rights to
the captured programme.

The ignored `ddp_joc_external_sample_has_positioned_objects` integration test
reads a raw E-AC-3 stream from `BIO_DDP_JOC_SAMPLE`. Supply a locally permitted
sample, then run `cargo test -p decode ddp_joc_external_sample -- --ignored`.
The JOC bitstream path remains unverified without that test; synthetic unit
tests of coordinates are not a JOC decoding golden test.
