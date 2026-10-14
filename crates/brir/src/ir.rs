//! WAV impulse-response loading and band-limited resampling for control threads.

use std::path::{Path, PathBuf};

use rubato::audioadapter::Adapter;
use rubato::audioadapter_buffers::owned::InterleavedOwned;
use rubato::{Fft, FixedSync, Resampler, WindowFunction};

#[derive(Debug, thiserror::Error)]
pub enum IrError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path}: not a readable WAV: {detail}")]
    Wav { path: PathBuf, detail: String },
    #[error("resampling failed: {0}")]
    Resample(String),
}

#[derive(Debug)]
pub struct WavData {
    pub rate: u32,
    pub channels: Vec<Vec<f32>>,
}

/// Reads every WAV track and resamples the impulse responses to `target_rate`.
pub fn read_wav(path: &Path, target_rate: u32) -> Result<WavData, IrError> {
    let mut wav = read_wav_raw(path)?;
    if wav.rate != target_rate {
        wav.channels = resample_channels(&wav.channels, wav.rate, target_rate)?;
        wav.rate = target_rate;
    }
    Ok(wav)
}

pub(crate) fn read_wav_raw(path: &Path) -> Result<WavData, IrError> {
    let mut reader = hound::WavReader::open(path).map_err(|error| map_wav_error(path, error))?;
    let spec = reader.spec();
    if spec.channels == 0 {
        return Err(IrError::Wav {
            path: path.to_path_buf(),
            detail: "the file has no tracks".into(),
        });
    }
    let channel_count = usize::from(spec.channels);
    let frames = reader.duration() as usize;
    if frames == 0 {
        return Err(IrError::Wav {
            path: path.to_path_buf(),
            detail: "the file has no samples".into(),
        });
    }
    let mut channels = vec![Vec::with_capacity(frames); channel_count];
    match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16 | 24 | 32) => {
            let scale = 2.0_f32.powi(i32::from(spec.bits_per_sample) - 1);
            for (index, sample) in reader.samples::<i32>().enumerate() {
                let sample = sample.map_err(|error| map_wav_error(path, error))? as f32 / scale;
                channels[index % channel_count].push(sample);
            }
        }
        (hound::SampleFormat::Float, 32) => {
            for (index, sample) in reader.samples::<f32>().enumerate() {
                channels[index % channel_count]
                    .push(sample.map_err(|error| map_wav_error(path, error))?);
            }
        }
        (format, bits) => {
            return Err(IrError::Wav {
                path: path.to_path_buf(),
                detail: format!("unsupported {bits}-bit {format:?} samples"),
            });
        }
    }
    Ok(WavData {
        rate: spec.sample_rate,
        channels,
    })
}

fn map_wav_error(path: &Path, error: hound::Error) -> IrError {
    match error {
        hound::Error::IoError(source) => IrError::Io {
            path: path.to_path_buf(),
            source,
        },
        other => IrError::Wav {
            path: path.to_path_buf(),
            detail: other.to_string(),
        },
    }
}

pub fn resample_channels(
    channels: &[Vec<f32>],
    input_rate: u32,
    output_rate: u32,
) -> Result<Vec<Vec<f32>>, IrError> {
    if input_rate == output_rate {
        return Ok(channels.to_vec());
    }
    if channels.is_empty() || channels.iter().any(Vec::is_empty) {
        return Err(IrError::Resample(
            "cannot resample an empty channel set or signal".into(),
        ));
    }
    let input_frames = channels.iter().map(Vec::len).max().unwrap_or(0);
    let channel_count = channels.len();
    let mut interleaved = vec![0.0; input_frames * channel_count];
    for (channel, samples) in channels.iter().enumerate() {
        for (frame, sample) in samples.iter().enumerate() {
            interleaved[frame * channel_count + channel] = *sample;
        }
    }
    let input = InterleavedOwned::new_from(interleaved, channel_count, input_frames)
        .map_err(|error| IrError::Resample(error.to_string()))?;
    let chunk_size = input_frames.clamp(1, 1024);
    let mut resampler = Fft::<f32>::new_custom(
        input_rate as usize,
        output_rate as usize,
        chunk_size,
        1,
        channel_count,
        WindowFunction::BlackmanHarris2,
        FixedSync::Both,
    )
    .map_err(|error| IrError::Resample(error.to_string()))?;
    let output = resampler
        .process_all(&input, input_frames, None)
        .map_err(|error| IrError::Resample(error.to_string()))?;
    let scale = input_rate as f32 / output_rate as f32;
    let mut deinterleaved = vec![Vec::with_capacity(output.frames()); channel_count];
    for frame in 0..output.frames() {
        for (channel, destination) in deinterleaved.iter_mut().enumerate() {
            destination.push(
                output
                    .read_sample(channel, frame)
                    .expect("channel and frame are within the output adapter")
                    * scale,
            );
        }
    }
    Ok(deinterleaved)
}
