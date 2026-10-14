#![forbid(unsafe_code)]

//! Runner stimulus using SAL's documented PCM 7.1.4 endpoint format.
//! This is a synthetic transport probe, not a Dolby encoder or a SAL golden.

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

    wasapi::initialize_mta().ok()?;
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let pattern = std::env::args()
            .nth(1)
            .ok_or("pass the SAL endpoint name")?;
        let endpoint = ssr_sys::devices::find(Some(&pattern))?;
        println!("stimulus endpoint: {}", endpoint.name);
        let enumerator = DeviceEnumerator::new()?;
        let device = enumerator.get_device(&endpoint.id)?;
        let mut opened = None;
        for (bits, kind) in [
            (32, SampleType::Int),
            (16, SampleType::Int),
            (32, SampleType::Float),
        ] {
            let wave = WaveFormat::new(bits, bits, &kind, 48_000, 12, Some(0x2D63F));
            let mut client = device.get_iaudioclient()?;
            let (period, _) = client.get_device_period()?;
            match client.initialize_client(
                &wave,
                &Direction::Render,
                &StreamMode::EventsExclusive { period_hns: period },
            ) {
                Ok(()) => {
                    println!(
                        "selected PCM: {bits}-bit {kind:?}, 48000 Hz, 12 channels, mask 0x2D63F"
                    );
                    opened = Some((client, bits, kind));
                    break;
                }
                Err(error) => println!("rejected {bits}-bit {kind:?}: {error}"),
            }
        }
        let (client, bits, kind) = opened.ok_or("no documented 12-channel PCM format accepted")?;
        let frames = client.get_buffer_size()? as usize;
        let event = client.set_get_eventhandle()?;
        let render = client.get_audiorenderclient()?;
        let sample_bytes = bits / 8;
        let mut bytes = vec![0; frames * 12 * sample_bytes];
        render.write_to_device(frames, &bytes, None)?;
        client.start_stream()?;
        let mut position = 0_usize;
        // 12 distinct low-level tones let downstream artifacts show whether
        // the live path carries signal, rather than just silent buffers.
        while position < 48_000 * 12 {
            event.wait_for_event(2000)?;
            for (i, frame) in bytes.chunks_exact_mut(12 * sample_bytes).enumerate() {
                for (channel, sample) in frame.chunks_exact_mut(sample_bytes).enumerate() {
                    let hz = 220.0 + 37.0 * channel as f64;
                    let phase = (position + i) as f64 * hz * std::f64::consts::TAU / 48_000.0;
                    let value = phase.sin() * 0.03;
                    match (&kind, bits) {
                        (SampleType::Float, 32) => {
                            sample.copy_from_slice(&(value as f32).to_le_bytes())
                        }
                        (SampleType::Int, 32) => sample
                            .copy_from_slice(&((value * i32::MAX as f64) as i32).to_le_bytes()),
                        (SampleType::Int, 16) => sample
                            .copy_from_slice(&((value * i16::MAX as f64) as i16).to_le_bytes()),
                        _ => unreachable!("negotiated format"),
                    }
                }
            }
            render.write_to_device(frames, &bytes, None)?;
            position += frames;
        }
        client.stop_stream()?;
        println!(
            "submitted {position} frames of PCM 48000 Hz, 12 channels, {bits}-bit {kind:?}, mask 0x2D63F"
        );
        Ok(())
    })();
    wasapi::deinitialize();
    result
}

#[cfg(not(windows))]
fn main() {
    eprintln!("sal_tone requires Windows");
}
