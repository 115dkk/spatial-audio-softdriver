//! Active Windows render endpoints. Enumeration is not a real-time operation.

use crate::output::Encoding;
use ssr_types::{OutputError, OutputLayout};
use wasapi::{Device, DeviceEnumerator, Direction, ShareMode, WasapiError};
use windows::Win32::{
    Foundation::ERROR_NOT_FOUND,
    Media::Audio::{
        AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_E_RESOURCES_INVALIDATED,
        AUDCLNT_E_UNSUPPORTED_FORMAT,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MixFormat {
    pub rate: u32,
    pub channels: u16,
    pub channel_mask: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutSupport {
    pub channels: u16,
    pub channel_mask: u32,
    pub exclusive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointFormats {
    pub endpoint: Endpoint,
    pub mix: MixFormat,
    pub layouts: Vec<LayoutSupport>,
}

pub fn render_endpoints() -> Result<Vec<Endpoint>, OutputError> {
    let _com = ComGuard::new()?;
    let enumerator = DeviceEnumerator::new().map_err(output_error)?;
    let default = match enumerator.get_default_device(&Direction::Render) {
        Ok(device) => Some(device.get_id().map_err(output_error)?),
        Err(error) => match output_error(error) {
            OutputError::NotFound => None,
            error => return Err(error),
        },
    };
    let devices = enumerator
        .get_device_collection(&Direction::Render)
        .map_err(output_error)?;
    let mut endpoints = Vec::new();
    for index in 0..devices.get_nbr_devices().map_err(output_error)? {
        let device = devices.get_device_at_index(index).map_err(output_error)?;
        let id = device.get_id().map_err(output_error)?;
        endpoints.push(Endpoint {
            is_default: default.as_ref() == Some(&id),
            id,
            name: device.get_friendlyname().map_err(output_error)?,
        });
    }
    Ok(endpoints)
}

const STANDARD_LAYOUTS: [OutputLayout; 5] = [
    OutputLayout::STEREO,
    OutputLayout {
        channels: 4,
        channel_mask: 0x33,
    },
    OutputLayout {
        channels: 6,
        channel_mask: 0x3f,
    },
    OutputLayout {
        channels: 8,
        channel_mask: 0x63f,
    },
    OutputLayout {
        channels: 12,
        channel_mask: 0x2d63f,
    },
];

pub(crate) fn supports_layout(device: &Device, layout: OutputLayout) -> bool {
    Encoding::ALL.into_iter().any(|encoding| {
        device.get_iaudioclient().is_ok_and(|client| {
            client
                .is_supported(&encoding.wave(48_000, layout), &ShareMode::Exclusive)
                .is_ok_and(|closest| closest.is_none())
        })
    })
}

pub fn render_endpoint_formats() -> Result<Vec<EndpointFormats>, OutputError> {
    let _com = ComGuard::new()?;
    let enumerator = DeviceEnumerator::new().map_err(output_error)?;
    let default = enumerator
        .get_default_device(&Direction::Render)
        .ok()
        .and_then(|device| device.get_id().ok());
    let devices = enumerator
        .get_device_collection(&Direction::Render)
        .map_err(output_error)?;
    let mut formats = Vec::new();
    for index in 0..devices.get_nbr_devices().map_err(output_error)? {
        let device = devices.get_device_at_index(index).map_err(output_error)?;
        let id = device.get_id().map_err(output_error)?;
        let wave = device
            .get_iaudioclient()
            .and_then(|client| client.get_mixformat())
            .map_err(output_error)?;
        formats.push(EndpointFormats {
            endpoint: Endpoint {
                is_default: default.as_ref() == Some(&id),
                id,
                name: device.get_friendlyname().map_err(output_error)?,
            },
            mix: MixFormat {
                rate: wave.get_samplespersec(),
                channels: wave.get_nchannels(),
                channel_mask: wave.get_dwchannelmask(),
            },
            layouts: STANDARD_LAYOUTS
                .into_iter()
                .map(|layout| LayoutSupport {
                    channels: layout.channels,
                    channel_mask: layout.channel_mask,
                    exclusive: supports_layout(&device, layout),
                })
                .collect(),
        });
    }
    Ok(formats)
}

pub fn find(pattern: Option<&str>) -> Result<Endpoint, OutputError> {
    let endpoints = render_endpoints()?;
    let pattern = pattern.map(str::to_lowercase);
    endpoints
        .into_iter()
        .find(|endpoint| {
            pattern.as_ref().map_or(endpoint.is_default, |p| {
                endpoint.name.to_lowercase().contains(p)
            })
        })
        .ok_or(OutputError::NotFound)
}

// A successful S_FALSE also needs balancing. The Rc marker ensures that COM
// is uninitialized on the same thread, after all interface fields are dropped.
#[derive(Debug)]
pub(crate) struct ComGuard(std::marker::PhantomData<std::rc::Rc<()>>);

impl ComGuard {
    pub(crate) fn new() -> Result<Self, OutputError> {
        wasapi::initialize_mta()
            .ok()
            .map_err(|e| output_error(e.into()))?;
        crate::apartment::pin();
        Ok(Self(std::marker::PhantomData))
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

pub(crate) fn output_error(error: WasapiError) -> OutputError {
    match &error {
        WasapiError::DeviceNotFound(_) => OutputError::NotFound,
        WasapiError::UnsupportedFormat | WasapiError::UnsupportedSubformat(_) => {
            OutputError::FormatRejected(error.to_string())
        }
        WasapiError::Windows(e)
            if e.code() == AUDCLNT_E_DEVICE_INVALIDATED
                || e.code() == AUDCLNT_E_RESOURCES_INVALIDATED =>
        {
            OutputError::Removed
        }
        WasapiError::Windows(e) if e.code() == AUDCLNT_E_UNSUPPORTED_FORMAT => {
            OutputError::FormatRejected(error.to_string())
        }
        WasapiError::Windows(e) if e.code() == AUDCLNT_E_DEVICE_IN_USE => OutputError::Busy,
        WasapiError::Windows(e) if e.code() == ERROR_NOT_FOUND.to_hresult() => {
            OutputError::NotFound
        }
        _ => OutputError::Io(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_errors_keep_removed_busy_and_not_found_distinct() {
        for (code, expected) in [
            (AUDCLNT_E_DEVICE_INVALIDATED, OutputError::Removed),
            (AUDCLNT_E_RESOURCES_INVALIDATED, OutputError::Removed),
            (AUDCLNT_E_DEVICE_IN_USE, OutputError::Busy),
            (ERROR_NOT_FOUND.to_hresult(), OutputError::NotFound),
        ] {
            assert_eq!(
                output_error(windows::core::Error::from_hresult(code).into()),
                expected
            );
        }
    }
}
