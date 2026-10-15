#![allow(unsafe_code)]

//! Thread-local MMCSS registration. Keep the guard on the promoted thread.

use std::{cell::RefCell, io, marker::PhantomData, rc::Rc};

use ssr_types::EngineThread;
use windows::{
    Win32::{
        Foundation::HANDLE,
        System::Threading::{AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW},
    },
    core::PCWSTR,
};

#[derive(Debug)]
pub struct MmcssGuard {
    handle: HANDLE,
    _thread: PhantomData<Rc<()>>,
}

pub fn promote(task: &str) -> Result<MmcssGuard, io::Error> {
    if task.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "MMCSS task contains a NUL",
        ));
    }
    let task: Vec<u16> = task.encode_utf16().chain(Some(0)).collect();
    let mut index = 0;
    // SAFETY: task is NUL-terminated and index is writable for the synchronous call.
    let handle = unsafe { AvSetMmThreadCharacteristicsW(PCWSTR(task.as_ptr()), &mut index) }
        .map_err(io::Error::from)?;
    Ok(MmcssGuard {
        handle,
        _thread: PhantomData,
    })
}

impl Drop for MmcssGuard {
    fn drop(&mut self) {
        // SAFETY: this guard owns the registration and cannot move to another thread.
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.handle) };
    }
}

thread_local! {
    // Keeps each engine thread registered with MMCSS until it exits.
    static ENGINE_THREAD: RefCell<Option<MmcssGuard>> = const { RefCell::new(None) };
}

/// The engine's thread hook (`engine::EngineConfig::thread_hook`), shared by
/// `ssr-live` and the app: the real-time threads take "Pro Audio", the decode
/// thread, which allocates and may wait, takes "Audio", one class below. The
/// registration lasts until the thread exits. A refused registration leaves
/// the thread at its normal priority and is not reported.
pub fn promote_engine_thread(thread: EngineThread) {
    let task = match thread {
        EngineThread::Capture | EngineThread::Output => "Pro Audio",
        EngineThread::Decode => "Audio",
    };
    if let Ok(guard) = promote(task) {
        ENGINE_THREAD.with(|slot| *slot.borrow_mut() = Some(guard));
    }
}
