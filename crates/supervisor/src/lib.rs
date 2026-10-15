#![forbid(unsafe_code)]
#![recursion_limit = "256"]

//! State machine and authority: setup steps, profiles, device selectors, engine lifetime (M4).

pub mod driver_stats;
pub mod recorder;
pub mod report;
pub mod session;

pub use driver_stats::{DriverStatsSource, DriverStatsTotals};
pub use recorder::{Recorder, RecorderConfig, TeeCapture};
pub use report::{DirectLayoutField, RequestFields, RunFigures, SessionReport, chain_errors_json};
pub use session::{Session, SessionChange, SessionConfig, SessionError, SessionNotice};
