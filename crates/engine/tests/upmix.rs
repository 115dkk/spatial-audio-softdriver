#![forbid(unsafe_code)]

//! The upmix stage inside the chain (ADR 0021, ADR 0035), and its upmixers.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use engine::{
    Chain, ChainConfig, ChainEvent, MatrixUpmixer, Prepared, RingFault, RingUpmixer, SpeakerFault,
    UpmixError, UpmixMode, UpmixStage, UpmixStatus, Upmixer,
};
use speakers::{PreparedSpeaker, PreparedSpeakers, PreparedSubwoofer};
use ssr_abi::Record;
use ssr_types::app::UpmixBypass;
use ssr_types::{
    BitstreamCodec, BitstreamFormat, Direction, Layout, PcmFormat, SampleType, SourceFormat,
    Speaker,
};
use streamring::{Consumer, Geometry, InProcessEvent, Producer, Publisher, RegionBuf, State};

const QPC: u64 = 10_000_000;
const RATE: u32 = 48_000;
const B: usize = 480;
const C: usize = 96;

/// Device channels in this order, each its speaker's feed unchanged; LFE
/// goes to channel 5.
const FL: usize = 0;
const FR: usize = 1;
const FC: usize = 2;
const SL: usize = 3;

fn surround_speakers() -> Box<Prepared> {
    let directions = [
        ("FL", 30.0),
        ("FR", -30.0),
        ("FC", 0.0),
        ("SL", 110.0),
        ("SR", -110.0),
    ];
    let layout = Layout::new(
        directions
            .iter()
            .map(|(name, azimuth)| Direction::new(*name, *azimuth, 0.0))
            .collect(),
    )
    .unwrap();
    let speakers = directions
        .iter()
        .enumerate()
        .map(|(channel, (name, _))| PreparedSpeaker {
            name: (*name).into(),
            device_channel: channel,
            small: false,
            crossover: None,
            delay_frames: 0,
            gain: 1.0,
            fir: None,
        })
        .collect();
    Box::new(
        Prepared::speakers(
            Arc::new(PreparedSpeakers {
                layout,
                rate: RATE,
                partition: B,
                device_channels: 6,
                channel_mask: 0x3f,
                speakers,
                subwoofers: vec![PreparedSubwoofer {
                    device_channel: 5,
                    delay_frames: 0,
                    gain: 1.0,
                    fir: None,
                }],
                bass_to: Vec::new(),
                lfe_gain: 1.0,
                gain: 1.0,
                limiter_ceiling: 1.0,
            }),
            B,
        )
        .unwrap(),
    )
}

fn pcm(rate: u32, channels: u16, channel_mask: u32) -> SourceFormat {
    SourceFormat::Pcm(PcmFormat {
        channels,
        sample_rate: rate,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: SampleType::Float,
        channel_mask,
    })
}

fn stereo() -> SourceFormat {
    pcm(RATE, 2, 0x3)
}

fn eac3() -> SourceFormat {
    SourceFormat::Bitstream(BitstreamFormat {
        codec: BitstreamCodec::Eac3,
        container_rate: 192_000,
        container_channels: 2,
        encoded_rate: RATE,
        encoded_channels: 6,
    })
}

fn stage(endpoint: u16, upmixer: impl Upmixer + 'static) -> Box<UpmixStage> {
    Box::new(UpmixStage::new(endpoint, Box::new(upmixer), QPC).unwrap())
}

/// A deterministic signal in [-0.25, 0.25).
fn noise(frames: usize, seed: u32) -> Vec<f32> {
    let mut state = seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    (0..frames)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((state >> 8) as f32 / (1 << 24) as f32 - 0.5) * 0.5
        })
        .collect()
}

fn interleave(planes: &[Vec<f32>], start: usize, frames: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(frames * planes.len() * 4);
    for frame in start..start + frames {
        for plane in planes {
            bytes.extend_from_slice(&plane[frame].to_le_bytes());
        }
    }
    bytes
}

/// Device channels as the chain rendered them.
#[derive(Debug, Default, PartialEq)]
struct Output {
    channels: Vec<Vec<f32>>,
    errors: usize,
}

impl Output {
    fn take(&mut self, event: ChainEvent<'_>) {
        match event {
            ChainEvent::Block(block) => {
                if self.channels.is_empty() {
                    self.channels = vec![Vec::new(); block.channels];
                }
                for (channel, samples) in self.channels.iter_mut().enumerate() {
                    samples.extend_from_slice(block.channel(channel));
                }
            }
            ChainEvent::Error { .. } => self.errors += 1,
            _ => {}
        }
    }

    fn first_audible(&self, channel: usize) -> usize {
        self.channels[channel]
            .iter()
            .position(|sample| sample.abs() > 1e-6)
            .unwrap()
    }

    fn bits(&self) -> Vec<Vec<u32>> {
        self.channels
            .iter()
            .map(|channel| channel.iter().map(|sample| sample.to_bits()).collect())
            .collect()
    }
}

/// A source feeding one endpoint: a format change, then PCM records of the
/// given sizes (cycling), each stamped with its last byte's time.
struct Feed {
    endpoint: u16,
    epoch: u32,
    frame: usize,
    base_qpc: u64,
}

impl Feed {
    fn new(endpoint: u16) -> Self {
        Self {
            endpoint,
            epoch: 0,
            frame: 0,
            base_qpc: 1_000_000,
        }
    }

    fn qpc(&self, frame: usize, rate: u32) -> u64 {
        self.base_qpc + frame as u64 * QPC / u64::from(rate)
    }

    fn format(&mut self, chain: &mut Chain, format: SourceFormat, output: &mut Output) {
        self.epoch += 1;
        // A new epoch starts after what came before, on the same clock.
        self.base_qpc = self.qpc(self.frame, RATE);
        self.frame = 0;
        chain.push_record(
            Record::FormatChange {
                endpoint: self.endpoint,
                epoch: self.epoch,
                repeat: false,
                qpc: self.base_qpc,
                format,
            },
            &mut |event| output.take(event),
        );
    }

    fn data(
        &mut self,
        chain: &mut Chain,
        bytes: &[u8],
        frames: usize,
        rate: u32,
        out: &mut Output,
    ) {
        self.frame += frames;
        chain.push_record(
            Record::Data {
                endpoint: self.endpoint,
                epoch: self.epoch,
                flags: 0,
                pos: 0,
                qpc: self.qpc(self.frame, rate),
                qpc_written: 0,
                bytes,
            },
            &mut |event| out.take(event),
        );
    }

    fn pcm(
        &mut self,
        chain: &mut Chain,
        planes: &[Vec<f32>],
        sizes: &[usize],
        rate: u32,
        output: &mut Output,
    ) {
        let frames = planes[0].len();
        let mut start = 0;
        let mut size = sizes.iter().cycle();
        while start < frames {
            let length = (*size.next().unwrap()).min(frames - start);
            let bytes = interleave(planes, start, length);
            self.data(chain, &bytes, length, rate, output);
            start += length;
        }
    }

    /// E-AC-3 syncframes `frames` of the 5.1 fixture, one burst each.
    fn eac3(&mut self, chain: &mut Chain, frames: std::ops::Range<usize>, output: &mut Output) {
        let fixture = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../decode/tests/fixtures/ddp/tones-5.1.eac3"),
        )
        .unwrap();
        for frame in frames {
            let mut burst = Vec::new();
            bitstream::frame_burst(
                bitstream::DataType::Eac3,
                &fixture[frame * 2560..(frame + 1) * 2560],
                24_576,
                &mut burst,
            );
            self.data(chain, &burst, 1_536, RATE, output);
        }
    }
}

fn chain(stage: Option<Box<UpmixStage>>, endpoint: u16) -> Chain {
    let mut chain = Chain::with_qpc_frequency(ChainConfig::default(), QPC);
    chain.set_prepared(surround_speakers());
    if stage.is_some() {
        assert!(chain.set_upmixer(endpoint, stage).is_none());
    }
    chain
}

fn status(chain: &Chain, endpoint: u16) -> UpmixStatus {
    chain.upmix_status(endpoint).unwrap()
}

#[test]
fn upmixed_pcm_leaves_the_chain_exactly_d_later() {
    for latency in [0, 37, 480] {
        let delay = 2 * B + latency + C;
        let frames = 20 * B;
        let t = 2_000;
        let mut left = vec![0.0; frames];
        left[t] = 0.5;
        let planes = [left, vec![0.0; frames]];
        let render = |stage: Option<Box<UpmixStage>>| {
            let mut chain = chain(stage, 0);
            let mut output = Output::default();
            let mut feed = Feed::new(0);
            feed.format(&mut chain, stereo(), &mut output);
            feed.pcm(&mut chain, &planes, &[B], RATE, &mut output);
            let status = status(&chain, 0);
            (output, status)
        };
        let (plain, _) = render(None);
        let (upmixed, status) = render(Some(stage(0, MatrixUpmixer::stereo_to_5_1(RATE, latency))));
        assert!(status.upmixing, "{status:?}");
        assert_eq!(status.delay_frames as usize, delay);
        assert!((status.delay_ms() - delay as f32 / 48.0).abs() < 1e-4);
        let at = plain.first_audible(FL);
        assert_eq!(upmixed.first_audible(FL), at + delay, "latency {latency}");
        assert_eq!(upmixed.channels[FL][at + delay], 0.5);
        // The centre and left surround carry half of the left impulse.
        assert_eq!(upmixed.first_audible(FC), at + delay);
        assert_eq!(upmixed.channels[FC][at + delay], 0.25);
        assert_eq!(upmixed.first_audible(SL), at + delay);
        assert_eq!(upmixed.channels[SL][at + delay], 0.25);
        assert!(upmixed.channels[FR].iter().all(|sample| *sample == 0.0));
        assert_eq!(upmixed.errors, 0);
    }
}

#[test]
fn ten_seconds_of_adversarial_records_keep_the_lane_accounting() {
    let frames = 10 * RATE as usize;
    let planes = [noise(frames, 1), noise(frames, 2)];
    let sizes = [1, 7, B - 1, B, B + 1, 3 * B + 5];
    for stage in [None, Some(stage(0, MatrixUpmixer::stereo_to_5_1(RATE, 37)))] {
        let staged = stage.is_some();
        let mut chain = chain(stage, 0);
        let mut output = Output::default();
        let mut feed = Feed::new(0);
        feed.format(&mut chain, stereo(), &mut output);
        feed.pcm(&mut chain, &planes, &sizes, RATE, &mut output);
        assert_eq!(chain.lane_underrun_frames(), 0, "staged: {staged}");
        assert_eq!(chain.dropped_frames(), 0, "staged: {staged}");
        assert_eq!(output.errors, 0);
        if staged {
            let status = status(&chain, 0);
            assert!(status.upmixing);
            assert_eq!(status.missed_blocks, 0);
            assert_eq!(status.delivered_blocks as usize, frames / B - 1);
        }
        // Every 10 ms of input left as 10 ms of output.
        assert!(output.channels[FL].len() >= frames - B);
    }
}

#[test]
fn a_5_1_bitstream_is_transparent_and_bit_exact_on_music_and_pass() {
    for endpoint in [1, 2] {
        let render = |stage: Option<Box<UpmixStage>>| {
            let mut chain = chain(stage, endpoint);
            let mut output = Output::default();
            let mut feed = Feed::new(endpoint);
            feed.format(&mut chain, eac3(), &mut output);
            feed.eac3(&mut chain, 1..9, &mut output);
            chain.finish(&mut |event| output.take(event));
            (output, chain.upmix_status(endpoint).unwrap())
        };
        let (plain, _) = render(None);
        let (staged, status) = render(Some(stage(endpoint, MatrixUpmixer::stereo_to_5_1(RATE, 0))));
        assert!(plain.channels[FC].iter().any(|sample| *sample != 0.0));
        assert_eq!(staged.bits(), plain.bits(), "endpoint {endpoint}");
        assert!(status.installed && !status.upmixing);
        assert_eq!(status.bypass, Some(UpmixBypass::NativeSurround));
    }
}

#[test]
fn native_surround_pcm_is_never_upmixed() {
    for (channels, mask) in [(6, 0x3f), (8, 0x63f)] {
        let frames = 12 * B;
        let planes: Vec<Vec<f32>> = (0..channels)
            .map(|channel| noise(frames, 10 + channel))
            .collect();
        let render = |stage: Option<Box<UpmixStage>>| {
            let mut chain = chain(stage, 0);
            let mut output = Output::default();
            let mut feed = Feed::new(0);
            feed.format(&mut chain, pcm(RATE, channels as u16, mask), &mut output);
            feed.pcm(&mut chain, &planes, &[B, 7, 333], RATE, &mut output);
            chain.finish(&mut |event| output.take(event));
            (
                output,
                chain.upmix_status(0).unwrap(),
                chain.lane_restarts(0),
            )
        };
        let (plain, _, plain_restarts) = render(None);
        let (staged, status, restarts) =
            render(Some(stage(0, MatrixUpmixer::stereo_to_5_1(RATE, 0))));
        assert_eq!(staged.bits(), plain.bits(), "{channels} channels");
        assert_eq!(restarts, plain_restarts);
        assert_eq!(status.bypass, Some(UpmixBypass::NativeSurround));
        assert!(!status.upmixing);
    }
}

#[test]
fn mono_pcm_is_upmixed_onto_left_and_right() {
    let frames = 16 * B;
    let t = 1_500;
    let mut mono = vec![0.0; frames];
    mono[t] = 0.5;
    let render = |stage: Option<Box<UpmixStage>>| {
        let mut chain = chain(stage, 0);
        let mut output = Output::default();
        let mut feed = Feed::new(0);
        feed.format(&mut chain, pcm(RATE, 1, 0x4), &mut output);
        feed.pcm(
            &mut chain,
            std::slice::from_ref(&mono),
            &[B],
            RATE,
            &mut output,
        );
        (output, chain.upmix_status(0).unwrap())
    };
    let (plain, _) = render(None);
    let (upmixed, status) = render(Some(stage(0, MatrixUpmixer::stereo_to_5_1(RATE, 0))));
    assert!(status.upmixing);
    let at = plain.first_audible(FC);
    let delay = 2 * B + C;
    assert_eq!(upmixed.first_audible(FL), at + delay);
    assert_eq!(upmixed.channels[FL], upmixed.channels[FR]);
    let side = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
    assert!((upmixed.channels[FL][at + delay] - side).abs() < 1e-6);
}

#[test]
fn transparent_stages_are_bit_exact_to_no_stage() {
    let frames = 16 * B;
    let planes = [noise(frames, 3), noise(frames, 4)];
    let render = |stage: Option<Box<UpmixStage>>, apply: bool| {
        let mut chain = chain(stage, 0);
        chain.set_upmix_apply(apply);
        let mut output = Output::default();
        let mut feed = Feed::new(0);
        feed.format(&mut chain, stereo(), &mut output);
        feed.pcm(&mut chain, &planes, &[B, 13], RATE, &mut output);
        chain.finish(&mut |event| output.take(event));
        (output, chain.upmix_status(0).unwrap())
    };
    let (plain, none) = render(None, true);
    assert_eq!(none, UpmixStatus::not_installed());
    let (kind_off, status) = render(Some(stage(0, MatrixUpmixer::stereo_to_5_1(RATE, 0))), false);
    assert_eq!(kind_off.bits(), plain.bits());
    assert_eq!(status.bypass, Some(UpmixBypass::KindOff));
    let (other_rate, status) = render(
        Some(stage(0, MatrixUpmixer::stereo_to_5_1(44_100, 0))),
        true,
    );
    assert_eq!(other_rate.bits(), plain.bits());
    assert_eq!(status.bypass, Some(UpmixBypass::RateChange));
    assert!(!status.upmixing);
}

#[test]
fn changes_while_playing_restart_the_lane_once() {
    let frames = 4 * B;
    let planes = [noise(frames, 5), noise(frames, 6)];
    let mut chain = chain(Some(stage(0, MatrixUpmixer::stereo_to_5_1(RATE, 0))), 0);
    let mut output = Output::default();
    let mut feed = Feed::new(0);
    feed.format(&mut chain, stereo(), &mut output);
    let mut play = |chain: &mut Chain, output: &mut Output| {
        feed.pcm(chain, &planes, &[B], RATE, output);
    };
    play(&mut chain, &mut output);
    assert!(status(&chain, 0).upmixing);
    let mut restarts = chain.lane_restarts(0);
    let mut expect_one_restart = |chain: &Chain| {
        assert_eq!(chain.lane_restarts(0), restarts + 1);
        restarts += 1;
    };
    // The apply switch off, and on again.
    chain.set_upmix_apply(false);
    expect_one_restart(&chain);
    play(&mut chain, &mut output);
    assert_eq!(status(&chain, 0).bypass, Some(UpmixBypass::KindOff));
    chain.set_upmix_apply(true);
    expect_one_restart(&chain);
    play(&mut chain, &mut output);
    assert!(status(&chain, 0).upmixing);
    // An upmixer set up for another rate.
    let old = chain.set_upmixer(0, Some(stage(0, MatrixUpmixer::stereo_to_5_1(44_100, 0))));
    assert!(old.is_some());
    expect_one_restart(&chain);
    play(&mut chain, &mut output);
    assert_eq!(status(&chain, 0).bypass, Some(UpmixBypass::RateChange));
    // The right rate again, then removed.
    chain.set_upmixer(0, Some(stage(0, MatrixUpmixer::stereo_to_5_1(RATE, 0))));
    expect_one_restart(&chain);
    play(&mut chain, &mut output);
    assert!(status(&chain, 0).upmixing);
    assert!(chain.set_upmixer(0, None).is_some());
    expect_one_restart(&chain);
    play(&mut chain, &mut output);
    assert_eq!(status(&chain, 0), UpmixStatus::not_installed());
    assert_eq!(output.errors, 0);
}

#[test]
fn removing_while_playing_matches_switching_off() {
    let frames = 10 * B;
    let planes = [noise(frames, 7), noise(frames, 8)];
    let render = |remove: bool| {
        let mut chain = chain(Some(stage(0, MatrixUpmixer::stereo_to_5_1(RATE, 0))), 0);
        let mut output = Output::default();
        let mut feed = Feed::new(0);
        feed.format(&mut chain, stereo(), &mut output);
        let (first, second): (Vec<_>, Vec<_>) = planes
            .iter()
            .map(|plane| plane.split_at(frames / 2))
            .unzip();
        let first: Vec<Vec<f32>> = first.into_iter().map(<[f32]>::to_vec).collect();
        let second: Vec<Vec<f32>> = second.into_iter().map(<[f32]>::to_vec).collect();
        feed.pcm(&mut chain, &first, &[B], RATE, &mut output);
        if remove {
            chain.set_upmixer(0, None);
        } else {
            chain.set_upmix_apply(false);
        }
        feed.pcm(&mut chain, &second, &[B], RATE, &mut output);
        chain.finish(&mut |event| output.take(event));
        output
    };
    assert_eq!(render(true).bits(), render(false).bits());
}

#[test]
fn pass_upmixes_and_endpoint_three_is_refused() {
    let upmixer = || Box::new(MatrixUpmixer::stereo_to_5_1(RATE, 0));
    assert_eq!(
        UpmixStage::new(3, upmixer(), QPC).unwrap_err(),
        UpmixError::Endpoint
    );
    let mut chain = chain(None, 0);
    // A stage only goes where it was built for; endpoint 3 has no slot.
    assert!(
        chain
            .set_upmixer(3, Some(stage(2, MatrixUpmixer::stereo_to_5_1(RATE, 0))))
            .is_some()
    );
    assert!(
        chain
            .set_upmixer(1, Some(stage(2, MatrixUpmixer::stereo_to_5_1(RATE, 0))))
            .is_some()
    );
    assert!(chain.upmix_status(3).is_none());
    assert_eq!(status(&chain, 1), UpmixStatus::not_installed());
    assert!(
        chain
            .set_upmixer(2, Some(stage(2, MatrixUpmixer::stereo_to_5_1(RATE, 0))))
            .is_none()
    );
    let frames = 6 * B;
    let mut output = Output::default();
    let mut feed = Feed::new(2);
    feed.format(&mut chain, stereo(), &mut output);
    feed.pcm(
        &mut chain,
        &[noise(frames, 9), noise(frames, 10)],
        &[B],
        RATE,
        &mut output,
    );
    assert!(status(&chain, 2).upmixing);
}

#[test]
fn format_changes_switch_between_upmixed_and_transparent() {
    let mut chain = chain(Some(stage(1, MatrixUpmixer::stereo_to_5_1(RATE, 0))), 1);
    let mut output = Output::default();
    let mut feed = Feed::new(1);
    let frames = 6 * B;
    let stereo_planes = [noise(frames, 11), noise(frames, 12)];
    let surround: Vec<Vec<f32>> = (0..6).map(|channel| noise(frames, 20 + channel)).collect();
    let mut seen = Vec::new();
    let mut restarts = Vec::new();
    let mut record = |chain: &Chain, seen: &mut Vec<(bool, Option<UpmixBypass>)>| {
        let status = status(chain, 1);
        seen.push((status.upmixing, status.bypass));
        restarts.push(chain.lane_restarts(1));
    };
    feed.format(&mut chain, stereo(), &mut output);
    feed.pcm(&mut chain, &stereo_planes, &[B], RATE, &mut output);
    record(&chain, &mut seen);
    feed.format(&mut chain, pcm(RATE, 6, 0x3f), &mut output);
    feed.pcm(&mut chain, &surround, &[B], RATE, &mut output);
    record(&chain, &mut seen);
    feed.format(&mut chain, eac3(), &mut output);
    feed.eac3(&mut chain, 1..4, &mut output);
    record(&chain, &mut seen);
    feed.format(&mut chain, stereo(), &mut output);
    feed.pcm(&mut chain, &stereo_planes, &[B], RATE, &mut output);
    record(&chain, &mut seen);
    let surround = Some(UpmixBypass::NativeSurround);
    assert_eq!(
        seen,
        [
            (true, None),
            (false, surround),
            (false, surround),
            (true, None)
        ]
    );
    // Each format change restarts the lane once; the stage adds none.
    assert_eq!(
        restarts
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .collect::<Vec<_>>(),
        [1, 1, 1]
    );
    assert_eq!(output.errors, 0);
}

// ---- Refusals ----

/// An upmixer of any shape; it never delivers.
struct Shape {
    rate: u32,
    block: usize,
    inputs: Vec<Speaker>,
    outputs: Vec<Speaker>,
    latency: usize,
}

impl Shape {
    fn good() -> Self {
        Self {
            rate: RATE,
            block: B,
            inputs: MatrixUpmixer::STEREO.to_vec(),
            outputs: MatrixUpmixer::OUTPUTS_5_1.to_vec(),
            latency: 0,
        }
    }
}

impl Upmixer for Shape {
    fn rate(&self) -> u32 {
        self.rate
    }
    fn block_frames(&self) -> usize {
        self.block
    }
    fn inputs(&self) -> &[Speaker] {
        &self.inputs
    }
    fn outputs(&self) -> &[Speaker] {
        &self.outputs
    }
    fn latency_frames(&self) -> usize {
        self.latency
    }
    fn process(&mut self, _: u64, _: u64, _: &[f32], _: &mut [f32]) -> bool {
        false
    }
}

#[test]
fn every_refusal_reason() {
    use Speaker::{L, Lfe, Lfe2, R, Tfl, Tfr};
    let build = |endpoint: u16, change: &dyn Fn(&mut Shape)| {
        let mut shape = Shape::good();
        change(&mut shape);
        UpmixStage::new(endpoint, Box::new(shape), QPC)
    };
    assert!(build(0, &|_| {}).is_ok());
    assert_eq!(build(3, &|_| {}).unwrap_err(), UpmixError::Endpoint);
    type Change<'a> = &'a dyn Fn(&mut Shape);
    let cases: [(Change<'_>, UpmixError); 13] = [
        (
            &|s| s.inputs.clear(),
            UpmixError::Inputs(SpeakerFault::Count),
        ),
        (
            &|s| s.inputs = vec![L, L],
            UpmixError::Inputs(SpeakerFault::Duplicate),
        ),
        (
            &|s| s.inputs = vec![L, Lfe, Lfe2],
            UpmixError::Inputs(SpeakerFault::Lfe),
        ),
        (
            &|s| s.outputs.clear(),
            UpmixError::Outputs(SpeakerFault::Count),
        ),
        (
            &|s| s.outputs = vec![L, R, R],
            UpmixError::Outputs(SpeakerFault::Duplicate),
        ),
        (
            &|s| s.outputs = vec![L, Lfe2, Lfe],
            UpmixError::Outputs(SpeakerFault::Lfe),
        ),
        (&|s| s.rate = 32_000, UpmixError::Rate),
        (&|s| s.block = 512, UpmixError::BlockFrames),
        (&|s| s.latency = 2 * RATE as usize + 1, UpmixError::Latency),
        // Nothing to pan onto, or two height speakers the renderer cannot
        // pan between.
        (&|s| s.inputs = vec![Lfe], UpmixError::InputLayout),
        (&|s| s.inputs = vec![Tfl, Tfr], UpmixError::InputLayout),
        (&|s| s.outputs = vec![Lfe], UpmixError::OutputLayout),
        (&|s| s.outputs = vec![Tfl, Tfr], UpmixError::OutputLayout),
    ];
    for (change, error) in cases {
        assert_eq!(build(0, change).unwrap_err(), error);
    }
    // Two seconds of latency is the limit, and it is all delay.
    let longest = build(0, &|s| s.latency = 2 * RATE as usize).unwrap();
    assert_eq!(longest.delay_frames(), 2 * B + 2 * RATE as usize + C);
}

// ---- The ring upmixer ----

fn ring_geometry() -> Geometry {
    Geometry {
        lanes: 1,
        input_channels: 2,
        output_channels: 6,
        max_frames: B as u32,
        sample_rate: RATE,
        latency_bound_frames: 2 * RATE,
    }
}

/// An announced region and its publisher.
fn ring(geometry: Geometry) -> (Arc<RegionBuf>, Publisher, Arc<InProcessEvent>) {
    let storage = Arc::new(RegionBuf::new(geometry).unwrap());
    let ready = Arc::new(InProcessEvent::new());
    let mut producer = Producer::new(storage.region(), [None, None], ready.as_ref());
    producer.announce(geometry, 1).unwrap();
    let publisher = producer.publisher().unwrap();
    (storage, publisher, ready)
}

fn ring_upmixer(storage: &Arc<RegionBuf>, publisher: Publisher, latency: usize) -> RingUpmixer {
    RingUpmixer::new(
        Box::new(Arc::clone(storage)),
        Box::new(InProcessEvent::new()),
        publisher,
        MatrixUpmixer::STEREO.to_vec(),
        MatrixUpmixer::OUTPUTS_5_1.to_vec(),
        latency,
    )
    .unwrap()
}

/// A plugin host on its own thread: the stereo to 5.1 matrix, in place.
/// It processes while `run` is set.
fn host(
    storage: Arc<RegionBuf>,
    ready: Arc<InProcessEvent>,
    latency: usize,
    run: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut consumer = Consumer::new(storage.region(), ready.as_ref(), [None, None]);
        consumer.validate_and_ready(2).unwrap();
        let mut matrix = MatrixUpmixer::stereo_to_5_1(RATE, latency);
        let mut slot = vec![0.0; ring_geometry().samples_per_slot()];
        let mut result = vec![0.0; 6 * B];
        while !stop.load(Ordering::Acquire) {
            let next = run
                .load(Ordering::Acquire)
                .then(|| consumer.next_published(0))
                .flatten();
            let Some(sequence) = next else {
                thread::yield_now();
                continue;
            };
            assert!(consumer.slot_mut(0, sequence, &mut slot, |samples| {
                matrix.matrix_block(&samples[..2 * B], &mut result);
                samples.copy_from_slice(&result);
            }));
            consumer.complete(0, sequence);
        }
    })
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out");
        thread::yield_now();
    }
}

#[test]
fn the_ring_upmixer_matches_the_matrix_block_by_block() {
    let latency = 37;
    let (storage, publisher, ready) = ring(ring_geometry());
    let run = Arc::new(AtomicBool::new(true));
    let stop = Arc::new(AtomicBool::new(false));
    let host = host(
        Arc::clone(&storage),
        ready,
        latency,
        Arc::clone(&run),
        Arc::clone(&stop),
    );
    wait_until(|| publisher.state(storage.region()) == Some(State::Ready));
    let mut ring = ring_upmixer(&storage, publisher, latency);
    let mut matrix = MatrixUpmixer::stereo_to_5_1(RATE, latency);
    let (mut from_ring, mut from_matrix) = (vec![0.0; 6 * B], vec![0.0; 6 * B]);
    for block in 0..60_u64 {
        // Block k - 1 went out as sequence k; let the host finish it.
        wait_until(|| publisher.completed(storage.region(), 0) == block as u32);
        let input: Vec<f32> = [noise(B, block as u32), noise(B, 1_000 + block as u32)].concat();
        let delivered = ring.process(block, 1, &input, &mut from_ring);
        assert_eq!(
            delivered,
            matrix.process(block, 1, &input, &mut from_matrix)
        );
        assert_eq!(delivered, block > 0);
        if delivered {
            assert_eq!(from_ring, from_matrix, "block {block}");
        }
    }
    stop.store(true, Ordering::Release);
    host.join().unwrap();
}

#[test]
fn a_generation_change_discards_a_late_ring_result() {
    let (storage, publisher, ready) = ring(ring_geometry());
    let mut ring = ring_upmixer(&storage, publisher, 0);
    let mut consumer = Consumer::new(storage.region(), ready.as_ref(), [None, None]);
    consumer.validate_and_ready(2).unwrap();
    let mut slot = vec![0.0; ring_geometry().samples_per_slot()];
    let mut host = |consumer: &Consumer<'_>| {
        while let Some(sequence) = consumer.next_published(0) {
            consumer.slot_mut(0, sequence, &mut slot, |samples| {
                samples.fill(sequence as f32)
            });
            consumer.complete(0, sequence);
        }
    };
    let input = vec![0.25; 2 * B];
    let mut output = vec![0.0; 6 * B];
    assert!(!ring.process(0, 1, &input, &mut output));
    // The host is late with block 0: its output instant passes.
    assert!(!ring.process(1, 1, &input, &mut output));
    host(&consumer);
    // A restart: block 1 of generation 1 is complete but not this one's.
    assert!(!ring.process(0, 2, &input, &mut output));
    host(&consumer);
    assert!(ring.process(1, 2, &input, &mut output));
    // Generation 2's block 0 went out as sequence 3.
    assert!(output.iter().all(|sample| *sample == 3.0));
}

#[test]
fn a_stalled_host_is_concealed_and_never_blocks_the_producer() {
    let (storage, publisher, ready) = ring(ring_geometry());
    let run = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let host = host(
        Arc::clone(&storage),
        ready,
        0,
        Arc::clone(&run),
        Arc::clone(&stop),
    );
    wait_until(|| publisher.state(storage.region()) == Some(State::Ready));
    let ring = ring_upmixer(&storage, publisher, 0);
    let mut chain = chain(
        Some(Box::new(UpmixStage::new(0, Box::new(ring), QPC).unwrap())),
        0,
    );
    let mut output = Output::default();
    let mut feed = Feed::new(0);
    feed.format(&mut chain, stereo(), &mut output);
    let block = [vec![0.25; B], vec![0.25; B]];
    // Stalled: every block is missed, the fallback plays, nothing waits.
    let started = Instant::now();
    for _ in 0..30 {
        feed.pcm(&mut chain, &block, &[B], RATE, &mut output);
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    let stalled = status(&chain, 0);
    assert_eq!((stalled.delivered_blocks, stalled.missed_blocks), (0, 29));
    assert_eq!(stalled.mode, UpmixMode::Fallback);
    // The fallback is the stereo, delayed: no centre.
    assert!(output.channels[FL].iter().any(|sample| *sample != 0.0));
    assert!(output.channels[FC].iter().all(|sample| *sample == 0.0));
    // Two sequences went out before the slots filled; the host catches up
    // and then keeps up.
    run.store(true, Ordering::Release);
    wait_until(|| publisher.completed(storage.region(), 0) == 2);
    for published in 3..13 {
        feed.pcm(&mut chain, &block, &[B], RATE, &mut output);
        wait_until(|| publisher.completed(storage.region(), 0) == published);
    }
    let resumed = status(&chain, 0);
    assert_eq!(resumed.mode, UpmixMode::Plugin);
    assert_eq!(resumed.missed_blocks, 30);
    assert_eq!(resumed.delivered_blocks, 9);
    assert!(output.channels[FC].iter().any(|sample| *sample != 0.0));
    stop.store(true, Ordering::Release);
    host.join().unwrap();
}

#[test]
fn the_ring_must_match_the_upmixer() {
    let refuse = |geometry: Geometry, inputs: &[Speaker], latency: usize| {
        let (storage, publisher, _) = ring(geometry);
        RingUpmixer::new(
            Box::new(storage),
            Box::new(InProcessEvent::new()),
            publisher,
            inputs.to_vec(),
            MatrixUpmixer::OUTPUTS_5_1.to_vec(),
            latency,
        )
        .unwrap_err()
    };
    let stereo = &MatrixUpmixer::STEREO[..];
    let geometry = ring_geometry();
    let cases = [
        (
            Geometry {
                lanes: 2,
                ..geometry
            },
            stereo,
            0,
            UpmixError::Ring(RingFault::Lanes),
        ),
        (
            geometry,
            &MatrixUpmixer::STEREO_LFE[..],
            0,
            UpmixError::Ring(RingFault::InputChannels),
        ),
        (
            Geometry {
                output_channels: 8,
                ..geometry
            },
            stereo,
            0,
            UpmixError::Ring(RingFault::OutputChannels),
        ),
        (
            Geometry {
                max_frames: 512,
                ..geometry
            },
            stereo,
            0,
            UpmixError::BlockFrames,
        ),
        (
            Geometry {
                sample_rate: 32_000,
                max_frames: 320,
                ..geometry
            },
            stereo,
            0,
            UpmixError::Rate,
        ),
        (geometry, stereo, 2 * RATE as usize + 1, UpmixError::Latency),
        (
            geometry,
            &[Speaker::L, Speaker::L][..],
            0,
            UpmixError::Inputs(SpeakerFault::Duplicate),
        ),
    ];
    for (geometry, inputs, latency, error) in cases {
        assert_eq!(refuse(geometry, inputs, latency), error);
    }
}
