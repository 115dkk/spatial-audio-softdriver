//! Active Windows render endpoints. Enumeration is not a real-time operation.

use bio_types::OutputError;
use wasapi::{DeviceEnumerator, Direction, WasapiError};
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
