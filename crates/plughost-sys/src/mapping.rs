#![allow(unsafe_code)]
//! Page-aligned mappings for x86-64/ARM64. A mapping's atomic split is fixed on
//! first use; no byte is ever accessed at both widths. Only atomic references
//! are exposed, borrowed from the mapping, and Drop runs after their lifetimes.
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::{
    OnceLock,
    atomic::{AtomicU32, AtomicU64, Ordering},
};
use streamring::{Geometry, Region};
use windows::Win32::{
    Foundation::{HANDLE, INVALID_HANDLE_VALUE},
    Security::SECURITY_ATTRIBUTES,
    System::Memory::*,
};

// A process is either the app (creates mappings) or one disposable host (opens
// one mapping). This also prevents safe callers from reopening their own view
// with a different atomic split through its exported handle value.
static MAPPING_ROLE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

#[derive(Debug)]
pub struct SharedRegion {
    handle: OwnedHandle,
    address: usize,
    bytes: usize,
    split: OnceLock<usize>,
}
impl SharedRegion {
    pub fn create(bytes: usize) -> windows::core::Result<Self> {
        if MAPPING_ROLE
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |role| {
                (role != 2).then_some(1)
            })
            .is_err()
        {
            return Err(windows::core::Error::from_hresult(
                windows::Win32::Foundation::E_INVALIDARG,
            ));
        }
        if !(640..streamring::MAX_REGION_BYTES).contains(&bytes) {
            return Err(windows::core::Error::from_hresult(
                windows::Win32::Foundation::E_INVALIDARG,
            ));
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            bInheritHandle: true.into(),
            ..Default::default()
        };
        // SAFETY: unnamed paging-file mapping, valid attributes and bounded nonzero size.
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                Some(&attributes),
                PAGE_READWRITE,
                0,
                bytes as u32,
                None,
            )
        }?;
        // SAFETY: successful creation transfers a unique live kernel handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        Self::map(handle, bytes)
    }
    /// Opens the inherited host mapping once per process. The app creates mappings
    /// with `create`; only the disposable host calls this entry point.
    pub fn open(value: usize, bytes: usize) -> windows::core::Result<Self> {
        if MAPPING_ROLE
            .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(windows::core::Error::from_hresult(
                windows::Win32::Foundation::E_INVALIDARG,
            ));
        }
        Self::map(crate::runtime::duplicate_value(value, false)?, bytes)
    }
    fn map(handle: OwnedHandle, bytes: usize) -> windows::core::Result<Self> {
        if !(640..streamring::MAX_REGION_BYTES).contains(&bytes) {
            return Err(windows::core::Error::from_hresult(
                windows::Win32::Foundation::E_INVALIDARG,
            ));
        }
        // SAFETY: live mapping handle; Windows validates that the requested view fits.
        let view = unsafe {
            MapViewOfFile(
                HANDLE(handle.as_raw_handle()),
                FILE_MAP_ALL_ACCESS,
                0,
                0,
                bytes,
            )
        };
        if view.Value.is_null() {
            return Err(windows::core::Error::from_thread());
        }
        Ok(Self {
            handle,
            address: view.Value as usize,
            bytes,
            split: OnceLock::new(),
        })
    }
    pub fn handle_value(&self) -> usize {
        self.handle.as_raw_handle() as usize
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn latency_bound(&self) -> u32 {
        // SAFETY: word 8 is in the fixed 512-byte header and only accessed as AtomicU32.
        let word = unsafe { &*((self.address + 32) as *const AtomicU32) };
        u32::from_le(word.load(Ordering::Acquire))
    }
    pub fn region_for(&self, geometry: Geometry) -> Result<Region<'_>, streamring::GeometryError> {
        let total = geometry.total_bytes()?;
        if total > self.bytes {
            return Err(streamring::GeometryError::RegionTooLarge);
        }
        let prefix = total - 128;
        if *self.split.get_or_init(|| prefix) != prefix {
            return Err(streamring::GeometryError::RegionTooLarge);
        }
        Ok(self.region())
    }
    /// The split must first be fixed with `region_for`.
    pub fn region(&self) -> Region<'_> {
        let prefix = *self
            .split
            .get()
            .expect("region_for must fix the geometry first");
        // SAFETY: page-aligned live mapping, fixed split, bounded prefix, atomic-only access.
        let words32 =
            unsafe { std::slice::from_raw_parts(self.address as *const AtomicU32, prefix / 4) };
        // SAFETY: disjoint 128-byte suffix at a 64-byte boundary, bounded by mapping.
        let words64 =
            unsafe { std::slice::from_raw_parts((self.address + prefix) as *const AtomicU64, 16) };
        Region::new(words32, words64)
    }
}
/// The owning side of a [`streamring::Publisher`]: an `Arc<SharedRegion>`
/// keeps the mapping alive for the engine's `RingUpmixer` after the
/// [`streamring::Producer`] that announced the header is gone. The split must
/// already be fixed with [`SharedRegion::region_for`].
impl streamring::RegionMemory for SharedRegion {
    fn region(&self) -> Region<'_> {
        SharedRegion::region(self)
    }
}
impl Drop for SharedRegion {
    fn drop(&mut self) {
        // SAFETY: address is our live MapViewOfFile view and no borrowed slices remain.
        let _ = unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.address as *mut _,
            })
        };
    }
}
