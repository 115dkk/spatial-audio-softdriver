#![forbid(unsafe_code)]

//! Captures the far end of VB-CABLE for runner evidence. Samples stay in a
//! preallocated queue until capture stops; only then is the WAV written.

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{collections::VecDeque, time::Instant};
    use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

    wasapi::initialize_mta().ok()?;
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::args().nth(1).ok_or("pass the output WAV path")?;
        let enumerator = DeviceEnumerator::new()?;
        let devices = enumerator.get_device_collection(&Direction::Capture)?;
        let mut cable = None;
        for i in 0..devices.get_nbr_devices()? {
            let device = devices.get_device_at_index(i)?;
            if device.get_friendlyname()?.contains("CABLE Output") {
                cable = Some(device);
                break;
            }
        }
        let device = cable.ok_or("CABLE Output capture endpoint missing")?;
        println!("capture endpoint: {}", device.get_friendlyname()?);
        let mut client = device.get_iaudioclient()?;
        let wave = WaveFormat::new(32, 32, &SampleType::Float, 48_000, 2, Some(3));
        client.initialize_client(
            &wave,
            &Direction::Capture,
            &StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: 200_000,
            },
        )?;
        let event = client.set_get_eventhandle()?;
        let capture = client.get_audiocaptureclient()?;
        let mut queue = VecDeque::with_capacity(48_000 * 2 * 4 * 12);
        client.start_stream()?;
        let start = Instant::now();
        let capture_result = (|| -> Result<(), Box<dyn std::error::Error>> {
            while start.elapsed().as_secs_f64() < 10.0 {
                event.wait_for_event(2000)?;
                // Keep a full device buffer free without growing the queue.
                if queue.capacity() - queue.len() < client.get_buffer_size()? as usize * 8 {
                    return Err("capture exceeded preallocated capacity".into());
                }
                capture.read_from_device_to_deque(&mut queue)?;
            }
            Ok(())
        })();
        client.stop_stream()?;
        capture_result?;
        let bytes: Vec<_> = queue.into_iter().collect();
        let mut writer = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels: 2,
                sample_rate: 48_000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )?;
        let mut peak = [0.0_f32; 2];
        let mut energy = [0.0_f64; 2];
        for (i, sample) in bytes.as_chunks::<4>().0.iter().enumerate() {
            let sample = f32::from_le_bytes(*sample);
            if !sample.is_finite() {
                return Err("non-finite capture sample".into());
            }
            peak[i % 2] = peak[i % 2].max(sample.abs());
            energy[i % 2] += f64::from(sample).powi(2);
            writer.write_sample(sample)?;
        }
        writer.finalize()?;
        let frames = bytes.len() / 8;
        println!(
            "captured {frames} frames; peak {peak:?}; RMS {:?}",
            energy.map(|sum| (sum / frames.max(1) as f64).sqrt())
        );
        if frames < 48_000 || peak.iter().any(|&p| p < 0.0001) {
            return Err("VB-CABLE capture is empty or silent in an ear".into());
        }
        Ok(())
    })();
    wasapi::deinitialize();
    result
}

#[cfg(not(windows))]
fn main() {
    eprintln!("cable_capture requires Windows");
}
