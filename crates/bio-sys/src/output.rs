//! Event-driven stereo WASAPI output, opened and used on the output thread.
//!
//! Playback timestamps use the audio clock's correlated (position, QPC) pair.
//! GetPosition's QPC is in 100 ns units, NOT raw performance-counter ticks.
//! Its position is the sample playing at the speakers, so the distance from
//! that position to our submitted frame already includes queued stream latency.
//! Adding GetStreamLatency again would count latency twice. wasapi 0.25 does
//! not expose GetStreamLatency; no raw COM access is needed here.

use crate::{
    devices::{ComGuard, output_error},
    qpc,
};
use bio_types::{OutputError, OutputFormat, OutputOpener, OutputSink};
use std::{fmt, time::Duration};
use wasapi::{
    AudioClient, AudioClock, AudioRenderClient, Device, DeviceEnumerator, Direction, Handle,
    SampleType, StreamMode, WasapiError, WaveFormat,
};
use windows::Win32::{
    Foundation::E_INVALIDARG,
    Media::Audio::{
        AUDCLNT_E_BUFFER_SIZE_ERROR, AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED,
        AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED, AUDCLNT_E_INVALID_DEVICE_PERIOD,
        AUDCLNT_E_UNSUPPORTED_FORMAT,
    },
};

#[derive(Debug, Clone)]
pub struct WasapiOpener {
    pub endpoint: Option<String>,
    pub rate: u32,
    pub exclusive: bool,
    pub period: Option<Duration>,
}

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
    submitted: u64,
    clock_frequency: u64,
    qpc_frequency: u64,
    started_qpc: u64,
    // Must be last: interface fields are released before CoUninitialize.
    _com: ComGuard,
}

impl fmt::Debug for WasapiSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WasapiSink")
            .field("format", &self.format)
            .field("encoding", &self.encoding)
            .field("exclusive", &self.exclusive)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug)]
enum Encoding {
    Float32,
    Int32,
    Int24In32,
    Int16,
}

impl Encoding {
    fn wave(self, rate: u32) -> WaveFormat {
        let (bits, valid, kind) = match self {
            Self::Float32 => (32, 32, SampleType::Float),
            Self::Int32 => (32, 32, SampleType::Int),
            Self::Int24In32 => (32, 24, SampleType::Int),
            Self::Int16 => (16, 16, SampleType::Int),
        };
        WaveFormat::new(bits, valid, &kind, rate as usize, 2, Some(3))
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

impl OutputOpener for WasapiOpener {
    type Sink = WasapiSink;

    fn open(self) -> Result<WasapiSink, OutputError> {
        if self.rate == 0 || self.rate > u32::MAX / 8 {
            return Err(OutputError::FormatRejected(
                "invalid stereo sample rate".into(),
            ));
        }
        let com = ComGuard::new()?;
        let enumerator = DeviceEnumerator::new().map_err(output_error)?;
        let device = match &self.endpoint {
            Some(id) => enumerator.get_device(id),
            None => enumerator.get_default_device(&Direction::Render),
        }
        .map_err(output_error)?;
        let opened = if self.exclusive {
            match self.open_exclusive(&device) {
                Ok(opened) => opened,
                Err(error) if mode_error(&error) => {
                    self.open_shared(&device).map_err(output_error)?
                }
                Err(error) => return Err(output_error(error)),
            }
        } else {
            self.open_shared(&device).map_err(output_error)?
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
        let mut bytes = vec![0; buffer_frames * 2 * encoding.sample_bytes()];
        // Prime the entire buffer before Start. This silence belongs to the
        // stream position just like every subsequently submitted frame.
        render
            .write_to_device(buffer_frames, &bytes, None)
            .map_err(output_error)?;
        bytes.truncate(period_frames * 2 * encoding.sample_bytes());
        let samples = vec![0.0; period_frames * 2];
        let started_qpc = qpc::now();
        client.start_stream().map_err(output_error)?;
        Ok(WasapiSink {
            client,
            render,
            clock,
            event,
            format: OutputFormat {
                rate: self.rate,
                channels: 2,
                period_frames,
            },
            encoding,
            exclusive,
            samples,
            bytes,
            submitted: buffer_frames as u64,
            clock_frequency,
            qpc_frequency: qpc::frequency(),
            started_qpc,
            _com: com,
        })
    }
}

type Opened = (AudioClient, Encoding, bool, usize);

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

    fn open_exclusive(&self, device: &Device) -> Result<Opened, WasapiError> {
        let mut last_error = WasapiError::UnsupportedFormat;
        for encoding in [
            Encoding::Float32,
            Encoding::Int32,
            Encoding::Int24In32,
            Encoding::Int16,
        ] {
            let mut client = device.get_iaudioclient()?;
            let wave = encoding.wave(self.rate);
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

    fn open_shared(&self, device: &Device) -> Result<Opened, WasapiError> {
        let mut client = device.get_iaudioclient()?;
        let (default, _) = client.get_device_period()?;
        let buffer = self.period_hns(default)?;
        // The shared engine controls event cadence; the optional duration is
        // a buffer request, not a promise that its processing period changes.
        client.initialize_client(
            &Encoding::Float32.wave(self.rate),
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

fn mode_error(error: &WasapiError) -> bool {
    match error {
        WasapiError::UnsupportedFormat | WasapiError::UnsupportedSubformat(_) => true,
        WasapiError::Windows(e) => matches!(
            e.code(),
            AUDCLNT_E_UNSUPPORTED_FORMAT
                | AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED
                | AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED
                | AUDCLNT_E_BUFFER_SIZE_ERROR
                | AUDCLNT_E_INVALID_DEVICE_PERIOD
                | E_INVALIDARG
        ),
        _ => false,
    }
}

impl OutputSink for WasapiSink {
    fn format(&self) -> OutputFormat {
        self.format
    }

    fn next_period(&mut self, fill: &mut dyn FnMut(&mut [f32])) -> Result<u64, OutputError> {
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
                break;
            }
        }
        self.samples.fill(0.0);
        fill(&mut self.samples);
        self.encoding.convert(&self.samples, &mut self.bytes);
        let (position, clock_qpc) = self.clock.get_position().map_err(output_error)?;
        let played_frames = (u128::from(position) * u128::from(self.format.rate))
            .div_ceil(u128::from(self.clock_frequency)) as u64;
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
        if position == 0 && clock_qpc == 0 {
            // Some endpoints have no correlated reading during startup. This
            // estimate cannot include their unreported initial hardware delay.
            return Ok(self.started_qpc.saturating_add(scale(
                first_frame,
                self.qpc_frequency,
                u64::from(self.format.rate),
            )));
        }
        Ok(playback_qpc(
            clock_qpc,
            position,
            self.clock_frequency,
            first_frame,
            self.format.rate,
            self.qpc_frequency,
        ))
    }
}

impl Drop for WasapiSink {
    fn drop(&mut self) {
        let _ = self.client.stop_stream();
    }
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
    fn playback_timestamp_uses_clock_units_and_queue_distance() {
        // A 24 MHz QPC, 192 kHz device clock, 48 kHz stream: 10 ms queued.
        assert_eq!(
            playback_qpc(20_000_000, 192_000, 192_000, 48_480, 48_000, 24_000_000),
            48_240_000
        );
    }

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
