#![deny(unsafe_code)]

//! Platform isolation for the out-of-process plugin host.

pub mod state_buffer;
#[cfg(windows)]
pub mod vst3;
#[cfg(windows)]
mod vst3_support;

#[cfg(windows)]
pub mod events;
#[cfg(windows)]
pub mod mapping;
#[cfg(windows)]
pub mod vst2;
#[cfg(windows)]
pub use events::{Event, SharedEvent};
#[cfg(windows)]
pub use mapping::SharedRegion;
#[cfg(windows)]
pub mod editor;
#[cfg(windows)]
pub mod library;
#[cfg(windows)]
pub mod plugin;
#[cfg(windows)]
pub mod runtime;
