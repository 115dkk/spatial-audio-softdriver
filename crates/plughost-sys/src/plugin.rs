#![allow(unsafe_code)]
//! VST 2 calls are serialized with a non-waiting audio admission gate. Control
//! closes admission and waits for an in-flight block; audio never waits and
//! leaves blocks pending during control work. Foreign callbacks touch only atomics.
use crate::{editor::EditorWindow, library::Library, vst2::*};
use std::{
    ffi::c_void,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PluginError(pub String);
#[derive(Clone, Debug)]
pub struct Info {
    pub name: String,
    pub vendor: String,
    pub inputs: u16,
    pub outputs: u16,
    pub latency: u32,
    pub input_types: Option<Vec<i32>>,
    pub output_types: Option<Vec<i32>>,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Events {
    pub params: bool,
    pub latency: bool,
    pub closed: bool,
}
#[derive(Debug)]
struct CallbackState {
    flags: AtomicU32,
    rate: AtomicU32,
    frames: AtomicU32,
    width: AtomicU32,
    height: AtomicU32,
}
#[derive(Debug)]
struct Shared {
    effect: usize,
    admitted: AtomicBool,
    processing: AtomicBool,
    active: AtomicBool,
    closed: AtomicBool,
    callback: CallbackState,
    _library: Library,
}
// SAFETY: the library stays loaded; raw plugin access is serialized by admission
// and processing, and callbacks access only atomics. No borrowed Rust data crosses threads.
unsafe impl Send for Shared {}
// SAFETY: all plugin calls are serialized by the admission protocol; the rest is atomic.
unsafe impl Sync for Shared {}
impl Shared {
    fn control(&self) -> Control<'_> {
        self.admitted.store(false, Ordering::SeqCst);
        while self.processing.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        Control(self)
    }
    fn effect(&self) -> *mut AEffect {
        self.effect as *mut AEffect
    }
}
struct Control<'a>(&'a Shared);
impl Drop for Control<'_> {
    fn drop(&mut self) {
        self.0.admitted.store(true, Ordering::SeqCst);
    }
}
impl Control<'_> {
    fn effect(&self) -> &AEffect {
        // SAFETY: admission is closed and the audio thread has left its foreign call.
        unsafe { &*self.0.effect() }
    }
    fn call(
        &self,
        opcode: i32,
        index: i32,
        value: isize,
        pointer: *mut c_void,
        option: f32,
    ) -> isize {
        let Some(dispatch) = self.effect().dispatcher else {
            return 0;
        };
        // SAFETY: validated dispatcher; caller supplies the payload required by this opcode.
        unsafe { dispatch(self.0.effect(), opcode, index, value, pointer, option) }
    }
    fn text(&self, opcode: i32) -> String {
        let mut buffer = [0u8; 256];
        self.call(opcode, 0, 0, buffer.as_mut_ptr().cast(), 0.0);
        let length = buffer
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(buffer.len());
        String::from_utf8_lossy(&buffer[..length]).into_owned()
    }
}
impl Drop for Shared {
    fn drop(&mut self) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        // Load-error cleanup: no processor exists and this is the main thread.
        let control = self.control();
        if self.active.swap(false, Ordering::SeqCst) {
            control.call(STOP_PROCESS, 0, 0, std::ptr::null_mut(), 0.0);
            control.call(MAINS_CHANGED, 0, 0, std::ptr::null_mut(), 0.0);
        }
        control.call(CLOSE, 0, 0, std::ptr::null_mut(), 0.0);
    }
}

#[derive(Debug)]
pub struct Vst2Plugin {
    shared: Arc<Shared>,
    info: Info,
    editor: Option<EditorWindow>,
    closed: bool,
    _main: std::marker::PhantomData<std::rc::Rc<()>>,
}
#[derive(Debug)]
pub struct Vst2Processor {
    shared: Arc<Shared>,
    inputs: usize,
    outputs: usize,
    max_frames: usize,
}
impl Vst2Plugin {
    pub fn load(path: &Path) -> Result<Self, PluginError> {
        let library = Library::load(path).map_err(|e| PluginError(e.to_string()))?;
        let symbol = library
            .symbol(c"VSTPluginMain")
            .or_else(|| library.symbol(c"main"))
            .ok_or_else(|| PluginError("missing VSTPluginMain".into()))?;
        // SAFETY: symbol is the required VST 2 entry point in a disposable host process.
        let entry: Entry = unsafe { std::mem::transmute(symbol) };
        // SAFETY: host callback has the clean-room ABI and static lifetime.
        let effect = unsafe { entry(callback) };
        if effect.is_null() {
            return Err(PluginError("null plugin instance".into()));
        }
        // SAFETY: successful entry returns an AEffect that remains live until CLOSE.
        let fields = unsafe { &*effect };
        if fields.magic != MAGIC
            || fields.dispatcher.is_none()
            || fields.process_replacing.is_none()
            || !(0..=32).contains(&fields.num_inputs)
            || !(1..=32).contains(&fields.num_outputs)
            || !(0..=16384).contains(&fields.num_params)
        {
            return Err(PluginError("invalid plugin ABI or channel count".into()));
        }
        // Parameter and bus counts are frozen at load; unsupported IO changes
        // are reported for replacement rather than silently resizing RT buffers.
        let inputs = fields.num_inputs as u16;
        let outputs = fields.num_outputs as u16;
        let shared = Arc::new(Shared {
            effect: effect as usize,
            admitted: AtomicBool::new(true),
            processing: AtomicBool::new(false),
            active: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            callback: CallbackState {
                flags: AtomicU32::new(0),
                rate: AtomicU32::new(48000),
                frames: AtomicU32::new(480),
                width: AtomicU32::new(0),
                height: AtomicU32::new(0),
            },
            _library: library,
        });
        // SAFETY: before any processing; resvd1 is reserved for the host. Callback state
        // stays at a stable Arc allocation until after effClose and all processors drop.
        unsafe { (*effect).resvd1 = (&shared.callback as *const CallbackState).cast_mut().cast() };
        let control = shared.control();
        control.call(OPEN, 0, 0, std::ptr::null_mut(), 0.0);
        let mut input = std::ptr::null_mut::<Arrangement>();
        let mut output = std::ptr::null_mut::<Arrangement>();
        let arrangements = control.call(
            GET_SPEAKER_ARRANGEMENT,
            0,
            (&mut input as *mut *mut Arrangement) as isize,
            (&mut output as *mut *mut Arrangement).cast(),
            0.0,
        );
        let types =
            |pointer: *mut Arrangement, count: u16| -> Result<Option<Vec<i32>>, PluginError> {
                if arrangements == 0 || pointer.is_null() {
                    return Ok(None);
                }
                // SAFETY: successful arrangement query returns its live header.
                let channels = unsafe { (*pointer).channels };
                if channels != i32::from(count) {
                    return Err(PluginError("speaker arrangement count mismatch".into()));
                }
                // SAFETY: address of the first speaker only; do not reference a fixed-size
                // struct since third-party plugins may allocate exactly channels entries.
                let first = unsafe { std::ptr::addr_of!((*pointer).speakers).cast::<Speaker>() };
                // SAFETY: validated count <= 32; query guarantees that many live speaker records.
                let speakers = unsafe { std::slice::from_raw_parts(first, count as usize) };
                Ok(Some(speakers.iter().map(|speaker| speaker.kind).collect()))
            };
        let info = Info {
            name: control.text(GET_EFFECT_NAME),
            vendor: control.text(GET_VENDOR_STRING),
            inputs,
            outputs,
            latency: control.effect().initial_delay.max(0) as u32,
            input_types: types(input, inputs)?,
            output_types: types(output, outputs)?,
        };
        drop(control);
        Ok(Self {
            shared,
            info,
            editor: None,
            closed: false,
            _main: std::marker::PhantomData,
        })
    }
    pub fn info(&self) -> Info {
        self.info.clone()
    }
    pub fn configure(
        &mut self,
        rate: u32,
        frames: u32,
        inputs: u16,
        outputs: u16,
    ) -> Result<(), PluginError> {
        if self.shared.active.load(Ordering::SeqCst)
            || rate < 1000
            || frames == 0
            || frames > 65536
            || inputs != self.info.inputs
            || outputs != self.info.outputs
        {
            return Err(PluginError("unsupported configuration".into()));
        }
        let control = self.shared.control();
        self.shared.callback.rate.store(rate, Ordering::Relaxed);
        self.shared.callback.frames.store(frames, Ordering::Relaxed);
        control.call(SET_SAMPLE_RATE, 0, 0, std::ptr::null_mut(), rate as f32);
        control.call(
            SET_BLOCK_SIZE,
            0,
            frames as isize,
            std::ptr::null_mut(),
            0.0,
        );
        Ok(())
    }
    pub fn activate(&mut self) {
        let control = self.shared.control();
        if !self.shared.active.swap(true, Ordering::SeqCst) {
            control.call(MAINS_CHANGED, 0, 1, std::ptr::null_mut(), 0.0);
            control.call(START_PROCESS, 0, 0, std::ptr::null_mut(), 0.0);
        }
    }
    pub fn deactivate(&mut self) {
        let control = self.shared.control();
        if self.shared.active.swap(false, Ordering::SeqCst) {
            control.call(STOP_PROCESS, 0, 0, std::ptr::null_mut(), 0.0);
            control.call(MAINS_CHANGED, 0, 0, std::ptr::null_mut(), 0.0);
        }
    }
    pub fn processor(&self) -> Vst2Processor {
        Vst2Processor {
            shared: self.shared.clone(),
            inputs: self.info.inputs as usize,
            outputs: self.info.outputs as usize,
            max_frames: self.shared.callback.frames.load(Ordering::Relaxed) as usize,
        }
    }
    pub fn get_state(&mut self) -> Result<Vec<u8>, PluginError> {
        let control = self.shared.control();
        if control.effect().flags & PROGRAM_CHUNKS != 0 {
            let mut data = std::ptr::null::<u8>();
            let size = control.call(GET_CHUNK, 0, 0, (&mut data as *mut *const u8).cast(), 0.0);
            if !(0..64 * 1024 * 1024 - 8).contains(&size) || (size != 0 && data.is_null()) {
                return Err(PluginError("invalid plugin chunk".into()));
            }
            if size == 0 {
                return Ok(Vec::new());
            }
            // SAFETY: GET_CHUNK guarantees size readable bytes until the next dispatcher call.
            return Ok(unsafe { std::slice::from_raw_parts(data, size as usize) }.to_vec());
        }
        let mut state = (control.call(GET_PROGRAM, 0, 0, std::ptr::null_mut(), 0.0) as i32)
            .to_le_bytes()
            .to_vec();
        let count = control.effect().num_params;
        let get = control
            .effect()
            .get_parameter
            .ok_or_else(|| PluginError("missing getParameter".into()))?;
        for index in 0..count {
            // SAFETY: validated parameter index; exclusive control access to plugin.
            let value = unsafe { get(self.shared.effect(), index as u32) };
            state.extend(value.to_le_bytes());
        }
        Ok(state)
    }
    pub fn set_state(&mut self, blob: &[u8]) -> Result<(), PluginError> {
        if blob.starts_with(b"SSV3") {
            return Err(PluginError(
                "VST 3 state cannot be restored into VST 2".into(),
            ));
        }
        if blob.len() >= 64 * 1024 * 1024 - 8 {
            return Err(PluginError("state too large".into()));
        }
        let control = self.shared.control();
        if control.effect().flags & PROGRAM_CHUNKS != 0 {
            let mut blob = blob.to_vec();
            control.call(
                SET_CHUNK,
                0,
                blob.len() as isize,
                blob.as_mut_ptr().cast(),
                0.0,
            );
        } else {
            let count = control.effect().num_params as usize;
            if blob.len() != 4 + count * 4 {
                return Err(PluginError("invalid parameter state".into()));
            }
            let program = i32::from_le_bytes(blob[..4].try_into().expect("four bytes"));
            if program < 0 || program >= control.effect().num_programs {
                return Err(PluginError("invalid program".into()));
            }
            control.call(SET_PROGRAM, 0, program as isize, std::ptr::null_mut(), 0.0);
            let set = control
                .effect()
                .set_parameter
                .ok_or_else(|| PluginError("missing setParameter".into()))?;
            for (index, value) in blob[4..].as_chunks::<4>().0.iter().enumerate() {
                let value = f32::from_le_bytes(*value);
                // SAFETY: exact state size checked; index is a valid parameter, control exclusive.
                unsafe { set(self.shared.effect(), index as u32, value) };
            }
        }
        Ok(())
    }
    pub fn open_editor(&mut self) -> Result<(), PluginError> {
        if self.editor.is_some() {
            return Ok(());
        }
        let control = self.shared.control();
        if control.effect().flags & HAS_EDITOR == 0 {
            return Err(PluginError("plugin has no editor".into()));
        }
        // effEditGetRect writes a pointer to the plugin's own rectangle, or
        // leaves the out-pointer alone (some plugins return null instead).
        let fallback = Rect {
            top: 0,
            left: 0,
            bottom: 480,
            right: 640,
        };
        let mut rect: *const Rect = &fallback;
        control.call(
            EDIT_GET_RECT,
            0,
            0,
            (&mut rect as *mut *const Rect).cast(),
            0.0,
        );
        // SAFETY: `rect` is still `&fallback`, or null, or the plugin's
        // rectangle, which stays valid until the next dispatcher call.
        let rect = unsafe { rect.as_ref() }.copied().unwrap_or(fallback);
        let (width, height) = (
            i32::from(rect.right) - i32::from(rect.left),
            i32::from(rect.bottom) - i32::from(rect.top),
        );
        let editor = EditorWindow::open(&self.info.name, width, height)
            .map_err(|e| PluginError(e.to_string()))?;
        control.call(EDIT_OPEN, 0, 0, editor.handle_value() as *mut c_void, 0.0);
        self.editor = Some(editor);
        Ok(())
    }
    pub fn request_editor_close(&self) -> Result<(), PluginError> {
        if let Some(editor) = &self.editor {
            editor
                .request_close()
                .map_err(|e| PluginError(e.to_string()))?;
        }
        Ok(())
    }
    pub fn idle(&mut self) {
        if let Some(editor) = &self.editor {
            let control = self.shared.control();
            control.call(EDIT_IDLE, 0, 0, std::ptr::null_mut(), 0.0);
            let width = self.shared.callback.width.swap(0, Ordering::Relaxed);
            let height = self.shared.callback.height.load(Ordering::Relaxed);
            if width != 0 {
                let _ = editor.resize(width as i32, height as i32);
            }
            if editor.pump() {
                control.call(EDIT_CLOSE, 0, 0, std::ptr::null_mut(), 0.0);
                self.editor = None;
                self.closed = true;
            }
        }
    }
    pub fn take_events(&mut self) -> Events {
        let flags = self.shared.callback.flags.swap(0, Ordering::AcqRel);
        if flags & 2 != 0 {
            let control = self.shared.control();
            self.info.latency = control.effect().initial_delay.max(0) as u32;
        }
        Events {
            params: flags & 1 != 0,
            latency: flags & 2 != 0,
            closed: std::mem::take(&mut self.closed),
        }
    }
}
impl Drop for Vst2Plugin {
    fn drop(&mut self) {
        if self.editor.is_some() {
            let control = self.shared.control();
            control.call(EDIT_CLOSE, 0, 0, std::ptr::null_mut(), 0.0);
            self.editor = None;
        }
        self.deactivate();
        let control = self.shared.control();
        control.call(CLOSE, 0, 0, std::ptr::null_mut(), 0.0);
        self.shared.closed.store(true, Ordering::SeqCst);
    }
}
impl Vst2Processor {
    pub fn process(&mut self, planes: &mut [&mut [f32]], frames: usize) -> bool {
        if planes.len() < self.inputs.max(self.outputs)
            || frames > self.max_frames
            || planes.iter().any(|plane| plane.len() < frames)
            || self.shared.closed.load(Ordering::SeqCst)
            || !self.shared.admitted.load(Ordering::SeqCst)
            || self.shared.processing.swap(true, Ordering::SeqCst)
        {
            return false;
        }
        let mut processed = false;
        if self.shared.admitted.load(Ordering::SeqCst) && self.shared.active.load(Ordering::SeqCst)
        {
            let mut inputs = [std::ptr::null(); 32];
            let mut outputs = [std::ptr::null_mut(); 32];
            for (pointer, plane) in inputs.iter_mut().zip(planes.iter()).take(self.inputs) {
                *pointer = plane.as_ptr();
            }
            for (pointer, plane) in outputs.iter_mut().zip(planes.iter_mut()).take(self.outputs) {
                *pointer = plane.as_mut_ptr();
            }
            // SAFETY: processing token excludes control and other processors; instance is live.
            let process = unsafe { (*self.shared.effect()).process_replacing };
            if let Some(process) = process {
                // SAFETY: active instance, valid preallocated pointer arrays and frame bounds.
                unsafe {
                    process(
                        self.shared.effect(),
                        inputs.as_ptr(),
                        outputs.as_mut_ptr(),
                        frames as i32,
                    )
                };
                processed = true;
            }
        }
        self.shared.processing.store(false, Ordering::SeqCst);
        processed
    }
}

unsafe extern "C" fn callback(
    effect: *mut AEffect,
    opcode: i32,
    index: i32,
    value: isize,
    pointer: *mut c_void,
    _option: f32,
) -> isize {
    if opcode == MASTER_VERSION {
        return 2400;
    }
    if opcode == MASTER_VENDOR || opcode == MASTER_PRODUCT {
        if pointer.is_null() {
            return 0;
        }
        let text = b"Plugin host\0";
        // SAFETY: callback opcode supplies a writable host-name buffer of at least 64 bytes.
        unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), pointer.cast(), text.len()) };
        return 1;
    }
    if effect.is_null() {
        return 0;
    }
    // SAFETY: callback's effect belongs to the plugin; resvd1 is host-owned storage.
    let state = unsafe { (*effect).resvd1.cast::<CallbackState>() };
    if state.is_null() {
        return 0;
    }
    // SAFETY: host installed a stable CallbackState before effOpen and retains it through effClose.
    let state = unsafe { &*state };
    match opcode {
        MASTER_SAMPLE_RATE => state.rate.load(Ordering::Relaxed) as isize,
        MASTER_BLOCK_SIZE => state.frames.load(Ordering::Relaxed) as isize,
        MASTER_AUTOMATE => {
            state.flags.fetch_or(1, Ordering::Release);
            1
        }
        MASTER_IO_CHANGED => {
            state.flags.fetch_or(2, Ordering::Release);
            1
        }
        MASTER_SIZE_WINDOW if index > 0 && value > 0 && index <= 8192 && value <= 8192 => {
            state.height.store(value as u32, Ordering::Relaxed);
            state.width.store(index as u32, Ordering::Release);
            1
        }
        _ => 0,
    }
}
