#![allow(unsafe_code)]

//! Safe ownership and synchronous IOCTLs for Windows control devices.

use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND,
            ERROR_SHARING_VIOLATION, GENERIC_READ, GENERIC_WRITE, HANDLE,
        },
        Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_MODE, OPEN_EXISTING},
        System::IO::DeviceIoControl,
    },
    core::{Error, PCWSTR},
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum DeviceError {
    #[error("the device is not present")]
    NotPresent,
    #[error("access to the device was denied")]
    AccessDenied,
    #[error("the device is open in another process")]
    Busy,
    #[error("Windows device operation failed: {0}")]
    Io(Error),
}

#[derive(Debug)]
pub(crate) struct Device(HANDLE);

// SAFETY: a synchronous file handle has no thread affinity; shared access is
// limited to immutable methods and Windows serializes each DeviceIoControl call.
unsafe impl Send for Device {}
// SAFETY: Device exposes only synchronous IOCTLs over immutable buffers or
// caller-owned mutable outputs; Windows serializes operations on the handle.
unsafe impl Sync for Device {}

impl Device {
    pub(crate) fn open(path: &str) -> Result<Self, DeviceError> {
        let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        // SAFETY: path is NUL-terminated; no optional pointers or borrowed handles are supplied.
        let handle = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .map_err(DeviceError::from)?;
        Ok(Self(handle))
    }

    pub(crate) fn ioctl(
        &self,
        code: u32,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<usize, DeviceError> {
        let input_len = u32::try_from(input.len()).map_err(|_| {
            DeviceError::Io(Error::new(
                windows::core::HRESULT(0x8007_0057_u32 as i32),
                "IOCTL input is larger than DWORD",
            ))
        })?;
        let output_len = u32::try_from(output.len()).map_err(|_| {
            DeviceError::Io(Error::new(
                windows::core::HRESULT(0x8007_0057_u32 as i32),
                "IOCTL output is larger than DWORD",
            ))
        })?;
        let mut returned = 0;
        // SAFETY: input and output point to their advertised lengths and remain
        // live for this synchronous call; returned is a writable DWORD.
        unsafe {
            DeviceIoControl(
                self.0,
                code,
                (!input.is_empty()).then_some(input.as_ptr().cast()),
                input_len,
                (!output.is_empty()).then_some(output.as_mut_ptr().cast()),
                output_len,
                Some(&mut returned),
                None,
            )
        }
        .map_err(DeviceError::from)?;
        let returned = returned as usize;
        if returned > output.len() {
            return Err(DeviceError::Io(Error::new(
                windows::core::HRESULT(0x8007_000D_u32 as i32),
                "driver returned more bytes than the IOCTL output buffer",
            )));
        }
        Ok(returned)
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: this value uniquely owns the handle returned by CreateFileW.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

impl From<Error> for DeviceError {
    fn from(error: Error) -> Self {
        match error.code() {
            code if code == ERROR_FILE_NOT_FOUND.to_hresult()
                || code == ERROR_PATH_NOT_FOUND.to_hresult() =>
            {
                Self::NotPresent
            }
            code if code == ERROR_ACCESS_DENIED.to_hresult() => Self::AccessDenied,
            code if code == ERROR_SHARING_VIOLATION.to_hresult() => Self::Busy,
            _ => Self::Io(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_errors_keep_missing_denied_and_busy_distinct() {
        for (code, expected) in [
            (ERROR_FILE_NOT_FOUND, "NotPresent"),
            (ERROR_PATH_NOT_FOUND, "NotPresent"),
            (ERROR_ACCESS_DENIED, "AccessDenied"),
            (ERROR_SHARING_VIOLATION, "Busy"),
        ] {
            let error = DeviceError::from(Error::from_hresult(code.to_hresult()));
            assert_eq!(format!("{error:?}"), expected);
        }
        let other = DeviceError::from(Error::from_hresult(windows::core::HRESULT(
            0x8000_4005_u32 as i32,
        )));
        assert!(matches!(other, DeviceError::Io(_)));
    }
}
