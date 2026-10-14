#![allow(unsafe_code)]

//! Keeps the process's multithreaded apartment until the process exits.
//!
//! Every WASAPI user here (the output thread, endpoint enumeration) joins the
//! MTA through `ComGuard` for its own lifetime. Without this pin the output
//! thread's CoUninitialize is the last one when a run stops, so COM tears the
//! apartment down and unloads the DLLs it loaded into it while the audio stack
//! may still have work queued for the stream. ssr-live died with 0xC0000005
//! while stopping exclusive streams under CPU load, about one stop in two on
//! the CI's VB-CABLE runners (ADR 0026); `tools/ci/restart-stress.ps1` keeps
//! checking that path.

use std::sync::Once;
use windows::Win32::System::Com::CoIncrementMTAUsage;

pub(crate) fn pin() {
    static PIN: Once = Once::new();
    PIN.call_once(|| {
        // SAFETY: takes no arguments. The cookie is deliberately never passed
        // to CoDecrementMTAUsage: the apartment lives as long as the process.
        let _ = unsafe { CoIncrementMTAUsage() };
    });
}
