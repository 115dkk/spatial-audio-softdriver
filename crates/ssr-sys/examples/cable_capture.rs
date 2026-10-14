#![forbid(unsafe_code)]

//! Captures the far end of a virtual cable. Samples stay in preallocated memory
//! until capture stops; only then is the extensible float WAV written. In
//! exclusive mode the stream takes the first of float, 32-bit, 24-in-32 and
//! 16-bit integer that the endpoint accepts (the order format_probe reports),
//! and integer samples are converted to float for the WAV.

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::VecDeque;
    use std::io::{BufWriter, Write};
    use std::time::Instant;
    use wasapi::{DeviceEnumerator, Direction, SampleType, ShareMode, StreamMode, WaveFormat};

    struct Options {
        endpoint: String,
        seconds: f64,
        channels: u16,
        mask: u32,
        exclusive: bool,
        path: String,
    }

    fn number(value: &str) -> Result<u32, Box<dyn std::error::Error>> {
        if let Some(hex) = value.strip_prefix("0x") {
            Ok(u32::from_str_radix(hex, 16)?)
        } else {
            Ok(value.parse()?)
        }
    }

    fn options() -> Result<Options, Box<dyn std::error::Error>> {
        let mut endpoint = "CABLE Output".to_owned();
        let mut seconds = 10.0_f64;
        let mut channels = 2_u16;
        let mut mask = None;
        let mut exclusive = false;
        let mut path = None;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--endpoint" => endpoint = args.next().ok_or("--endpoint needs a name")?,
                "--seconds" => seconds = args.next().ok_or("--seconds needs a value")?.parse()?,
                "--channels" => {
                    channels = args.next().ok_or("--channels needs a value")?.parse()?
                }
                "--mask" => mask = Some(number(&args.next().ok_or("--mask needs a value")?)?),
                "--exclusive" => exclusive = true,
                _ if arg.starts_with('-') => return Err(format!("unknown argument {arg:?}").into()),
                _ if path.is_none() => path = Some(arg),
                _ => return Err("pass one output WAV path".into()),
            }
        }
        if !seconds.is_finite() || seconds <= 0.0 {
            return Err("--seconds must be finite and positive".into());
        }
        if channels == 0 || channels > 32 {
            return Err("--channels must be in 1..=32".into());
        }
        let mask = mask.unwrap_or(if channels == 2 { 3 } else { 0 });
        if mask != 0 && mask.count_ones() != u32::from(channels) {
            return Err("--mask bit count must match --channels".into());
        }
        Ok(Options {
            endpoint,
            seconds,
            channels,
            mask,
            exclusive,
            path: path.ok_or("pass the output WAV path")?,
        })
    }

    fn write_wav(
        path: &str,
        channels: u16,
        mask: u32,
        bytes: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let data_bytes = u32::try_from(bytes.len())?;
        let block_align = channels.checked_mul(4).ok_or("block alignment overflow")?;
        let byte_rate = 48_000_u32
            .checked_mul(u32::from(block_align))
            .ok_or("byte rate overflow")?;
        let riff_size = 60_u32.checked_add(data_bytes).ok_or("RIFF size overflow")?;
        let mut out = BufWriter::new(std::fs::File::create(path)?);
        out.write_all(b"RIFF")?;
        out.write_all(&riff_size.to_le_bytes())?;
        out.write_all(b"WAVEfmt ")?;
        out.write_all(&40_u32.to_le_bytes())?;
        out.write_all(&0xfffe_u16.to_le_bytes())?;
        out.write_all(&channels.to_le_bytes())?;
        out.write_all(&48_000_u32.to_le_bytes())?;
        out.write_all(&byte_rate.to_le_bytes())?;
        out.write_all(&block_align.to_le_bytes())?;
        out.write_all(&32_u16.to_le_bytes())?;
        out.write_all(&22_u16.to_le_bytes())?;
        out.write_all(&32_u16.to_le_bytes())?;
        out.write_all(&mask.to_le_bytes())?;
        out.write_all(&[
            0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38,
            0x9b, 0x71,
        ])?;
        out.write_all(b"data")?;
        out.write_all(&data_bytes.to_le_bytes())?;
        out.write_all(bytes)?;
        out.flush()?;
        Ok(())
    }

    let options = options()?;
    wasapi::initialize_mta().ok()?;
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let enumerator = DeviceEnumerator::new()?;
        let devices = enumerator.get_device_collection(&Direction::Capture)?;
        let mut cable = None;
        for i in 0..devices.get_nbr_devices()? {
            let device = devices.get_device_at_index(i)?;
            if device
                .get_friendlyname()?
                .to_lowercase()
                .contains(&options.endpoint.to_lowercase())
            {
                cable = Some(device);
                break;
            }
        }
        let device = cable.ok_or("capture endpoint missing")?;
        println!("capture endpoint: {}", device.get_friendlyname()?);
        let mut client = device.get_iaudioclient()?;
        let shape = |bits: usize, valid: usize, kind: &SampleType| {
            WaveFormat::new(
                bits,
                valid,
                kind,
                48_000,
                usize::from(options.channels),
                Some(options.mask),
            )
        };
        let (bits, kind) = if options.exclusive {
            [
                (32, 32, SampleType::Float),
                (32, 32, SampleType::Int),
                (32, 24, SampleType::Int),
                (16, 16, SampleType::Int),
            ]
            .into_iter()
            .find(|(bits, valid, kind)| {
                client
                    .is_supported(&shape(*bits, *valid, kind), &ShareMode::Exclusive)
                    .is_ok_and(|closest| closest.is_none())
            })
            .map(|(bits, valid, kind)| ((bits, valid), kind))
            .ok_or("the capture endpoint takes none of the exclusive formats")?
        } else {
            ((32, 32), SampleType::Float)
        };
        let wave = shape(bits.0, bits.1, &kind);
        println!(
            "capture format: {}-bit {} ({} valid bits)",
            bits.0,
            if matches!(kind, SampleType::Float) {
                "float"
            } else {
                "integer"
            },
            bits.1
        );
        if options.exclusive {
            let (_, minimum) = client.get_device_period()?;
            client.initialize_client(
                &wave,
                &Direction::Capture,
                &StreamMode::EventsExclusive {
                    period_hns: minimum,
                },
            )?;
        } else {
            client.initialize_client(
                &wave,
                &Direction::Capture,
                &StreamMode::EventsShared {
                    autoconvert: true,
                    buffer_duration_hns: 200_000,
                },
            )?;
        }
        let event = client.set_get_eventhandle()?;
        let capture = client.get_audiocaptureclient()?;
        let capacity = ((options.seconds + 2.0) * 48_000.0).ceil() as usize
            * usize::from(options.channels)
            * 4;
        let mut queue = VecDeque::with_capacity(capacity);
        client.start_stream()?;
        let start = Instant::now();
        let capture_result = (|| -> Result<(), Box<dyn std::error::Error>> {
            while start.elapsed().as_secs_f64() < options.seconds {
                event.wait_for_event(2000)?;
                let needed = client.get_buffer_size()? as usize * usize::from(options.channels) * 4;
                if queue.capacity() - queue.len() < needed {
                    return Err("capture exceeded preallocated capacity".into());
                }
                capture.read_from_device_to_deque(&mut queue)?;
            }
            Ok(())
        })();
        client.stop_stream()?;
        capture_result?;
        let raw: Vec<_> = queue.into_iter().collect();
        // Integer samples are left-aligned in their container (24-in-32
        // included), so dividing by the container's full scale is exact.
        let samples: Vec<f32> = match (bits.0, &kind) {
            (32, SampleType::Float) => raw
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect(),
            (32, _) => raw
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| (f64::from(i32::from_le_bytes(*b)) / 2_147_483_648.0) as f32)
                .collect(),
            _ => raw
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| f32::from(i16::from_le_bytes(*b)) / 32_768.0)
                .collect(),
        };
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let channels = usize::from(options.channels);
        let mut peak = vec![0.0_f32; channels];
        let mut energy = vec![0.0_f64; channels];
        let mut active = vec![0_usize; channels];
        for (i, &sample) in samples.iter().enumerate() {
            if !sample.is_finite() {
                return Err("non-finite capture sample".into());
            }
            let channel = i % channels;
            peak[channel] = peak[channel].max(sample.abs());
            energy[channel] += f64::from(sample).powi(2);
            active[channel] += usize::from(sample.abs() >= 0.0001);
        }
        write_wav(&options.path, options.channels, options.mask, &bytes)?;
        let frames = bytes.len() / (4 * channels);
        let rms = energy
            .iter()
            .map(|sum| (sum / frames.max(1) as f64).sqrt())
            .collect::<Vec<_>>();
        println!("captured {frames} frames; peak {peak:?}; active frames {active:?}; RMS {rms:?}");
        if options.channels == 2 && (frames < 48_000 || active.iter().any(|&count| count < 96_000))
        {
            return Err("VB-CABLE capture lacks sustained signal in an ear".into());
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
