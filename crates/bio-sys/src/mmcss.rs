#![allow(unsafe_code)]

//! Thread-local MMCSS registration. Keep the guard on the promoted thread.

use std::{io, marker::PhantomData, rc::Rc};
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
