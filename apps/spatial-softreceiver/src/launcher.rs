//! The supervisor's [`Launcher`] on Windows: the engine on our driver's ring,
//! rendering to the chosen output through WASAPI.
//!
//! The launcher reads no profile. The supervisor resolves it (the state
//! directory's `profile.toml`, else the imported BRIR set, else stereo
//! speakers) and hands it over with the output (ADR 0001, 2026-10-15
//! addendum).

use std::sync::Arc;

use engine::{EngineConfig, OutputProfile};
use ssr_sys::driver_ring::DriverRing;
use ssr_sys::mmcss;
use ssr_sys::output::WasapiOpener;
use ssr_types::OutputDevice;
use supervisor::{EngineHandle, Launcher, Session, SessionConfig, SessionError};

/// Starts the engine for the supervisor on Windows.
#[derive(Clone, Debug, Default)]
pub struct WindowsLauncher;

impl Launcher for WindowsLauncher {
    /// Opens the driver ring and starts a session with a copy of `profile`
    /// on `output`: 48 kHz exclusive, falling back to shared when the device
    /// refuses, each engine thread registered with MMCSS. The supervisor
    /// keeps the profile and answers the engine's re-prepares from it.
    fn launch(
        &mut self,
        output: &OutputDevice,
        profile: &OutputProfile,
    ) -> Result<Box<dyn EngineHandle>, SessionError> {
        let ring = DriverRing::open()?;
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
                profile: match profile {
                    OutputProfile::Headphones(profile) => {
                        OutputProfile::Headphones(profile.clone())
                    }
                    OutputProfile::Speakers(profile) => OutputProfile::Speakers(profile.clone()),
                },
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
