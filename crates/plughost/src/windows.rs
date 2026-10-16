//! Windows control transport. Blocking pipe I/O is confined to reader/writer
//! threads; deadlines and process termination never wait for pipe writes.
use crate::{
    Plugin, PluginError, PluginEvents, PluginInfo, Processor,
    host::{Args, AudioBuffer},
    layout::{default_arrangement, main_bus, vst2_arrangement},
    watch::ControlWatch,
};
use plughost_proto::{
    Action, Bus, ClassInfo, CodecError, DecodedFrame, Frame, Message, PluginFormat, RequestKind,
    read_frame, write_frame,
};
use plughost_sys::{
    Event, SharedRegion,
    plugin::{Vst2Plugin, Vst2Processor},
    runtime::{self, ChildProcess, Mmcss, Pipe},
};
use std::{
    ffi::OsString,
    io::{Read, Write},
    os::windows::io::AsRawHandle,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};
use streamring::{Consumer, Geometry, Producer, Publisher, Signal, State, Wait, Waited};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct StartError(pub String);
fn error(error: impl std::fmt::Display) -> StartError {
    StartError(error.to_string())
}
fn plugin_error(error: impl std::fmt::Display) -> PluginError {
    PluginError(error.to_string())
}

struct Transport {
    pending: std::cell::RefCell<std::collections::VecDeque<Frame>>,
    send: Sender<Frame>,
    receive: Receiver<Result<Option<DecodedFrame>, CodecError>>,
}
impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transport").finish_non_exhaustive()
    }
}
impl Transport {
    fn new(mut input: impl Read + Send + 'static, mut output: impl Write + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let (frames, received) = mpsc::channel();
        thread::spawn(move || {
            loop {
                let result = read_frame(&mut input);
                let end = !matches!(result, Ok(Some(_)));
                if frames.send(result).is_err() || end {
                    break;
                }
            }
        });
        thread::spawn(move || {
            while let Ok(frame) = rx.recv() {
                if write_frame(&mut output, &frame).is_err() {
                    break;
                }
            }
        });
        Self {
            pending: Default::default(),
            send: tx,
            receive: received,
        }
    }
    fn send(&self, request: u32, message: Message) -> Result<(), StartError> {
        self.send
            .send(Frame {
                flags: 0,
                request,
                message,
            })
            .map_err(error)
    }
    fn response(&self, request: u32, timeout: Duration) -> Result<Frame, StartError> {
        let deadline = Instant::now() + timeout;
        loop {
            let frame = self
                .receive
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(error)?
                .map_err(error)?;
            let Some(frame) = frame else {
                return Err(StartError("host closed control pipe".into()));
            };
            if let DecodedFrame::Known(frame) = frame {
                if let Message::Fault { code, text } = &frame.message {
                    return Err(StartError(format!("host fault {code}: {text}")));
                }
                if frame.request == request {
                    return Ok(frame);
                }
                self.pending.borrow_mut().push_back(frame);
            }
        }
    }
}

#[derive(Debug)]
struct Vst2 {
    inner: Vst2Plugin,
    processor: Option<Vst2ProcessorAdapter>,
    info: PluginInfo,
}
impl Vst2 {
    fn load(path: &Path) -> Result<Self, PluginError> {
        let inner = Vst2Plugin::load(path).map_err(plugin_error)?;
        let info = inner.info();
        let arrangement = |types: Option<Vec<i32>>, count| {
            types
                .map_or_else(
                    || default_arrangement(count),
                    |types| vst2_arrangement(&types),
                )
                .map_err(plugin_error)
        };
        let info = PluginInfo {
            name: info.name,
            vendor: info.vendor,
            latency_frames: info.latency,
            inputs: vec![Bus {
                channels: info.inputs,
                arrangement: arrangement(info.input_types, info.inputs)?,
            }],
            outputs: vec![Bus {
                channels: info.outputs,
                arrangement: arrangement(info.output_types, info.outputs)?,
            }],
        };
        let processor = Vst2ProcessorAdapter(inner.processor());
        Ok(Self {
            inner,
            processor: Some(processor),
            info,
        })
    }
}
impl Processor for Vst2ProcessorAdapter {
    fn process(&mut self, planes: &mut [&mut [f32]], frames: usize) -> bool {
        self.0.process(planes, frames)
    }
}
#[derive(Debug)]
struct Vst2ProcessorAdapter(Vst2Processor);
impl Plugin for Vst2 {
    fn info(&self) -> PluginInfo {
        self.info.clone()
    }
    fn configure(
        &mut self,
        rate: u32,
        frames: u32,
        inputs: u16,
        outputs: u16,
    ) -> Result<(), PluginError> {
        self.inner
            .configure(rate, frames, inputs, outputs)
            .map_err(plugin_error)?;
        self.processor = Some(Vst2ProcessorAdapter(self.inner.processor()));
        Ok(())
    }
    fn activate(&mut self) -> Result<(), PluginError> {
        self.inner.activate();
        Ok(())
    }
    fn deactivate(&mut self) {
        self.inner.deactivate();
    }
    fn take_processor(&mut self) -> Result<Box<dyn Processor>, PluginError> {
        self.processor
            .take()
            .map(|processor| Box::new(processor) as Box<dyn Processor>)
            .ok_or_else(|| PluginError("processor already taken".into()))
    }
    fn get_state(&mut self) -> Result<Vec<u8>, PluginError> {
        self.inner.get_state().map_err(plugin_error)
    }
    fn set_state(&mut self, blob: &[u8]) -> Result<(), PluginError> {
        self.inner.set_state(blob).map_err(plugin_error)
    }
    fn open_editor(&mut self) -> Result<(), PluginError> {
        self.inner.open_editor().map_err(plugin_error)
    }
    fn idle(&mut self) {
        self.inner.idle();
    }
    fn take_events(&mut self) -> PluginEvents {
        let events = self.inner.take_events();
        self.info.latency_frames = self.inner.info().latency;
        PluginEvents {
            params_changed: events.params,
            latency_changed: events.latency.then_some(self.info.latency_frames),
            editor_closed: events.closed,
            fault: None,
        }
    }
}

pub fn host_main(args: Args) -> Result<(), StartError> {
    runtime::suppress_error_dialogs();
    let input = runtime::inherited_file(args.control_in).map_err(error)?;
    let mut output = runtime::inherited_file(args.control_out).map_err(error)?;
    // Main-thread writes are acceptable in the disposable host. The app's reader
    // drains continuously; app-side writes use a dedicated thread.
    write_frame(
        &mut output,
        &Frame {
            flags: 0,
            request: 0,
            message: Message::hello(),
        },
    )
    .map_err(error)?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut input = input;
        loop {
            let next = read_frame(&mut input);
            let end = !matches!(next, Ok(Some(_)));
            if tx.send(next).is_err() || end {
                break;
            }
        }
    });
    let parent = Arc::new(runtime::duplicate_value(args.parent, false).map_err(error)?);
    let region = Arc::new(SharedRegion::open(args.region, args.region_bytes).map_err(error)?);
    let work = Arc::new(
        Event::open(args.work)
            .map_err(error)?
            .with_peer(parent.try_clone().map_err(error)?),
    );
    let done = Arc::new(Event::open(args.done).map_err(error)?);
    let ready = Arc::new(Event::open(args.ready).map_err(error)?);
    let stop = Arc::new(AtomicBool::new(false));
    let mut audio = None;
    let mut plugin: Option<Box<dyn Plugin>> = None;
    let mut configured = false;
    let mut failed = false;
    // Always join audio before lifecycle teardown, including malformed frames
    // and broken control pipes (early ? returns must not detach the audio thread).
    let result = (|| -> Result<(), StartError> {
        loop {
            if runtime::process_gone(&parent) {
                break;
            }
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(Ok(Some(DecodedFrame::Known(frame)))) => {
                    let request = frame.request;
                    let shutdown = matches!(frame.message, Message::Shutdown);
                    let reply: Result<Message, StartError> = (|| match frame.message {
                        Message::Scan { path } => {
                            if Path::new(&path)
                                .extension()
                                .is_some_and(|ext| ext.eq_ignore_ascii_case("vst3"))
                            {
                                let module = plughost_sys::vst3::Vst3Module::load(Path::new(&path))
                                    .map_err(error)?;
                                let entries = module
                                    .scan()
                                    .map_err(error)?
                                    .into_iter()
                                    .map(|class| ClassInfo {
                                        id: class.id,
                                        name: class.name,
                                        vendor: class.vendor,
                                        category: class.category,
                                    })
                                    .collect();
                                return Ok(Message::Classes { entries });
                            }
                            let scanned = Vst2::load(Path::new(&path)).map_err(error)?;
                            let info = scanned.info();
                            Ok(Message::Classes {
                                entries: vec![ClassInfo {
                                    id: [0; 16],
                                    name: info.name,
                                    vendor: info.vendor,
                                    category: "Audio Module".into(),
                                }],
                            })
                        }
                        Message::Load {
                            path,
                            format,
                            class_id,
                        } if plugin.is_none() => {
                            let loaded: Box<dyn Plugin> = match format {
                                PluginFormat::Vst2 => {
                                    Box::new(Vst2::load(Path::new(&path)).map_err(error)?)
                                }
                                PluginFormat::Vst3 => Box::new(
                                    crate::vst3::Vst3::load(Path::new(&path), class_id)
                                        .map_err(error)?,
                                ),
                            };
                            let info = loaded.info();
                            plugin = Some(loaded);
                            Ok(Message::Loaded {
                                name: info.name,
                                vendor: info.vendor,
                                latency_frames: info.latency_frames,
                                inputs: info.inputs,
                                outputs: info.outputs,
                            })
                        }
                        Message::Configure {
                            rate,
                            max_frames,
                            in_channels,
                            out_channels,
                        } if !configured => {
                            let plugin = plugin
                                .as_mut()
                                .ok_or_else(|| StartError("Load required".into()))?;
                            let previous_latency = plugin.info().latency_frames;
                            plugin
                                .configure(rate, max_frames, in_channels, out_channels)
                                .map_err(error)?;
                            let latency = plugin.info().latency_frames;
                            if latency != previous_latency {
                                write_frame(
                                    &mut output,
                                    &Frame {
                                        flags: 0,
                                        request: 0,
                                        message: Message::LatencyChanged { frames: latency },
                                    },
                                )
                                .map_err(error)?;
                            }
                            let geometry = Geometry {
                                lanes: 1,
                                input_channels: in_channels.into(),
                                output_channels: out_channels.into(),
                                max_frames,
                                sample_rate: rate,
                                latency_bound_frames: region.latency_bound(),
                            };
                            region
                                .region_for(geometry)
                                .map_err(|e| error(format!("{e:?}")))?;
                            let (validated, validation) = mpsc::sync_channel(1);
                            let mut scratch =
                                AudioBuffer::new(geometry).map_err(|e| error(format!("{e:?}")))?;
                            let mut processor = plugin.take_processor().map_err(error)?;
                            let region = region.clone();
                            let work = work.clone();
                            let done = done.clone();
                            let ready = ready.clone();
                            let stop = stop.clone();
                            audio = Some(thread::spawn(move || {
                                let _mmcss = Mmcss::pro_audio();
                                let view = region.region();
                                let mut consumer =
                                    Consumer::new(view, &*ready, [Some(&*done), None]);
                                let result = consumer.validate_and_ready(std::process::id());
                                let valid = result == Ok(geometry);
                                let _ = validated.send(valid);
                                if !valid {
                                    consumer.fault(1);
                                    return;
                                }
                                while !stop.load(Ordering::Acquire) {
                                    let wake = work.wait(Duration::from_millis(100));
                                    consumer.beat();
                                    if wake == Waited::PeerGone
                                        || view.state() != Some(State::Ready)
                                    {
                                        break;
                                    }
                                    scratch.drain(&consumer, processor.as_mut());
                                }
                                processor.stop();
                            }));
                            if !validation
                                .recv_timeout(Duration::from_secs(5))
                                .map_err(error)?
                            {
                                return Err(StartError("ring validation failed".into()));
                            }
                            configured = true;
                            Ok(Message::Ack)
                        }
                        Message::Activate if configured => {
                            plugin
                                .as_mut()
                                .expect("configured plugin")
                                .activate()
                                .map_err(error)?;
                            Ok(Message::Ack)
                        }
                        Message::SetState { blob } => {
                            plugin
                                .as_mut()
                                .ok_or_else(|| StartError("Load required".into()))?
                                .set_state(&blob)
                                .map_err(error)?;
                            Ok(Message::Ack)
                        }
                        Message::GetState => Ok(Message::State {
                            blob: plugin
                                .as_mut()
                                .ok_or_else(|| StartError("Load required".into()))?
                                .get_state()
                                .map_err(error)?,
                        }),
                        Message::OpenEditor => {
                            plugin
                                .as_mut()
                                .ok_or_else(|| StartError("Load required".into()))?
                                .open_editor()
                                .map_err(error)?;
                            Ok(Message::Ack)
                        }
                        Message::Shutdown => Ok(Message::Ack),
                        _ => Err(StartError("invalid host request".into())),
                    })();
                    let scan = matches!(&reply, Ok(Message::Classes { .. }));
                    let fault = reply.is_err();
                    let message = reply.unwrap_or_else(|error| Message::Fault {
                        code: 1,
                        text: error.to_string(),
                    });
                    write_frame(
                        &mut output,
                        &Frame {
                            flags: 0,
                            request,
                            message,
                        },
                    )
                    .map_err(error)?;
                    if shutdown || scan || fault {
                        failed = fault;
                        break;
                    }
                }
                Ok(Ok(Some(DecodedFrame::Unknown(_)))) => {}
                Ok(Ok(None)) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(Err(error_value)) => {
                    let _ = write_frame(
                        &mut output,
                        &Frame {
                            flags: 0,
                            request: 0,
                            message: Message::Fault {
                                code: 3,
                                text: error_value.to_string(),
                            },
                        },
                    );
                    return Err(error(error_value));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if let Some(plugin) = plugin.as_mut() {
                plugin.idle();
                let events = plugin.take_events();
                let mut send = |message| {
                    write_frame(
                        &mut output,
                        &Frame {
                            flags: 0,
                            request: 0,
                            message,
                        },
                    )
                    .map_err(error)
                };
                if let Some(text) = events.fault {
                    send(Message::Fault {
                        code: 4,
                        text: text.into(),
                    })?;
                    failed = true;
                    break;
                }
                if events.params_changed {
                    send(Message::ParamsChanged)?;
                }
                if let Some(frames) = events.latency_changed {
                    send(Message::LatencyChanged { frames })?;
                }
                if events.editor_closed {
                    send(Message::EditorClosed)?;
                }
            }
        }
        if failed {
            Err(StartError("host request failed".into()))
        } else {
            Ok(())
        }
    })();
    stop.store(true, Ordering::Release);
    work.signal();
    if let Some(audio) = audio {
        let _ = audio.join();
    }
    if let Some(plugin) = plugin.as_mut() {
        plugin.deactivate();
    }
    result
}

/// A host that loaded, configured and activated its plugin, with the ring it
/// marked Ready.
///
/// `publisher` is the ring's real-time half; it works on
/// `region.region()` (an `Arc<SharedRegion>` is a [`streamring::RegionMemory`])
/// and signals `work` after each publish, so the engine can own all three
/// (`RingUpmixer`) without borrowing from this value. The [`Producer`] that
/// announced the header is gone by the time this is returned.
#[derive(Debug)]
pub struct Started {
    pub region: Arc<SharedRegion>,
    pub work: Arc<Event>,
    pub done: Arc<Event>,
    pub ready: Arc<Event>,
    pub publisher: Publisher,
    pub geometry: Geometry,
    pub input_wave_bits: Vec<u32>,
    pub output_wave_bits: Vec<u32>,
    pub latency_frames: u32,
    pub control: PluginControl,
}
#[derive(Debug)]
pub enum ControlEvent {
    Message(Frame),
    Action(Action),
    Exited(u32),
    NeedsRespawn,
}
#[derive(Debug)]
pub struct PluginControl {
    process: ChildProcess,
    transport: Transport,
    region: Arc<SharedRegion>,
    work: Arc<Event>,
    watch: ControlWatch,
    next_request: u32,
    origin: Instant,
    clock_base: u64,
    exit_seen: bool,
    pub needs_respawn: bool,
}
impl PluginControl {
    fn now(&self) -> u64 {
        self.clock_base
            .saturating_add(self.origin.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
    }
    fn request(&mut self, message: Message, kind: RequestKind) -> Result<u32, StartError> {
        let id = self.next_request;
        self.next_request = self
            .next_request
            .checked_add(1)
            .ok_or_else(|| StartError("request number exhausted".into()))?;
        self.watch.request(self.now(), id, kind);
        self.transport.send(id, message)?;
        Ok(id)
    }
    pub fn get_state(&mut self) -> Result<u32, StartError> {
        self.request(Message::GetState, RequestKind::StateSave)
    }
    pub fn set_state(&mut self, blob: &[u8]) -> Result<u32, StartError> {
        self.request(
            Message::SetState {
                blob: blob.to_vec(),
            },
            RequestKind::StateSave,
        )
    }
    pub fn open_editor(&mut self) -> Result<u32, StartError> {
        self.request(Message::OpenEditor, RequestKind::Editor)
    }
    pub fn latency(&self) -> u32 {
        self.watch.latency()
    }
    pub fn elapsed_ms(&self) -> u64 {
        self.now()
    }
    #[doc(hidden)]
    pub fn close_editor_for_test(&self) -> Result<(), StartError> {
        plughost_sys::editor::close_process_editor(self.pid()).map_err(error)
    }
    pub fn pid(&self) -> u32 {
        self.process.pid()
    }
    pub fn exit_code(&self) -> Result<Option<u32>, StartError> {
        self.process.exit_code().map_err(error)
    }
    pub fn poll(&mut self, now_ms: u64) -> Vec<ControlEvent> {
        let mut events = Vec::new();
        loop {
            let pending = self.transport.pending.borrow_mut().pop_front();
            let frame = if let Some(frame) = pending {
                Ok(Some(DecodedFrame::Known(frame)))
            } else if let Ok(frame) = self.transport.receive.try_recv() {
                frame
            } else {
                break;
            };
            match frame {
                Ok(Some(DecodedFrame::Known(frame))) => {
                    let action = self.watch.received(now_ms, &frame);
                    if action == Action::SaveStateNow {
                        let _ = self.get_state();
                    }
                    if action != Action::None {
                        events.push(ControlEvent::Action(action));
                    }
                    events.push(ControlEvent::Message(frame));
                }
                Ok(Some(DecodedFrame::Unknown(_))) => {}
                _ => {
                    if !matches!(
                        self.watch.state(),
                        plughost_proto::WatchState::Stopping | plughost_proto::WatchState::Stopped
                    ) {
                        let _ = self.process.terminate(3);
                    }
                }
            }
        }
        let exit = if self.exit_seen {
            None
        } else {
            self.process.exit_code().ok().flatten()
        };
        if let Some(code) = exit {
            self.exit_seen = true;
            events.push(ControlEvent::Exited(code));
        }
        for action in self.watch.poll(
            now_ms,
            self.region.region().heartbeat(),
            exit.map(|code| code as i32),
        ) {
            match action {
                Action::Kill { .. } | Action::Disable { .. } => {
                    let _ = self.process.terminate(3);
                }
                Action::SaveStateNow => {
                    let _ = self.get_state();
                }
                Action::Spawn => {
                    self.needs_respawn = true;
                    events.push(ControlEvent::NeedsRespawn);
                }
                _ => {}
            }
            events.push(ControlEvent::Action(action));
        }
        events
    }
    /// Preserve this value across respawns so the three-failure window and the
    /// two saved states belong to the plugin session, not one child process.
    pub fn take_watch(&mut self) -> ControlWatch {
        let now = self.now();
        std::mem::replace(&mut self.watch, ControlWatch::new(now))
    }
    /// `now_ms` uses the same caller-owned clock as subsequent `poll` calls.
    pub fn restore_watch(&mut self, mut watch: ControlWatch, now_ms: u64) {
        let hello = Frame {
            flags: 0,
            request: 0,
            message: Message::hello(),
        };
        watch.received(now_ms, &hello);
        watch.received(
            now_ms,
            &Frame {
                flags: 0,
                request: 0,
                message: Message::Loaded {
                    name: String::new(),
                    vendor: String::new(),
                    latency_frames: self.latency(),
                    inputs: vec![],
                    outputs: vec![],
                },
            },
        );
        watch.activated(now_ms);
        self.origin = Instant::now();
        self.clock_base = now_ms;
        self.watch = watch;
    }
    pub fn shutdown(&mut self) -> Result<(), StartError> {
        self.get_state()?;
        self.watch.shutdown(self.now());
        self.transport.send(self.next_request, Message::Shutdown)?;
        self.work.signal();
        Ok(())
    }
}

#[derive(Debug)]
pub struct PluginProcess;
impl PluginProcess {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        exe: &Path,
        plugin: &Path,
        format: PluginFormat,
        class: Option<[u8; 16]>,
        rate: u32,
        state: Option<&[u8]>,
    ) -> Result<Started, StartError> {
        let origin = Instant::now();
        let maximum = Geometry {
            lanes: 1,
            input_channels: 32,
            output_channels: 32,
            max_frames: rate / 100,
            sample_rate: rate,
            latency_bound_frames: rate
                .checked_mul(2)
                .ok_or_else(|| StartError("invalid rate".into()))?,
        };
        let (process, transport, region, work, done, ready) = spawn(exe, maximum)?;
        let mut watch = ControlWatch::new(0);
        let hello = transport.response(0, Duration::from_secs(5))?;
        if hello.message != Message::hello() {
            return Err(StartError("expected Hello".into()));
        }
        watch.received(0, &hello);
        transport.send(
            1,
            Message::Load {
                path: plugin.to_string_lossy().into_owned(),
                format,
                class_id: class,
            },
        )?;
        let loaded = transport.response(1, Duration::from_secs(60))?;
        watch.received(origin.elapsed().as_millis() as u64, &loaded);
        let Message::Loaded {
            outputs,
            inputs,
            latency_frames,
            ..
        } = loaded.message
        else {
            return Err(StartError("expected Loaded".into()));
        };
        let input_wave_bits = main_bus(&inputs, "input").map_err(error)?;
        let output_wave_bits = main_bus(&outputs, "output").map_err(error)?;
        if latency_frames > maximum.latency_bound_frames {
            return Err(StartError("unsupported plugin latency".into()));
        }
        let in_channels = inputs[0].channels;
        let geometry = Geometry {
            input_channels: in_channels.into(),
            output_channels: outputs[0].channels.into(),
            ..maximum
        };
        // The producer borrows the mapping and the events only while it
        // announces the header and waits for the host's Ready; its real-time
        // half, the publisher, owns nothing and leaves with `Started`.
        let publisher = {
            let view = region
                .region_for(geometry)
                .map_err(|e| error(format!("{e:?}")))?;
            let mut producer = Producer::new(view, [Some(&*work), None], &*ready);
            producer
                .announce(geometry, std::process::id())
                .map_err(|e| error(format!("{e:?}")))?;
            transport.send(
                2,
                Message::Configure {
                    rate,
                    max_frames: geometry.max_frames,
                    in_channels,
                    out_channels: outputs[0].channels,
                },
            )?;
            expect_ack(transport.response(2, Duration::from_secs(10))?)?;
            if let Some(blob) = state {
                transport.send(
                    3,
                    Message::SetState {
                        blob: blob.to_vec(),
                    },
                )?;
                expect_ack(transport.response(3, Duration::from_secs(10))?)?;
            }
            transport.send(4, Message::Activate)?;
            expect_ack(transport.response(4, Duration::from_secs(10))?)?;
            if producer.wait_ready(Duration::from_secs(10)) != Waited::Signalled {
                return Err(StartError("ring readiness timeout".into()));
            }
            producer
                .publisher()
                .ok_or_else(|| StartError("ring was not announced".into()))?
        };
        watch.activated(origin.elapsed().as_millis() as u64);
        let mut control = PluginControl {
            process,
            transport,
            region: region.clone(),
            work: work.clone(),
            watch,
            next_request: 5,
            origin,
            clock_base: 0,
            exit_seen: false,
            needs_respawn: false,
        };
        let saved = control.get_state()?;
        let state_reply = control.transport.response(saved, Duration::from_secs(10))?;
        control.watch.received(control.now(), &state_reply);
        for frame in control.transport.pending.borrow_mut().drain(..) {
            control.watch.received(control.now(), &frame);
        }
        let latency_frames = control.latency();
        if latency_frames > maximum.latency_bound_frames {
            return Err(StartError("unsupported configured plugin latency".into()));
        }
        Ok(Started {
            region,
            work,
            done,
            ready,
            publisher,
            geometry,
            input_wave_bits,
            output_wave_bits,
            latency_frames,
            control,
        })
    }
    pub fn scan(
        exe: &Path,
        path: &Path,
        format: PluginFormat,
    ) -> Result<Vec<ClassInfo>, StartError> {
        // Frozen Scan wire message has no format field; VST 3 files/bundles use .vst3.
        if (format == PluginFormat::Vst3)
            != path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("vst3"))
        {
            return Err(StartError(
                "scan format does not match plugin extension".into(),
            ));
        }
        let geometry = Geometry {
            lanes: 1,
            input_channels: 2,
            output_channels: 32,
            max_frames: 480,
            sample_rate: 48000,
            latency_bound_frames: 96000,
        };
        let (process, transport, ..) = spawn(exe, geometry)?;
        if transport.response(0, Duration::from_secs(5))?.message != Message::hello() {
            return Err(StartError("expected Hello".into()));
        }
        transport.send(
            1,
            Message::Scan {
                path: path.to_string_lossy().into_owned(),
            },
        )?;
        let reply = transport.response(1, Duration::from_secs(60))?;
        let Message::Classes { entries } = reply.message else {
            return Err(StartError("expected Classes".into()));
        };
        if !process.wait(Duration::from_secs(2)) {
            return Err(StartError("scan did not exit".into()));
        }
        Ok(entries)
    }
}
fn expect_ack(frame: Frame) -> Result<(), StartError> {
    if frame.message == Message::Ack {
        Ok(())
    } else {
        Err(StartError("expected Ack".into()))
    }
}
type Spawned = (
    ChildProcess,
    Transport,
    Arc<SharedRegion>,
    Arc<Event>,
    Arc<Event>,
    Arc<Event>,
);
fn spawn(exe: &Path, geometry: Geometry) -> Result<Spawned, StartError> {
    let region = Arc::new(
        SharedRegion::create(
            geometry
                .total_bytes()
                .map_err(|e| error(format!("{e:?}")))?,
        )
        .map_err(error)?,
    );
    let input = Pipe::create(false).map_err(error)?;
    let output = Pipe::create(true).map_err(error)?;
    let work = Arc::new(Event::create().map_err(error)?);
    let done = Arc::new(Event::create().map_err(error)?);
    let ready = Arc::new(Event::create().map_err(error)?);
    let parent = runtime::parent_handle().map_err(error)?;
    let values = [
        input.child_value(),
        output.child_value(),
        region.handle_value(),
        work.handle_value(),
        done.handle_value(),
        ready.handle_value(),
        parent.as_raw_handle() as usize,
    ];
    let names = [
        "--control-in",
        "--control-out",
        "--region",
        "--work",
        "--done",
        "--ready",
        "--parent",
    ];
    let mut args = Vec::<OsString>::new();
    for (name, value) in names.into_iter().zip(values) {
        args.push(name.into());
        args.push(value.to_string().into());
    }
    args.push("--region-bytes".into());
    args.push(region.bytes().to_string().into());
    let process = runtime::spawn_handles(exe, &args, &values).map_err(error)?;
    drop(input.child);
    drop(output.child);
    drop(parent);
    let transport = Transport::new(output.parent, input.parent);
    Ok((process, transport, region, work, done, ready))
}
