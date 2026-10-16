#![allow(unsafe_code, non_snake_case)]
//! Test-only COM implementation using vst3 0.3.0's generated interfaces.
use std::{
    cell::{RefCell, UnsafeCell},
    ffi::{CStr, c_char, c_void},
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};
use vst3::{Class, ComPtr, ComRef, ComWrapper, Steinberg::Vst::*, Steinberg::*, uid};
const CID: TUID = uid(0x53535233, 0x74657374, 0x6d617472, 0x69783031);
fn narrow(text: &str, output: &mut [c_char]) {
    output.fill(0);
    let capacity = output.len().saturating_sub(1);
    for (dst, src) in output.iter_mut().take(capacity).zip(text.bytes()) {
        *dst = src as c_char;
    }
}
fn wide(text: &str, output: &mut [u16]) {
    output.fill(0);
    let capacity = output.len().saturating_sub(1);
    for (dst, src) in output.iter_mut().take(capacity).zip(text.encode_utf16()) {
        *dst = src;
    }
}
struct Delay {
    samples: [[f32; 64]; 6],
    cursor: usize,
}
struct Matrix {
    mode: AtomicU8,
    width: AtomicU64,
    active: AtomicBool,
    delay: UnsafeCell<Delay>,
    handler: RefCell<Option<ComPtr<IComponentHandler>>>,
}
impl Matrix {
    fn new() -> Self {
        Self {
            mode: AtomicU8::new(0),
            width: AtomicU64::new(1.0f64.to_bits()),
            active: AtomicBool::new(false),
            delay: UnsafeCell::new(Delay {
                samples: [[0.0; 64]; 6],
                cursor: 0,
            }),
            handler: RefCell::new(None),
        }
    }
    unsafe fn restore(&self, state: *mut IBStream) -> tresult {
        // SAFETY: SDK passes a live stream borrowed only for this state call.
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
            return kInvalidArgument;
        };
        let mut bytes = [0u8; 2];
        let mut read = 0;
        // SAFETY: two-byte writable output and byte-count output are live for this call.
        let result = unsafe { stream.read(bytes.as_mut_ptr().cast(), 2, &mut read) };
        if result != kResultOk || read != 2 || bytes[0] != 1 || bytes[1] > 4 {
            return kInvalidArgument;
        }
        if bytes[1] == 4 {
            std::process::abort();
        }
        self.mode.store(bytes[1], Ordering::Relaxed);
        kResultOk
    }
    unsafe fn save(&self, state: *mut IBStream) -> tresult {
        // SAFETY: live stream borrowed from SDK for synchronous main-thread state save.
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
            return kInvalidArgument;
        };
        let mut bytes = [1, self.mode.load(Ordering::Relaxed)];
        let mut written = 0;
        // SAFETY: stream reads two stack bytes synchronously, count output is writable.
        let result = unsafe { stream.write(bytes.as_mut_ptr().cast(), 2, &mut written) };
        if result == kResultOk && written == 2 {
            kResultOk
        } else {
            kResultFalse
        }
    }
}
impl Class for Matrix {
    type Interfaces = (IComponent, IAudioProcessor, IEditController);
}
impl IPluginBaseTrait for Matrix {
    unsafe fn initialize(&self, _: *mut FUnknown) -> tresult {
        kResultOk
    }
    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}
impl IComponentTrait for Matrix {
    unsafe fn getControllerClassId(&self, _: *mut TUID) -> tresult {
        kNotImplemented
    }
    unsafe fn setIoMode(&self, _: i32) -> tresult {
        kResultOk
    }
    unsafe fn getBusCount(&self, media: i32, direction: i32) -> i32 {
        i32::from(
            media == MediaTypes_::kAudio
                && [{ BusDirections_::kInput }, { BusDirections_::kOutput }].contains(&direction),
        )
    }
    unsafe fn getBusInfo(
        &self,
        media: i32,
        direction: i32,
        index: i32,
        info: *mut BusInfo,
    ) -> tresult {
        if media != MediaTypes_::kAudio || index != 0 || info.is_null() {
            return kInvalidArgument;
        }
        let channels = match direction {
            0 => 2,
            1 => 6,
            _ => return kInvalidArgument,
        };
        // SAFETY: non-null SDK BusInfo output for the queried main bus.
        let info = unsafe { &mut *info };
        info.mediaType = media;
        info.direction = direction;
        info.channelCount = channels;
        info.busType = BusTypes_::kMain;
        info.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
        wide(
            if channels == 2 {
                "Stereo input"
            } else {
                "Surround output"
            },
            &mut info.name,
        );
        kResultOk
    }
    unsafe fn getRoutingInfo(&self, _: *mut RoutingInfo, _: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }
    unsafe fn activateBus(&self, media: i32, direction: i32, index: i32, _: TBool) -> tresult {
        if media == MediaTypes_::kAudio && (0..=1).contains(&direction) && index == 0 {
            kResultOk
        } else {
            kInvalidArgument
        }
    }
    unsafe fn setActive(&self, active: TBool) -> tresult {
        self.active.store(active != 0, Ordering::Relaxed);
        kResultOk
    }
    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        // SAFETY: forwards SDK stream with identical synchronous lifetime contract.
        unsafe { self.restore(state) }
    }
    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        // SAFETY: forwards SDK stream; state uses atomics and can overlap process.
        unsafe { self.save(state) }
    }
}
impl IAudioProcessorTrait for Matrix {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut u64,
        ni: i32,
        outputs: *mut u64,
        no: i32,
    ) -> tresult {
        if ni != 1 || no != 1 || inputs.is_null() || outputs.is_null() {
            return kResultFalse;
        }
        // SAFETY: caller supplied one input arrangement as declared by ni.
        let input = unsafe { *inputs };
        // SAFETY: caller supplied one output arrangement as declared by no.
        let output = unsafe { *outputs };
        if input == SpeakerArr::kStereo && output == SpeakerArr::k51 {
            kResultTrue
        } else {
            kResultFalse
        }
    }
    unsafe fn getBusArrangement(
        &self,
        direction: i32,
        index: i32,
        arrangement: *mut u64,
    ) -> tresult {
        if index != 0 || arrangement.is_null() {
            return kInvalidArgument;
        }
        let value = match direction {
            0 => SpeakerArr::kStereo,
            1 => SpeakerArr::k51,
            _ => return kInvalidArgument,
        };
        // SAFETY: non-null writable arrangement output for the main bus.
        unsafe { *arrangement = value };
        kResultOk
    }
    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 {
            kResultOk
        } else {
            kResultFalse
        }
    }
    unsafe fn getLatencySamples(&self) -> u32 {
        64
    }
    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        if setup.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: caller provides a live ProcessSetup during inactive configuration.
        let setup = unsafe { &*setup };
        if setup.symbolicSampleSize != SymbolicSampleSizes_::kSample32
            || setup.maxSamplesPerBlock <= 0
        {
            kInvalidArgument
        } else {
            kResultOk
        }
    }
    unsafe fn setProcessing(&self, _: TBool) -> tresult {
        kResultOk
    }
    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        if data.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: sole processing thread receives a live ProcessData for this block.
        let data = unsafe { &*data };
        if !self.active.load(Ordering::Relaxed)
            || data.symbolicSampleSize != SymbolicSampleSizes_::kSample32
            || data.numInputs != 1
            || data.numOutputs != 1
            || data.inputs.is_null()
            || data.outputs.is_null()
            || data.numSamples < 0
        {
            return kInvalidArgument;
        }
        match self.mode.load(Ordering::Relaxed) {
            1 => std::process::abort(),
            2 => loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            },
            _ => {}
        }
        // SAFETY: optional SDK parameter changes interface is borrowed for process only.
        if let Some(changes) = unsafe { ComRef::from_raw(data.inputParameterChanges) } {
            // SAFETY: live parameter changes interface, queried synchronously on audio thread.
            let count = unsafe { changes.getParameterCount() };
            for index in 0..count {
                // SAFETY: index from reported parameter queue count.
                let pointer = unsafe { changes.getParameterData(index) };
                // SAFETY: borrowed queue is valid until process returns.
                if let Some(queue) = unsafe { ComRef::from_raw(pointer) } {
                    // SAFETY: live input parameter queue.
                    let id = unsafe { queue.getParameterId() };
                    // SAFETY: live input parameter queue.
                    let points = unsafe { queue.getPointCount() };
                    if id == 0 && points > 0 {
                        let mut offset = 0;
                        let mut value: f64 = 0.0;
                        // SAFETY: valid last-point index and writable offset/value outputs.
                        if unsafe { queue.getPoint(points - 1, &mut offset, &mut value) }
                            == kResultOk
                            && value.is_finite()
                        {
                            self.width
                                .store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
                        }
                    }
                }
            }
        }
        // SAFETY: data declares one live input AudioBusBuffers.
        let input = unsafe { &*data.inputs };
        // SAFETY: data declares one live output AudioBusBuffers.
        let output = unsafe { &*data.outputs };
        if input.numChannels != 2 || output.numChannels != 6 {
            return kInvalidArgument;
        }
        // SAFETY: sample format validated as kSample32 above.
        let inputs = unsafe { input.__field0.channelBuffers32 };
        // SAFETY: sample format validated as kSample32 above.
        let outputs = unsafe { output.__field0.channelBuffers32 };
        if inputs.is_null() || outputs.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: input bus declares two live channel pointers.
        let inputs = unsafe { std::slice::from_raw_parts(inputs, 2) };
        // SAFETY: output bus declares six live channel pointers.
        let outputs = unsafe { std::slice::from_raw_parts(outputs, 6) };
        if inputs.iter().chain(outputs).any(|p| p.is_null()) {
            return kInvalidArgument;
        }
        // SAFETY: delay storage accessed only by the sole audio thread, never state/controller calls.
        let delay = unsafe { &mut *self.delay.get() };
        let width = f64::from_bits(self.width.load(Ordering::Relaxed)) as f32;
        for frame in 0..data.numSamples as usize {
            // SAFETY: input channel has numSamples readable f32s, read before any in-place writes.
            let left = unsafe { *inputs[0].wrapping_add(frame) };
            // SAFETY: input channel has numSamples readable f32s, read before any in-place writes.
            let right = unsafe { *inputs[1].wrapping_add(frame) };
            let center = 0.5 * (left + right);
            let values = [
                left,
                right,
                center,
                center,
                0.5 * left * width,
                0.5 * right * width,
            ];
            for channel in 0..6 {
                let delayed = delay.samples[channel][delay.cursor];
                delay.samples[channel][delay.cursor] = values[channel];
                let value = if self.mode.load(Ordering::Relaxed) == 3 && channel == 2 {
                    f32::NAN
                } else {
                    delayed
                };
                // SAFETY: output channel has numSamples writable f32s; no aliased Rust references exist.
                unsafe { *outputs[channel].wrapping_add(frame) = value };
            }
            delay.cursor = (delay.cursor + 1) % 64;
        }
        kResultOk
    }
    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}
impl IEditControllerTrait for Matrix {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        // SAFETY: fresh SDK component state stream; restores only atomics.
        unsafe { self.restore(state) }
    }
    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        // SAFETY: controller state stream uses the same test fixture representation.
        unsafe { self.restore(state) }
    }
    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        // SAFETY: writable controller state stream supplied by host.
        unsafe { self.save(state) }
    }
    unsafe fn getParameterCount(&self) -> i32 {
        1
    }
    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        if index != 0 || info.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: SDK output for parameter zero is a writable ParameterInfo.
        let info = unsafe { &mut *info };
        info.id = 0;
        info.stepCount = 0;
        info.defaultNormalizedValue = 1.0;
        info.unitId = 0;
        info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate;
        wide("Width", &mut info.title);
        wide("Width", &mut info.shortTitle);
        wide("", &mut info.units);
        kResultOk
    }
    unsafe fn getParamStringByValue(&self, id: u32, value: f64, string: *mut String128) -> tresult {
        if id != 0 || string.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: writable fixed-size SDK parameter string output.
        let string = unsafe { &mut *string };
        wide(&value.to_string(), string);
        kResultOk
    }
    unsafe fn getParamValueByString(&self, _: u32, _: *mut TChar, _: *mut f64) -> tresult {
        kNotImplemented
    }
    unsafe fn normalizedParamToPlain(&self, _: u32, value: f64) -> f64 {
        value
    }
    unsafe fn plainParamToNormalized(&self, _: u32, value: f64) -> f64 {
        value
    }
    unsafe fn getParamNormalized(&self, _: u32) -> f64 {
        f64::from_bits(self.width.load(Ordering::Relaxed))
    }
    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        if id != 0 || !value.is_finite() {
            return kInvalidArgument;
        }
        self.width
            .store(value.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        kResultOk
    }
    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        // SAFETY: SDK handler is borrowed; to_com_ptr retains it for controller lifetime.
        *self.handler.borrow_mut() =
            unsafe { ComRef::from_raw(handler) }.map(|handler| handler.to_com_ptr());
        kResultOk
    }
    unsafe fn createView(&self, name: *const c_char) -> *mut IPlugView {
        if name.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: SDK passes a terminated view type identifier.
        if unsafe { CStr::from_ptr(name) } != c"editor" {
            return std::ptr::null_mut();
        }
        ComWrapper::new(View {
            handler: self.handler.borrow().clone(),
        })
        .to_com_ptr::<IPlugView>()
        .expect("view interface")
        .into_raw()
    }
}
struct View {
    handler: Option<ComPtr<IComponentHandler>>,
}
impl Class for View {
    type Interfaces = (IPlugView,);
}
impl IPlugViewTrait for View {
    unsafe fn isPlatformTypeSupported(&self, platform: FIDString) -> tresult {
        if platform.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: SDK platform string is null-terminated.
        if unsafe { CStr::from_ptr(platform) } == c"HWND" {
            kResultTrue
        } else {
            kResultFalse
        }
    }
    unsafe fn attached(&self, parent: *mut c_void, platform: FIDString) -> tresult {
        if parent.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: forwards identical SDK platform string contract.
        if unsafe { self.isPlatformTypeSupported(platform) } != kResultTrue {
            return kResultFalse;
        }
        // A deterministic editor gesture exercises the host's lock-free automation path.
        if let Some(handler) = &self.handler {
            // SAFETY: attached is on UI thread; retained host handler and known parameter zero.
            let _ = unsafe { handler.beginEdit(0) };
            // SAFETY: UI-thread normalized edit for Width; processor receives it via queue.
            let _ = unsafe { handler.performEdit(0, 0.5) };
            // SAFETY: finishes the preceding UI-thread edit gesture.
            let _ = unsafe { handler.endEdit(0) };
        }
        kResultOk
    }
    unsafe fn removed(&self) -> tresult {
        kResultOk
    }
    unsafe fn onWheel(&self, _: f32) -> tresult {
        kResultFalse
    }
    unsafe fn onKeyDown(&self, _: char16, _: i16, _: i16) -> tresult {
        kResultFalse
    }
    unsafe fn onKeyUp(&self, _: char16, _: i16, _: i16) -> tresult {
        kResultFalse
    }
    unsafe fn getSize(&self, rect: *mut ViewRect) -> tresult {
        if rect.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: writable SDK rectangle output for this fixed-size editor.
        unsafe {
            *rect = ViewRect {
                left: 0,
                top: 0,
                right: 320,
                bottom: 200,
            }
        };
        kResultOk
    }
    unsafe fn onSize(&self, _: *mut ViewRect) -> tresult {
        kResultOk
    }
    unsafe fn onFocus(&self, _: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setFrame(&self, _: *mut IPlugFrame) -> tresult {
        kResultOk
    }
    unsafe fn canResize(&self) -> tresult {
        kResultFalse
    }
    unsafe fn checkSizeConstraint(&self, _: *mut ViewRect) -> tresult {
        kResultOk
    }
}
struct Factory;
impl Class for Factory {
    type Interfaces = (IPluginFactory2,);
}
impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        if info.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: factory-info output is a writable SDK structure.
        let info = unsafe { &mut *info };
        narrow("Test fixtures", &mut info.vendor);
        narrow("", &mut info.url);
        narrow("", &mut info.email);
        info.flags = 0;
        kResultOk
    }
    unsafe fn countClasses(&self) -> i32 {
        1
    }
    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        if index != 0 || info.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: index zero output is a writable SDK class-info structure.
        let info = unsafe { &mut *info };
        info.cid = CID;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances;
        narrow("Audio Module Class", &mut info.category);
        narrow("Test matrix", &mut info.name);
        kResultOk
    }
    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        object: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || iid.is_null() || object.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: required writable interface output initialized even on failure.
        unsafe { *object = std::ptr::null_mut() };
        // SAFETY: SDK class ID contains 16 bytes; read_unaligned avoids alignment assumptions.
        let id = unsafe { cid.cast::<TUID>().read_unaligned() };
        if id != CID {
            return kInvalidArgument;
        }
        let instance = ComWrapper::new(Matrix::new())
            .to_com_ptr::<FUnknown>()
            .expect("component unknown");
        let pointer = instance.as_ptr();
        // SAFETY: retained COM object exposes its FUnknown vtable for queryInterface.
        let table = unsafe { (*pointer).vtbl };
        // SAFETY: the live COM instance retains its static FUnknown vtable.
        let vtable = unsafe { &*table };
        // SAFETY: caller supplies SDK IID and writable owned-interface output; query retains result.
        unsafe { (vtable.queryInterface)(pointer, iid as *const TUID, object) }
    }
}
impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        if index != 0 || info.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: writable extended class-info output for the only factory class.
        let info = unsafe { &mut *info };
        info.cid = CID;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances;
        info.classFlags = 0;
        narrow("Audio Module Class", &mut info.category);
        narrow("Test matrix", &mut info.name);
        narrow("Fx|Spatial", &mut info.subCategories);
        narrow("Test fixtures", &mut info.vendor);
        narrow("1.0.0", &mut info.version);
        narrow("VST 3.8", &mut info.sdkVersion);
        kResultOk
    }
}
// SAFETY: unique Windows SDK module initialization export; no external state required.
#[unsafe(no_mangle)]
pub extern "system" fn InitDll() -> bool {
    true
}
// SAFETY: unique Windows SDK module shutdown export; objects already released.
#[unsafe(no_mangle)]
pub extern "system" fn ExitDll() -> bool {
    true
}
// SAFETY: unique SDK factory export returning one owned COM reference.
#[unsafe(no_mangle)]
pub extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    ComWrapper::new(Factory)
        .to_com_ptr::<IPluginFactory>()
        .expect("factory interface")
        .into_raw()
}
