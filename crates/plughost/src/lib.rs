#![forbid(unsafe_code)]

//! Safe orchestration for an out-of-process audio plugin.

pub mod host;
pub mod layout;
pub mod state;
#[cfg(windows)]
mod vst3;
pub mod watch;
#[cfg(windows)]
mod windows;
pub use layout::wave_bits;
pub use plughost_proto::{Bus, ClassInfo, PluginFormat};
#[cfg(windows)]
pub use plughost_sys::{Event, SharedEvent, SharedRegion};
#[cfg(windows)]
pub use windows::{ControlEvent, PluginControl, PluginProcess, StartError, Started};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginInfo {
    pub name: String,
    pub vendor: String,
    pub latency_frames: u32,
    pub inputs: Vec<Bus>,
    pub outputs: Vec<Bus>,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{0}")]
pub struct PluginError(pub String);

#[derive(Clone, Copy, Debug, Default)]
pub struct PluginEvents {
    pub params_changed: bool,
    pub latency_changed: Option<u32>,
    pub editor_closed: bool,
    pub fault: Option<&'static str>,
}

/// Main-thread lifecycle and state interface. The Windows adapter creates an
/// independent processor handle for the audio thread. A non-waiting admission
/// gate excludes foreign processing during control calls; only the main thread
/// waits for an in-flight block. Closed admission leaves blocks uncompleted so
/// the engine uses fallback. All editor and state calls stay on the main thread.
pub trait Plugin {
    fn info(&self) -> PluginInfo;
    fn configure(
        &mut self,
        rate: u32,
        max_frames: u32,
        in_channels: u16,
        out_channels: u16,
    ) -> Result<(), PluginError>;
    fn activate(&mut self) -> Result<(), PluginError>;
    fn deactivate(&mut self);
    /// Transfer the unique audio handle after configuration.
    fn take_processor(&mut self) -> Result<Box<dyn Processor>, PluginError>;
    fn get_state(&mut self) -> Result<Vec<u8>, PluginError>;
    fn set_state(&mut self, blob: &[u8]) -> Result<(), PluginError>;
    fn open_editor(&mut self) -> Result<(), PluginError>;
    fn idle(&mut self);
    fn take_events(&mut self) -> PluginEvents;
}

pub trait Processor: Send {
    /// In-place processing. Return false without changing samples when admission
    /// is closed; the ring must leave this sequence pending and retry on a wake.
    fn process(&mut self, planes: &mut [&mut [f32]], frames: usize) -> bool;

    /// Runs on the audio thread after its last block, before main-thread teardown.
    fn stop(&mut self) {}
}
