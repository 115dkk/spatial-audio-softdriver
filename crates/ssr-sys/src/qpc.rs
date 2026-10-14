#![allow(unsafe_code)]

//! Windows performance-counter ticks, matching capture records.

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

pub fn now() -> u64 {
    let mut value = 0;
    // SAFETY: value is a live, aligned output for one i64.
    unsafe { QueryPerformanceCounter(&mut value) }.expect("QPC is available on Windows 11");
    value as u64
}

pub fn frequency() -> u64 {
    let mut value = 0;
    // SAFETY: value is a live, aligned output for one i64.
    unsafe { QueryPerformanceFrequency(&mut value) }.expect("QPC is available on Windows 11");
    value as u64
}
