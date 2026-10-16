#![allow(unsafe_code, non_snake_case)]
//! Host-owned COM objects. Controller callbacks are UI-thread-only (SDK threading model).
use crate::{editor::EditorWindow, state_buffer::StateBuffer};
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    io::{Read, Seek, SeekFrom, Write},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
    },
};
use vst3::{Class, ComPtr, ComRef, ComWrapper, Steinberg::Vst::*, Steinberg::*};

pub(crate) const CAPACITY: usize = 256;
pub(crate) struct Edits {
    slots: [(AtomicU32, AtomicU64); CAPACITY],
    read: AtomicUsize,
    write: AtomicUsize,
    pub flags: AtomicU32,
}
impl Edits {
    pub fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| (AtomicU32::new(0), AtomicU64::new(0))),
            read: AtomicUsize::new(0),
            write: AtomicUsize::new(0),
            flags: AtomicU32::new(0),
        }
    }
    fn push(&self, id: u32, value: f64) -> bool {
        let write = self.write.load(Ordering::Relaxed);
        if write.wrapping_sub(self.read.load(Ordering::Acquire)) == CAPACITY {
            return false;
        }
        let slot = &self.slots[write % CAPACITY];
        slot.0.store(id, Ordering::Relaxed);
        slot.1.store(value.to_bits(), Ordering::Relaxed);
        self.write.store(write.wrapping_add(1), Ordering::Release);
        true
    }
    pub fn pop(&self) -> Option<(u32, f64)> {
        let read = self.read.load(Ordering::Relaxed);
        if read == self.write.load(Ordering::Acquire) {
            return None;
        }
        let slot = &self.slots[read % CAPACITY];
        let edit = (
            slot.0.load(Ordering::Relaxed),
            f64::from_bits(slot.1.load(Ordering::Relaxed)),
        );
        self.read.store(read.wrapping_add(1), Ordering::Release);
        Some(edit)
    }
}
pub(crate) struct Host;
impl Class for Host {
    type Interfaces = (IHostApplication,);
}
impl IHostApplicationTrait for Host {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        if name.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: SDK supplies one writable String128 for this synchronous call.
        let name = unsafe { &mut *name };
        name.fill(0);
        for (dst, src) in name
            .iter_mut()
            .zip("Spatial SoftReceiver plug-in host".encode_utf16())
        {
            *dst = src;
        }
        kResultOk
    }
    unsafe fn createInstance(
        &self,
        _: *mut TUID,
        _: *mut TUID,
        object: *mut *mut c_void,
    ) -> tresult {
        if !object.is_null() {
            // SAFETY: optional SDK output pointer; no object is created.
            unsafe { *object = std::ptr::null_mut() };
        }
        kNotImplemented
    }
}
pub(crate) struct Handler(pub Arc<Edits>);
impl Class for Handler {
    type Interfaces = (IComponentHandler,);
}
impl IComponentHandlerTrait for Handler {
    unsafe fn beginEdit(&self, _: u32) -> tresult {
        self.0.flags.fetch_or(1, Ordering::Release);
        kResultOk
    }
    unsafe fn performEdit(&self, id: u32, value: f64) -> tresult {
        self.0.flags.fetch_or(1, Ordering::Release);
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return kInvalidArgument;
        }
        if self.0.push(id, value) {
            kResultOk
        } else {
            // Never silently drop automation: report a fault if the bounded queue is exhausted.
            self.0.flags.fetch_or(8, Ordering::Release);
            kResultFalse
        }
    }
    unsafe fn endEdit(&self, _: u32) -> tresult {
        self.0.flags.fetch_or(1, Ordering::Release);
        kResultOk
    }
    unsafe fn restartComponent(&self, flags: i32) -> tresult {
        if flags & RestartFlags_::kLatencyChanged != 0 {
            self.0.flags.fetch_or(2, Ordering::Release);
        }
        if flags & RestartFlags_::kIoChanged != 0 {
            self.0.flags.fetch_or(4, Ordering::Release);
        }
        kResultOk
    }
}

pub(crate) struct Stream(pub RefCell<StateBuffer>);
impl Stream {
    pub fn new(bytes: Vec<u8>) -> ComWrapper<Self> {
        ComWrapper::new(Self(RefCell::new(StateBuffer::new(bytes))))
    }
}
impl Class for Stream {
    type Interfaces = (IBStream,);
}
impl IBStreamTrait for Stream {
    unsafe fn read(&self, buffer: *mut c_void, count: i32, actual: *mut i32) -> tresult {
        if count < 0 || (count > 0 && buffer.is_null()) {
            return kInvalidArgument;
        }
        let mut empty = [];
        let bytes = if count == 0 {
            &mut empty[..]
        } else {
            // SAFETY: IBStream caller provides count writable bytes; used only in this call.
            unsafe { std::slice::from_raw_parts_mut(buffer.cast::<u8>(), count as usize) }
        };
        let Ok(n) = self.0.borrow_mut().read(bytes) else {
            return kResultFalse;
        };
        if !actual.is_null() {
            // SAFETY: optional writable byte-count output supplied by the caller.
            unsafe { *actual = n as i32 };
        }
        if n == count as usize {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn write(&self, buffer: *mut c_void, count: i32, actual: *mut i32) -> tresult {
        if count < 0 || (count > 0 && buffer.is_null()) {
            return kInvalidArgument;
        }
        let bytes = if count == 0 {
            &[][..]
        } else {
            // SAFETY: IBStream caller provides count readable bytes for this synchronous write.
            unsafe { std::slice::from_raw_parts(buffer.cast::<u8>(), count as usize) }
        };
        let Ok(n) = self.0.borrow_mut().write(bytes) else {
            return kResultFalse;
        };
        if !actual.is_null() {
            // SAFETY: optional writable byte-count output supplied by the caller.
            unsafe { *actual = n as i32 };
        }
        kResultOk
    }
    unsafe fn seek(&self, position: i64, mode: i32, actual: *mut i64) -> tresult {
        let from = match mode {
            IBStream_::IStreamSeekMode_::kIBSeekSet if position >= 0 => {
                SeekFrom::Start(position as u64)
            }
            IBStream_::IStreamSeekMode_::kIBSeekCur => SeekFrom::Current(position),
            IBStream_::IStreamSeekMode_::kIBSeekEnd => SeekFrom::End(position),
            _ => return kInvalidArgument,
        };
        let Ok(position) = self.0.borrow_mut().seek(from) else {
            return kInvalidArgument;
        };
        if !actual.is_null() {
            // SAFETY: optional writable seek-position output supplied by caller.
            unsafe { *actual = position as i64 };
        }
        kResultOk
    }
    unsafe fn tell(&self, position: *mut i64) -> tresult {
        if position.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: non-null writable position output required by IBStream.
        unsafe { *position = self.0.borrow().position() as i64 };
        kResultOk
    }
}

struct Point {
    id: Cell<u32>,
    value: Cell<f64>,
}
impl Class for Point {
    type Interfaces = (IParamValueQueue,);
}
impl IParamValueQueueTrait for Point {
    unsafe fn getParameterId(&self) -> u32 {
        self.id.get()
    }
    unsafe fn getPointCount(&self) -> i32 {
        1
    }
    unsafe fn getPoint(&self, index: i32, offset: *mut i32, value: *mut f64) -> tresult {
        if index != 0 || offset.is_null() || value.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: caller supplies writable sample-offset output for point zero.
        unsafe { *offset = 0 };
        // SAFETY: caller supplies writable parameter-value output for point zero.
        unsafe { *value = self.value.get() };
        kResultOk
    }
    unsafe fn addPoint(&self, _: i32, _: f64, _: *mut i32) -> tresult {
        kNotImplemented
    }
}
pub(crate) struct Changes {
    points: Vec<ComWrapper<Point>>,
    pointers: Vec<ComPtr<IParamValueQueue>>,
    count: Cell<usize>,
}
impl Changes {
    pub fn new() -> ComWrapper<Self> {
        let points: Vec<_> = (0..CAPACITY)
            .map(|_| {
                ComWrapper::new(Point {
                    id: Cell::new(0),
                    value: Cell::new(0.0),
                })
            })
            .collect();
        let pointers = points
            .iter()
            .map(|point| point.to_com_ptr().expect("point interface"))
            .collect();
        ComWrapper::new(Self {
            points,
            pointers,
            count: Cell::new(0),
        })
    }
    pub fn drain(&self, edits: &Edits) {
        let mut count = 0;
        // Bound work even if the producer continues to write. Coalesce same-param edits
        // at sample offset zero, retaining the last value for this block.
        for _ in 0..CAPACITY {
            let Some((id, value)) = edits.pop() else {
                break;
            };
            let index = self.points[..count]
                .iter()
                .position(|point| point.id.get() == id)
                .unwrap_or(count);
            self.points[index].id.set(id);
            self.points[index].value.set(value);
            if index == count {
                count += 1;
            }
        }
        self.count.set(count);
    }
}
impl Class for Changes {
    type Interfaces = (IParameterChanges,);
}
impl IParameterChangesTrait for Changes {
    unsafe fn getParameterCount(&self) -> i32 {
        self.count.get() as i32
    }
    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        if index < 0 || index as usize >= self.count.get() {
            std::ptr::null_mut()
        } else {
            self.pointers[index as usize].as_ptr()
        }
    }
    unsafe fn addParameterData(&self, _: *const u32, _: *mut i32) -> *mut IParamValueQueue {
        std::ptr::null_mut()
    }
}

pub(crate) struct Frame(pub Rc<EditorWindow>);
impl Class for Frame {
    type Interfaces = (IPlugFrame,);
}
impl IPlugFrameTrait for Frame {
    unsafe fn resizeView(&self, view: *mut IPlugView, rect: *mut ViewRect) -> tresult {
        if rect.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: SDK resize callback supplies a rectangle valid for this call.
        let size = unsafe { *rect };
        let Some(width) = size.right.checked_sub(size.left) else {
            return kInvalidArgument;
        };
        let Some(height) = size.bottom.checked_sub(size.top) else {
            return kInvalidArgument;
        };
        if !(1..=8192).contains(&width) || !(1..=8192).contains(&height) {
            return kInvalidArgument;
        }
        // SAFETY: callback supplies the requesting live view; borrow does not escape.
        let Some(view) = (unsafe { ComRef::from_raw(view) }) else {
            return kInvalidArgument;
        };
        if self.0.resize(width, height).is_err() {
            return kResultFalse;
        }
        // SAFETY: main-thread callback, rectangle remains valid; window has been resized first.
        unsafe { view.onSize(rect) }
    }
}
