#![allow(unsafe_code)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::Arc;
use std::time::Duration;
use streamring::{Signal, Wait, Waited};
use windows::Win32::{
    Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Security::SECURITY_ATTRIBUTES,
    System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects},
};

#[derive(Debug)]
pub struct Event {
    handle: OwnedHandle,
    peer: Option<OwnedHandle>,
}
impl Event {
    pub fn create() -> windows::core::Result<Self> {
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            bInheritHandle: true.into(),
            ..Default::default()
        };
        // SAFETY: valid attributes; unnamed, auto-reset, initially unsignalled event.
        let handle = unsafe { CreateEventW(Some(&attributes), false, false, None) }?;
        // SAFETY: successful creation transfers a unique kernel handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        Ok(Self { handle, peer: None })
    }
    pub fn open(value: usize) -> windows::core::Result<Self> {
        Ok(Self {
            handle: crate::runtime::duplicate_value(value, false)?,
            peer: None,
        })
    }
    pub fn with_peer(mut self, peer: OwnedHandle) -> Self {
        self.peer = Some(peer);
        self
    }
    pub fn handle_value(&self) -> usize {
        self.handle.as_raw_handle() as usize
    }
}
impl Signal for Event {
    fn signal(&self) {
        // SAFETY: the owned event handle stays live throughout this nonblocking call.
        let _ = unsafe { SetEvent(HANDLE(self.handle.as_raw_handle())) };
    }
}
impl Wait for Event {
    fn wait(&self, timeout: Duration) -> Waited {
        let event = HANDLE(self.handle.as_raw_handle());
        let handles = [
            event,
            self.peer
                .as_ref()
                .map_or(event, |peer| HANDLE(peer.as_raw_handle())),
        ];
        let count = if self.peer.is_some() { 2 } else { 1 };
        let milliseconds = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
        // SAFETY: handles are owned for the entire wait; slice has one or two entries.
        let result = unsafe { WaitForMultipleObjects(&handles[..count], false, milliseconds) };
        if result == WAIT_OBJECT_0 {
            Waited::Signalled
        } else if result == WAIT_TIMEOUT {
            Waited::TimedOut
        } else {
            Waited::PeerGone
        }
    }
}

/// A shared [`Event`] that is itself a [`Signal`] and a [`Wait`]. `Arc<Event>`
/// cannot be one here (`Arc` is not a local type), so an owner that needs
/// `Box<dyn Signal>`, such as the engine's `RingUpmixer`, holds this instead.
#[derive(Clone, Debug)]
pub struct SharedEvent(pub Arc<Event>);
impl Signal for SharedEvent {
    fn signal(&self) {
        self.0.signal();
    }
}
impl Wait for SharedEvent {
    fn wait(&self, timeout: Duration) -> Waited {
        self.0.wait(timeout)
    }
}
