#![forbid(unsafe_code)]

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use wasapi::{DeviceEnumerator, Direction, SampleType, ShareMode, WaveFormat};

    fn json_string(value: &str) -> String {
        let mut out = String::from("\"");
        for ch in value.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                ch if ch < ' ' => out.push_str(&format!("\\u{:04x}", ch as u32)),
                ch => out.push(ch),
            }
        }
        out.push('"');
        out
    }

    let mut capture = false;
    let mut pattern = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--capture" => capture = true,
            "--endpoint" => pattern = Some(args.next().ok_or("--endpoint needs a name")?),
            _ if pattern.is_none() => pattern = Some(arg),
            _ => return Err(format!("unknown argument {arg:?}").into()),
        }
    }
    let direction = if capture {
        Direction::Capture
    } else {
        Direction::Render
    };
    wasapi::initialize_mta().ok()?;
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let enumerator = DeviceEnumerator::new()?;
        let devices = enumerator.get_device_collection(&direction)?;
        let pattern = pattern.as_deref().unwrap_or("").to_lowercase();
        let mut selected = None;
        for index in 0..devices.get_nbr_devices()? {
            let device = devices.get_device_at_index(index)?;
            if device.get_friendlyname()?.to_lowercase().contains(&pattern) {
                selected = Some(device);
                break;
            }
        }
        let device = selected.ok_or("no endpoint matches the name")?;
        let mix = device.get_iaudioclient()?.get_mixformat()?;
        let layouts = [
            (2, 0x3_u32),
            (4, 0x33),
            (6, 0x3f),
            (8, 0x63f),
            (12, 0x2d63f),
        ];
        let mut probed = Vec::new();
        for (channels, mask) in layouts {
            let supported = [
                (32, 32, SampleType::Float),
                (32, 32, SampleType::Int),
                (32, 24, SampleType::Int),
                (16, 16, SampleType::Int),
            ]
            .into_iter()
            .any(|(bits, valid, kind)| {
                device.get_iaudioclient().is_ok_and(|client| {
                    let wave = WaveFormat::new(bits, valid, &kind, 48_000, channels, Some(mask));
                    client
                        .is_supported(&wave, &ShareMode::Exclusive)
                        .is_ok_and(|closest| closest.is_none())
                })
            });
            probed.push((channels, mask, supported));
        }
        println!("{{");
        println!("  \"id\": {},", json_string(&device.get_id()?));
        println!("  \"name\": {},", json_string(&device.get_friendlyname()?));
        println!(
            "  \"direction\": \"{}\",",
            if capture { "capture" } else { "render" }
        );
        println!(
            "  \"mix\": {{\"rate\": {}, \"channels\": {}, \"channel_mask\": {}}},",
            mix.get_samplespersec(),
            mix.get_nchannels(),
            mix.get_dwchannelmask()
        );
        println!("  \"layouts\": [");
        for (index, (channels, mask, supported)) in probed.iter().enumerate() {
            println!(
                "    {{\"channels\": {channels}, \"channel_mask\": {mask}, \"exclusive\": {supported}}}{}",
                if index + 1 == probed.len() { "" } else { "," }
            );
        }
        println!("  ]\n}}");
        Ok(())
    })();
    wasapi::deinitialize();
    result
}

#[cfg(not(windows))]
fn main() {
    eprintln!("format_probe requires Windows");
}
