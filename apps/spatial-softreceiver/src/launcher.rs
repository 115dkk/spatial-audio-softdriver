//! The supervisor's [`Launcher`] on Windows: the engine on our driver's ring,
//! rendering to the chosen output through WASAPI.
//!
//! The profile is `profile.toml` in the state directory when there is one.
//! Without it the engine runs a plain stereo speaker profile (FL at +30°, FR
//! at -30°), so until the BRIR import of the next slice the app pans every
//! source to two channels and makes sound from the first day; headphone
//! rendering arrives with that import (ADR 0001, 2026-10-15 addendum).

use std::path::PathBuf;
use std::sync::Arc;

use engine::{EngineConfig, OutputProfile};
use speakers::{SpeakerConfig, SpeakerProfile, SpeakerRange};
use ssr_sys::driver_ring::DriverRing;
use ssr_sys::mmcss;
use ssr_sys::output::WasapiOpener;
use ssr_types::OutputDevice;
use supervisor::{EngineHandle, Launcher, Session, SessionConfig, SessionError};

/// The profile's file name in the state directory.
pub const PROFILE: &str = "profile.toml";

/// Starts the engine for the supervisor on Windows.
#[derive(Clone, Debug)]
pub struct WindowsLauncher {
    /// `%LOCALAPPDATA%\Spatial SoftReceiver`, where `profile.toml` may be.
    pub state_dir: PathBuf,
}

impl WindowsLauncher {
    fn profile(&self) -> Result<OutputProfile, SessionError> {
        let path = self.state_dir.join(PROFILE);
        if path.is_file() {
            engine::load_output_profile(&path)
                .map_err(|error| SessionError::Profile(error.to_string()))
        } else {
            Ok(stereo_profile())
        }
    }
}

impl Launcher for WindowsLauncher {
    /// Opens the driver ring, loads the profile, and starts a session on
    /// `output`: 48 kHz exclusive, falling back to shared when the device
    /// refuses, each engine thread registered with MMCSS.
    fn launch(&mut self, output: &OutputDevice) -> Result<Box<dyn EngineHandle>, SessionError> {
        let ring = DriverRing::open()?;
        let profile = self.profile()?;
        let opener = WasapiOpener {
            endpoint: Some(output.id.0.clone()),
            rate: 48_000,
            exclusive: true,
            shared_fallback: true,
            period: None,
        };
        let engine = EngineConfig {
            thread_hook: Some(Arc::new(mmcss::promote_engine_thread)),
            ..EngineConfig::default()
        };
        let qpc_frequency = ring.info().qpc_frequency;
        let observer = ring.observer();
        let session = Session::start(
            SessionConfig {
                engine,
                profile,
                qpc_frequency,
                mute: false,
                recorder: None,
            },
            ring,
            opener,
            Some(Box::new(observer)),
        )?;
        Ok(Box::new(session))
    }
}

/// Two full-range speakers at ±30° on the device's first two channels.
fn stereo_profile() -> OutputProfile {
    let speaker = |name: &str, azimuth_deg: f32, channel: usize| SpeakerConfig {
        name: name.into(),
        azimuth_deg,
        elevation_deg: 0.0,
        channel,
        distance_m: None,
        delay_ms: None,
        trim_db: 0.0,
        range: SpeakerRange::Full,
        crossover_hz: None,
        fir: None,
    };
    OutputProfile::Speakers(SpeakerProfile {
        name: "Stereo".into(),
        device_channels: 2,
        channel_mask: 3,
        gain_db: 0.0,
        limiter_ceiling_dbfs: -1.0,
        crossover_hz: 80.0,
        lfe_gain_db: 10.0,
        speakers: vec![speaker("FL", 30.0, 0), speaker("FR", -30.0, 1)],
        subwoofers: Vec::new(),
        bass_to: Vec::new(),
    })
}
