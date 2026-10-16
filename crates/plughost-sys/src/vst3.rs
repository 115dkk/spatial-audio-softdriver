#![allow(unsafe_code)]
//! VST 3 isolation. Lifecycle/controller/editor/state run on the creating main
//! thread. Only process/setProcessing run on the sole audio thread. getState is
//! permitted in the Processing state; setState uses a nonblocking admission gate.
//! <https://steinbergmedia.github.io/vst3_dev_portal/pages/Technical+Documentation/API+Documentation/Index.html>
use crate::{
    editor::EditorWindow,
    library::Library,
    plugin::PluginError,
    vst3_support::{Changes, Edits, Frame, Handler, Host, Stream},
};
use std::{
    ffi::{c_char, c_void},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use vst3::{ComPtr, ComWrapper, Interface, Steinberg::Vst::*, Steinberg::*};

fn check(result: tresult, operation: &str) -> Result<(), PluginError> {
    if result == kResultOk {
        Ok(())
    } else {
        Err(PluginError(format!("VST 3 {operation}: {result}")))
    }
}
fn text(bytes: &[c_char]) -> String {
    let bytes: Vec<_> = bytes
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}
#[derive(Clone, Debug)]
pub struct ClassInfo {
    pub id: [u8; 16],
    pub name: String,
    pub vendor: String,
    pub category: String,
}
#[derive(Clone, Debug)]
pub struct Info {
    pub class: ClassInfo,
    pub input: u64,
    pub output: u64,
    pub inputs: u16,
    pub outputs: u16,
    pub latency: u32,
}
#[derive(Debug, Default)]
pub struct Events {
    pub params: bool,
    pub latency: Option<u32>,
    pub closed: bool,
    pub fault: Option<&'static str>,
}
struct ModuleLibrary {
    library: Library,
    exit: Option<unsafe extern "system" fn() -> bool>,
}
impl Drop for ModuleLibrary {
    fn drop(&mut self) {
        if let Some(exit) = self.exit {
            // SAFETY: module still loaded, all factory and instance COM references already released.
            let _ = unsafe { exit() };
        }
    }
}
// SAFETY: module ownership may follow the last processor reference; no calls are
// made through Library concurrently. Host teardown joins audio before main drops.
unsafe impl Send for ModuleLibrary {}
// SAFETY: shared lifetime keeper only; Library is never mutated after loading.
unsafe impl Sync for ModuleLibrary {}
pub struct Vst3Module {
    factory: ComPtr<IPluginFactory>,
    host: ComPtr<FUnknown>,
    library: Arc<ModuleLibrary>,
}
impl std::fmt::Debug for Vst3Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3Module").finish_non_exhaustive()
    }
}
fn binary(path: &Path) -> Result<PathBuf, PluginError> {
    if !path.is_dir() {
        return Ok(path.to_owned());
    }
    let name = path
        .file_name()
        .ok_or_else(|| PluginError("invalid VST 3 bundle".into()))?;
    #[cfg(target_arch = "aarch64")]
    let architectures = ["arm64-win", "arm64ec-win"];
    #[cfg(not(target_arch = "aarch64"))]
    let architectures = ["x86_64-win"];
    architectures
        .iter()
        .map(|arch| path.join("Contents").join(arch).join(name))
        .find(|file| file.is_file())
        .ok_or_else(|| PluginError("VST 3 bundle has no binary for this architecture".into()))
}
impl Vst3Module {
    pub fn load(path: &Path) -> Result<Self, PluginError> {
        let library = Library::load(&binary(path)?).map_err(|e| PluginError(e.to_string()))?;
        let exit = library.symbol(c"ExitDll").map(|symbol| {
            // SAFETY: SDK Windows module exit export has this exact ABI.
            unsafe {
                std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    unsafe extern "system" fn() -> bool,
                >(symbol)
            }
        });
        if let Some(symbol) = library.symbol(c"InitDll") {
            // SAFETY: SDK Windows module initialization export has this exact ABI.
            let init: unsafe extern "system" fn() -> bool = unsafe { std::mem::transmute(symbol) };
            // SAFETY: DLL is loaded; initialization precedes all factory calls.
            if !unsafe { init() } {
                return Err(PluginError("VST 3 InitDll failed".into()));
            }
        }
        let library = Arc::new(ModuleLibrary { library, exit });
        let symbol = library
            .library
            .symbol(c"GetPluginFactory")
            .ok_or_else(|| PluginError("missing GetPluginFactory".into()))?;
        // SAFETY: SDK factory export uses system ABI and returns an owned factory reference.
        let entry: unsafe extern "system" fn() -> *mut IPluginFactory =
            unsafe { std::mem::transmute(symbol) };
        // SAFETY: initialized module, factory entry point validated by export name.
        let pointer = unsafe { entry() };
        // SAFETY: adopt the reference returned by GetPluginFactory, checking null.
        let factory = unsafe { ComPtr::from_raw(pointer) }
            .ok_or_else(|| PluginError("null VST 3 factory".into()))?;
        let host = ComWrapper::new(Host)
            .to_com_ptr::<FUnknown>()
            .expect("host interface");
        if let Some(factory3) = factory.cast::<IPluginFactory3>() {
            // SAFETY: main thread, live factory and retained host context.
            let _ = unsafe { factory3.setHostContext(host.as_ptr()) };
        }
        Ok(Self {
            factory,
            host,
            library,
        })
    }
    pub fn scan(&self) -> Result<Vec<ClassInfo>, PluginError> {
        // SAFETY: plain SDK struct containing only integers and fixed character arrays.
        let mut vendor: PFactoryInfo = unsafe { std::mem::zeroed() };
        check(
            // SAFETY: main-thread live factory and correctly sized output structure.
            unsafe { self.factory.getFactoryInfo(&mut vendor) },
            "getFactoryInfo",
        )?;
        // SAFETY: initialized factory, called on main thread.
        let count = unsafe { self.factory.countClasses() };
        if !(0..=4096).contains(&count) {
            return Err(PluginError("invalid VST 3 class count".into()));
        }
        let factory2 = self.factory.cast::<IPluginFactory2>();
        let mut classes = Vec::new();
        for index in 0..count {
            // SAFETY: plain fixed-array/integer output struct has a valid all-zero representation.
            let mut info: PClassInfo = unsafe { std::mem::zeroed() };
            check(
                // SAFETY: valid enumerated index and writable class-info output.
                unsafe { self.factory.getClassInfo(index, &mut info) },
                "getClassInfo",
            )?;
            if text(&info.category) != "Audio Module Class" {
                continue;
            }
            let mut class = ClassInfo {
                id: info.cid.map(|b| b as u8),
                name: text(&info.name),
                vendor: text(&vendor.vendor),
                category: String::new(),
            };
            if let Some(factory2) = &factory2 {
                // SAFETY: fixed-array/integer class-info output has a valid zero representation.
                let mut info: PClassInfo2 = unsafe { std::mem::zeroed() };
                // SAFETY: factory2 was queried successfully, index and output are valid.
                if unsafe { factory2.getClassInfo2(index, &mut info) } == kResultOk {
                    class.name = text(&info.name);
                    if !text(&info.vendor).is_empty() {
                        class.vendor = text(&info.vendor);
                    }
                    class.category = text(&info.subCategories);
                }
            }
            classes.push(class);
        }
        Ok(classes)
    }
    fn create<I: Interface>(&self, id: [u8; 16]) -> Result<ComPtr<I>, PluginError> {
        let mut object = std::ptr::null_mut();
        check(
            // SAFETY: 16-byte IDs match SDK TUID byte order; factory writes an owned requested interface.
            unsafe {
                self.factory
                    .createInstance(id.as_ptr().cast(), I::IID.as_ptr().cast(), &mut object)
            },
            "createInstance",
        )?;
        // SAFETY: successful factory call transfers one owned reference of the requested interface.
        unsafe { ComPtr::from_raw(object.cast()) }
            .ok_or_else(|| PluginError("null VST 3 instance".into()))
    }
}
struct Shared {
    processor: ComPtr<IAudioProcessor>,
    admitted: AtomicBool,
    busy: AtomicBool,
    active: AtomicBool,
    closed: AtomicBool,
    edits: Arc<Edits>,
    _library: Arc<ModuleLibrary>,
}
struct Admission<'a>(&'a Shared);
impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.0.admitted.store(true, Ordering::SeqCst);
    }
}
impl Shared {
    fn control(&self) -> Admission<'_> {
        self.admitted.store(false, Ordering::SeqCst);
        while self.busy.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        Admission(self)
    }
}
struct Editor {
    view: ComPtr<IPlugView>,
    _frame: ComPtr<IPlugFrame>,
    window: Rc<EditorWindow>,
}
impl Drop for Editor {
    fn drop(&mut self) {
        // SAFETY: attached view, main-thread teardown before window destruction.
        let _ = unsafe { self.view.removed() };
        // SAFETY: detach frame reference to avoid retaining the host window.
        let _ = unsafe { self.view.setFrame(std::ptr::null_mut()) };
    }
}
pub struct Vst3Plugin {
    component: ComPtr<IComponent>,
    controller: Option<ComPtr<IEditController>>,
    separate: bool,
    component_initialized: bool,
    controller_initialized: bool,
    connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    editor: Option<Editor>,
    handler: ComPtr<IComponentHandler>,
    shared: Arc<Shared>,
    info: Info,
    module: Vst3Module,
    closed_editor: bool,
    processor_taken: bool,
    max_frames: usize,
    _main: std::marker::PhantomData<Rc<()>>,
}
impl std::fmt::Debug for Vst3Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3Plugin")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}
impl Vst3Plugin {
    pub fn load(path: &Path, class_id: Option<[u8; 16]>) -> Result<Self, PluginError> {
        let module = Vst3Module::load(path)?;
        let class = module
            .scan()?
            .into_iter()
            .find(|class| class_id.is_none_or(|id| class.id == id))
            .ok_or_else(|| PluginError("VST 3 audio effect class not found".into()))?;
        let component = module.create::<IComponent>(class.id)?;
        let processor = component
            .cast::<IAudioProcessor>()
            .ok_or_else(|| PluginError("missing IAudioProcessor".into()))?;
        let edits = Arc::new(Edits::new());
        let handler = ComWrapper::new(Handler(edits.clone()))
            .to_com_ptr()
            .expect("handler interface");
        let shared = Arc::new(Shared {
            processor,
            admitted: AtomicBool::new(true),
            busy: AtomicBool::new(false),
            active: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            edits,
            _library: module.library.clone(),
        });
        let mut plugin = Self {
            component,
            controller: None,
            separate: false,
            component_initialized: false,
            controller_initialized: false,
            connection: None,
            editor: None,
            handler,
            shared,
            info: Info {
                class,
                input: 0,
                output: 0,
                inputs: 0,
                outputs: 0,
                latency: 0,
            },
            module,
            closed_editor: false,
            processor_taken: false,
            max_frames: 0,
            _main: std::marker::PhantomData,
        };
        check(
            // SAFETY: first initialization on main thread with retained host context.
            unsafe { plugin.component.initialize(plugin.module.host.as_ptr()) },
            "component initialize",
        )?;
        plugin.component_initialized = true;
        plugin.controller = plugin.component.cast::<IEditController>();
        if plugin.controller.is_none() {
            let mut id = [0; 16];
            // SAFETY: initialized component, writable controller class ID output.
            if unsafe { plugin.component.getControllerClassId(&mut id) } == kResultOk {
                let controller = plugin
                    .module
                    .create::<IEditController>(id.map(|b| b as u8))?;
                plugin.separate = true;
                plugin.controller = Some(controller);
                check(
                    // SAFETY: distinct controller initialized once on main thread.
                    unsafe {
                        plugin
                            .controller
                            .as_ref()
                            .expect("controller")
                            .initialize(plugin.module.host.as_ptr())
                    },
                    "controller initialize",
                )?;
                plugin.controller_initialized = true;
            }
        }
        if let Some(controller) = &plugin.controller {
            check(
                // SAFETY: main-thread controller and retained host handler.
                unsafe { controller.setComponentHandler(plugin.handler.as_ptr()) },
                "setComponentHandler",
            )?;
            if plugin.separate
                && let (Some(a), Some(b)) = (
                    plugin.component.cast::<IConnectionPoint>(),
                    controller.cast::<IConnectionPoint>(),
                )
            {
                // SAFETY: both connection endpoints are live and main-thread initialized.
                check(unsafe { a.connect(b.as_ptr()) }, "component connect")?;
                plugin.connection = Some((a, b));
                let (a, b) = plugin.connection.as_ref().expect("connection");
                // SAFETY: both endpoints retained, opposite direction of the connection.
                check(unsafe { b.connect(a.as_ptr()) }, "controller connect")?;
            }
        }
        let input = plugin.arrangements(BusDirections_::kInput)?;
        let output = plugin.arrangements(BusDirections_::kOutput)?;
        if input.is_empty() || output.is_empty() {
            return Err(PluginError("VST 3 missing main audio bus".into()));
        }
        let mut requested = input;
        requested[0] = kSpeakerL | kSpeakerR;
        let mut output = output;
        // SAFETY: inactive component, arrays cover all declared audio buses. Rejection is allowed.
        let _ = unsafe {
            plugin.shared.processor.setBusArrangements(
                requested.as_mut_ptr(),
                requested.len() as i32,
                output.as_mut_ptr(),
                output.len() as i32,
            )
        };
        // Always read back: rejection may retain or select a default arrangement.
        let input = plugin.arrangements(BusDirections_::kInput)?;
        let output = plugin.arrangements(BusDirections_::kOutput)?;
        plugin.info.input = input[0];
        plugin.info.output = output[0];
        plugin.info.inputs = plugin.bus_channels(BusDirections_::kInput)?;
        plugin.info.outputs = plugin.bus_channels(BusDirections_::kOutput)?;
        // SAFETY: initialized audio processor; latency queried on main thread.
        plugin.info.latency = unsafe { plugin.shared.processor.getLatencySamples() };
        Ok(plugin)
    }
    fn arrangements(&self, direction: i32) -> Result<Vec<u64>, PluginError> {
        // SAFETY: main-thread initialized component; audio media and valid direction.
        let count = unsafe { self.component.getBusCount(MediaTypes_::kAudio, direction) };
        if !(1..=32).contains(&count) {
            return Err(PluginError("VST 3 invalid audio bus count".into()));
        }
        let mut buses = vec![0; count as usize];
        for (index, bus) in buses.iter_mut().enumerate() {
            check(
                // SAFETY: queried bus index and writable arrangement output.
                unsafe {
                    self.shared
                        .processor
                        .getBusArrangement(direction, index as i32, bus)
                },
                "getBusArrangement",
            )?;
        }
        Ok(buses)
    }
    fn bus_channels(&self, direction: i32) -> Result<u16, PluginError> {
        // SAFETY: BusInfo contains only fixed arrays and integers, zero is valid.
        let mut bus: BusInfo = unsafe { std::mem::zeroed() };
        check(
            // SAFETY: main bus index exists per arrangements, output struct has correct layout.
            unsafe {
                self.component
                    .getBusInfo(MediaTypes_::kAudio, direction, 0, &mut bus)
            },
            "getBusInfo",
        )?;
        if bus.busType != BusTypes_::kMain || !(0..=32).contains(&bus.channelCount) {
            return Err(PluginError("VST 3 unsupported main bus".into()));
        }
        Ok(bus.channelCount as u16)
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
            || self.processor_taken
            || frames == 0
            || frames > 65536
            || rate < 1000
            || inputs != self.info.inputs
            || outputs != self.info.outputs
        {
            return Err(PluginError("unsupported VST 3 configuration".into()));
        }
        check(
            // SAFETY: inactive processor, querying 32-bit sample support on main thread.
            unsafe {
                self.shared
                    .processor
                    .canProcessSampleSize(SymbolicSampleSizes_::kSample32)
            },
            "canProcessSampleSize",
        )?;
        let mut setup = ProcessSetup {
            processMode: ProcessModes_::kRealtime,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32,
            maxSamplesPerBlock: frames as i32,
            sampleRate: f64::from(rate),
        };
        check(
            // SAFETY: main-thread inactive processor and fully initialized ProcessSetup.
            unsafe { self.shared.processor.setupProcessing(&mut setup) },
            "setupProcessing",
        )?;
        for direction in [BusDirections_::kInput, BusDirections_::kOutput] {
            // SAFETY: component is set up; query audio bus count on the main thread.
            let count = unsafe { self.component.getBusCount(MediaTypes_::kAudio, direction) };
            for index in 0..count {
                check(
                    // SAFETY: main bus enabled, all auxiliary buses disabled; indices were queried.
                    unsafe {
                        self.component.activateBus(
                            MediaTypes_::kAudio,
                            direction,
                            index,
                            u8::from(index == 0),
                        )
                    },
                    "activateBus",
                )?;
            }
        }
        // SAFETY: main-thread latency query after setupProcessing.
        self.info.latency = unsafe { self.shared.processor.getLatencySamples() };
        self.max_frames = frames as usize;
        Ok(())
    }
    pub fn activate(&mut self) -> Result<(), PluginError> {
        if self.max_frames == 0 {
            return Err(PluginError("configure before activate".into()));
        }
        // SAFETY: configured component, lifecycle calls stay on the main thread.
        check(unsafe { self.component.setActive(1) }, "setActive")?;
        self.shared.active.store(true, Ordering::SeqCst);
        Ok(())
    }
    pub fn deactivate(&mut self) {
        let _control = self.shared.control();
        if self.shared.active.swap(false, Ordering::SeqCst) {
            // SAFETY: host joined audio and called setProcessing(false) before deactivation.
            let _ = unsafe { self.component.setActive(0) };
        }
    }
    pub fn take_processor(&mut self) -> Result<Vst3Processor, PluginError> {
        if self.processor_taken || self.max_frames == 0 {
            return Err(PluginError(
                "VST 3 processor already taken or not configured".into(),
            ));
        }
        self.processor_taken = true;
        let changes = Changes::new();
        let pointer = changes.to_com_ptr().expect("parameter changes interface");
        Ok(Vst3Processor {
            shared: self.shared.clone(),
            changes,
            pointer,
            inputs: self.info.inputs as usize,
            outputs: self.info.outputs as usize,
            frames: self.max_frames,
            processing: false,
        })
    }
    pub fn get_state(&mut self) -> Result<(Vec<u8>, Vec<u8>), PluginError> {
        // IComponent documents getState in Processing state: do NOT close admission.
        let component = Stream::new(Vec::new());
        let pointer = component
            .to_com_ptr::<IBStream>()
            .expect("stream interface");
        check(
            // SAFETY: main-thread state call, owned writable stream; SDK permits concurrent process.
            unsafe { self.component.getState(pointer.as_ptr()) },
            "component getState",
        )?;
        let controller = Stream::new(Vec::new());
        if let Some(editor) = &self.controller {
            let pointer = controller
                .to_com_ptr::<IBStream>()
                .expect("stream interface");
            // SAFETY: main-thread controller state call with owned stream.
            let result = unsafe { editor.getState(pointer.as_ptr()) };
            if result != kNotImplemented {
                check(result, "controller getState")?;
            }
        }
        let result = (
            component.0.borrow().snapshot(),
            controller.0.borrow().snapshot(),
        );
        Ok(result)
    }
    pub fn set_state(&mut self, component: &[u8], controller: &[u8]) -> Result<(), PluginError> {
        let _control = self.shared.control();
        let stream = Stream::new(component.to_vec());
        let pointer = stream.to_com_ptr::<IBStream>().expect("stream interface");
        check(
            // SAFETY: main thread, admitted processing excluded, stream owns component bytes.
            unsafe { self.component.setState(pointer.as_ptr()) },
            "component setState",
        )?;
        if let Some(editor) = &self.controller {
            let stream = Stream::new(component.to_vec());
            let pointer = stream.to_com_ptr::<IBStream>().expect("stream interface");
            // SAFETY: main thread; fresh stream rewound to start of component state.
            let result = unsafe { editor.setComponentState(pointer.as_ptr()) };
            if result != kNotImplemented {
                check(result, "setComponentState")?;
            }
            let stream = Stream::new(controller.to_vec());
            let pointer = stream.to_com_ptr::<IBStream>().expect("stream interface");
            // SAFETY: controller UI state restored after the component state, on main thread.
            let result = unsafe { editor.setState(pointer.as_ptr()) };
            if result != kNotImplemented {
                check(result, "controller setState")?;
            }
        }
        Ok(())
    }
    pub fn open_editor(&mut self) -> Result<(), PluginError> {
        if self.editor.is_some() {
            return Ok(());
        }
        let controller = self
            .controller
            .as_ref()
            .ok_or_else(|| PluginError("no VST 3 controller".into()))?;
        // SAFETY: controller queried on main thread with SDK editor view name.
        let pointer = unsafe { controller.createView(ViewType::kEditor) };
        // SAFETY: createView transfers an owned interface reference, nullable for no editor.
        let view = unsafe { ComPtr::from_raw(pointer) }
            .ok_or_else(|| PluginError("no VST 3 editor".into()))?;
        check(
            // SAFETY: live view, SDK Windows platform string, on main thread.
            unsafe { view.isPlatformTypeSupported(kPlatformTypeHWND) },
            "HWND support",
        )?;
        let mut size = ViewRect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: writable view rectangle, queried before attachment.
        check(unsafe { view.getSize(&mut size) }, "getSize")?;
        let width = size
            .right
            .checked_sub(size.left)
            .filter(|v| (1..=8192).contains(v))
            .ok_or_else(|| PluginError("invalid editor width".into()))?;
        let height = size
            .bottom
            .checked_sub(size.top)
            .filter(|v| (1..=8192).contains(v))
            .ok_or_else(|| PluginError("invalid editor height".into()))?;
        let window = Rc::new(
            EditorWindow::open(&self.info.class.name, width, height)
                .map_err(|e| PluginError(e.to_string()))?,
        );
        let frame = ComWrapper::new(Frame(window.clone()))
            .to_com_ptr::<IPlugFrame>()
            .expect("frame interface");
        // SAFETY: retained frame and main-thread view; frame owns the window lifetime.
        check(unsafe { view.setFrame(frame.as_ptr()) }, "setFrame")?;
        // SAFETY: live host-owned HWND, platform type matches support query.
        let result =
            unsafe { view.attached(window.handle_value() as *mut c_void, kPlatformTypeHWND) };
        if result != kResultOk {
            // SAFETY: release installed frame before returning attachment error.
            let _ = unsafe { view.setFrame(std::ptr::null_mut()) };
            check(result, "attached")?;
        }
        self.editor = Some(Editor {
            view,
            _frame: frame,
            window,
        });
        Ok(())
    }
    pub fn idle(&mut self) {
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.window.pump())
        {
            self.editor = None;
            self.closed_editor = true;
        }
    }
    pub fn take_events(&mut self) -> Events {
        let flags = self.shared.edits.flags.swap(0, Ordering::AcqRel);
        let latency = if flags & 2 != 0 {
            // SAFETY: SDK latency notification handled on main thread, live processor.
            self.info.latency = unsafe { self.shared.processor.getLatencySamples() };
            Some(self.info.latency)
        } else {
            None
        };
        Events {
            params: flags & 1 != 0,
            latency,
            closed: std::mem::take(&mut self.closed_editor),
            fault: if flags & 4 != 0 {
                Some("bus layout changed")
            } else if flags & 8 != 0 {
                Some("parameter queue overflow")
            } else if flags & 16 != 0 {
                Some("VST 3 audio processing failed")
            } else {
                None
            },
        }
    }
}
impl Drop for Vst3Plugin {
    fn drop(&mut self) {
        self.editor = None;
        self.deactivate();
        let _control = self.shared.control();
        self.shared.closed.store(true, Ordering::SeqCst);
        if let Some((a, b)) = self.connection.take() {
            // SAFETY: main-thread connected endpoints still retained through disconnect.
            let _ = unsafe { a.disconnect(b.as_ptr()) };
            // SAFETY: disconnect reverse endpoint before termination.
            let _ = unsafe { b.disconnect(a.as_ptr()) };
        }
        if let Some(controller) = &self.controller {
            // SAFETY: release handler before controller termination, on creating thread.
            let _ = unsafe { controller.setComponentHandler(std::ptr::null_mut()) };
            if self.controller_initialized {
                // SAFETY: separately initialized controller terminated once on main thread.
                let _ = unsafe { controller.terminate() };
            }
        }
        if self.component_initialized {
            // SAFETY: component initialized once; audio joined and controller disconnected.
            let _ = unsafe { self.component.terminate() };
        }
    }
}
pub struct Vst3Processor {
    shared: Arc<Shared>,
    changes: ComWrapper<Changes>,
    pointer: ComPtr<IParameterChanges>,
    inputs: usize,
    outputs: usize,
    frames: usize,
    processing: bool,
}
impl std::fmt::Debug for Vst3Processor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vst3Processor").finish_non_exhaustive()
    }
}
// SAFETY: Changes contains Cells but is owned by this unique audio handle; no
// other thread receives its interfaces. All buffers and COM calls stay on audio.
unsafe impl Send for Vst3Processor {}
impl Vst3Processor {
    pub fn process(&mut self, planes: &mut [&mut [f32]], frames: usize) -> bool {
        if frames > self.frames
            || planes.len() < self.inputs.max(self.outputs)
            || planes.iter().any(|p| p.len() < frames)
            || self.shared.closed.load(Ordering::SeqCst)
            || !self.shared.admitted.load(Ordering::SeqCst)
            || self.shared.busy.swap(true, Ordering::SeqCst)
        {
            return false;
        }
        if !self.shared.admitted.load(Ordering::SeqCst)
            || !self.shared.active.load(Ordering::SeqCst)
            || self.shared.closed.load(Ordering::SeqCst)
        {
            self.shared.busy.store(false, Ordering::SeqCst);
            return false;
        }
        if !self.processing {
            // SAFETY: active component, sole audio thread before its first process call.
            let result = unsafe { self.shared.processor.setProcessing(1) };
            if result != kResultOk {
                self.shared.edits.flags.fetch_or(16, Ordering::Release);
                self.shared.busy.store(false, Ordering::SeqCst);
                return false;
            }
            self.processing = true;
        }
        self.changes.drain(&self.shared.edits);
        let mut input_pointers = [std::ptr::null_mut(); 32];
        let mut output_pointers = [std::ptr::null_mut(); 32];
        for (index, plane) in planes.iter_mut().enumerate().take(32) {
            if index < self.inputs {
                input_pointers[index] = plane.as_mut_ptr();
            }
            if index < self.outputs {
                output_pointers[index] = plane.as_mut_ptr();
            }
        }
        let mut input = AudioBusBuffers {
            numChannels: self.inputs as i32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: input_pointers.as_mut_ptr(),
            },
        };
        let mut output = AudioBusBuffers {
            numChannels: self.outputs as i32,
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: output_pointers.as_mut_ptr(),
            },
        };
        let mut data = ProcessData {
            processMode: ProcessModes_::kRealtime,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32,
            numSamples: frames as i32,
            numInputs: 1,
            numOutputs: 1,
            inputs: &mut input,
            outputs: &mut output,
            inputParameterChanges: self.pointer.as_ptr(),
            outputParameterChanges: std::ptr::null_mut(),
            inputEvents: std::ptr::null_mut(),
            outputEvents: std::ptr::null_mut(),
            processContext: std::ptr::null_mut(),
        };
        // SAFETY: exclusive audio admission, configured active processor, bounded live planar buffers.
        let result = unsafe { self.shared.processor.process(&mut data) };
        if result != kResultOk {
            self.shared.edits.flags.fetch_or(16, Ordering::Release);
        }
        self.shared.busy.store(false, Ordering::SeqCst);
        result == kResultOk
    }
    pub fn stop(&mut self) {
        if !self.shared.admitted.load(Ordering::SeqCst)
            || self.shared.busy.swap(true, Ordering::SeqCst)
        {
            return;
        }
        if self.processing
            && self.shared.admitted.load(Ordering::SeqCst)
            && !self.shared.closed.load(Ordering::SeqCst)
        {
            // SAFETY: sole audio thread holds admission after its last process, before teardown.
            let _ = unsafe { self.shared.processor.setProcessing(0) };
            self.processing = false;
        }
        self.shared.busy.store(false, Ordering::SeqCst);
    }
}
