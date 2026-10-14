//! Development-only adapter for SAL's black-box capture interface.

use crate::{
    device::{Device, DeviceError},
    qpc,
    sal_format::{
        SAL_DEVICE_PATH, SAL_HEADER_LEN, SAL_IOCTL_READ, SAL_IOCTL_RESET, SalError, SalTranslator,
    },
};
use ssr_abi::RecordWriter;
use ssr_types::{CaptureError, CaptureSource, TrimUnsupported};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct SalRing {
    device: Device,
    scratch: Vec<u8>,
    pending: Option<(usize, u64)>,
    translator: SalTranslator,
}

impl SalRing {
    pub fn open() -> Result<Self, CaptureError> {
        let ring = Self {
            device: Device::open(SAL_DEVICE_PATH).map_err(capture_error)?,
            scratch: vec![0; SAL_HEADER_LEN + 512 * 1024],
            pending: None,
            translator: SalTranslator::with_qpc_frequency(qpc::frequency()),
        };
        ring.device
            .ioctl(SAL_IOCTL_RESET, &[], &mut [])
            .map_err(capture_error)?;
        Ok(ring)
    }

    fn poll(&mut self) -> Result<(), CaptureError> {
        let returned = self
            .device
            .ioctl(SAL_IOCTL_READ, &[], &mut self.scratch)
            .map_err(capture_error)?;
        self.pending = Some((returned, qpc::now()));
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
                .translate_chunked(&self.scratch[..len], qpc, &mut writer)
            {
                Ok(_) => {
                    if !self.translator.has_pending() {
                        self.pending = None;
                    }
                }
                // No single record fits, so a later call with the same buffer
                // cannot progress either. The engine buffer fits every record.
                Err(SalError::BufferFull) => {
                    return Err(CaptureError::Io(
                        "record buffer cannot hold one pending SAL record".into(),
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

    fn now_qpc(&self) -> Option<u64> {
        Some(qpc::now())
    }

    fn set_rate_trim(&mut self, _ppm: f64) -> Result<(), TrimUnsupported> {
        Err(TrimUnsupported)
    }

    fn reconnect(&mut self) -> Result<(), CaptureError> {
        *self = Self::open()?;
        Ok(())
    }
}

pub(crate) fn capture_error(error: DeviceError) -> CaptureError {
    match error {
        DeviceError::NotPresent => CaptureError::NotPresent,
        DeviceError::AccessDenied => CaptureError::AccessDenied,
        DeviceError::Busy => CaptureError::Busy,
        DeviceError::Io(error) => CaptureError::Io(error.to_string()),
    }
}
