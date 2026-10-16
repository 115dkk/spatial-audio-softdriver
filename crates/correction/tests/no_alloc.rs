#![forbid(unsafe_code)]

use std::path::PathBuf;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn every_processing_variant_and_live_tail_do_not_allocate() {
    let directory = temporary_directory();
    let wav = directory.join("fir.wav");
    write_wav(&wav);
    let yaml = r#"
description: "inputs 0 L, 1 R | outputs 0 L, 1 R | generated"
devices:
  samplerate: 48000
  capture: {channels: 2}
  playback: {channels: 2}
filters:
  peak: {type: Biquad, parameters: {type: Peaking, freq: 1000, q: 1, gain: 3}}
  allpass: {type: Biquad, parameters: {type: Allpass, freq: 800, q: 0.7}}
  bwlp: {type: BiquadCombo, parameters: {type: ButterworthLowpass, freq: 8000, order: 3}}
  bwhp: {type: BiquadCombo, parameters: {type: ButterworthHighpass, freq: 40, order: 4}}
  lrlp: {type: BiquadCombo, parameters: {type: LinkwitzRileyLowpass, freq: 6000, order: 4}}
  lrhp: {type: BiquadCombo, parameters: {type: LinkwitzRileyHighpass, freq: 30, order: 8}}
  fir: {type: Conv, parameters: {type: Wav, filename: fir.wav}}
  delay: {type: Delay, parameters: {delay: 20, unit: samples}}
  gain: {type: Gain, parameters: {gain: 0.5, scale: linear, inverted: true}}
mixers:
  split:
    channels: {in: 2, out: 3}
    mapping:
      - {dest: 0, sources: [{channel: 0}]}
      - {dest: 1, sources: [{channel: 1}]}
      - {dest: 2, sources: [{channel: 0, gain: -6, scale: dB}, {channel: 1, inverted: true}]}
  bus:
    channels: {in: 3, out: 2}
    mapping:
      - {dest: 0, sources: [{channel: 0}, {channel: 2, gain: 0.25, scale: linear}]}
      - {dest: 1, sources: [{channel: 1}]}
pipeline:
  - {type: Filter, channels: [0], names: [peak, allpass, bwlp, bwhp, lrlp, lrhp, fir, delay, gain]}
  - {type: Mixer, name: split}
  - {type: Mixer, name: bus}
"#;
    let correction = correction::parse(yaml, &directory).unwrap();
    let prepared = correction.prepare(48_000, 16).unwrap();
    let mut state = correction::CorrectionState::new(&prepared, 32);
    let mut output = [0.0_f32; 64];

    assert_no_alloc(|| {
        state.input(0, 16)[0] = 1.0;
        state.input(1, 16)[0] = -0.25;
        state.run(&prepared, 16, &mut output[..32]);
        for _ in 0..8 {
            state.run(&prepared, 16, &mut output[..32]);
        }
        state.reset();
        state.input(0, 32)[0] = 0.5;
        state.run(&prepared, 32, &mut output);
    });

    std::fs::remove_dir_all(directory).unwrap();
}

fn temporary_directory() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "correction-no-alloc-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn write_wav(path: &std::path::Path) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for sample in [1.0_f32, 0.5, -0.25, 0.125, 0.0, 0.0, 0.0, 0.0] {
        writer.write_sample(sample).unwrap();
    }
    writer.finalize().unwrap();
}
