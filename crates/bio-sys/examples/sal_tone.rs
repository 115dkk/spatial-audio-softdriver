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
        let endpoint = bio_sys::devices::find(Some(&pattern))?;
        println!("stimulus endpoint: {}", endpoint.name);
        let enumerator = DeviceEnumerator::new()?;
        let device = enumerator.get_device(&endpoint.id)?;
        let wave = WaveFormat::new(32, 32, &SampleType::Int, 48_000, 12, Some(0x2D63F));
        let mut client = device.get_iaudioclient()?;
        let (period, _) = client.get_device_period()?;
        client.initialize_client(
            &wave,
            &Direction::Render,
            &StreamMode::EventsExclusive { period_hns: period },
        )?;
        let frames = client.get_buffer_size()? as usize;
        let event = client.set_get_eventhandle()?;
        let render = client.get_audiorenderclient()?;
        let mut bytes = vec![0; frames * 12 * 4];
        render.write_to_device(frames, &bytes, None)?;
        client.start_stream()?;
        let mut position = 0_usize;
        // 12 distinct low-level tones let downstream artifacts show whether
        // the live path carries signal, rather than just silent buffers.
        while position < 48_000 * 12 {
            event.wait_for_event(2000)?;
            for (i, frame) in bytes.as_chunks_mut::<48>().0.iter_mut().enumerate() {
                for (channel, sample) in frame.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let hz = 220.0 + 37.0 * channel as f64;
                    let phase = (position + i) as f64 * hz * std::f64::consts::TAU / 48_000.0;
                    let value = (phase.sin() * 0.03 * i32::MAX as f64) as i32;
                    sample.copy_from_slice(&value.to_le_bytes());
                }
            }
            render.write_to_device(frames, &bytes, None)?;
            position += frames;
        }
        client.stop_stream()?;
        println!("submitted {position} frames of PCM 48000 Hz, 12 channels, 32-bit, mask 0x2D63F");
        Ok(())
    })();
    wasapi::deinitialize();
    result
}

#[cfg(not(windows))]
fn main() {
    eprintln!("sal_tone requires Windows");
}
