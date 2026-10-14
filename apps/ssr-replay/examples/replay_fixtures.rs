#![forbid(unsafe_code)]

//! Writes the inputs of the replay determinism check (ci.yml, `replay`): a
//! synthetic 11-direction BRIR folder with its profile, a 7.1 PCM dump and a
//! DD+ 5.1 dump built from the decode crate's E-AC-3 fixture.
//!
//! ```text
//! cargo run -p ssr-replay --example replay_fixtures -- <out-dir>
//! ```
//!
//! Everything is generated from fixed seeds, so the same commit always writes
//! the same bytes; the check then renders the dumps with two builds of
//! ssr-replay and compares the output hashes.

use std::path::{Path, PathBuf};

use bitstream::{DataType, frame_burst};
use ssr_abi::{RecordWriter, dump_header_bytes, record_size, wave_format_bytes};
use ssr_types::{BitstreamCodec, BitstreamFormat, PcmFormat, SampleType, SourceFormat};

const RATE: u32 = 48_000;
/// Fake QPC ticks per second, as Windows reports on most machines.
const QPC_HZ: u64 = 10_000_000;

/// (name, azimuth) of the synthetic set; tops sit at 45 degrees elevation.
const DIRECTIONS: [(&str, f32, f32); 11] = [
    ("FL", 30.0, 0.0),
    ("FR", -30.0, 0.0),
    ("FC", 0.0, 0.0),
    ("SL", 90.0, 0.0),
    ("SR", -90.0, 0.0),
    ("BL", 150.0, 0.0),
    ("BR", -150.0, 0.0),
    ("TFL", 45.0, 45.0),
    ("TFR", -45.0, 45.0),
    ("TBL", 135.0, 45.0),
    ("TBR", -135.0, 45.0),
];

/// xorshift32: deterministic noise without a dependency.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

fn write_stereo(path: &Path, left: &[f32], right: &[f32]) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).expect("create WAV");
    for (l, r) in left.iter().zip(right) {
        w.write_sample(*l).expect("write");
        w.write_sample(*r).expect("write");
    }
    w.finalize().expect("finalize WAV");
}

/// A 100 ms BRIR per direction: a direct impulse whose interaural delay and
/// level follow the azimuth, then decaying noise.
fn write_brir_set(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create brir dir");
    for (i, (name, azimuth, _)) in DIRECTIONS.iter().enumerate() {
        let frames = 4_800;
        let mut noise = Noise(0x9E37_79B9 ^ (i as u32 + 1));
        let lateral = azimuth.to_radians().sin(); // +1 left, -1 right
        let itd = (lateral.abs() * 30.0) as usize; // up to 30 samples (0.6 ms)
        let (near, far) = (1.0, 1.0 - 0.5 * lateral.abs());
        let (left_gain, right_gain) = if lateral >= 0.0 {
            (near, far)
        } else {
            (far, near)
        };
        let (left_delay, right_delay) = if lateral >= 0.0 {
            (8, 8 + itd)
        } else {
            (8 + itd, 8)
        };
        let mut left = vec![0.0_f32; frames];
        let mut right = vec![0.0_f32; frames];
        left[left_delay] = left_gain;
        right[right_delay] = right_gain;
        for n in 64..frames {
            let decay = (-(n as f32) / 900.0).exp() * 0.05;
            left[n] += noise.next() * decay * left_gain;
            right[n] += noise.next() * decay * right_gain;
        }
        write_stereo(&dir.join(format!("{name}.wav")), &left, &right);
    }
}

fn write_profile(path: &Path) {
    let text = "name = \"replay-fixture\"\n\
                gain_db = -6.0\n\
                limiter_ceiling_dbfs = -1.0\n\
                angles = \"standard\"\n\n\
                [brir]\n\
                folder = \"brir\"\n\n\
                [lfe]\n\
                direction = \"FC\"\n\
                gain_db = 0.0\n";
    std::fs::write(path, text).expect("write profile");
}

fn finish_dump(path: &Path, records: &[u8]) {
    let mut file = dump_header_bytes(QPC_HZ);
    file.extend_from_slice(records);
    std::fs::write(path, file).expect("write dump");
}

/// Two seconds of 7.1 float PCM, one tone per channel, in 10 ms records.
fn write_pcm_dump(path: &Path) {
    let channels = 8_usize;
    let format = SourceFormat::Pcm(PcmFormat {
        channels: channels as u16,
        sample_rate: RATE,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask: 0x63f,
    });
    let block = (RATE / 100) as usize;
    let blocks = 200;
    let mut stream =
        vec![0_u8; record_size(0) + record_size(52) + blocks * record_size(block * channels * 4)];
    let mut w = RecordWriter::new(&mut stream);
    assert!(w.stream_start(1, 0));
    assert!(w.format_change(1, 0, &wave_format_bytes(&format)));
    let mut payload = vec![0_u8; block * channels * 4];
    for b in 0..blocks {
        for f in 0..block {
            let t = (b * block + f) as f32 / RATE as f32;
            for c in 0..channels {
                let freq = 220.0 * (c as f32 + 1.0);
                let s = (std::f32::consts::TAU * freq * t).sin() * 0.2;
                let at = (f * channels + c) * 4;
                payload[at..at + 4].copy_from_slice(&s.to_le_bytes());
            }
        }
        let qpc = (b as u64 + 1) * QPC_HZ / 100;
        let pos = (b * payload.len()) as u64;
        assert!(w.data(1, pos, qpc, &payload));
    }
    let n = w.len();
    finish_dump(path, &stream[..n]);
}

/// The decode fixture's E-AC-3 syncframes, one per IEC 61937 burst.
fn write_ddp_dump(path: &Path, eac3: &Path) {
    let bytes = std::fs::read(eac3).expect("read E-AC-3 fixture");
    let mut frames = Vec::new();
    let mut rest = bytes.as_slice();
    while rest.len() >= 4 {
        assert_eq!(&rest[..2], &[0x0B, 0x77], "E-AC-3 syncword");
        // frmsiz: 11 bits after strmtyp (2) and substreamid (3); words - 1.
        let frmsiz = (usize::from(rest[2] & 0x07) << 8) | usize::from(rest[3]);
        let size = (frmsiz + 1) * 2;
        frames.push(&rest[..size]);
        rest = &rest[size..];
    }
    let format = SourceFormat::Bitstream(BitstreamFormat {
        codec: BitstreamCodec::Eac3,
        container_rate: 192_000,
        container_channels: 2,
        encoded_rate: RATE,
        encoded_channels: 6,
    });
    let period = 24_576;
    let mut stream =
        vec![0_u8; record_size(0) + record_size(52) + frames.len() * record_size(period)];
    let mut w = RecordWriter::new(&mut stream);
    assert!(w.stream_start(1, 0));
    assert!(w.format_change(1, 0, &wave_format_bytes(&format)));
    let mut burst = Vec::with_capacity(period);
    for (i, frame) in frames.iter().enumerate() {
        burst.clear();
        frame_burst(DataType::Eac3, frame, period, &mut burst);
        // One access unit is 32 ms of audio.
        let qpc = (i as u64 + 1) * QPC_HZ * 32 / 1000;
        assert!(w.data(1, (i * period) as u64, qpc, &burst));
    }
    let n = w.len();
    finish_dump(path, &stream[..n]);
}

fn main() {
    let out: PathBuf = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: replay_fixtures <out-dir>");
    std::fs::create_dir_all(&out).expect("create output dir");
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let eac3 = manifest.join("../../crates/decode/tests/fixtures/ddp/tones-5.1.eac3");
    write_brir_set(&out.join("brir"));
    write_profile(&out.join("profile.toml"));
    write_pcm_dump(&out.join("pcm71.ssrdump"));
    write_ddp_dump(&out.join("ddp51.ssrdump"), &eac3);
    println!("{}", out.display());
}
