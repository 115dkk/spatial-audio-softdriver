#![forbid(unsafe_code)]
#![recursion_limit = "256"]

//! State machine and authority: setup steps, profiles, device selectors, engine lifetime (M4).
//!
//! * [`machine`]: the [`Supervisor`] state machine behind the screen (ADR 0013).
//! * [`restore_point`]: what `ssr-guard` reads, and the one rule that gives the
//!   default roles back (ADR 0023).
//! * [`guard`]: the decision core of `ssr-guard`.
//! * [`session`]: one engine run, the [`EngineHandle`] the machine drives.
//! * [`upmix`]: each endpoint's upmixer, the [`PluginHost`] port that runs
//!   the VST upmixers' hosts, and their settings (ADR 0035).

pub mod driver_stats;
pub mod guard;
pub mod machine;
pub mod recorder;
pub mod report;
pub mod restore_point;
pub mod session;
pub mod text;
pub mod upmix;

pub use driver_stats::{DriverStatsSource, DriverStatsTotals};
pub use machine::{
    EngineHandle, Launcher, ProfileSettings, Settings, Supervisor, default_profile,
    default_profile_view,
};
pub use recorder::{Recorder, RecorderConfig, TeeCapture};
pub use report::{DirectLayoutField, RequestFields, RunFigures, SessionReport, chain_errors_json};
pub use session::{Session, SessionConfig, SessionError, SessionNotice};
pub use upmix::{
    DEFAULT_UPMIXER, EndpointUpmixSettings, PluginClass, PluginEvent, PluginHost, PluginHostError,
    PluginInstance, PluginSetting, TickChanges, UpmixSettings, default_upmix_view,
};
