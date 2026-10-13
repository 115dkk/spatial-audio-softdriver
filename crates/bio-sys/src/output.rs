//! Event-driven WASAPI output, opened and used on the output thread.
//!
//! Playback timestamps use the audio clock's correlated (position, QPC) pair.
//! GetPosition's QPC is in 100 ns units, NOT raw performance-counter ticks.
//! Its position is the sample playing at the speakers, so the distance from
//! that position to our submitted frame already includes queued stream latency.
//! Adding GetStreamLatency again would count latency twice. wasapi 0.25 does
//! not expose GetStreamLatency; no raw COM access is needed here.

#[cfg(windows)]
use crate::{
    devices::{ComGuard, output_error},
    qpc,
};
#[cfg(windows)]
use bio_types::{OutputError, OutputFormat, OutputLayout, OutputMode, OutputOpener, OutputSink};
#[cfg(windows)]
use std::{fmt, time::Duration};
#[cfg(windows)]
use wasapi::{
    AudioClient, AudioClock, AudioRenderClient, Device, DeviceEnumerator, Direction, Handle,
    SampleType, StreamMode, WasapiError, WaveFormat,
};
#[cfg(windows)]
use windows::Win32::{
    Foundation::E_INVALIDARG,
    Media::Audio::{
        AUDCLNT_E_BUFFER_SIZE_ERROR, AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED,
        AUDCLNT_E_ENDPOINT_CREATE_FAILED, AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED,
        AUDCLNT_E_INVALID_DEVICE_PERIOD, AUDCLNT_E_UNSUPPORTED_FORMAT,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExclusiveFailurePolicy {
    Fail,
    FallBackToShared,
}

fn exclusive_failure_policy(exclusive: bool, shared_fallback: bool) -> ExclusiveFailurePolicy {
    if exclusive && shared_fallback {
        ExclusiveFailurePolicy::FallBackToShared
    } else {
        ExclusiveFailurePolicy::Fail
    }
}

#[derive(Clone, Copy, Debug)]
struct ClockReading {
    position: u64,
    qpc_100ns: u64,
}

#[derive(Debug, Default)]
struct PlaybackClock {
    last_qpc: Option<u64>,
}

impl PlaybackClock {
    #[allow(clippy::too_many_arguments)]
    fn next(
        &mut self,
        first_frame: u64,
        reading: Option<ClockReading>,
        started_qpc: u64,
        stream_rate: u32,
        clock_frequency: u64,
        qpc_frequency: u64,
        period_frames: usize,
    ) -> u64 {
        let candidate = reading.map_or_else(
            || {
                started_qpc.saturating_add(scale(
                    first_frame,
                    qpc_frequency,
                    u64::from(stream_rate),
                ))
            },
            |reading| {
                playback_qpc(
                    reading.qpc_100ns,
                    reading.position,
                    clock_frequency,
                    first_frame,
                    stream_rate,
                    qpc_frequency,
                )
            },
        );
        let next = self.last_qpc.map_or(candidate, |last| match reading {
            Some(_) => candidate.max(last),
            None => {
                let period_ticks =
                    scale(period_frames as u64, qpc_frequency, u64::from(stream_rate));
                candidate.max(last.saturating_add(period_ticks))
            }
        });
        self.last_qpc = Some(next);
        next
    }
}

#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct WasapiOpener {
    pub endpoint: Option<String>,
    pub rate: u32,
    pub exclusive: bool,
    /// Permit an explicit exclusive request to fall back to shared mode.
    pub shared_fallback: bool,
    pub period: Option<Duration>,
}

#[cfg(windows)]
impl Default for WasapiOpener {
    fn default() -> Self {
        Self {
            endpoint: None,
            rate: 48_000,
            exclusive: true,
            shared_fallback: false,
            period: None,
        }
    }
}

#[cfg(windows)]
pub struct WasapiSink {
    client: AudioClient,
    render: AudioRenderClient,
    clock: AudioClock,
    event: Handle,
    format: OutputFormat,
    encoding: Encoding,
    exclusive: bool,
    samples: Vec<f32>,
    bytes: Vec<u8>,
    /// One whole device buffer of silence, written before each start.
    silence: Vec<u8>,
    buffer_frames: usize,
    running: bool,
    restarts: u64,
    /// A wait for the device longer than this (QPC ticks) means the stream
    /// has stalled.
    stall_ticks: u64,
    submitted: u64,
    clock_frequency: u64,
    qpc_frequency: u64,
    started_qpc: u64,
    playback_clock: PlaybackClock,
    latest_played_frames: u64,
    // Must be last: interface fields are released before CoUninitialize.
    _com: ComGuard,
}

#[cfg(windows)]
impl fmt::Debug for WasapiSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WasapiSink")
            .field("format", &self.format)
            .field("encoding", &self.encoding)
            .field("exclusive", &self.exclusive)
            .finish_non_exhaustive()
    }
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum Encoding {
    Float32,
    Int32,
    Int24In32,
    Int16,
}

#[cfg(windows)]
impl Encoding {
    pub(crate) const ALL: [Self; 4] = [Self::Float32, Self::Int32, Self::Int24In32, Self::Int16];

    pub(crate) fn wave(self, rate: u32, layout: OutputLayout) -> WaveFormat {
        let (bits, valid, kind) = match self {
            Self::Float32 => (32, 32, SampleType::Float),
            Self::Int32 => (32, 32, SampleType::Int),
            Self::Int24In32 => (32, 24, SampleType::Int),
            Self::Int16 => (16, 16, SampleType::Int),
        };
        WaveFormat::new(
            bits,
            valid,
            &kind,
            rate as usize,
            usize::from(layout.channels),
            Some(layout.channel_mask),
        )
    }

    fn sample_bytes(self) -> usize {
        if matches!(self, Self::Int16) { 2 } else { 4 }
    }

    fn convert(self, samples: &[f32], bytes: &mut [u8]) {
        for (&sample, dst) in samples
            .iter()
            .zip(bytes.chunks_exact_mut(self.sample_bytes()))
        {
            let sample = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            match self {
                Self::Float32 => dst.copy_from_slice(&sample.to_le_bytes()),
                Self::Int32 => dst.copy_from_slice(
                    &((f64::from(sample) * 2_147_483_648.0).round() as i32).to_le_bytes(),
                ),
                Self::Int24In32 => {
                    let value = (f64::from(sample) * 8_388_608.0)
                        .round()
                        .clamp(-8_388_608.0, 8_388_607.0) as i32;
                    dst.copy_from_slice(&(value << 8).to_le_bytes());
                }
                Self::Int16 => {
                    dst.copy_from_slice(&((sample * 32768.0).round() as i16).to_le_bytes())
                }
            }
        }
    }
}

#[cfg(windows)]
impl OutputOpener for WasapiOpener {
    type Sink = WasapiSink;

    fn open(&mut self, layout: OutputLayout) -> Result<WasapiSink, OutputError> {
        if layout.channels == 0 || layout.channels > 32 {
            return Err(OutputError::FormatRejected(format!(
                "output channel count {} is outside 1..=32",
                layout.channels
            )));
        }
        if layout.channel_mask != 0
            && layout.channel_mask.count_ones() != u32::from(layout.channels)
        {
            return Err(OutputError::FormatRejected(format!(
                "{} channels do not match mask {:#x}",
                layout.channels, layout.channel_mask
            )));
        }
        if self.rate == 0 || self.rate > u32::MAX / (u32::from(layout.channels) * 4) {
            return Err(OutputError::FormatRejected(
                "invalid output sample rate".into(),
            ));
        }
        let com = ComGuard::new()?;
        let enumerator = DeviceEnumerator::new().map_err(output_error)?;
        let device = match &self.endpoint {
            Some(id) => enumerator.get_device(id),
            None => enumerator.get_default_device(&Direction::Render),
        }
        .map_err(output_error)?;
        let (opened, fell_back, mix_channels) = if self.exclusive {
            match self.open_exclusive(&device, layout) {
                Ok(opened) => (opened, false, layout.channels),
                Err(error)
                    if mode_error(&error)
                        && exclusive_failure_policy(self.exclusive, self.shared_fallback)
                            == ExclusiveFailurePolicy::FallBackToShared =>
                {
                    let mix_channels = device
                        .get_iaudioclient()
                        .and_then(|client| client.get_mixformat())
                        .map_err(output_error)?
                        .get_nchannels();
                    (
                        self.open_shared(&device, layout).map_err(output_error)?,
                        true,
                        mix_channels,
                    )
                }
                Err(error) if mode_error(&error) => {
                    return Err(OutputError::ExclusiveUnavailable(error.to_string()));
                }
                Err(error) => return Err(output_error(error)),
            }
        } else {
            (
                self.open_shared(&device, layout).map_err(output_error)?,
                false,
                layout.channels,
            )
        };
        let (client, encoding, exclusive, period_frames) = opened;
        let buffer_frames = client.get_buffer_size().map_err(output_error)? as usize;
        if buffer_frames == 0 || period_frames == 0 || period_frames > buffer_frames {
            return Err(OutputError::FormatRejected(
                "invalid WASAPI period size".into(),
            ));
        }
        let event = client.set_get_eventhandle().map_err(output_error)?;
        let render = client.get_audiorenderclient().map_err(output_error)?;
        let clock = client.get_audioclock().map_err(output_error)?;
        let clock_frequency = clock.get_frequency().map_err(output_error)?;
        if clock_frequency == 0 {
            return Err(OutputError::Io("audio clock has zero frequency".into()));
        }
        // The stream starts on the first `next_period`, not here: whatever the
        // caller sets up between open and its first period must not count
        // against the device (see `start`).
        let channels = usize::from(layout.channels);
        let silence = vec![0; buffer_frames * channels * encoding.sample_bytes()];
        let bytes = vec![0; period_frames * channels * encoding.sample_bytes()];
        let samples = vec![0.0; period_frames * channels];
        Ok(WasapiSink {
            client,
            render,
            clock,
            event,
            format: OutputFormat {
                rate: self.rate,
                channels: layout.channels,
                channel_mask: layout.channel_mask,
                period_frames,
                mode: if exclusive {
                    OutputMode::Exclusive
                } else {
                    OutputMode::Shared
                },
                fell_back,
                mix_channels,
            },
            encoding,
            exclusive,
            samples,
            bytes,
            silence,
            buffer_frames,
            running: false,
            restarts: 0,
            stall_ticks: stall_ticks(period_frames, self.rate, qpc::frequency()),
            submitted: 0,
            clock_frequency,
            qpc_frequency: qpc::frequency(),
            started_qpc: 0,
            playback_clock: PlaybackClock::default(),
            latest_played_frames: 0,
            _com: com,
        })
    }

    fn set_period(&mut self, period: Duration) -> bool {
        self.period = Some(period);
        true
    }
}

#[cfg(windows)]
type Opened = (AudioClient, Encoding, bool, usize);

#[cfg(windows)]
impl WasapiOpener {
    fn period_hns(&self, default: i64) -> Result<i64, WasapiError> {
        match self.period {
            None => Ok(default),
            Some(period) => i64::try_from(period.as_nanos() / 100)
                .ok()
                .filter(|&n| n > 0)
                .ok_or(WasapiError::UnsupportedFormat),
        }
    }

    fn open_exclusive(&self, device: &Device, layout: OutputLayout) -> Result<Opened, WasapiError> {
        let mut last_error = WasapiError::UnsupportedFormat;
        for encoding in Encoding::ALL {
            let mut client = device.get_iaudioclient()?;
            // Older WDM drivers accept a stereo format only as a plain
            // WAVEFORMATEX or only with another channel mask, and fail
            // Initialize with AUDCLNT_E_ENDPOINT_CREATE_FAILED when handed
            // one they did not accept, so ask for the form they take first.
            let requested = encoding.wave(self.rate, layout);
            // Preserve the old stereo quirk negotiation. Multichannel layouts
            // are exact contracts: a different mask would route samples to the
            // wrong physical outputs, so query and initialize the requested
            // WAVEFORMATEXTENSIBLE unchanged.
            let wave = if layout == OutputLayout::STEREO {
                match client.is_supported_exclusive_with_quirks(&requested) {
                    Ok(wave) => wave,
                    Err(error) => {
                        last_error = error;
                        continue;
                    }
                }
            } else {
                match client.is_supported(&requested, &wasapi::ShareMode::Exclusive) {
                    Ok(None) => requested,
                    Ok(Some(_)) | Err(WasapiError::UnsupportedFormat) => {
                        last_error = WasapiError::UnsupportedFormat;
                        continue;
                    }
                    Err(error) => {
                        last_error = error;
                        continue;
                    }
                }
            };
            let (_, minimum) = client.get_device_period()?;
            let period = self.period_hns(minimum)?;
            let mut result = client.initialize_client(
                &wave,
                &Direction::Render,
                &StreamMode::EventsExclusive { period_hns: period },
            );
            if matches!(&result, Err(WasapiError::Windows(e)) if e.code() == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED)
            {
                // Microsoft requires a NEW client for the alignment retry.
                let frames = client.get_buffer_size()?;
                let aligned =
                    wasapi::calculate_period_100ns(i64::from(frames), i64::from(self.rate));
                client = device.get_iaudioclient()?;
                result = client.initialize_client(
                    &wave,
                    &Direction::Render,
                    &StreamMode::EventsExclusive {
                        period_hns: aligned,
                    },
                );
            }
            match result {
                Ok(()) => {
                    let frames = client.get_buffer_size()? as usize;
                    return Ok((client, encoding, true, frames));
                }
                Err(error) if mode_error(&error) => last_error = error,
                Err(error) => return Err(error),
            }
        }
        Err(last_error)
    }

    fn open_shared(&self, device: &Device, layout: OutputLayout) -> Result<Opened, WasapiError> {
        let mut client = device.get_iaudioclient()?;
        let (default, _) = client.get_device_period()?;
        let buffer = self.period_hns(default)?;
        // The shared engine controls event cadence; the optional duration is
        // a buffer request, not a promise that its processing period changes.
        client.initialize_client(
            &Encoding::Float32.wave(self.rate, layout),
            &Direction::Render,
            &StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: buffer,
            },
        )?;
        let frames = (default as u128 * u128::from(self.rate)).div_ceil(10_000_000) as usize;
        Ok((client, Encoding::Float32, false, frames))
    }
}

#[cfg(windows)]
fn mode_error(error: &WasapiError) -> bool {
    match error {
        WasapiError::UnsupportedFormat | WasapiError::UnsupportedSubformat(_) => true,
        WasapiError::Windows(e) => matches!(
            e.code(),
            AUDCLNT_E_UNSUPPORTED_FORMAT
                | AUDCLNT_E_ENDPOINT_CREATE_FAILED
                | AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED
                | AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED
                | AUDCLNT_E_BUFFER_SIZE_ERROR
                | AUDCLNT_E_INVALID_DEVICE_PERIOD
                | E_INVALIDARG
        ),
        _ => false,
    }
}

#[cfg(windows)]
impl WasapiSink {
    /// Primes the whole buffer with silence and starts the stream. The silence
    /// belongs to the stream position like every later frame.
    fn start(&mut self) -> Result<(), OutputError> {
        self.render
            .write_to_device(self.buffer_frames, &self.silence, None)
            .map_err(output_error)?;
        self.submitted = self.buffer_frames as u64;
        self.started_qpc = qpc::now();
        self.client.start_stream().map_err(output_error)?;
        self.running = true;
        Ok(())
    }

    /// Stops, rewinds and restarts the stream (Reset sets the position to 0).
    fn restart(&mut self) -> Result<(), OutputError> {
        self.client.stop_stream().map_err(output_error)?;
        self.client.reset_stream().map_err(output_error)?;
        self.running = false;
        self.restarts += 1;
        self.start()
    }

    /// Waits until the device asks for a period; returns the QPC ticks that
    /// took.
    fn wait_for_space(&mut self) -> Result<u64, OutputError> {
        let entered = qpc::now();
        loop {
            if let Err(error) = self.event.wait_for_event(2000) {
                // Device invalidation can stop the event, so query the client
                // before reporting a generic timeout.
                self.client.get_current_padding().map_err(output_error)?;
                return Err(output_error(error));
            }
            if self
                .client
                .get_available_space_in_frames()
                .map_err(output_error)? as usize
                >= self.format.period_frames
            {
                return Ok(qpc::now().saturating_sub(entered));
            }
        }
    }

    fn played_frames(&self, position: u64) -> u64 {
        (u128::from(position) * u128::from(self.format.rate))
            .div_ceil(u128::from(self.clock_frequency)) as u64
    }
}

#[cfg(windows)]
impl OutputSink for WasapiSink {
    fn format(&self) -> OutputFormat {
        self.format
    }

    fn restarts(&self) -> u64 {
        self.restarts
    }

    fn played_frames(&self) -> Option<u64> {
        Some(self.latest_played_frames)
    }

    fn next_period(&mut self, fill: &mut dyn FnMut(&mut [f32])) -> Result<u64, OutputError> {
        if !self.running {
            self.start()?;
        }
        let waited = self.wait_for_space()?;
        if self.exclusive && waited > self.stall_ticks {
            // After one missed deadline an exclusive-mode USB endpoint (a
            // TOPPING DAC on usbaudio2) was seen to signal only every ~33 ms
            // from then on, at every period size, so the stream fell behind
            // for good. Its position stops while it starves, so the wait is
            // the only sign. A restart brings back the normal cadence.
            self.restart()?;
            self.wait_for_space()?;
        }
        self.samples.fill(0.0);
        fill(&mut self.samples);
        self.encoding.convert(&self.samples, &mut self.bytes);
        let (position, clock_qpc) = self.clock.get_position().map_err(output_error)?;
        let played_frames = self.played_frames(position);
        self.latest_played_frames = played_frames;
        // GetPosition continues across underruns. Do not timestamp new audio
        // in the past merely because fewer frames were submitted than played.
        self.submitted = self.submitted.max(played_frames);
        let first_frame = self.submitted;
        self.render
            .write_to_device(self.format.period_frames, &self.bytes, None)
            .map_err(output_error)?;
        self.submitted = self
            .submitted
            .saturating_add(self.format.period_frames as u64);
        let reading = (position != 0 || clock_qpc != 0).then_some(ClockReading {
            position,
            qpc_100ns: clock_qpc,
        });
        Ok(self.playback_clock.next(
            first_frame,
            reading,
            self.started_qpc,
            self.format.rate,
            self.clock_frequency,
            self.qpc_frequency,
            self.format.period_frames,
        ))
    }
}

#[cfg(windows)]
impl Drop for WasapiSink {
    fn drop(&mut self) {
        let _ = self.client.stop_stream();
    }
}

/// Two periods, and at least 10 ms. A healthy wait is about one period: 3.0 to
/// 4.3 ms at a 3 ms period and 10.0 to 10.9 ms at 10 ms were measured, while
/// the stalls lasted 28 to 43 ms.
#[cfg(windows)]
fn stall_ticks(period_frames: usize, rate: u32, qpc_frequency: u64) -> u64 {
    let two_periods = scale(2 * period_frames as u64, qpc_frequency, u64::from(rate));
    two_periods.max(qpc_frequency / 100)
}

fn scale(value: u64, numerator: u64, denominator: u64) -> u64 {
    (u128::from(value) * u128::from(numerator) / u128::from(denominator)).min(u128::from(u64::MAX))
        as u64
}

fn playback_qpc(
    clock_qpc: u64,
    position: u64,
    clock_frequency: u64,
    frame: u64,
    rate: u32,
    qpc_frequency: u64,
) -> u64 {
    let base = i128::from(scale(clock_qpc, qpc_frequency, 10_000_000));
    let queued = i128::from(scale(frame, qpc_frequency, u64::from(rate)))
        - i128::from(scale(position, qpc_frequency, clock_frequency));
    (base + queued).clamp(0, i128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_failure_requires_an_explicit_shared_fallback() {
        assert_eq!(
            exclusive_failure_policy(true, false),
            ExclusiveFailurePolicy::Fail
        );
        assert_eq!(
            exclusive_failure_policy(true, true),
            ExclusiveFailurePolicy::FallBackToShared
        );
        assert_eq!(
            exclusive_failure_policy(false, true),
            ExclusiveFailurePolicy::Fail
        );
    }

    #[test]
    fn playback_timestamp_uses_clock_units_and_queue_distance() {
        // A 24 MHz QPC, 192 kHz device clock, 48 kHz stream: 10 ms queued.
        assert_eq!(
            playback_qpc(20_000_000, 192_000, 192_000, 48_480, 48_000, 24_000_000),
            48_240_000
        );
    }

    #[test]
    fn playback_clock_is_monotonic_across_estimates_measurements_and_restarts() {
        #[derive(Clone, Copy, Debug)]
        struct Row {
            first_frame: u64,
            reading: Option<ClockReading>,
            started_qpc: u64,
        }

        for qpc_frequency in [10_000_000, 24_000_000] {
            let period = qpc_frequency / 100;
            let base = 7 * qpc_frequency;
            let rows = [
                // Startup estimate.
                Row {
                    first_frame: 480,
                    reading: None,
                    started_qpc: base,
                },
                // First correlation reports the hardware delay and moves ahead.
                Row {
                    first_frame: 960,
                    reading: Some(ClockReading {
                        position: 480,
                        qpc_100ns: 70_200_000,
                    }),
                    started_qpc: base,
                },
                // A regressing correlation is clamped by one whole period.
                Row {
                    first_frame: 1_440,
                    reading: Some(ClockReading {
                        position: 960,
                        qpc_100ns: 70_150_000,
                    }),
                    started_qpc: base,
                },
                // Correlation can disappear temporarily after it existed.
                Row {
                    first_frame: 1_920,
                    reading: None,
                    started_qpc: base,
                },
                // Reset rewinds submitted frames and changes the estimate base.
                Row {
                    first_frame: 480,
                    reading: None,
                    started_qpc: base - qpc_frequency,
                },
                // Large device positions must not overflow intermediate math.
                Row {
                    first_frame: u64::MAX - 480,
                    reading: Some(ClockReading {
                        position: u64::MAX - 960,
                        qpc_100ns: 80_000_000,
                    }),
                    started_qpc: base,
                },
            ];
            let mut clock = PlaybackClock::default();
            let mut actual = Vec::new();
            for row in rows {
                actual.push(clock.next(
                    row.first_frame,
                    row.reading,
                    row.started_qpc,
                    48_000,
                    48_000,
                    qpc_frequency,
                    480,
                ));
            }
            assert!(
                actual.windows(2).all(|pair| pair[1] >= pair[0]),
                "qpc_frequency={qpc_frequency}, rows={rows:#?}, actual={actual:?}"
            );
            assert!(
                actual[1] > base + 2 * period,
                "forward measurement was not accepted: qpc_frequency={qpc_frequency}, rows={rows:#?}, actual={actual:?}"
            );
        }
    }

    #[test]
    fn fast_dac_measurements_do_not_accumulate_nominal_period_error() {
        let qpc_frequency = 10_000_000;
        let rate = 48_000;
        let period_frames = 480;
        let periods = 360_000_u64;
        let period_100ns = 100_000_u64;
        let mut clock = PlaybackClock::default();
        let mut last = 0;
        for period in 0..periods {
            let first_frame = (period + 1) * period_frames as u64;
            let position = period * period_frames as u64;
            let qpc_100ns = 50_000_000 + period * period_100ns * 9_999 / 10_000;
            last = clock.next(
                first_frame,
                Some(ClockReading {
                    position,
                    qpc_100ns,
                }),
                5 * qpc_frequency,
                rate,
                u64::from(rate),
                qpc_frequency,
                period_frames,
            );
        }
        let expected =
            5 * qpc_frequency + (periods - 1) * period_100ns * 9_999 / 10_000 + qpc_frequency / 100;
        assert_eq!(last, expected);
    }

    #[cfg(windows)]
    #[test]
    fn integer_samples_clip_and_left_align() {
        let input = [-2.0, -1.0, 0.0, 1.0, 2.0, f32::NAN];
        let mut bytes = [0; 24];
        Encoding::Int24In32.convert(&input, &mut bytes);
        let values: Vec<_> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| i32::from_le_bytes(*b))
            .collect();
        assert_eq!(values, [i32::MIN, i32::MIN, 0, 0x7fffff00, 0x7fffff00, 0]);
        Encoding::Int32.convert(&input, &mut bytes);
        assert_eq!(
            i32::from_le_bytes(bytes[12..16].try_into().unwrap()),
            i32::MAX
        );
        Encoding::Int16.convert(&input, &mut bytes[..12]);
        assert_eq!(
            i16::from_le_bytes(bytes[6..8].try_into().unwrap()),
            i16::MAX
        );
    }
}
