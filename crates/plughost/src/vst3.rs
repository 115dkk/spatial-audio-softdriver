//! Safe VST 3 adapter; all foreign calls live in plughost-sys.
use crate::{Bus, Plugin, PluginError, PluginEvents, PluginInfo, Processor, state};
use plughost_sys::vst3::{Vst3Plugin, Vst3Processor};
use std::path::Path;
fn error(error: impl std::fmt::Display) -> PluginError {
    PluginError(error.to_string())
}
#[derive(Debug)]
pub(crate) struct Vst3 {
    inner: Vst3Plugin,
}
impl Vst3 {
    pub fn load(path: &Path, class: Option<[u8; 16]>) -> Result<Self, PluginError> {
        Ok(Self {
            inner: Vst3Plugin::load(path, class).map_err(error)?,
        })
    }
    pub fn take_processor(&mut self) -> Result<Box<dyn Processor>, PluginError> {
        Ok(Box::new(Audio(self.inner.take_processor().map_err(error)?)))
    }
}
#[derive(Debug)]
struct Audio(Vst3Processor);
impl Processor for Audio {
    fn process(&mut self, planes: &mut [&mut [f32]], frames: usize) -> bool {
        self.0.process(planes, frames)
    }
    fn stop(&mut self) {
        self.0.stop();
    }
}
impl Plugin for Vst3 {
    fn info(&self) -> PluginInfo {
        let info = self.inner.info();
        PluginInfo {
            name: info.class.name,
            vendor: info.class.vendor,
            latency_frames: info.latency,
            inputs: vec![Bus {
                channels: info.inputs,
                arrangement: info.input,
            }],
            outputs: vec![Bus {
                channels: info.outputs,
                arrangement: info.output,
            }],
        }
    }
    fn configure(
        &mut self,
        rate: u32,
        frames: u32,
        inputs: u16,
        outputs: u16,
    ) -> Result<(), PluginError> {
        self.inner
            .configure(rate, frames, inputs, outputs)
            .map_err(error)
    }
    fn activate(&mut self) -> Result<(), PluginError> {
        self.inner.activate().map_err(error)
    }
    fn deactivate(&mut self) {
        self.inner.deactivate();
    }
    fn take_processor(&mut self) -> Result<Box<dyn Processor>, PluginError> {
        Vst3::take_processor(self)
    }
    fn get_state(&mut self) -> Result<Vec<u8>, PluginError> {
        let (component, controller) = self.inner.get_state().map_err(error)?;
        state::encode(&component, &controller)
    }
    fn set_state(&mut self, blob: &[u8]) -> Result<(), PluginError> {
        let (component, controller) = state::decode(blob)?;
        self.inner.set_state(component, controller).map_err(error)
    }
    fn open_editor(&mut self) -> Result<(), PluginError> {
        self.inner.open_editor().map_err(error)
    }
    fn idle(&mut self) {
        self.inner.idle();
    }
    fn take_events(&mut self) -> PluginEvents {
        let events = self.inner.take_events();
        PluginEvents {
            params_changed: events.params,
            latency_changed: events.latency,
            editor_closed: events.closed,
            fault: events.fault,
        }
    }
}
