#![allow(unsafe_code)]

//! Development-only adapter for SAL's black-box capture interface.

use crate::{
    qpc,
    sal_format::{
        SAL_DEVICE_PATH, SAL_HEADER_LEN, SAL_IOCTL_READ, SAL_IOCTL_RESET, SalError, SalTranslator,
    },
};
use bio_abi::RecordWriter;
use bio_types::{CaptureError, CaptureSource, TrimUnsupported};
use std::time::{Duration, Instant};
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

#[derive(Debug)]
pub struct SalRing {
    handle: HANDLE,
    scratch: Vec<u8>,
    pending: Option<(usize, u64)>,
    translator: SalTranslator,
}

// SAFETY: a synchronous file handle has no thread affinity; exclusive &mut
// access serializes all IOCTLs and ownership closes it exactly once.
unsafe impl Send for SalRing {}

impl SalRing {
    pub fn open() -> Result<Self, CaptureError> {
        let path: Vec<u16> = SAL_DEVICE_PATH.encode_utf16().chain(Some(0)).collect();
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
        .map_err(capture_error)?;
        let ring = Self {
            handle,
            scratch: vec![0; SAL_HEADER_LEN + 512 * 1024],
            pending: None,
            translator: SalTranslator::new(),
        };
        let mut returned = 0;
        // SAFETY: the live handle is synchronous; RESET has no input/output and returned is writable.
        unsafe {
            DeviceIoControl(
                ring.handle,
                SAL_IOCTL_RESET,
                None,
                0,
                None,
                0,
                Some(&mut returned),
                None,
            )
        }
        .map_err(capture_error)?;
        Ok(ring)
    }

    fn poll(&mut self) -> Result<(), CaptureError> {
        let mut returned = 0;
        // SAFETY: scratch owns the full advertised writable output; the synchronous
        // call finishes before scratch or returned is accessed again.
        unsafe {
            DeviceIoControl(
                self.handle,
                SAL_IOCTL_READ,
                None,
                0,
                Some(self.scratch.as_mut_ptr().cast()),
                self.scratch.len() as u32,
                Some(&mut returned),
                None,
            )
        }
        .map_err(capture_error)?;
        if returned as usize > self.scratch.len() {
            return Err(CaptureError::Io(
                "SAL returned an invalid output length".into(),
            ));
        }
        self.pending = Some((returned as usize, qpc::now()));
        Ok(())
    }
}

impl CaptureSource for SalRing {
    fn read(&mut self, buf: &mut [u8], wait: Duration) -> Result<usize, CaptureError> {
        let started = Instant::now();
        loop {
            if self.pending.is_none() {
                self.poll()?;
            }
            let (len, qpc) = self.pending.expect("poll produced a result");
            let mut writer = RecordWriter::new(buf);
            match self
                .translator
                .translate(&self.scratch[..len], qpc, &mut writer)
            {
                Ok(_) => self.pending = None,
                // Preserve the READ and its original QPC. Returning an error is
                // essential: Ok(0) would falsely report a timeout forever.
                Err(SalError::BufferFull) => {
                    return Err(CaptureError::Io(
                        "record buffer cannot hold the pending SAL READ".into(),
                    ));
                }
                Err(error) => {
                    self.pending = None;
                    return Err(CaptureError::Io(format!(
                        "{error}; SAL READ header: {:?}",
                        crate::sal_format::parse_header(&self.scratch[..len])
                    )));
                }
            }
            if !writer.is_empty() {
                return Ok(writer.len());
            }
            let remaining = wait.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok(0);
            }
            std::thread::sleep(remaining.min(Duration::from_millis(2)));
            if started.elapsed() >= wait {
                return Ok(0);
            }
        }
    }

    fn set_rate_trim(&mut self, _ppm: f64) -> Result<(), TrimUnsupported> {
        Err(TrimUnsupported)
    }
}

impl Drop for SalRing {
    fn drop(&mut self) {
        // SAFETY: this struct uniquely owns the handle returned by CreateFileW.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

fn capture_error(error: Error) -> CaptureError {
    match error.code() {
        code if code == ERROR_FILE_NOT_FOUND.to_hresult()
            || code == ERROR_PATH_NOT_FOUND.to_hresult() =>
        {
            CaptureError::NotPresent
        }
        code if code == ERROR_ACCESS_DENIED.to_hresult() => CaptureError::AccessDenied,
        code if code == ERROR_SHARING_VIOLATION.to_hresult() => CaptureError::Busy,
        _ => CaptureError::Io(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sal_open_errors_keep_missing_denied_and_busy_distinct() {
        for (code, expected) in [
            (ERROR_FILE_NOT_FOUND, CaptureError::NotPresent),
            (ERROR_PATH_NOT_FOUND, CaptureError::NotPresent),
            (ERROR_ACCESS_DENIED, CaptureError::AccessDenied),
            (ERROR_SHARING_VIOLATION, CaptureError::Busy),
        ] {
            assert_eq!(
                capture_error(Error::from_hresult(code.to_hresult())),
                expected
            );
        }
    }
}
