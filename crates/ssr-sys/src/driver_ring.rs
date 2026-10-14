//! Spatial SoftReceiver driver control-ring capture.

#[cfg(any(windows, test))]
use std::mem::size_of;
#[cfg(windows)]
use std::sync::{Arc, Mutex, Weak};
#[cfg(windows)]
use std::time::Duration;

use ssr_abi::TRIM_LIMIT_PPM;
#[cfg(any(windows, test))]
use ssr_abi::{ABI_VERSION, AbiInfo};
#[cfg(windows)]
use ssr_abi::{
    CONTROL_DEVICE_PATH, DriverStats, IOCTL_SSR_GET_STATS, IOCTL_SSR_QUERY_INFO, IOCTL_SSR_READ,
    IOCTL_SSR_SET_RATE_TRIM, MAX_DATA_PAYLOAD, RateTrim, ReadRequest, record_size,
};
#[cfg(any(windows, test))]
use ssr_types::CaptureError;
#[cfg(windows)]
use ssr_types::{CaptureSource, TrimUnsupported};

#[cfg(any(windows, test))]
use zerocopy::{FromBytes, IntoBytes};

#[cfg(windows)]
use crate::device::Device;
#[cfg(windows)]
use crate::sal::capture_error;

const Q16_SCALE: f64 = 65_536.0;

/// Converts a requested trim to the driver's signed Q16 ppm contract.
///
/// Non-finite requests become zero; finite values are clamped before rounding.
pub fn trim_ppm_q16(ppm: f64) -> i32 {
    if !ppm.is_finite() {
        return 0;
    }
    let limit = f64::from(TRIM_LIMIT_PPM);
    (ppm.clamp(-limit, limit) * Q16_SCALE).round() as i32
}

#[cfg(any(windows, test))]
fn parse_query_info(output: &[u8]) -> Result<AbiInfo, CaptureError> {
    let info = AbiInfo::read_from_bytes(output).map_err(|_| {
        CaptureError::Io(format!(
            "QUERY_INFO returned {} bytes, expected {}",
            output.len(),
            size_of::<AbiInfo>()
        ))
    })?;
    let expected_size = size_of::<AbiInfo>() as u32;
    if info.struct_size != expected_size || info.abi_version != ABI_VERSION {
        return Err(CaptureError::Io(format!(
            "driver ABI mismatch: version {} (expected {}), struct_size {} (expected {})",
            info.abi_version, ABI_VERSION, info.struct_size, expected_size
        )));
    }
    Ok(info)
}

#[cfg(windows)]
#[derive(Debug)]
pub struct DriverRing {
    device: Option<Arc<Device>>,
    observer_connection: Arc<Mutex<ObserverConnection>>,
    info: AbiInfo,
}

#[cfg(windows)]
#[derive(Debug)]
struct ObserverConnection {
    device: Weak<Device>,
    generation: u64,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug)]
pub struct DriverStatsSample {
    pub generation: u64,
    pub stats: DriverStats,
}

#[cfg(windows)]
impl DriverRing {
    pub fn open() -> Result<Self, CaptureError> {
        let device = Device::open(CONTROL_DEVICE_PATH).map_err(capture_error)?;
        let mut output = [0; size_of::<AbiInfo>()];
        let returned = device
            .ioctl(IOCTL_SSR_QUERY_INFO, &[], &mut output)
            .map_err(capture_error)?;
        let bytes = output.get(..returned).ok_or_else(|| {
            CaptureError::Io(format!("QUERY_INFO returned invalid byte count {returned}"))
        })?;
        let info = parse_query_info(bytes)?;
        let device = Arc::new(device);
        Ok(Self {
            observer_connection: Arc::new(Mutex::new(ObserverConnection {
                device: Arc::downgrade(&device),
                generation: 0,
            })),
            device: Some(device),
            info,
        })
    }

    pub fn observer(&self) -> DriverRingObserver {
        DriverRingObserver {
            connection: Arc::clone(&self.observer_connection),
        }
    }

    pub fn info(&self) -> &AbiInfo {
        &self.info
    }

    pub fn stats_sample(&self) -> Result<DriverStatsSample, CaptureError> {
        self.observer().stats_sample()
    }

    pub fn stats(&self) -> Result<DriverStats, CaptureError> {
        self.stats_sample().map(|sample| sample.stats)
    }
}

#[cfg(windows)]
#[derive(Clone, Debug)]
pub struct DriverRingObserver {
    connection: Arc<Mutex<ObserverConnection>>,
}

#[cfg(windows)]
impl DriverRingObserver {
    pub fn stats_sample(&self) -> Result<DriverStatsSample, CaptureError> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| CaptureError::Io("driver connection lock was poisoned".into()))?;
        let device = connection
            .device
            .upgrade()
            .ok_or(CaptureError::NotPresent)?;
        // Keep the connection lock through the ioctl. Reconnect clears the
        // owner's strong reference before taking this lock, so after it acquires
        // the lock every observer ioctl on the old handle has also finished.
        Ok(DriverStatsSample {
            generation: connection.generation,
            stats: read_stats(&device)?,
        })
    }

    pub fn stats(&self) -> Result<DriverStats, CaptureError> {
        self.stats_sample().map(|sample| sample.stats)
    }
}

#[cfg(windows)]
fn read_stats(device: &Device) -> Result<DriverStats, CaptureError> {
    let mut output = [0; size_of::<DriverStats>()];
    let returned = device
        .ioctl(IOCTL_SSR_GET_STATS, &[], &mut output)
        .map_err(capture_error)?;
    if returned != output.len() {
        return Err(CaptureError::Io(format!(
            "GET_STATS returned {returned} bytes, expected {}",
            output.len()
        )));
    }
    let stats = DriverStats::read_from_bytes(&output).expect("exact DriverStats size");
    if stats.struct_size != size_of::<DriverStats>() as u32 {
        return Err(CaptureError::Io(format!(
            "GET_STATS struct_size is {}, expected {}",
            stats.struct_size,
            size_of::<DriverStats>()
        )));
    }
    Ok(stats)
}

#[cfg(windows)]
impl CaptureSource for DriverRing {
    fn read(&mut self, buf: &mut [u8], wait: Duration) -> Result<usize, CaptureError> {
        let minimum_record = record_size(MAX_DATA_PAYLOAD as usize);
        if buf.len() < minimum_record {
            return Err(CaptureError::Io(format!(
                "record buffer is {} bytes; at least {minimum_record} bytes are required",
                buf.len()
            )));
        }
        // One queued byte is enough to complete, but the ring contains only
        // whole records. With no record queued, the driver's deadline waits.
        let request = ReadRequest {
            min_bytes: 1,
            timeout_ms: duration_millis(wait),
        };
        let device = self.device.as_ref().ok_or(CaptureError::NotPresent)?;
        device
            .ioctl(IOCTL_SSR_READ, request.as_bytes(), buf)
            .map_err(capture_error)
    }

    fn now_qpc(&self) -> Option<u64> {
        Some(crate::qpc::now())
    }

    fn set_rate_trim(&mut self, ppm: f64) -> Result<(), TrimUnsupported> {
        let request = RateTrim {
            ppm_q16: trim_ppm_q16(ppm),
            reserved: 0,
        };
        let Some(device) = self.device.as_ref() else {
            return Err(TrimUnsupported);
        };
        device
            .ioctl(IOCTL_SSR_SET_RATE_TRIM, request.as_bytes(), &mut [])
            .map(|_| ())
            .map_err(|_| TrimUnsupported)
    }

    fn reconnect(&mut self) -> Result<(), CaptureError> {
        self.device = None;
        {
            let mut connection = self
                .observer_connection
                .lock()
                .map_err(|_| CaptureError::Io("driver connection lock was poisoned".into()))?;
            connection.device = Weak::new();
        }
        let reopened = Self::open()?;
        let device = reopened.device.expect("a newly opened ring has a device");
        self.info = reopened.info;
        {
            let mut connection = self
                .observer_connection
                .lock()
                .map_err(|_| CaptureError::Io("driver connection lock was poisoned".into()))?;
            connection.generation = connection.generation.wrapping_add(1);
            connection.device = Arc::downgrade(&device);
        }
        self.device = Some(device);
        Ok(())
    }
}

#[cfg(windows)]
fn duration_millis(wait: Duration) -> u32 {
    wait.as_millis().min(u128::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_info_preserves_pcm_rules_and_rejects_old_layout() {
        let formats = ssr_abi::EndpointFormats {
            codecs: ssr_abi::DEFAULT_ENABLED_CODECS,
            pcm: ssr_abi::DEFAULT_PCM,
        };
        let mut info = AbiInfo {
            abi_version: ABI_VERSION,
            struct_size: size_of::<AbiInfo>() as u32,
            driver_version: 1,
            ring_bytes: 8 * 1024 * 1024,
            compiled: ssr_abi::EndpointFormats {
                codecs: ssr_abi::COMPILED_CODECS,
                pcm: ssr_abi::COMPILED_PCM,
            },
            features: 3,
            qpc_frequency: 10_000_000,
            endpoint_mask: ssr_abi::DEFAULT_ENDPOINT_MASK,
            reserved: 0,
            endpoints: [
                formats,
                formats,
                formats,
                ssr_abi::EndpointFormats::default(),
            ],
        };
        assert_eq!(parse_query_info(info.as_bytes()).unwrap(), info);
        assert!(parse_query_info(&info.as_bytes()[..80]).is_err());
        assert!(parse_query_info(&info.as_bytes()[..159]).is_err());
        info.struct_size = 80;
        assert!(parse_query_info(info.as_bytes()).is_err());
        info.struct_size = 160;
        info.abi_version += 1;
        assert!(parse_query_info(info.as_bytes()).is_err());
        info.abi_version = ABI_VERSION;
        info.reserved = u32::MAX;
        assert_eq!(parse_query_info(info.as_bytes()).unwrap(), info);
    }

    #[test]
    fn ppm_to_q16_rounds_clamps_and_rejects_non_finite_values() {
        assert_eq!(trim_ppm_q16(0.0), 0);
        assert_eq!(trim_ppm_q16(0.5 / Q16_SCALE), 1);
        assert_eq!(trim_ppm_q16(-0.5 / Q16_SCALE), -1);
        assert_eq!(trim_ppm_q16(100.25), 6_569_984);
        assert_eq!(trim_ppm_q16(3000.0), TRIM_LIMIT_PPM * 65_536);
        assert_eq!(trim_ppm_q16(-3000.0), -TRIM_LIMIT_PPM * 65_536);
        assert_eq!(trim_ppm_q16(f64::NAN), 0);
        assert_eq!(trim_ppm_q16(f64::INFINITY), 0);
        assert_eq!(trim_ppm_q16(f64::NEG_INFINITY), 0);
    }

    #[cfg(windows)]
    #[test]
    fn wait_is_saturated_to_the_driver_field() {
        assert_eq!(duration_millis(Duration::from_nanos(999_999)), 0);
        assert_eq!(duration_millis(Duration::from_millis(7)), 7);
        assert_eq!(duration_millis(Duration::MAX), u32::MAX);
    }
}
