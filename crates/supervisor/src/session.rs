use std::sync::{Arc, Mutex};
use std::time::Instant;

use engine::{EngineEvent, LiveChange, OutputProfile, StartOptions};
use ssr_types::{CaptureError, CaptureSource, EngineFault, EngineStatus, OutputOpener};

use crate::driver_stats::{
    DriverStatsSource, DriverStatsTotals, SharedStatsSource, SharedStatsTotals,
};
use crate::recorder::{Recorder, RecorderConfig, TeeCapture};
use crate::report::{RunFigures, SessionReport};

pub struct SessionConfig {
    pub engine: engine::EngineConfig,
    pub profile: OutputProfile,
    pub qpc_frequency: u64,
    pub mute: bool,
    pub recorder: Option<RecorderConfig>,
}

impl std::fmt::Debug for SessionConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionConfig")
            .field("engine", &self.engine)
            .field("profile", &self.profile)
            .field("qpc_frequency", &self.qpc_frequency)
            .field("mute", &self.mute)
            .field("recorder", &self.recorder)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Start(#[from] engine::StartError),
    #[error("output preparation failed: {0}")]
    Prepare(#[from] engine::PrepareError),
    #[error("the engine rejected a live change")]
    Rejected,
    #[error("diagnostic recorder failed: {0}")]
    Recorder(String),
    #[error("driver statistics failed: {0}")]
    DriverStats(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionNotice {
    FormatChanged {
        endpoint: u16,
        epoch: u32,
        format: ssr_types::SourceFormat,
        unsupported: Option<ssr_types::UnsupportedReason>,
    },
    NeedsPrepared,
    Prepared {
        kind: engine::OutputKind,
        channels: usize,
        millis: f64,
    },
    PrepareRetry,
    Overrun {
        lost_bytes: u64,
    },
    OutputOverflow {
        frames: u64,
    },
    OutputLayoutChanged {
        channels: u16,
        channel_mask: u32,
    },
    OutputReconnected,
    FaultCleared,
    OutputCadenceFault {
        ratio: f32,
    },
    OutputCadenceReopen {
        period_ms: f32,
    },
    Fault(EngineFault),
}

#[derive(Debug)]
pub enum SessionChange {
    Mute(bool),
    TargetMs(f32),
    Profile(OutputProfile),
}

pub struct Session {
    engine: engine::Engine,
    profile: OutputProfile,
    pending_prepared: bool,
    output_kind: engine::OutputKind,
    output_channels: usize,
    output_channel_mask: u32,
    started: Instant,
    figures: RunFigures,
    recorder: Option<Recorder>,
    driver_source: Option<SharedStatsSource>,
    driver_totals: Option<SharedStatsTotals>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("status", &self.status())
            .field("output_kind", &self.output_kind)
            .field("output_channels", &self.output_channels)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub fn start<C: CaptureSource, O: OutputOpener>(
        config: SessionConfig,
        capture: C,
        output: O,
        driver_stats: Option<Box<dyn DriverStatsSource>>,
    ) -> Result<Self, SessionError> {
        // The baseline is taken as soon as the source is open, before the
        // output is prepared: a driver ring that overflows while a large
        // profile prepares belongs to this run's losses.
        let driver_baseline = match driver_stats {
            Some(source) => {
                let source = Arc::new(Mutex::new(source));
                let (generation, stats) = sample(&source).map_err(SessionError::DriverStats)?;
                let totals = Arc::new(Mutex::new(DriverStatsTotals::new(generation, stats)));
                Some((source, totals))
            }
            None => None,
        };
        let prepared = config.profile.prepare()?;
        let output_kind = prepared.kind();
        let output_channels = prepared.channels();
        let output_channel_mask = prepared.channel_mask();
        let mut tee = TeeCapture::without_recorder(
            capture,
            config
                .recorder
                .as_ref()
                .map_or(default_now, |recorder| recorder.now),
        );

        let (driver_source, driver_totals) = if let Some((source, totals)) = driver_baseline {
            let before_source = Arc::clone(&source);
            let before_totals = Arc::clone(&totals);
            let after_source = Arc::clone(&source);
            let after_totals = Arc::clone(&totals);
            tee = tee.with_reconnect_observers(
                move || {
                    if let Ok((generation, stats)) = sample(&before_source) {
                        before_totals
                            .lock()
                            .map_err(|_| stats_lock_error())?
                            .observe(generation, stats);
                    }
                    Ok(())
                },
                move || {
                    let (generation, stats) = sample(&after_source).map_err(CaptureError::Io)?;
                    after_totals
                        .lock()
                        .map_err(|_| stats_lock_error())?
                        .begin_generation(generation, stats);
                    Ok(())
                },
            );
            (Some(source), Some(totals))
        } else {
            (None, None)
        };

        let (capture, recorder) = if let Some(recorder_config) = config.recorder {
            let (capture, recorder) = Recorder::start(recorder_config, config.qpc_frequency, tee)
                .map_err(SessionError::Recorder)?;
            (capture, Some(recorder))
        } else {
            (tee, None)
        };
        let engine = engine::start_with(
            config.engine,
            capture,
            output,
            StartOptions {
                qpc_frequency: config.qpc_frequency,
                prepared: Some(Box::new(prepared)),
            },
        )?;
        if config.mute {
            engine
                .apply(LiveChange::Mute(true))
                .map_err(|_| SessionError::Rejected)?;
        }
        Ok(Self {
            engine,
            profile: config.profile,
            pending_prepared: false,
            output_kind,
            output_channels,
            output_channel_mask,
            started: Instant::now(),
            figures: RunFigures::default(),
            recorder,
            driver_source,
            driver_totals,
        })
    }

    /// Drains engine events and re-prepares the output when the engine
    /// asked, retrying while its change queue is full. A profile that fails
    /// to prepare is an error: the output cannot be rebuilt, as at start.
    pub fn poll(&mut self) -> Result<Vec<SessionNotice>, SessionError> {
        let mut notices = Vec::new();
        for event in self.engine.poll() {
            self.handle_event(event, &mut notices);
        }
        self.retry_prepared(&mut notices)?;
        Ok(notices)
    }

    fn handle_event(&mut self, event: EngineEvent, notices: &mut Vec<SessionNotice>) {
        match event {
            EngineEvent::FormatChanged {
                endpoint,
                epoch,
                format,
                unsupported,
            } => notices.push(SessionNotice::FormatChanged {
                endpoint,
                epoch,
                format,
                unsupported,
            }),
            EngineEvent::NeedsPrepared => {
                self.pending_prepared = true;
                notices.push(SessionNotice::NeedsPrepared);
            }
            EngineEvent::Overrun { lost_bytes } => {
                self.figures.overruns += 1;
                notices.push(SessionNotice::Overrun { lost_bytes });
            }
            EngineEvent::OutputOverflow { frames } => {
                notices.push(SessionNotice::OutputOverflow { frames });
            }
            EngineEvent::OutputLayoutChanged {
                channels,
                channel_mask,
            } => notices.push(SessionNotice::OutputLayoutChanged {
                channels,
                channel_mask,
            }),
            EngineEvent::OutputReconnected => notices.push(SessionNotice::OutputReconnected),
            EngineEvent::FaultCleared => notices.push(SessionNotice::FaultCleared),
            EngineEvent::OutputCadenceFault { ratio } => {
                self.figures.faults.push("OutputCadence".to_owned());
                notices.push(SessionNotice::OutputCadenceFault { ratio });
            }
            EngineEvent::OutputCadenceReopen { period_ms } => {
                notices.push(SessionNotice::OutputCadenceReopen { period_ms });
            }
            EngineEvent::Fault(fault) => {
                self.figures.faults.push(format!("{fault:?}"));
                notices.push(SessionNotice::Fault(fault));
            }
        }
    }

    fn retry_prepared(&mut self, notices: &mut Vec<SessionNotice>) -> Result<(), SessionError> {
        if !self.pending_prepared {
            return Ok(());
        }
        let started = Instant::now();
        let prepared = self.profile.prepare()?;
        self.output_kind = prepared.kind();
        self.output_channels = prepared.channels();
        self.output_channel_mask = prepared.channel_mask();
        let notice = SessionNotice::Prepared {
            kind: prepared.kind(),
            channels: prepared.channels(),
            millis: started.elapsed().as_secs_f64() * 1_000.0,
        };
        match self.engine.apply(LiveChange::Prepared(Box::new(prepared))) {
            Ok(()) => {
                self.pending_prepared = false;
                notices.push(notice);
            }
            Err(engine::Rejected) => notices.push(SessionNotice::PrepareRetry),
        }
        Ok(())
    }

    pub fn status(&self) -> EngineStatus {
        self.engine.status()
    }

    pub fn snapshot(&self) -> ssr_types::app::Snapshot {
        let status = self.status();
        let phase = status
            .fault
            .as_ref()
            .map_or(ssr_types::app::Phase::Running, |fault| {
                ssr_types::app::Phase::Faulted(format!("{fault:?}"))
            });
        ssr_types::app::Snapshot {
            phase,
            engine: Some(status),
        }
    }

    pub fn apply(&mut self, change: SessionChange) -> Result<(), SessionError> {
        match change {
            SessionChange::Mute(mute) => self
                .engine
                .apply(LiveChange::Mute(mute))
                .map_err(|_| SessionError::Rejected),
            SessionChange::TargetMs(ms) => self
                .engine
                .apply(LiveChange::TargetMs(ms))
                .map_err(|_| SessionError::Rejected),
            SessionChange::Profile(profile) => {
                self.profile = profile;
                self.pending_prepared = true;
                Ok(())
            }
        }
    }

    pub fn observe(&mut self, second: u64) {
        self.figures.observe(&self.status(), second);
    }

    pub fn source_ended(&mut self) {
        if self.figures.underruns_while_streaming.is_none() {
            self.figures.underruns_while_streaming = Some(self.status().underruns);
        }
    }

    pub fn recorder_dropped_bytes(&self) -> u64 {
        self.recorder.as_ref().map_or(0, Recorder::dropped_bytes)
    }

    /// Stops in the order the report relies on: the last driver-statistics
    /// sample while the engine still reads (once it stops, the driver's ring
    /// fills and counts overruns that are not the run's), the underruns so far
    /// when the source has not ended, the engine, and then the recorder's
    /// writer, after the last read it can be handed.
    pub fn stop(mut self) -> Result<SessionReport, SessionError> {
        let seconds = self.started.elapsed().as_secs_f64();
        let (driver, driver_stats_error) = match (&self.driver_source, &self.driver_totals) {
            (Some(source), Some(totals)) => {
                let error = match sample(source) {
                    Ok((generation, stats)) => {
                        totals
                            .lock()
                            .map_err(|_| {
                                SessionError::DriverStats(
                                    "driver statistics lock was poisoned".into(),
                                )
                            })?
                            .observe(generation, stats);
                        None
                    }
                    Err(error) => Some(error),
                };
                let totals = *totals.lock().map_err(|_| {
                    SessionError::DriverStats("driver statistics lock was poisoned".into())
                })?;
                (Some(totals), error)
            }
            _ => (None, None),
        };
        if self.figures.underruns_while_streaming.is_none() {
            self.figures.underruns_while_streaming = Some(self.engine.status().underruns);
        }
        let engine = self.engine.stop();
        let dropped = self.recorder.as_ref().map_or(0, Recorder::dropped_bytes);
        if let Some(recorder) = self.recorder {
            recorder.stop().map_err(SessionError::Recorder)?;
        }
        Ok(SessionReport {
            seconds,
            engine,
            figures: self.figures,
            output_kind: self.output_kind,
            output_channels: self.output_channels,
            output_channel_mask: self.output_channel_mask,
            recorder_lost_bytes: dropped,
            driver,
            driver_stats_error,
        })
    }
}

fn sample(source: &SharedStatsSource) -> Result<(u64, ssr_abi::DriverStats), String> {
    source
        .lock()
        .map_err(|_| "driver statistics source lock was poisoned".to_owned())?
        .sample()
}

fn stats_lock_error() -> CaptureError {
    CaptureError::Io("driver statistics lock was poisoned".into())
}

fn default_now() -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, SyncSender};
    use std::time::Duration;

    use speakers::{SpeakerConfig, SpeakerProfile, SpeakerRange};
    use ssr_types::{CaptureError, CaptureSource, TrimUnsupported};

    use super::*;

    struct BlockingCapture {
        entered: SyncSender<()>,
        release: Receiver<()>,
    }

    impl CaptureSource for BlockingCapture {
        fn read(&mut self, _buf: &mut [u8], _wait: Duration) -> Result<usize, CaptureError> {
            self.entered.send(()).map_err(|_| CaptureError::Ended)?;
            self.release.recv().map_err(|_| CaptureError::Ended)?;
            Err(CaptureError::Ended)
        }

        fn set_rate_trim(&mut self, _ppm: f64) -> Result<(), TrimUnsupported> {
            Err(TrimUnsupported)
        }
    }

    fn profile() -> OutputProfile {
        OutputProfile::Speakers(SpeakerProfile {
            name: "test".into(),
            device_channels: 2,
            channel_mask: 3,
            gain_db: 0.0,
            limiter_ceiling_dbfs: 0.0,
            crossover_hz: 80.0,
            lfe_gain_db: 10.0,
            speakers: vec![
                SpeakerConfig {
                    name: "FL".into(),
                    azimuth_deg: 30.0,
                    elevation_deg: 0.0,
                    channel: 0,
                    distance_m: None,
                    delay_ms: None,
                    trim_db: 0.0,
                    range: SpeakerRange::Full,
                    crossover_hz: None,
                    fir: None,
                },
                SpeakerConfig {
                    name: "FR".into(),
                    azimuth_deg: -30.0,
                    elevation_deg: 0.0,
                    channel: 1,
                    distance_m: None,
                    delay_ms: None,
                    trim_db: 0.0,
                    range: SpeakerRange::Full,
                    crossover_hz: None,
                    fir: None,
                },
            ],
            subwoofers: Vec::new(),
            bass_to: Vec::new(),
        })
    }

    #[test]
    fn needs_prepared_is_reprepared() {
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let capture = BlockingCapture {
            entered: entered_tx,
            release: release_rx,
        };
        let mut session = Session::start(
            SessionConfig {
                engine: engine::EngineConfig {
                    read_wait: Duration::ZERO,
                    ..Default::default()
                },
                profile: profile(),
                qpc_frequency: 1_000_000,
                mute: false,
                recorder: None,
            },
            capture,
            engine::adapters::NullOpener {
                rate: 48_000,
                period_frames: 480,
            },
            None,
        )
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let mut notices = Vec::new();
        session.handle_event(EngineEvent::NeedsPrepared, &mut notices);
        session.retry_prepared(&mut notices).unwrap();
        assert!(matches!(notices[0], SessionNotice::NeedsPrepared));
        assert!(matches!(notices[1], SessionNotice::Prepared { .. }));
        assert!(!session.pending_prepared);

        release_tx.send(()).unwrap();
        session.stop().unwrap();
    }

    #[test]
    fn prepared_change_retries_after_a_full_queue() {
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let capture = BlockingCapture {
            entered: entered_tx,
            release: release_rx,
        };
        let mut session = Session::start(
            SessionConfig {
                engine: engine::EngineConfig {
                    read_wait: Duration::ZERO,
                    ..Default::default()
                },
                profile: profile(),
                qpc_frequency: 1_000_000,
                mute: false,
                recorder: None,
            },
            capture,
            engine::adapters::NullOpener {
                rate: 48_000,
                period_frames: 480,
            },
            None,
        )
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        for _ in 0..16 {
            session
                .engine
                .apply(LiveChange::Mute(false))
                .expect("the change queue has sixteen slots");
        }
        session.pending_prepared = true;
        let mut notices = Vec::new();
        session.retry_prepared(&mut notices).unwrap();
        assert_eq!(notices, vec![SessionNotice::PrepareRetry]);
        assert!(session.pending_prepared);

        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            notices.clear();
            session.retry_prepared(&mut notices).unwrap();
            if notices
                .iter()
                .any(|notice| matches!(notice, SessionNotice::Prepared { .. }))
            {
                break;
            }
            assert!(Instant::now() < deadline, "the full queue was not drained");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!session.pending_prepared);
        session.stop().unwrap();
    }
}
