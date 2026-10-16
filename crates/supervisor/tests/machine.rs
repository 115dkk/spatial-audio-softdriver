#![forbid(unsafe_code)]

//! The supervisor state machine against a scripted host and engine (ADR 0013):
//! every wizard step, the output selector, and taking and giving back the
//! default roles, on Linux.

mod common;

use std::cell::{RefCell, RefMut};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use engine::adapters::NullOpener;
use engine::{LiveChange, MatrixUpmixer, OutputProfile, UpmixMonitor, UpmixStatus, Upmixer};
use ssr_types::app::{Intent, Language, NextAction, OutputChoice, Phase, SetupStep, UserError};
use ssr_types::ports::PolicyState;
use ssr_types::{
    Arch, DefaultRoles, DeviceId, DeviceState, DirectionView, DriverState, EngineFault,
    EngineStatus, Host, HostError, OutputDevice, OutputLayout, Platform, Role, SecureBoot,
    SetupOutcome, SetupTask,
};
use supervisor::machine::Config;
use supervisor::restore_point::{self, RestorePoint, RestorePointError};
use supervisor::{
    EngineHandle, Launcher, PluginClass, PluginEvent, PluginHost, PluginHostError, PluginInstance,
    PluginSetting, Session, SessionError, SessionNotice, Settings, Supervisor,
};

const OURS: &str = "{ours-main}";
const DAC: &str = "{dac}";
const DAC_NAME: &str = "Speakers (USB DAC)";
const REALTEK: &str = "{realtek}";
const HEADSET: &str = "{headset}";

/// Everything the fakes share with the test: the host's state the test sets,
/// the engine's script, and one log of every mutating call in call order.
struct World {
    platform: Platform,
    driver: DriverState,
    outputs: Vec<OutputDevice>,
    roles: DefaultRoles,
    restore_point: Option<String>,
    /// `run_setup` results in order; an empty queue succeeds.
    setup_results: VecDeque<Result<SetupOutcome, HostError>>,
    failing_role: Option<Role>,
    launch_fails: bool,
    /// What each engine `poll` returns in order; an empty queue is quiet.
    engine_polls: VecDeque<Result<Vec<SessionNotice>, SessionError>>,
    /// Every profile a launch received, described, in order.
    launched: Vec<String>,
    /// Every change the running engine took, described.
    applied: Vec<String>,
    /// The engine refuses the next changes with these errors, in order.
    apply_errors: VecDeque<SessionError>,
    /// Result returned by the layout probe; defaults to accepting the layout.
    layout_support: Result<bool, HostError>,
    /// The directions the running engine reports in its status.
    engine_directions: Vec<DirectionView>,
    /// The recoverable fault the running engine reports in its status.
    engine_fault: Option<EngineFault>,
    /// The status readers of the upmix stages the running engine took.
    stages: [Option<UpmixMonitor>; 3],
    /// Upmix statuses the running engine reports instead of its stages'.
    upmix_status: [Option<UpmixStatus>; 3],
    plugins: PluginScript,
    calls: Vec<String>,
}

/// What the fake plugin host does, and the log of what it was asked.
#[derive(Default)]
struct PluginScript {
    /// What the next scans return, in order; an empty queue finds one class.
    scans: VecDeque<Result<Vec<PluginClass>, PluginHostError>>,
    /// The next starts fail with these, in order.
    start_errors: VecDeque<PluginHostError>,
    /// Hosts come up with as many outputs as inputs (5.1 to 5.1).
    not_upmixer: bool,
    /// Hosts come up only when told to: a start (or restart) gives no
    /// upmixer until a scripted `Running` is polled.
    slow: bool,
    /// What each endpoint's host reports on its next poll.
    events: [VecDeque<PluginEvent>; 3],
    /// The plugin latency of hosts that start from now on.
    latency: usize,
    /// Every scan, start, restart, editor, shutdown, exit and forgotten
    /// state, in order.
    log: Vec<String>,
}

#[derive(Clone)]
struct Shared(Rc<RefCell<World>>);

impl Shared {
    fn new(world: World) -> Self {
        Self(Rc::new(RefCell::new(world)))
    }

    fn get(&self) -> RefMut<'_, World> {
        self.0.borrow_mut()
    }

    fn take_calls(&self) -> Vec<String> {
        std::mem::take(&mut self.get().calls)
    }

    fn output(&self, id: &str) -> RefMut<'_, OutputDevice> {
        RefMut::map(self.get(), |world| {
            world
                .outputs
                .iter_mut()
                .find(|output| output.id.0 == id)
                .expect("the output is listed")
        })
    }
}

struct FakeHost(Shared);

impl Host for FakeHost {
    fn platform(&mut self) -> Platform {
        self.0.get().platform.clone()
    }

    fn driver(&mut self) -> DriverState {
        self.0.get().driver.clone()
    }

    fn outputs(&mut self) -> Vec<OutputDevice> {
        self.0.get().outputs.clone()
    }

    fn default_roles(&mut self) -> DefaultRoles {
        self.0.get().roles.clone()
    }

    fn set_default_role(&mut self, role: Role, device: &DeviceId) -> Result<(), HostError> {
        let mut world = self.0.get();
        world.calls.push(format!("set {role:?} {}", device.0));
        if world.failing_role == Some(role) {
            return Err(HostError::Failed("scripted".into()));
        }
        world.roles.set(role, Some(device.clone()));
        Ok(())
    }

    fn supports_layout(
        &mut self,
        output: &DeviceId,
        layout: OutputLayout,
    ) -> Result<bool, HostError> {
        let mut world = self.0.get();
        world.calls.push(format!(
            "supports {} {}ch {:#x}",
            output.0, layout.channels, layout.channel_mask
        ));
        world.layout_support.clone()
    }

    /// Plays `ssr-setup`: a successful run leaves the driver installed, or
    /// waiting for a reboot. Unscripted runs succeed with no message.
    fn run_setup(&mut self, task: &SetupTask) -> Result<SetupOutcome, HostError> {
        let mut world = self.0.get();
        world.calls.push(format!("setup {task:?}"));
        let result = world.setup_results.pop_front().unwrap_or(Ok(SetupOutcome {
            reboot_required: false,
            message: String::new(),
        }));
        if let Ok(outcome) = &result {
            world.driver = if outcome.reboot_required {
                DriverState::RebootRequired
            } else {
                fine_driver()
            };
        }
        result
    }

    fn write_restore_point(&mut self, json: Option<&str>) -> Result<(), HostError> {
        let mut world = self.0.get();
        world.calls.push(
            match json {
                Some(_) => "write restore point",
                None => "remove restore point",
            }
            .into(),
        );
        world.restore_point = json.map(str::to_owned);
        Ok(())
    }
}

struct FakeLauncher(Shared);

impl Launcher for FakeLauncher {
    fn launch(
        &mut self,
        output: &OutputDevice,
        profile: &OutputProfile,
    ) -> Result<Box<dyn EngineHandle>, SessionError> {
        let mut world = self.0.get();
        world.calls.push(format!("launch {}", output.id.0));
        world.launched.push(describe(profile));
        if world.launch_fails {
            return Err(SessionError::Recorder("scripted failure".into()));
        }
        Ok(Box::new(FakeEngine(self.0.clone())))
    }
}

struct FakeEngine(Shared);

impl EngineHandle for FakeEngine {
    fn poll(&mut self) -> Result<Vec<SessionNotice>, SessionError> {
        self.0
            .get()
            .engine_polls
            .pop_front()
            .unwrap_or(Ok(Vec::new()))
    }

    fn status(&self) -> EngineStatus {
        let world = self.0.get();
        EngineStatus {
            directions: world.engine_directions.clone(),
            fault: world.engine_fault.clone(),
            ..EngineStatus::default()
        }
    }

    fn apply(&mut self, change: LiveChange) -> Result<(), SessionError> {
        self.apply_retaining(change).map_err(|(error, _)| error)
    }

    fn apply_retaining(&mut self, change: LiveChange) -> Result<(), (SessionError, LiveChange)> {
        let mut world = self.0.get();
        if let Some(error) = world.apply_errors.pop_front() {
            return Err((error, change));
        }
        let described = match change {
            LiveChange::Prepared(prepared) => format!(
                "prepared {:?} {}ch [{}]",
                prepared.kind(),
                prepared.channels(),
                prepared
                    .direction_views()
                    .iter()
                    .map(|direction| direction.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            LiveChange::Gain(db) => format!("gain {db}"),
            LiveChange::TestTone(direction) => format!("test {}", direction.0),
            LiveChange::Upmixer {
                endpoint,
                stage: Some(stage),
            } => {
                let status = stage.status();
                world.stages[usize::from(endpoint)] = Some(stage.monitor());
                format!(
                    "upmixer {endpoint} {} Hz [{}] -> [{}]",
                    status.rate,
                    names(&status.inputs),
                    names(&status.outputs)
                )
            }
            LiveChange::Upmixer {
                endpoint,
                stage: None,
            } => {
                world.stages[usize::from(endpoint)] = None;
                format!("upmixer {endpoint} off")
            }
            LiveChange::UpmixApply(on) => format!("upmix apply {on}"),
            other => format!("{other:?}"),
        };
        world.applied.push(described);
        Ok(())
    }

    fn stop(self: Box<Self>) -> Result<(), SessionError> {
        let mut world = self.0.get();
        world.calls.push("stop engine".into());
        world.stages = Default::default();
        Ok(())
    }

    fn upmix_status(&self, endpoint: u16) -> Option<UpmixStatus> {
        let world = self.0.get();
        let endpoint = usize::from(endpoint);
        world.upmix_status.get(endpoint)?.clone().or_else(|| {
            Some(
                world.stages[endpoint]
                    .as_ref()
                    .map_or_else(UpmixStatus::not_installed, UpmixMonitor::status),
            )
        })
    }
}

fn names(speakers: &[ssr_types::Speaker]) -> String {
    speakers
        .iter()
        .map(|speaker| speaker.name())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The plugin host port over the world's [`PluginScript`].
struct FakePlugins(Shared);

impl PluginHost for FakePlugins {
    fn scan(
        &mut self,
        path: &std::path::Path,
        format: ssr_types::app::PluginFormat,
    ) -> Result<Vec<PluginClass>, PluginHostError> {
        let mut world = self.0.get();
        world
            .plugins
            .log
            .push(format!("scan {} {format:?}", path.display()));
        world.plugins.scans.pop_front().unwrap_or_else(|| {
            Ok(vec![PluginClass {
                id: [7; 16],
                name: "Example Upmixer".into(),
                vendor: "Example Audio".into(),
            }])
        })
    }

    fn start(
        &mut self,
        endpoint: u16,
        plugin: &PluginSetting,
        rate: u32,
    ) -> Result<Box<dyn PluginInstance>, PluginHostError> {
        let mut world = self.0.get();
        world
            .plugins
            .log
            .push(format!("start {endpoint} {} {rate}", plugin.name));
        if let Some(error) = world.plugins.start_errors.pop_front() {
            return Err(error);
        }
        let mut instance = FakeInstance {
            world: self.0.clone(),
            endpoint,
            rate,
            latency: world.plugins.latency,
            upmixer: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            stopping: false,
        };
        if !world.plugins.slow {
            instance.come_up(&world.plugins);
        }
        Ok(Box::new(instance))
    }

    fn forget_state(&mut self, endpoint: u16) {
        self.0.get().plugins.log.push(format!("forget {endpoint}"));
    }
}

/// One host of the fake plugin host: [`MatrixUpmixer::stereo_to_5_1`].
struct FakeInstance {
    world: Shared,
    endpoint: u16,
    rate: u32,
    latency: usize,
    upmixer: Option<Box<dyn Upmixer>>,
    inputs: Vec<ssr_types::Speaker>,
    outputs: Vec<ssr_types::Speaker>,
    stopping: bool,
}

impl FakeInstance {
    fn come_up(&mut self, script: &PluginScript) {
        let upmixer = MatrixUpmixer::stereo_to_5_1(self.rate, self.latency);
        self.inputs = upmixer.inputs().to_vec();
        self.outputs = upmixer.outputs().to_vec();
        if script.not_upmixer {
            self.inputs.clone_from(&self.outputs);
        }
        self.upmixer = Some(Box::new(upmixer));
    }

    /// The world, borrowed through a clone of the handle so `self` stays free.
    fn script(&self) -> Shared {
        self.world.clone()
    }
}

impl PluginInstance for FakeInstance {
    fn take_upmixer(&mut self) -> Option<Box<dyn Upmixer>> {
        self.upmixer.take()
    }

    fn inputs(&self) -> &[ssr_types::Speaker] {
        &self.inputs
    }

    fn outputs(&self) -> &[ssr_types::Speaker] {
        &self.outputs
    }

    fn latency_frames(&self) -> usize {
        self.latency
    }

    fn poll(&mut self, _now_ms: u64) -> Vec<PluginEvent> {
        let shared = self.script();
        let mut world = shared.get();
        if self.stopping {
            world.plugins.log.push(format!("exited {}", self.endpoint));
            return vec![PluginEvent::Exited];
        }
        let events: Vec<_> = world.plugins.events[usize::from(self.endpoint)]
            .drain(..)
            .collect();
        for event in &events {
            match event {
                PluginEvent::Running if self.upmixer.is_none() => self.come_up(&world.plugins),
                PluginEvent::LatencyChanged(frames) => self.latency = *frames,
                _ => {}
            }
        }
        events
    }

    fn restart(&mut self, rate: u32) -> Result<(), PluginHostError> {
        let shared = self.script();
        let mut world = shared.get();
        world
            .plugins
            .log
            .push(format!("restart {} {rate}", self.endpoint));
        if let Some(error) = world.plugins.start_errors.pop_front() {
            return Err(error);
        }
        self.rate = rate;
        self.upmixer = None;
        if !world.plugins.slow {
            self.come_up(&world.plugins);
        }
        Ok(())
    }

    fn open_editor(&mut self) -> Result<(), PluginHostError> {
        self.world
            .get()
            .plugins
            .log
            .push(format!("editor {}", self.endpoint));
        Ok(())
    }

    fn shutdown(&mut self) {
        self.stopping = true;
        self.world
            .get()
            .plugins
            .log
            .push(format!("shutdown {}", self.endpoint));
    }
}

fn id(text: &str) -> DeviceId {
    DeviceId(text.to_owned())
}

fn device(device_id: &str, name: &str, endpoint: Option<u16>) -> OutputDevice {
    OutputDevice {
        id: id(device_id),
        name: name.to_owned(),
        endpoint,
        state: DeviceState::Active,
    }
}

fn fine_driver() -> DriverState {
    DriverState::Installed {
        problem_code: 0,
        service_running: true,
        exclusive_policy: PolicyState::Applied,
    }
}

fn previous_roles() -> DefaultRoles {
    DefaultRoles {
        console: Some(id(REALTEK)),
        multimedia: Some(id(REALTEK)),
        communications: Some(id(HEADSET)),
    }
}

fn taken_roles() -> DefaultRoles {
    DefaultRoles {
        console: Some(id(OURS)),
        multimedia: Some(id(OURS)),
        communications: Some(id(OURS)),
    }
}

/// A PC with everything in place: a supported x64 Windows in test-signing
/// mode, the driver installed and fine, our three endpoints, a DAC, the
/// onboard speakers (console and multimedia) and a headset (communications).
fn ready_pc() -> World {
    World {
        platform: Platform {
            os_build: 19_045,
            arch: Arch::X64,
            test_signing: true,
            secure_boot: SecureBoot::Disabled,
        },
        driver: fine_driver(),
        outputs: vec![
            device(OURS, "Spatial SoftReceiver", Some(0)),
            device("{ours-music}", "Spatial SoftReceiver Music", Some(1)),
            device("{ours-pass}", "Spatial SoftReceiver PASS", Some(2)),
            device(DAC, DAC_NAME, None),
            device(REALTEK, "Speakers (Realtek)", None),
            device(HEADSET, "Headset Earphone (USB Headset)", None),
        ],
        roles: previous_roles(),
        restore_point: None,
        setup_results: VecDeque::new(),
        failing_role: None,
        launch_fails: false,
        engine_polls: VecDeque::new(),
        launched: Vec::new(),
        applied: Vec::new(),
        apply_errors: VecDeque::new(),
        layout_support: Ok(true),
        engine_directions: Vec::new(),
        engine_fault: None,
        stages: Default::default(),
        upmix_status: Default::default(),
        plugins: PluginScript {
            latency: 64,
            ..PluginScript::default()
        },
        calls: Vec::new(),
    }
}

/// A profile as one line: its kind, name, gain, LFE route and directions.
fn describe(profile: &OutputProfile) -> String {
    match profile {
        OutputProfile::Speakers(profile) => format!(
            "speakers {} {} dB [{}]",
            profile.name,
            profile.gain_db,
            profile
                .speakers
                .iter()
                .map(|speaker| speaker.name.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        ),
        OutputProfile::Headphones(profile) => format!(
            "headphones {} {} dB lfe={:?} [{}]",
            profile.name,
            profile.gain_db,
            profile.lfe,
            brir::layout(profile).map_or_else(
                |error| format!("error: {error}"),
                |layout| layout
                    .directions()
                    .iter()
                    .map(|direction| direction.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        ),
        OutputProfile::Correction(profile) => format!(
            "correction {} {} dB",
            profile.path.to_string_lossy(),
            profile.gain_db
        ),
    }
}

fn this_build() -> Config {
    Config {
        default_language: Language::Ko,
        requires_test_signing: true,
        min_os_build: 19_041,
        arch: Arch::X64,
        fault_grace: Config::FAULT_GRACE,
        profile_file: None,
    }
}

fn dac_choice() -> OutputChoice {
    OutputChoice {
        id: id(DAC),
        name: DAC_NAME.into(),
    }
}

fn dac_settings() -> Settings {
    Settings {
        language: None,
        output: Some(dac_choice()),
        profile: Default::default(),
        upmix: Default::default(),
    }
}

type Machine = Supervisor<FakeHost, FakeLauncher>;

fn start(world: World, settings: Settings) -> (Shared, Machine) {
    let shared = Shared::new(world);
    let supervisor = Supervisor::new(
        FakeHost(shared.clone()),
        FakeLauncher(shared.clone()),
        Box::new(FakePlugins(shared.clone())),
        this_build(),
        settings,
    );
    (shared, supervisor)
}

#[test]
fn a_note_from_the_shell_stays_until_setup_begins() {
    let (_shared, mut supervisor) = start(ready_pc(), Settings::default());
    supervisor.note("지난 실행을 복원했습니다.".to_owned());
    assert_eq!(supervisor.snapshot().notes, ["지난 실행을 복원했습니다."]);

    supervisor.handle(Intent::BeginSetup).unwrap();
    assert!(supervisor.snapshot().notes.is_empty());
}

/// Starts Ready on the DAC and enables; the call log starts empty.
fn running(world: World) -> (Shared, Machine) {
    let (shared, mut supervisor) = start(world, dac_settings());
    assert_eq!(supervisor.snapshot().phase, Phase::Ready);
    supervisor.handle(Intent::Enable).unwrap();
    assert_eq!(supervisor.snapshot().phase, Phase::Running);
    shared.take_calls();
    (shared, supervisor)
}

fn error(message: &str, next: NextAction) -> UserError {
    UserError {
        message: message.into(),
        next,
    }
}

fn not_allowed() -> UserError {
    error("지금은 할 수 없는 동작입니다.", NextAction::None)
}

fn calls(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|line| (*line).to_owned()).collect()
}

fn give_back_calls() -> Vec<String> {
    calls(&[
        "set Console {realtek}",
        "set Multimedia {realtek}",
        "set Communications {headset}",
        "remove restore point",
    ])
}

#[test]
fn the_wizard_walks_every_step_on_a_fresh_pc() {
    let mut world = ready_pc();
    world.platform = Platform {
        os_build: 18_363,
        arch: Arch::X64,
        test_signing: false,
        secure_boot: SecureBoot::Enabled,
    };
    world.driver = DriverState::NotInstalled;
    let (shared, mut supervisor) = start(world, Settings::default());
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Setup(SetupStep::Welcome));
    assert_eq!(snapshot.platform, None);
    assert_eq!(snapshot.error, None);

    // Windows 10 1909 is too old.
    let refused = supervisor.handle(Intent::BeginSetup).unwrap_err();
    assert_eq!(
        refused,
        error(
            "Windows 10 2004(빌드 19041) 이상이 필요합니다.",
            NextAction::None
        )
    );
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Setup(SetupStep::Unsupported));
    assert_eq!(snapshot.error, Some(refused));
    assert_eq!(snapshot.platform.unwrap().os_build, 18_363);

    // After the Windows update: test signing is off and Secure Boot is on.
    shared.get().platform.os_build = 19_045;
    let refused = supervisor.handle(Intent::Recheck).unwrap_err();
    assert_eq!(
        supervisor.snapshot().phase,
        Phase::Setup(SetupStep::NotTestSigning)
    );
    assert_eq!(
        refused,
        error(
            "시험서명 모드가 꺼져 있습니다. Secure Boot를 끈 뒤 관리자 명령으로 \
             bcdedit /set testsigning on을 실행하고 다시 시작하십시오. \
             Secure Boot가 켜져 있어 먼저 꺼야 합니다.",
            NextAction::Reboot
        )
    );

    // Secure Boot off, test signing on: the driver is the next step.
    {
        let mut world = shared.get();
        world.platform.secure_boot = SecureBoot::Disabled;
        world.platform.test_signing = true;
    }
    supervisor.handle(Intent::Recheck).unwrap();
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Setup(SetupStep::Driver));
    assert_eq!(snapshot.error, None);
    assert_eq!(snapshot.driver, Some(DriverState::NotInstalled));

    // The install needs a reboot.
    shared.get().setup_results.push_back(Ok(SetupOutcome {
        reboot_required: true,
        message: "installed".into(),
    }));
    let refused = supervisor.handle(Intent::InstallDriver).unwrap_err();
    assert_eq!(
        refused,
        error(
            "드라이버 설치를 마치려면 다시 시작해야 합니다.",
            NextAction::Reboot
        )
    );
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Setup(SetupStep::RebootRequired));
    assert_eq!(snapshot.notes, ["설치 도우미: installed"]);
    assert_eq!(shared.take_calls(), calls(&["setup Install"]));

    // After the reboot the driver runs; the user picks an output, never ours.
    shared.get().driver = fine_driver();
    supervisor.handle(Intent::Recheck).unwrap();
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Setup(SetupStep::ChooseOutput));
    assert_eq!(
        snapshot
            .outputs
            .iter()
            .map(|output| output.id.0.as_str())
            .collect::<Vec<_>>(),
        [DAC, REALTEK, HEADSET]
    );
    assert_eq!(snapshot.chosen_output, None);
    assert_eq!(
        supervisor.handle(Intent::ChooseOutput(id(OURS))),
        Err(error(
            "Spatial SoftReceiver 자신을 출력으로 고를 수 없습니다.",
            NextAction::None
        ))
    );
    assert_eq!(
        supervisor.snapshot().phase,
        Phase::Setup(SetupStep::ChooseOutput)
    );
    supervisor.handle(Intent::ChooseOutput(id(DAC))).unwrap();
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Ready);
    assert_eq!(snapshot.chosen_output, Some(dac_choice()));
    assert_eq!(snapshot.error, None);
    assert_eq!(supervisor.settings(), &dac_settings());

    // Enable: the restore point first, the three roles in order, the engine.
    supervisor.handle(Intent::Enable).unwrap();
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Running);
    assert!(snapshot.roles_taken);
    assert!(snapshot.engine.is_some());
    assert_eq!(
        shared.take_calls(),
        calls(&[
            "write restore point",
            "set Console {ours-main}",
            "set Multimedia {ours-main}",
            "set Communications {ours-main}",
            "launch {dac}",
        ])
    );
    assert_eq!(shared.get().roles, taken_roles());
    let point = shared.get().restore_point.clone().expect("a restore point");
    assert_eq!(
        RestorePoint::from_json(&point),
        Ok(RestorePoint {
            ours: id(OURS),
            previous: previous_roles(),
            fallback: Some(id(DAC)),
            app_pid: std::process::id(),
        })
    );

    // Disable: the engine stops, the roles go back, the restore point goes.
    supervisor.handle(Intent::Disable).unwrap();
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Ready);
    assert!(!snapshot.roles_taken);
    assert_eq!(snapshot.engine, None);
    assert!(snapshot.notes.is_empty());
    let mut expected = calls(&["stop engine"]);
    expected.extend(give_back_calls());
    assert_eq!(shared.take_calls(), expected);
    assert_eq!(shared.get().roles, previous_roles());
    assert_eq!(shared.get().restore_point, None);
}

#[test]
fn a_recheck_at_the_output_step_lists_the_outputs_again() {
    let mut world = ready_pc();
    world.outputs.retain(|output| output.endpoint.is_some());
    let (shared, mut supervisor) = start(world, Settings::default());
    supervisor.handle(Intent::BeginSetup).unwrap();
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Setup(SetupStep::ChooseOutput));
    assert!(snapshot.outputs.is_empty());

    // The user plugs the DAC in and asks again.
    shared.get().outputs.push(device(DAC, DAC_NAME, None));
    supervisor.handle(Intent::Recheck).unwrap();
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Setup(SetupStep::ChooseOutput));
    assert_eq!(snapshot.outputs, vec![dac_choice()]);
    assert_eq!(snapshot.error, None);

    // With a remembered output that is present again, the recheck resumes.
    let mut world = ready_pc();
    world.outputs.retain(|output| output.endpoint.is_some());
    let (shared, mut supervisor) = start(world, dac_settings());
    assert_eq!(
        supervisor.handle(Intent::BeginSetup),
        Err(error(
            &format!("저장된 출력 장치를 찾을 수 없습니다: {DAC_NAME}"),
            NextAction::OpenSettings
        ))
    );
    assert_eq!(
        supervisor.snapshot().phase,
        Phase::Setup(SetupStep::ChooseOutput)
    );
    shared.get().outputs.push(device(DAC, DAC_NAME, None));
    supervisor.handle(Intent::Recheck).unwrap();
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Ready);
    assert_eq!(snapshot.error, None);
}

#[test]
fn a_declined_prompt_and_a_failed_install_stay_on_the_driver_step() {
    let mut world = ready_pc();
    world.driver = DriverState::NotInstalled;
    world.setup_results = VecDeque::from([
        Err(HostError::Declined),
        Err(HostError::Setup("the INF was rejected".into())),
    ]);
    let (shared, mut supervisor) = start(world, Settings::default());
    supervisor.handle(Intent::BeginSetup).unwrap();
    assert_eq!(supervisor.snapshot().phase, Phase::Setup(SetupStep::Driver));

    let refused = supervisor.handle(Intent::InstallDriver).unwrap_err();
    assert_eq!(
        refused,
        error("관리자 승인을 취소했습니다.", NextAction::Retry)
    );
    assert_eq!(supervisor.snapshot().phase, Phase::Setup(SetupStep::Driver));
    assert_eq!(supervisor.snapshot().error, Some(refused));

    let refused = supervisor.handle(Intent::InstallDriver).unwrap_err();
    assert_eq!(
        refused,
        error(
            "드라이버 설치에 실패했습니다: the INF was rejected",
            NextAction::Retry
        )
    );
    assert_eq!(supervisor.snapshot().phase, Phase::Setup(SetupStep::Driver));

    // The third try installs without a reboot and goes on to the output; the
    // helper said nothing, so there is no note.
    supervisor.handle(Intent::InstallDriver).unwrap();
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Setup(SetupStep::ChooseOutput));
    assert_eq!(snapshot.error, None);
    assert_eq!(snapshot.driver, Some(fine_driver()));
    assert!(snapshot.notes.is_empty());
    assert_eq!(
        shared.take_calls(),
        calls(&["setup Install", "setup Install", "setup Install"])
    );
}

#[test]
fn problem_code_52_names_test_signing() {
    let mut world = ready_pc();
    world.driver = DriverState::Installed {
        problem_code: 52,
        service_running: false,
        exclusive_policy: PolicyState::Applied,
    };
    let (shared, mut supervisor) = start(world, dac_settings());
    assert_eq!(
        supervisor.snapshot().phase,
        Phase::Setup(SetupStep::Welcome)
    );
    assert_eq!(
        supervisor.handle(Intent::BeginSetup),
        Err(error(
            "Windows가 드라이버 서명을 확인하지 못했습니다(문제 코드 52). \
             시험서명 모드를 확인한 뒤 다시 설치하십시오.",
            NextAction::Retry
        ))
    );
    assert_eq!(supervisor.snapshot().phase, Phase::Setup(SetupStep::Driver));

    // An installed driver is repaired, not installed again; the saved output
    // takes the wizard straight to Ready.
    supervisor.handle(Intent::InstallDriver).unwrap();
    assert_eq!(shared.take_calls(), calls(&["setup Repair"]));
    assert_eq!(supervisor.snapshot().phase, Phase::Ready);
}

#[test]
fn a_driver_problem_asks_for_a_repair() {
    let mut world = ready_pc();
    world.driver = DriverState::Installed {
        problem_code: 10,
        service_running: true,
        exclusive_policy: PolicyState::Applied,
    };
    let (shared, mut supervisor) = start(world, Settings::default());
    assert_eq!(
        supervisor.handle(Intent::BeginSetup),
        Err(error(
            "드라이버가 올바르게 설치되지 않았습니다(문제 코드 10). 복구를 실행하십시오.",
            NextAction::Retry
        ))
    );
    assert_eq!(supervisor.snapshot().phase, Phase::Setup(SetupStep::Driver));

    shared.get().driver = DriverState::Installed {
        problem_code: 0,
        service_running: false,
        exclusive_policy: PolicyState::Applied,
    };
    assert_eq!(
        supervisor.handle(Intent::Recheck),
        Err(error(
            "드라이버 서비스가 실행되고 있지 않습니다. 복구를 실행하십시오.",
            NextAction::Retry
        ))
    );
    assert_eq!(supervisor.snapshot().phase, Phase::Setup(SetupStep::Driver));
}

#[test]
fn a_missing_exclusive_policy_asks_for_a_repair() {
    // The install finished with a reboot, before ssr-setup could reach the
    // endpoints to apply their exclusive policy.
    let mut world = ready_pc();
    world.driver = DriverState::RebootRequired;
    let (shared, mut supervisor) = start(world, Settings::default());
    assert_eq!(
        supervisor.handle(Intent::BeginSetup),
        Err(error(
            "드라이버 설치를 마치려면 다시 시작해야 합니다.",
            NextAction::Reboot
        ))
    );
    assert_eq!(
        supervisor.snapshot().phase,
        Phase::Setup(SetupStep::RebootRequired)
    );

    // After the reboot the driver runs, but its endpoints lack the policy.
    let missing = DriverState::Installed {
        problem_code: 0,
        service_running: true,
        exclusive_policy: PolicyState::Missing,
    };
    shared.get().driver = missing.clone();
    let refused = supervisor.handle(Intent::Recheck).unwrap_err();
    assert_eq!(
        refused,
        error(
            "드라이버는 있지만 독점 정책이 적용되지 않았습니다. 복구를 실행하십시오.",
            NextAction::Retry
        )
    );
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Setup(SetupStep::Driver));
    assert_eq!(snapshot.driver, Some(missing));
    assert_eq!(snapshot.error, Some(refused));

    // The driver step repairs an installed driver; the fake's repair applies
    // the policy, and the wizard goes on to the output.
    supervisor.handle(Intent::InstallDriver).unwrap();
    assert_eq!(shared.take_calls(), calls(&["setup Repair"]));
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Setup(SetupStep::ChooseOutput));
    assert_eq!(snapshot.driver, Some(fine_driver()));
    assert_eq!(snapshot.error, None);
}

#[test]
fn an_unknown_exclusive_policy_counts_as_fine() {
    // No endpoint active yet, or a store that could not be read.
    let unknown = DriverState::Installed {
        problem_code: 0,
        service_running: true,
        exclusive_policy: PolicyState::Unknown,
    };
    let (_, supervisor) = start(
        World {
            driver: unknown.clone(),
            ..ready_pc()
        },
        dac_settings(),
    );
    assert_eq!(supervisor.snapshot().phase, Phase::Ready);

    let (shared, mut supervisor) = start(
        World {
            driver: unknown.clone(),
            ..ready_pc()
        },
        Settings::default(),
    );
    supervisor.handle(Intent::BeginSetup).unwrap();
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Setup(SetupStep::ChooseOutput));
    assert_eq!(snapshot.error, None);
    assert_eq!(snapshot.driver, Some(unknown));
    assert!(shared.take_calls().is_empty());
}

#[test]
fn another_architecture_is_unsupported() {
    let mut world = ready_pc();
    world.platform.arch = Arch::Arm64;
    let (shared, mut supervisor) = start(world, Settings::default());
    assert_eq!(
        supervisor.handle(Intent::BeginSetup),
        Err(error(
            "이 버전은 x64 PC용이지만 이 PC는 Arm64입니다.",
            NextAction::None
        ))
    );
    assert_eq!(
        supervisor.snapshot().phase,
        Phase::Setup(SetupStep::Unsupported)
    );
    shared.get().platform.arch = Arch::Other;
    assert_eq!(
        supervisor.handle(Intent::Recheck),
        Err(error(
            "이 버전은 x64 PC용이지만 이 PC는 다른 종류의 PC입니다.",
            NextAction::None
        ))
    );
}

#[test]
fn a_changed_device_id_is_found_by_name_and_remembered() {
    // After a reboot the DAC came back under a new id.
    let (shared, mut supervisor) = start(
        ready_pc(),
        Settings {
            language: None,
            output: Some(OutputChoice {
                id: id("{dac-before-reboot}"),
                name: DAC_NAME.into(),
            }),
            profile: Default::default(),
            upmix: Default::default(),
        },
    );
    let note = format!("출력 장치의 식별자가 바뀌어 이름으로 다시 찾았습니다: {DAC_NAME}");
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Ready);
    assert_eq!(snapshot.notes, std::slice::from_ref(&note));
    assert_eq!(supervisor.settings(), &dac_settings());
    assert!(shared.take_calls().is_empty());

    // It changes again while Ready (replugged into another port): Enable finds
    // it by name and starts the engine on it.
    shared.output(DAC).id = id("{dac-other-port}");
    supervisor.handle(Intent::Enable).unwrap();
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Running);
    assert_eq!(snapshot.notes, [note]);
    assert_eq!(
        supervisor.settings().output.as_ref().unwrap().id,
        id("{dac-other-port}")
    );
    assert!(
        shared
            .take_calls()
            .contains(&"launch {dac-other-port}".to_owned())
    );
}

#[test]
fn an_ambiguous_name_is_not_guessed() {
    // Two DACs of the same model, both with new ids: neither is guessed.
    let mut world = ready_pc();
    world.outputs.push(device("{dac-2}", DAC_NAME, None));
    let (shared, mut supervisor) = start(world, dac_settings());
    shared.output(DAC).id = id("{dac-1}");
    let missing = error(
        &format!("저장된 출력 장치를 찾을 수 없습니다: {DAC_NAME}"),
        NextAction::OpenSettings,
    );

    // Ready (the DAC was present at start): Enable refuses and touches nothing.
    assert_eq!(supervisor.snapshot().phase, Phase::Ready);
    assert_eq!(supervisor.handle(Intent::Enable), Err(missing.clone()));
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Setup(SetupStep::ChooseOutput));
    assert_eq!(snapshot.error, Some(missing.clone()));
    assert!(!snapshot.roles_taken);
    assert!(shared.take_calls().is_empty());
    assert_eq!(supervisor.settings(), &dac_settings());

    // A fresh start with the same devices goes through the wizard to the
    // same place.
    let (shared, mut fresh) = start(
        World {
            outputs: shared.get().outputs.clone(),
            ..ready_pc()
        },
        dac_settings(),
    );
    assert_eq!(fresh.snapshot().phase, Phase::Setup(SetupStep::Welcome));
    assert_eq!(fresh.handle(Intent::BeginSetup), Err(missing));
    assert_eq!(
        fresh.snapshot().phase,
        Phase::Setup(SetupStep::ChooseOutput)
    );
    assert_eq!(fresh.settings(), &dac_settings());
    assert!(shared.take_calls().is_empty());
}

#[test]
fn restore_leaves_roles_the_user_changed_meanwhile() {
    let mut world = ready_pc();
    world.roles.communications = Some(id(REALTEK));
    let (shared, mut supervisor) = running(world);

    // During Running the user points communications at the headset.
    shared
        .get()
        .roles
        .set(Role::Communications, Some(id(HEADSET)));
    supervisor.handle(Intent::Disable).unwrap();
    assert_eq!(
        shared.take_calls(),
        calls(&[
            "stop engine",
            "set Console {realtek}",
            "set Multimedia {realtek}",
            "remove restore point",
        ])
    );
    assert_eq!(shared.get().roles, previous_roles());
    assert!(supervisor.snapshot().notes.is_empty());
}

#[test]
fn restore_notes_a_previous_device_that_disappeared() {
    // Multimedia had no default; the headset and the chosen DAC are unplugged
    // while Running, so neither the previous device nor the fallback is usable.
    let mut world = ready_pc();
    world.roles.multimedia = None;
    let (shared, mut supervisor) = running(world);
    shared.output(HEADSET).state = DeviceState::Unplugged;
    shared.output(DAC).state = DeviceState::Unplugged;

    supervisor.handle(Intent::Disable).unwrap();
    assert_eq!(
        shared.take_calls(),
        calls(&[
            "stop engine",
            "set Console {realtek}",
            "remove restore point"
        ])
    );
    let snapshot = supervisor.snapshot();
    assert_eq!(
        snapshot.notes,
        [
            "이전 기본 장치(멀티미디어)를 찾을 수 없어 Windows가 고르게 둡니다.",
            "이전 기본 장치(통신)를 찾을 수 없어 Windows가 고르게 둡니다.",
        ]
    );
    assert_eq!(snapshot.phase, Phase::Ready);
    assert!(!snapshot.roles_taken);
    assert_eq!(shared.get().restore_point, None);
}

#[test]
fn a_previous_role_already_ours_falls_back_to_the_chosen_output() {
    // An earlier run never gave console back, so its "previous" is ours.
    let mut world = ready_pc();
    world.roles.console = Some(id(OURS));
    let (shared, mut supervisor) = running(world);
    let point = RestorePoint::from_json(shared.get().restore_point.as_deref().unwrap()).unwrap();
    assert_eq!(point.previous.console, Some(id(OURS)));

    supervisor.handle(Intent::Disable).unwrap();
    assert_eq!(
        shared.take_calls(),
        calls(&[
            "stop engine",
            "set Console {dac}",
            "set Multimedia {realtek}",
            "set Communications {headset}",
            "remove restore point",
        ])
    );
    assert_eq!(
        supervisor.snapshot().notes,
        [format!(
            "이전 기본 장치(콘솔)를 찾을 수 없어 고른 출력으로 되돌렸습니다: {DAC_NAME}"
        )]
    );
    assert_eq!(shared.get().roles.console, Some(id(DAC)));
}

#[test]
fn a_gone_previous_device_falls_back_to_the_chosen_output() {
    // The onboard speakers disappear while Running (disabled in the BIOS).
    let (shared, mut supervisor) = running(ready_pc());
    shared.get().outputs.retain(|output| output.id.0 != REALTEK);

    supervisor.handle(Intent::Disable).unwrap();
    assert_eq!(
        shared.take_calls(),
        calls(&[
            "stop engine",
            "set Console {dac}",
            "set Multimedia {dac}",
            "set Communications {headset}",
            "remove restore point",
        ])
    );
    assert_eq!(
        supervisor.snapshot().notes,
        [
            format!("이전 기본 장치(콘솔)를 찾을 수 없어 고른 출력으로 되돌렸습니다: {DAC_NAME}"),
            format!(
                "이전 기본 장치(멀티미디어)를 찾을 수 없어 고른 출력으로 되돌렸습니다: {DAC_NAME}"
            ),
        ]
    );
    assert_eq!(
        shared.get().roles,
        DefaultRoles {
            console: Some(id(DAC)),
            multimedia: Some(id(DAC)),
            communications: Some(id(HEADSET)),
        }
    );
}

#[test]
fn the_guard_gives_the_roles_back_from_the_restore_point_alone() {
    // The app dies while Running with the onboard speakers gone: the guard has
    // only the restore point, and gives back what the app would have.
    let (shared, _supervisor) = running(ready_pc());
    shared.get().outputs.retain(|output| output.id.0 != REALTEK);
    let point = RestorePoint::from_json(shared.get().restore_point.as_deref().unwrap()).unwrap();
    let notes = restore_point::restore(&point, &mut FakeHost(shared.clone()));
    assert_eq!(
        shared.take_calls(),
        calls(&[
            "set Console {dac}",
            "set Multimedia {dac}",
            "set Communications {headset}",
            "remove restore point",
        ])
    );
    assert_eq!(notes.len(), 2);
    assert_eq!(shared.get().restore_point, None);
}

#[test]
fn a_failed_role_change_rolls_back_and_removes_the_restore_point() {
    let mut world = ready_pc();
    world.failing_role = Some(Role::Multimedia);
    let (shared, mut supervisor) = start(world, dac_settings());
    assert_eq!(
        supervisor.handle(Intent::Enable),
        Err(error(
            "기본 장치를 바꾸지 못했습니다: the host refused: scripted",
            NextAction::Retry
        ))
    );
    assert_eq!(
        shared.take_calls(),
        calls(&[
            "write restore point",
            "set Console {ours-main}",
            "set Multimedia {ours-main}",
            "set Console {realtek}",
            "remove restore point",
        ])
    );
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Ready);
    assert!(!snapshot.roles_taken);
    assert_eq!(snapshot.engine, None);
    assert_eq!(shared.get().roles, previous_roles());
    assert_eq!(shared.get().restore_point, None);
}

#[test]
fn a_failed_launch_restores_the_roles() {
    let mut world = ready_pc();
    world.launch_fails = true;
    let (shared, mut supervisor) = start(world, dac_settings());
    assert_eq!(
        supervisor.handle(Intent::Enable),
        Err(error(
            "엔진을 시작하지 못했습니다: diagnostic recorder failed: scripted failure",
            NextAction::Retry
        ))
    );
    let mut expected = calls(&[
        "write restore point",
        "set Console {ours-main}",
        "set Multimedia {ours-main}",
        "set Communications {ours-main}",
        "launch {dac}",
    ]);
    expected.extend(give_back_calls());
    assert_eq!(shared.take_calls(), expected);
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Ready);
    assert!(!snapshot.roles_taken);
    assert_eq!(shared.get().roles, previous_roles());
    assert_eq!(shared.get().restore_point, None);
}

#[test]
fn an_engine_fault_stops_restores_and_faults() {
    let (shared, mut supervisor) = running(ready_pc());
    let start = Instant::now();
    assert!(supervisor.tick(start).is_empty());

    // A poll error stops at once, even while a fault waits for its reconnect.
    shared
        .get()
        .engine_polls
        .push_back(Ok(vec![SessionNotice::Fault(EngineFault::OutputLost(
            "the DAC was removed".into(),
        ))]));
    assert_eq!(supervisor.tick(start).len(), 1);
    assert_eq!(supervisor.snapshot().phase, Phase::Running);
    shared
        .get()
        .engine_polls
        .push_back(Err(SessionError::Rejected));
    assert!(supervisor.tick(start + Duration::from_secs(1)).is_empty());
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Faulted);
    assert_eq!(
        snapshot.error,
        Some(error(
            "엔진이 멈췄습니다: the engine rejected a live change",
            NextAction::Retry
        ))
    );
    assert!(!snapshot.roles_taken);
    assert_eq!(snapshot.engine, None);
    let mut expected = calls(&["stop engine"]);
    expected.extend(give_back_calls());
    assert_eq!(shared.take_calls(), expected);
    assert_eq!(shared.get().roles, previous_roles());
    assert!(supervisor.tick(start + Duration::from_secs(2)).is_empty());

    // Enable works again from Faulted, and the old fault's clock is gone.
    supervisor.handle(Intent::Enable).unwrap();
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Running);
    assert_eq!(snapshot.error, None);
    assert_eq!(shared.get().roles, taken_roles());
    supervisor.tick(start + Duration::from_secs(60));
    assert_eq!(supervisor.snapshot().phase, Phase::Running);

    // Disable during a fault works as usual and ends the reconnect wait.
    shared.take_calls();
    shared
        .get()
        .engine_polls
        .push_back(Ok(vec![SessionNotice::Fault(EngineFault::CaptureLost(
            "the ring closed".into(),
        ))]));
    supervisor.tick(start + Duration::from_secs(61));
    supervisor.handle(Intent::Disable).unwrap();
    let mut expected = calls(&["stop engine"]);
    expected.extend(give_back_calls());
    assert_eq!(shared.take_calls(), expected);
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Ready);
    assert_eq!(snapshot.error, None);
    supervisor.handle(Intent::Enable).unwrap();
    supervisor.tick(start + Duration::from_secs(120));
    assert_eq!(supervisor.snapshot().phase, Phase::Running);

    // Recheck leaves Faulted for Ready.
    shared
        .get()
        .engine_polls
        .push_back(Err(SessionError::Rejected));
    supervisor.tick(start + Duration::from_secs(121));
    assert_eq!(supervisor.snapshot().phase, Phase::Faulted);
    supervisor.handle(Intent::Recheck).unwrap();
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Ready);
    assert_eq!(snapshot.error, None);
}

#[test]
fn a_recoverable_fault_keeps_running_until_the_engine_reconnects() {
    let (shared, mut supervisor) = running(ready_pc());
    let start = Instant::now();
    let fault = EngineFault::OutputLost("the DAC was removed".into());
    let lost = SessionNotice::Fault(fault.clone());
    {
        let mut world = shared.get();
        world.engine_fault = Some(fault.clone());
        world.engine_polls.push_back(Ok(vec![lost.clone()]));
    }
    assert_eq!(supervisor.tick(start), [lost]);
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Running);
    assert_eq!(snapshot.error, None);
    assert_eq!(
        snapshot
            .engine
            .as_ref()
            .and_then(|engine| engine.fault.clone()),
        Some(fault)
    );
    assert!(snapshot.roles_taken);
    assert!(snapshot.engine.is_some());
    assert!(supervisor.tick(start + Duration::from_secs(1)).is_empty());
    assert_eq!(supervisor.snapshot().phase, Phase::Running);

    // The engine reconnects 3 s later.
    {
        let mut world = shared.get();
        world.engine_fault = None;
        world.engine_polls.push_back(Ok(vec![
            SessionNotice::OutputReconnected,
            SessionNotice::FaultCleared,
        ]));
    }
    assert_eq!(supervisor.tick(start + Duration::from_secs(3)).len(), 2);
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Running);
    assert_eq!(snapshot.error, None);
    assert_eq!(snapshot.notes, ["다시 연결했습니다."]);
    assert!(snapshot.roles_taken);

    // The grace no longer runs.
    supervisor.tick(start + Duration::from_secs(20));
    assert_eq!(supervisor.snapshot().phase, Phase::Running);

    // A lost capture is reported in the engine status without adding an error.
    let fault = EngineFault::CaptureLost("the ring closed".into());
    {
        let mut world = shared.get();
        world.engine_fault = Some(fault.clone());
        world
            .engine_polls
            .push_back(Ok(vec![SessionNotice::Fault(fault.clone())]));
    }
    supervisor.tick(start + Duration::from_secs(21));
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Running);
    assert_eq!(snapshot.error, None);
    assert_eq!(snapshot.engine.and_then(|engine| engine.fault), Some(fault));

    // Nothing was stopped and no role moved.
    assert!(shared.take_calls().is_empty());
    assert_eq!(shared.get().roles, taken_roles());
}

#[test]
fn a_reconnect_notice_does_not_clear_an_unrelated_failure() {
    let (shared, mut supervisor) = running(ready_pc());
    let existing = error(
        "출력 음량 조절 범위는 -20 dB에서 +10 dB까지입니다.",
        NextAction::None,
    );
    assert_eq!(
        supervisor.handle(Intent::SetGain(11.0)),
        Err(existing.clone())
    );
    assert_eq!(supervisor.snapshot().error, Some(existing.clone()));

    let start = Instant::now();
    let fault = EngineFault::OutputLost("the DAC was removed".into());
    {
        let mut world = shared.get();
        world.engine_fault = Some(fault.clone());
        world
            .engine_polls
            .push_back(Ok(vec![SessionNotice::Fault(fault.clone())]));
    }
    supervisor.tick(start);
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Running);
    assert_eq!(snapshot.error, Some(existing.clone()));
    assert_eq!(snapshot.engine.and_then(|engine| engine.fault), Some(fault));

    {
        let mut world = shared.get();
        world.engine_fault = None;
        world
            .engine_polls
            .push_back(Ok(vec![SessionNotice::FaultCleared]));
    }
    supervisor.tick(start + Duration::from_secs(1));
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Running);
    assert_eq!(snapshot.error, Some(existing));
    assert_eq!(snapshot.engine.and_then(|engine| engine.fault), None);
    assert_eq!(snapshot.notes, ["다시 연결했습니다."]);
}

#[test]
fn a_fault_that_outlasts_the_grace_stops_and_restores() {
    let (shared, mut supervisor) = running(ready_pc());
    let start = Instant::now();
    shared
        .get()
        .engine_polls
        .push_back(Ok(vec![SessionNotice::Fault(EngineFault::OutputLost(
            "the DAC was removed".into(),
        ))]));
    supervisor.tick(start);

    // A second fault does not restart the clock.
    shared
        .get()
        .engine_polls
        .push_back(Ok(vec![SessionNotice::Fault(EngineFault::CaptureLost(
            "the ring closed".into(),
        ))]));
    supervisor.tick(start + Duration::from_secs(5));
    supervisor.tick(start + Duration::from_secs(14));
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Running);
    assert!(snapshot.roles_taken);
    assert!(shared.take_calls().is_empty());

    supervisor.tick(start + Duration::from_secs(15));
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Faulted);
    assert_eq!(
        snapshot.error,
        Some(error(
            "엔진이 15초 동안 다시 연결하지 못했습니다. 출력 장치와의 연결이 끊어졌습니다: the DAC was removed",
            NextAction::Retry
        ))
    );
    assert!(!snapshot.roles_taken);
    assert_eq!(snapshot.engine, None);
    let mut expected = calls(&["stop engine"]);
    expected.extend(give_back_calls());
    assert_eq!(shared.take_calls(), expected);
    assert_eq!(shared.get().roles, previous_roles());
    assert_eq!(shared.get().restore_point, None);
}

#[test]
fn quit_from_running_stops_and_restores() {
    let (shared, mut supervisor) = running(ready_pc());
    supervisor.handle(Intent::Quit).unwrap();
    let mut expected = calls(&["stop engine"]);
    expected.extend(give_back_calls());
    assert_eq!(shared.take_calls(), expected);
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Quit);
    assert!(!snapshot.roles_taken);
    assert_eq!(shared.get().roles, previous_roles());
    assert_eq!(shared.get().restore_point, None);
}

#[test]
fn intents_after_quit_are_refused() {
    let mut world = ready_pc();
    world.driver = DriverState::NotInstalled;
    let (shared, mut supervisor) = start(world, dac_settings());
    supervisor.handle(Intent::Quit).unwrap();
    assert_eq!(supervisor.snapshot().phase, Phase::Quit);
    for intent in [
        Intent::BeginSetup,
        Intent::Recheck,
        Intent::InstallDriver,
        Intent::ChooseOutput(id(DAC)),
        Intent::Enable,
        Intent::Disable,
        Intent::SetGain(-3.0),
        Intent::Quit,
    ] {
        assert_eq!(supervisor.handle(intent), Err(not_allowed()));
        assert_eq!(supervisor.snapshot().phase, Phase::Quit);
    }
    assert!(supervisor.tick(Instant::now()).is_empty());
    assert!(shared.take_calls().is_empty());
}

#[test]
fn a_refused_intent_keeps_the_step_explanation() {
    let mut world = ready_pc();
    world.platform.os_build = 18_363;
    let (shared, mut supervisor) = start(world, Settings::default());
    let unsupported = error(
        "Windows 10 2004(빌드 19041) 이상이 필요합니다.",
        NextAction::None,
    );
    assert_eq!(
        supervisor.handle(Intent::BeginSetup),
        Err(unsupported.clone())
    );

    // Refusals are returned but leave the snapshot's explanation in place.
    assert_eq!(supervisor.handle(Intent::Enable), Err(not_allowed()));
    assert_eq!(supervisor.handle(Intent::SetGain(-3.0)), Err(not_allowed()));
    assert_eq!(
        supervisor.handle(Intent::StartDiagnosticRecording),
        Err(error(
            "진단 기록 기능은 이후 업데이트에서 제공됩니다.",
            NextAction::None
        ))
    );
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Setup(SetupStep::Unsupported));
    assert_eq!(snapshot.error, Some(unsupported));

    // The next success clears it.
    shared.get().platform.os_build = 19_045;
    supervisor.handle(Intent::Recheck).unwrap();
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Setup(SetupStep::ChooseOutput));
    assert_eq!(snapshot.error, None);
}

#[test]
fn unlisted_and_refused_intents_leave_the_state_alone() {
    let (shared, mut supervisor) = start(ready_pc(), dac_settings());
    for intent in [Intent::BeginSetup, Intent::Recheck, Intent::Disable] {
        assert_eq!(supervisor.handle(intent), Err(not_allowed()));
        assert_eq!(supervisor.snapshot().phase, Phase::Ready);
    }
    // Diagnostic recording is the one intent not wired yet; a test tone
    // needs a running engine. Both are refusals: nothing changes.
    for (intent, refusal) in [
        (
            Intent::StartDiagnosticRecording,
            "진단 기록 기능은 이후 업데이트에서 제공됩니다.",
        ),
        (
            Intent::TestDirection(ssr_types::DirectionId(0)),
            "켠 뒤에 시험용 신호를 재생할 수 있습니다.",
        ),
    ] {
        assert_eq!(
            supervisor.handle(intent),
            Err(error(refusal, NextAction::None))
        );
        assert_eq!(supervisor.snapshot().phase, Phase::Ready);
        assert_eq!(supervisor.snapshot().error, None);
    }
    assert_eq!(supervisor.settings(), &dac_settings());

    // Ready may change the output; Running may not.
    supervisor
        .handle(Intent::ChooseOutput(id(REALTEK)))
        .unwrap();
    assert_eq!(supervisor.snapshot().phase, Phase::Ready);
    assert_eq!(
        supervisor.settings().output.as_ref().unwrap().id,
        id(REALTEK)
    );
    supervisor.handle(Intent::Enable).unwrap();
    assert_eq!(
        supervisor.handle(Intent::ChooseOutput(id(DAC))),
        Err(error("먼저 끈 뒤 출력을 바꾸십시오.", NextAction::None))
    );
    assert_eq!(supervisor.snapshot().phase, Phase::Running);
    assert!(shared.take_calls().contains(&"launch {realtek}".to_owned()));
}

#[test]
fn a_hidden_main_endpoint_refuses_to_enable() {
    let (shared, mut supervisor) = start(ready_pc(), dac_settings());
    {
        let mut world = shared.get();
        world.driver = DriverState::Installed {
            problem_code: 10,
            service_running: false,
            exclusive_policy: PolicyState::Applied,
        };
    }
    shared.output(OURS).state = DeviceState::Disabled;
    assert_eq!(
        supervisor.handle(Intent::Enable),
        Err(error(
            "게임과 일반 앱의 소리를 받는 가상 오디오 장치를 찾을 수 없습니다. 드라이버 상태를 확인하십시오.",
            NextAction::Retry
        ))
    );
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Ready);
    assert_eq!(
        snapshot.driver,
        Some(DriverState::Installed {
            problem_code: 10,
            service_running: false,
            exclusive_policy: PolicyState::Applied,
        })
    );
    assert!(shared.take_calls().is_empty());
}

#[test]
fn new_starts_ready_when_the_driver_and_the_output_are_present() {
    let (shared, supervisor) = start(ready_pc(), dac_settings());
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Ready);
    assert_eq!(snapshot.driver, Some(fine_driver()));
    assert_eq!(snapshot.chosen_output, Some(dac_choice()));
    assert!(!snapshot.roles_taken);
    assert_eq!(snapshot.error, None);
    assert!(snapshot.notes.is_empty());
    // Never takes the roles at start.
    assert!(shared.take_calls().is_empty());
    assert_eq!(shared.get().roles, previous_roles());
}

#[test]
fn new_starts_at_welcome_otherwise() {
    let gone = Settings {
        language: None,
        output: Some(OutputChoice {
            id: id("{old-dac}"),
            name: "Old DAC".into(),
        }),
        profile: Default::default(),
        upmix: Default::default(),
    };
    let unplugged = |mut world: World| {
        world
            .outputs
            .iter_mut()
            .find(|output| output.id.0 == DAC)
            .unwrap()
            .state = DeviceState::Unplugged;
        world
    };
    let with_driver = |driver: DriverState| World {
        driver,
        ..ready_pc()
    };
    let cases = [
        (with_driver(DriverState::NotInstalled), dac_settings()),
        (with_driver(DriverState::RebootRequired), dac_settings()),
        (
            with_driver(DriverState::Installed {
                problem_code: 52,
                service_running: false,
                exclusive_policy: PolicyState::Applied,
            }),
            dac_settings(),
        ),
        (
            with_driver(DriverState::Installed {
                problem_code: 0,
                service_running: false,
                exclusive_policy: PolicyState::Applied,
            }),
            dac_settings(),
        ),
        (
            with_driver(DriverState::Installed {
                problem_code: 0,
                service_running: true,
                exclusive_policy: PolicyState::Missing,
            }),
            dac_settings(),
        ),
        (ready_pc(), Settings::default()),
        (ready_pc(), gone),
        (unplugged(ready_pc()), dac_settings()),
    ];
    for (world, settings) in cases {
        let (shared, supervisor) = start(world, settings.clone());
        let snapshot = supervisor.snapshot();
        assert_eq!(snapshot.phase, Phase::Setup(SetupStep::Welcome));
        assert_eq!(snapshot.error, None);
        assert!(!snapshot.roles_taken);
        assert_eq!(supervisor.settings(), &settings);
        assert!(shared.take_calls().is_empty());
    }
}

struct SessionLauncher;

impl Launcher for SessionLauncher {
    fn launch(
        &mut self,
        _output: &OutputDevice,
        profile: &OutputProfile,
    ) -> Result<Box<dyn EngineHandle>, SessionError> {
        let session = Session::start(
            supervisor::SessionConfig {
                profile: match profile {
                    OutputProfile::Headphones(profile) => {
                        OutputProfile::Headphones(profile.clone())
                    }
                    OutputProfile::Speakers(profile) => OutputProfile::Speakers(profile.clone()),
                    OutputProfile::Correction(profile) => {
                        OutputProfile::Correction(profile.clone())
                    }
                },
                ..common::config()
            },
            common::capture(),
            NullOpener {
                rate: 48_000,
                period_frames: 480,
            },
            None,
        )?;
        Ok(Box::new(session))
    }
}

#[test]
fn a_real_session_runs_behind_the_engine_handle() {
    let shared = Shared::new(ready_pc());
    let mut supervisor = Supervisor::new(
        FakeHost(shared.clone()),
        SessionLauncher,
        Box::new(FakePlugins(shared.clone())),
        this_build(),
        dac_settings(),
    );
    supervisor.handle(Intent::Enable).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let notices = supervisor.tick(Instant::now());
        let snapshot = supervisor.snapshot();
        assert_eq!(snapshot.phase, Phase::Running, "{:?}", snapshot.error);
        let status = snapshot.engine.expect("a running engine has a status");
        let format_seen = notices
            .iter()
            .any(|notice| matches!(notice, SessionNotice::FormatChanged { endpoint: 0, .. }));
        if format_seen || status.endpoints[0].format.is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "no engine status arrived");
        std::thread::sleep(Duration::from_millis(5));
    }
    supervisor.handle(Intent::Disable).unwrap();
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Ready);
    assert_eq!(snapshot.engine, None);
    assert!(snapshot.notes.is_empty(), "{:?}", snapshot.notes);
    assert_eq!(shared.get().roles, previous_roles());
}

#[test]
fn restore_point_round_trips_and_rejects_other_versions() {
    let point = RestorePoint {
        ours: id(OURS),
        previous: DefaultRoles {
            console: Some(id(REALTEK)),
            multimedia: None,
            communications: Some(id(HEADSET)),
        },
        fallback: Some(id(DAC)),
        app_pid: 4_242,
    };
    let json = point.to_json();
    assert!(!json.contains('\n'));
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["ours"], OURS);
    assert_eq!(value["previous"]["console"], REALTEK);
    assert!(value["previous"]["multimedia"].is_null());
    assert_eq!(value["previous"]["communications"], HEADSET);
    assert_eq!(value["fallback"], DAC);
    assert_eq!(value["app_pid"], 4_242);
    assert_eq!(RestorePoint::from_json(&json), Ok(point.clone()));

    let without_fallback = RestorePoint {
        fallback: None,
        ..point
    };
    let json = without_fallback.to_json();
    assert!(serde_json::from_str::<serde_json::Value>(&json).unwrap()["fallback"].is_null());
    assert_eq!(RestorePoint::from_json(&json), Ok(without_fallback));

    let mut newer = value.clone();
    newer["version"] = 2.into();
    assert_eq!(
        RestorePoint::from_json(&newer.to_string()),
        Err(RestorePointError::Version(2))
    );
    let mut unversioned = value.clone();
    unversioned.as_object_mut().unwrap().remove("version");
    assert_eq!(
        RestorePoint::from_json(&unversioned.to_string()),
        Err(RestorePointError::Field("version"))
    );
    let mut roleless = value;
    roleless["previous"]
        .as_object_mut()
        .unwrap()
        .remove("communications");
    assert_eq!(
        RestorePoint::from_json(&roleless.to_string()),
        Err(RestorePointError::Field("communications"))
    );
    let mut fallbackless = roleless;
    fallbackless["previous"]["communications"] = HEADSET.into();
    fallbackless.as_object_mut().unwrap().remove("fallback");
    assert_eq!(
        RestorePoint::from_json(&fallbackless.to_string()),
        Err(RestorePointError::Field("fallback"))
    );
    assert!(matches!(
        RestorePoint::from_json("not json"),
        Err(RestorePointError::Json(_))
    ));
}

/// A directory under the system's temporary directory, removed on drop.
struct TestDirectory(std::path::PathBuf);

impl TestDirectory {
    fn new(name: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "ssr-supervisor-{name}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn join(&self, name: &str) -> std::path::PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_wav(path: &std::path::Path, channels: &[Vec<f32>]) {
    let frames = channels.iter().map(Vec::len).max().unwrap();
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: channels.len() as u16,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for frame in 0..frames {
        for channel in channels {
            writer
                .write_sample(*channel.get(frame).unwrap_or(&0.0))
                .unwrap();
        }
    }
    writer.finalize().unwrap();
}

/// A folder BRIR set with one audible stereo WAV per name.
fn brir_folder(directory: &TestDirectory, folder: &str, names: &[&str]) -> std::path::PathBuf {
    let path = directory.join(folder);
    std::fs::create_dir_all(&path).unwrap();
    for name in names {
        write_wav(
            &path.join(format!("{name}.wav")),
            &[vec![0.5, 0.0, 0.0], vec![0.0, 0.25, 0.0]],
        );
    }
    path
}

/// An Impulcifer `hesuvi.wav` with the seven 7.1 directions, in a folder.
fn hesuvi(directory: &TestDirectory, folder: &str) -> std::path::PathBuf {
    let path = directory.join(folder);
    std::fs::create_dir_all(&path).unwrap();
    let path = path.join("hesuvi.wav");
    let tracks: Vec<Vec<f32>> = (0..14)
        .map(|track| {
            let mut samples = vec![0.0; 32];
            samples[track + 1] = 0.25;
            samples
        })
        .collect();
    write_wav(&path, &tracks);
    path
}

fn secs_correction(
    directory: &TestDirectory,
    name: &str,
    title: &str,
    stereo: bool,
) -> std::path::PathBuf {
    let folder = directory.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    write_wav(&folder.join("filter.wav"), &[vec![0.5, 0.0, 0.0]]);
    let (description, channels, mapping) = if stereo {
        (
            "inputs 0 L, 1 R | outputs 0 L, 1 R | synthetic",
            2,
            "[{dest: 0, sources: [{channel: 0}]}, {dest: 1, sources: [{channel: 1}]}]",
        )
    } else {
        (
            "inputs 0 L, 1 R, 2 LFE | outputs 0 L, 1 R, 2 SW1 | synthetic",
            3,
            "[{dest: 0, sources: [{channel: 0}]}, {dest: 1, sources: [{channel: 1}]}, {dest: 2, sources: [{channel: 2}]}]",
        )
    };
    let path = folder.join("correction.yml");
    std::fs::write(
        &path,
        format!(
            r#"title: "{title}"
description: "{description}"
devices:
  samplerate: 48000
  capture: {{channels: {channels}}}
  playback: {{channels: {channels}}}
filters:
  room: {{type: Conv, parameters: {{type: Wav, filename: filter.wav, channel: 0}}}}
mixers:
  route_out: {{channels: {{in: {channels}, out: {channels}}}, mapping: {mapping}}}
pipeline:
  - {{type: Filter, channels: [0], names: [room]}}
  - {{type: Mixer, name: route_out}}
"#
        ),
    )
    .unwrap();
    path
}

fn view_names(snapshot: &ssr_types::app::Snapshot) -> Vec<String> {
    snapshot
        .profile
        .directions
        .iter()
        .map(|direction| direction.name.clone())
        .collect()
}

fn launched(shared: &Shared) -> Vec<String> {
    shared.get().launched.clone()
}

#[test]
fn profile_kinds_switch_live_and_keep_each_kinds_settings() {
    use ssr_types::app::{ProfileKind, SpeakerLayout};

    let directory = TestDirectory::new("profile-kinds");
    let room = brir_folder(&directory, "room", &["FL", "FR"]);
    let (shared, mut supervisor) = running(ready_pc());

    supervisor.handle(Intent::ImportBrir(room.clone())).unwrap();
    assert!(shared.get().applied.is_empty());
    assert_eq!(supervisor.settings().profile.kind, ProfileKind::Speakers);
    assert_eq!(
        supervisor.settings().profile.headphones.brir,
        Some(room.clone())
    );

    supervisor
        .handle(Intent::SetSpeakerLayout(SpeakerLayout::Surround51))
        .unwrap();
    assert_eq!(
        std::mem::take(&mut shared.get().applied),
        ["prepared Speakers 6ch [L R C Ls Rs]"]
    );

    supervisor
        .handle(Intent::SetProfileKind(ProfileKind::Headphones))
        .unwrap();
    assert_eq!(supervisor.snapshot().profile.kind, ProfileKind::Headphones);
    assert_eq!(
        supervisor.snapshot().profile.headphones.brir,
        Some(room.to_string_lossy().into_owned())
    );
    assert_eq!(
        std::mem::take(&mut shared.get().applied),
        ["prepared Headphones 2ch [FL FR]"]
    );

    supervisor
        .handle(Intent::SetProfileKind(ProfileKind::Speakers))
        .unwrap();
    assert_eq!(supervisor.snapshot().profile.kind, ProfileKind::Speakers);
    assert_eq!(
        supervisor.snapshot().profile.speakers.layout,
        Some(SpeakerLayout::Surround51)
    );
}

#[test]
fn headphones_need_a_brir_set() {
    use ssr_types::app::ProfileKind;

    let (_shared, mut supervisor) = start(ready_pc(), dac_settings());
    assert_eq!(
        supervisor.handle(Intent::SetProfileKind(ProfileKind::Headphones)),
        Err(error("먼저 BRIR 세트를 가져오십시오.", NextAction::None))
    );
    assert_eq!(supervisor.settings().profile.kind, ProfileKind::Speakers);
}

#[test]
fn import_brir_while_speakers_play_only_stores_it() {
    let directory = TestDirectory::new("store-brir");
    let room = brir_folder(&directory, "room", &["FL", "FR"]);
    let (shared, mut supervisor) = running(ready_pc());

    supervisor.handle(Intent::ImportBrir(room.clone())).unwrap();
    assert!(shared.get().applied.is_empty());
    assert_eq!(supervisor.settings().profile.headphones.brir, Some(room));
    assert_eq!(
        supervisor.snapshot().profile.kind,
        ssr_types::app::ProfileKind::Speakers
    );
}

#[test]
fn speaker_layout_applies_live_only_to_the_kind_in_use() {
    use ssr_types::app::{ProfileKind, SpeakerLayout};

    let directory = TestDirectory::new("layout-kind");
    let room = brir_folder(&directory, "room", &["FL", "FR"]);
    let (shared, mut supervisor) = running(ready_pc());
    supervisor.handle(Intent::ImportBrir(room)).unwrap();
    supervisor
        .handle(Intent::SetProfileKind(ProfileKind::Headphones))
        .unwrap();
    shared.get().applied.clear();

    supervisor
        .handle(Intent::SetSpeakerLayout(SpeakerLayout::Surround714))
        .unwrap();
    assert!(shared.get().applied.is_empty());
    assert_eq!(
        supervisor.settings().profile.speakers.layout,
        SpeakerLayout::Surround714
    );

    supervisor
        .handle(Intent::SetProfileKind(ProfileKind::Speakers))
        .unwrap();
    assert_eq!(
        shared.get().applied,
        ["prepared Speakers 12ch [L R C Lrs Rrs Lss Rss Tfl Tfr Trl Trr]"]
    );
}

#[test]
fn import_correction_while_headphones_play_only_stores_it() {
    use ssr_types::app::ProfileKind;

    let directory = TestDirectory::new("store-correction");
    let room = brir_folder(&directory, "room", &["FL", "FR"]);
    let correction = secs_correction(&directory, "secs", "Desk correction", false);
    let (shared, mut supervisor) = running(ready_pc());
    supervisor.handle(Intent::ImportBrir(room)).unwrap();
    supervisor
        .handle(Intent::SetProfileKind(ProfileKind::Headphones))
        .unwrap();
    shared.get().applied.clear();

    supervisor
        .handle(Intent::ImportCorrection(correction.clone()))
        .unwrap();
    assert_eq!(supervisor.settings().profile.kind, ProfileKind::Headphones);
    assert_eq!(
        supervisor.settings().profile.speakers.correction,
        Some(correction.clone())
    );
    let view = supervisor.snapshot().profile;
    assert_eq!(view.kind, ProfileKind::Headphones);
    assert_eq!(view.speakers.correction.unwrap().title, "Desk correction");
    assert_eq!(
        view.speakers
            .channels
            .iter()
            .map(|channel| (channel.name.as_str(), channel.subwoofer))
            .collect::<Vec<_>>(),
        [("L", false), ("R", false), ("SW1", true)]
    );
    assert!(shared.get().applied.is_empty());
}

#[test]
fn import_correction_while_speakers_play_applies_live() {
    let directory = TestDirectory::new("live-correction");
    let correction = secs_correction(&directory, "secs", "Desk correction", false);
    let (shared, mut supervisor) = running(ready_pc());

    supervisor
        .handle(Intent::ImportCorrection(correction.clone()))
        .unwrap();
    assert_eq!(shared.get().applied, ["prepared Speakers 3ch [L R]"]);
    assert_eq!(
        supervisor.settings().profile.speakers.correction,
        Some(correction)
    );
    let view = supervisor.snapshot().profile;
    assert_eq!(view.name, "Desk correction");
    assert_eq!(
        view.directions
            .iter()
            .map(|direction| direction.name.as_str())
            .collect::<Vec<_>>(),
        ["L", "R"]
    );
    assert_eq!(
        view.speakers
            .channels
            .iter()
            .map(|channel| (channel.name.as_str(), channel.channel, channel.subwoofer))
            .collect::<Vec<_>>(),
        [("L", 0, false), ("R", 1, false), ("SW1", 2, true)]
    );
}

#[test]
fn clear_correction_returns_to_the_stored_layout_live() {
    use ssr_types::app::SpeakerLayout;

    let directory = TestDirectory::new("clear-correction");
    let correction = secs_correction(&directory, "secs", "Desk correction", false);
    let (shared, mut supervisor) = running(ready_pc());
    supervisor
        .handle(Intent::SetSpeakerLayout(SpeakerLayout::Surround51))
        .unwrap();
    supervisor
        .handle(Intent::ImportCorrection(correction))
        .unwrap();
    shared.get().applied.clear();

    supervisor.handle(Intent::ClearCorrection).unwrap();
    assert_eq!(supervisor.settings().profile.speakers.correction, None);
    assert_eq!(supervisor.snapshot().profile.name, "5.1");
    assert_eq!(
        shared.get().applied,
        ["prepared Speakers 6ch [L R C Ls Rs]"]
    );
}

#[test]
fn an_unreadable_correction_is_refused_and_changes_nothing() {
    let directory = TestDirectory::new("bad-correction");
    let (shared, mut supervisor) = running(ready_pc());
    let settings = supervisor.settings().clone();
    let view = supervisor.snapshot().profile;

    for path in [directory.join("missing.yml"), {
        let path = secs_correction(&directory, "bad", "Bad", false);
        let yaml = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, yaml.replace("channel: 0", "channel: 1")).unwrap();
        path
    }] {
        let failed = supervisor
            .handle(Intent::ImportCorrection(path))
            .unwrap_err();
        assert!(failed.message.starts_with("보정 파일을 읽지 못했습니다: "));
        assert_eq!(failed.next, NextAction::None);
        assert_eq!(supervisor.settings(), &settings);
        assert_eq!(supervisor.snapshot().profile, view);
        assert!(shared.get().applied.is_empty());
    }
}

#[test]
fn layout_changes_under_a_correction_are_stored_only() {
    use ssr_types::app::SpeakerLayout;

    let directory = TestDirectory::new("layout-under-correction");
    let correction = secs_correction(&directory, "secs", "Desk", false);
    let (shared, mut supervisor) = running(ready_pc());
    supervisor
        .handle(Intent::ImportCorrection(correction))
        .unwrap();
    shared.get().applied.clear();
    shared.take_calls();

    supervisor
        .handle(Intent::SetSpeakerLayout(SpeakerLayout::Surround714))
        .unwrap();
    assert_eq!(
        supervisor.settings().profile.speakers.layout,
        SpeakerLayout::Surround714
    );
    assert_eq!(supervisor.snapshot().profile.name, "Desk");
    assert!(shared.get().applied.is_empty());
    assert!(shared.take_calls().is_empty());
}

#[test]
fn the_correction_survives_a_restart_and_a_missing_file_becomes_a_note() {
    let directory = TestDirectory::new("restart-correction");
    let correction = secs_correction(&directory, "secs", "Desk", false);
    let mut settings = dac_settings();
    settings.profile.speakers.correction = Some(correction.clone());
    let (_shared, supervisor) = start(ready_pc(), settings.clone());
    assert_eq!(supervisor.snapshot().profile.name, "Desk");
    assert_eq!(
        supervisor
            .snapshot()
            .profile
            .speakers
            .correction
            .unwrap()
            .path,
        correction.to_string_lossy()
    );

    std::fs::remove_file(&correction).unwrap();
    let (shared, mut supervisor) = start(ready_pc(), settings);
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.profile.name, "correction");
    assert_eq!(snapshot.profile.speakers.channels, []);
    assert!(snapshot.notes[0].starts_with("보정 파일을 읽지 못했습니다: "));
    assert!(supervisor.handle(Intent::Enable).is_err());
    assert!(shared.take_calls().is_empty());
}

#[test]
fn the_device_must_accept_the_speaker_layout() {
    use ssr_types::app::{ProfileKind, SpeakerLayout};

    let mut world = ready_pc();
    world.layout_support = Ok(false);
    let (shared, mut supervisor) = start(world, dac_settings());

    supervisor
        .handle(Intent::SetSpeakerLayout(SpeakerLayout::Stereo))
        .unwrap();
    assert!(shared.take_calls().is_empty());

    assert_eq!(
        supervisor.handle(Intent::SetSpeakerLayout(SpeakerLayout::Surround714)),
        Err(error(
            "고른 출력 장치가 독점 모드에서 7.1.4 배치(12채널)를 지원하지 않습니다. 다른 배치나 출력 장치를 고르십시오.",
            NextAction::None,
        ))
    );
    assert_eq!(shared.take_calls(), ["supports {dac} 12ch 0x2d63f"]);

    let directory = TestDirectory::new("layout-probe-kind");
    let room = brir_folder(&directory, "room", &["FL", "FR"]);
    supervisor.handle(Intent::ImportBrir(room)).unwrap();
    supervisor
        .handle(Intent::SetProfileKind(ProfileKind::Headphones))
        .unwrap();
    supervisor
        .handle(Intent::SetSpeakerLayout(SpeakerLayout::Surround714))
        .unwrap();
    assert_eq!(
        supervisor.handle(Intent::SetProfileKind(ProfileKind::Speakers)),
        Err(error(
            "고른 출력 장치가 독점 모드에서 7.1.4 배치(12채널)를 지원하지 않습니다. 다른 배치나 출력 장치를 고르십시오.",
            NextAction::None,
        ))
    );

    let mut settings = dac_settings();
    settings.profile.speakers.layout = SpeakerLayout::Surround714;
    let mut world = ready_pc();
    world.layout_support = Ok(false);
    let (shared, mut supervisor) = start(world, settings);
    assert_eq!(
        supervisor.handle(Intent::Enable),
        Err(error(
            "고른 출력 장치가 독점 모드에서 7.1.4 배치(12채널)를 지원하지 않습니다. 다른 배치나 출력 장치를 고르십시오.",
            NextAction::None,
        ))
    );
    assert_eq!(shared.take_calls(), ["supports {dac} 12ch 0x2d63f"]);
    assert_eq!(shared.get().roles, previous_roles());

    let mut world = ready_pc();
    world.layout_support = Err(HostError::Unsupported);
    let (_shared, mut supervisor) = start(world, dac_settings());
    supervisor
        .handle(Intent::SetSpeakerLayout(SpeakerLayout::Surround714))
        .unwrap();
    supervisor.handle(Intent::Enable).unwrap();
}

#[test]
fn the_device_must_accept_the_correction_channels() {
    use ssr_types::app::ProfileKind;

    let directory = TestDirectory::new("correction-probe");
    let correction = secs_correction(&directory, "three", "Three", false);
    let stereo = secs_correction(&directory, "stereo", "Stereo", true);

    let mut world = ready_pc();
    world.layout_support = Ok(false);
    let (shared, mut supervisor) = start(world, dac_settings());
    let unsupported = error(
        "고른 출력 장치가 독점 모드에서 보정 파일에 지정된 3채널 출력을 지원하지 않습니다. 다른 출력 장치를 고르십시오.",
        NextAction::None,
    );
    assert_eq!(
        supervisor.handle(Intent::ImportCorrection(correction.clone())),
        Err(unsupported.clone())
    );
    assert_eq!(shared.take_calls(), ["supports {dac} 3ch 0xb"]);
    supervisor.handle(Intent::ImportCorrection(stereo)).unwrap();
    assert!(shared.take_calls().is_empty());

    let room = brir_folder(&directory, "room", &["FL", "FR"]);
    supervisor.handle(Intent::ImportBrir(room)).unwrap();
    supervisor
        .handle(Intent::SetProfileKind(ProfileKind::Headphones))
        .unwrap();
    supervisor
        .handle(Intent::ImportCorrection(correction.clone()))
        .unwrap();
    shared.take_calls();
    assert_eq!(
        supervisor.handle(Intent::SetProfileKind(ProfileKind::Speakers)),
        Err(unsupported.clone())
    );
    assert_eq!(shared.take_calls(), ["supports {dac} 3ch 0xb"]);

    let mut settings = dac_settings();
    settings.profile.speakers.correction = Some(correction);
    let mut world = ready_pc();
    world.layout_support = Ok(false);
    let (shared, mut supervisor) = start(world, settings);
    assert_eq!(supervisor.handle(Intent::Enable), Err(unsupported));
    assert_eq!(shared.take_calls(), ["supports {dac} 3ch 0xb"]);
    assert_eq!(shared.get().roles, previous_roles());
}

#[test]
fn each_kind_keeps_its_own_gain() {
    use ssr_types::app::ProfileKind;

    let directory = TestDirectory::new("kind-gains");
    let room = brir_folder(&directory, "room", &["FL", "FR"]);
    let (_shared, mut supervisor) = start(ready_pc(), dac_settings());

    supervisor.handle(Intent::SetGain(-3.0)).unwrap();
    supervisor.handle(Intent::ImportBrir(room)).unwrap();
    supervisor
        .handle(Intent::SetProfileKind(ProfileKind::Headphones))
        .unwrap();
    supervisor.handle(Intent::SetGain(2.0)).unwrap();
    supervisor
        .handle(Intent::SetProfileKind(ProfileKind::Speakers))
        .unwrap();
    assert_eq!(supervisor.snapshot().profile.gain_db, -3.0);
    assert_eq!(supervisor.settings().profile.speakers.gain_db, Some(-3.0));
    assert_eq!(supervisor.settings().profile.headphones.gain_db, Some(2.0));
}

#[test]
fn settings_from_before_kinds_migrate() {
    use ssr_types::app::ProfileKind;

    let old: Settings = serde_json::from_str(
        r#"{"profile":{"brir":"C:\\BRIR\\hesuvi.wav","gain_db":-4.5,"headphone_fir":"C:\\old.wav"}}"#,
    )
    .unwrap();
    assert_eq!(old.profile.kind, ProfileKind::Headphones);
    assert_eq!(
        old.profile.headphones.brir,
        Some(std::path::PathBuf::from(r"C:\BRIR\hesuvi.wav"))
    );
    assert_eq!(old.profile.headphones.gain_db, Some(-4.5));
    assert_eq!(old.profile.speakers.gain_db, None);

    let old: Settings = serde_json::from_str(r#"{"profile":{"gain_db":-2.0}}"#).unwrap();
    assert_eq!(old.profile.kind, ProfileKind::Speakers);
    assert_eq!(old.profile.speakers.gain_db, Some(-2.0));
    assert_eq!(old.profile.speakers.correction, None);
    assert!(
        !serde_json::to_string(&old)
            .unwrap()
            .contains("headphone_fir")
    );
}

#[test]
fn old_settings_without_profile_use_defaults() {
    let mut json = serde_json::to_value(dac_settings()).unwrap();
    json.as_object_mut().unwrap().remove("profile");
    let settings: Settings = serde_json::from_value(json).unwrap();
    assert_eq!(settings.profile, supervisor::ProfileSettings::default());

    let partial: Settings =
        serde_json::from_str(r#"{"output":null,"profile":{"gain_db":-2.0}}"#).unwrap();
    assert_eq!(partial.profile.kind, ssr_types::app::ProfileKind::Speakers);
    assert_eq!(partial.profile.speakers.gain_db, Some(-2.0));
    assert_eq!(partial.profile.speakers.correction, None);

    let (shared, mut supervisor) = start(ready_pc(), settings);
    assert_eq!(
        supervisor.snapshot().profile,
        supervisor::default_profile_view()
    );
    supervisor.handle(Intent::Enable).unwrap();
    assert_eq!(launched(&shared), ["speakers 2.0 0 dB [L R]"]);
}

#[test]
fn profile_file_refuses_correction_intents() {
    use ssr_types::app::{ProfileKind, SpeakerLayout};

    let directory = TestDirectory::new("profile-file-intents");
    brir_folder(&directory, "set", &["FL", "FR"]);
    let imported = brir_folder(&directory, "imported", &["FL", "FR"]);
    let file = directory.join("profile.toml");
    std::fs::write(
        &file,
        "name = \"desk\"\ngain_db = 4.0\nlimiter_ceiling_dbfs = -1.0\n\n[brir]\nfolder = \"set\"\n",
    )
    .unwrap();
    let shared = Shared::new(ready_pc());
    let mut settings = dac_settings();
    settings.profile.headphones.gain_db = Some(-2.0);
    let mut supervisor = Supervisor::new(
        FakeHost(shared.clone()),
        FakeLauncher(shared.clone()),
        Box::new(FakePlugins(shared.clone())),
        Config {
            profile_file: Some(file),
            ..this_build()
        },
        settings.clone(),
    );
    assert_eq!(supervisor.snapshot().profile.gain_db, -2.0);
    assert!(supervisor.snapshot().profile.from_file);

    let file_wins = error(
        "상태 폴더의 profile.toml이 우선합니다. 그 파일을 지우거나 고치십시오.",
        NextAction::None,
    );
    for intent in [
        Intent::SetProfileKind(ProfileKind::Speakers),
        Intent::ImportBrir(imported),
        Intent::SetSpeakerLayout(SpeakerLayout::Surround51),
    ] {
        assert_eq!(supervisor.handle(intent), Err(file_wins.clone()));
    }
    assert_eq!(
        supervisor.handle(Intent::ImportCorrection("correction.yml".into())),
        Err(file_wins.clone())
    );
    assert_eq!(
        supervisor.handle(Intent::ClearCorrection),
        Err(file_wins.clone())
    );
    assert_eq!(supervisor.settings(), &settings);

    supervisor.handle(Intent::SetGain(-1.0)).unwrap();
    supervisor.handle(Intent::Enable).unwrap();
    assert_eq!(
        launched(&shared),
        ["headphones desk -1 dB lfe=Mute [FL FR]"]
    );
}

#[test]
fn live_profile_changes_are_all_or_nothing() {
    use ssr_types::app::{ProfileKind, SpeakerLayout};

    let (shared, mut supervisor) = running(ready_pc());
    let settings = supervisor.settings().clone();
    let view = supervisor.snapshot().profile;
    shared.get().apply_errors.push_back(SessionError::Rejected);
    assert_eq!(
        supervisor.handle(Intent::SetSpeakerLayout(SpeakerLayout::Surround51)),
        Err(error(
            "엔진이 변경을 받지 못했습니다. 잠시 뒤 다시 시도하십시오.",
            NextAction::Retry,
        ))
    );
    assert_eq!(supervisor.settings(), &settings);
    assert_eq!(supervisor.snapshot().profile, view);
    assert!(shared.get().applied.is_empty());

    let directory = TestDirectory::new("live-all-or-nothing");
    let set = brir_folder(&directory, "set", &["FL", "FR"]);
    supervisor.handle(Intent::ImportBrir(set.clone())).unwrap();
    supervisor
        .handle(Intent::SetProfileKind(ProfileKind::Headphones))
        .unwrap();
    let truncated = directory.join("truncated");
    std::fs::create_dir_all(&truncated).unwrap();
    let cut = truncated.join("FL.wav");
    write_wav(&cut, &[vec![0.5; 4_096], vec![0.25; 4_096]]);
    let bytes = std::fs::read(&cut).unwrap();
    std::fs::write(&cut, &bytes[..bytes.len() / 2]).unwrap();
    let settings = supervisor.settings().clone();
    let view = supervisor.snapshot().profile;
    let failed = supervisor
        .handle(Intent::ImportBrir(truncated))
        .unwrap_err();
    assert!(failed.message.starts_with("프로필을 준비하지 못했습니다: "));
    assert_eq!(supervisor.settings(), &settings);
    assert_eq!(supervisor.snapshot().profile, view);
    assert_eq!(supervisor.settings().profile.headphones.brir, Some(set));

    supervisor
        .handle(Intent::SetProfileKind(ProfileKind::Speakers))
        .unwrap();
    let correction = secs_correction(&directory, "correction", "Desk", false);
    let settings = supervisor.settings().clone();
    let view = supervisor.snapshot().profile;
    shared.get().apply_errors.push_back(SessionError::Rejected);
    assert_eq!(
        supervisor.handle(Intent::ImportCorrection(correction)),
        Err(error(
            "엔진이 변경을 받지 못했습니다. 잠시 뒤 다시 시도하십시오.",
            NextAction::Retry,
        ))
    );
    assert_eq!(supervisor.settings(), &settings);
    assert_eq!(supervisor.snapshot().profile, view);
}

#[test]
fn a_missing_brir_set_stops_enable_before_the_roles_move() {
    use ssr_types::app::ProfileKind;

    let directory = TestDirectory::new("missing-set");
    let mut settings = dac_settings();
    settings.profile.kind = ProfileKind::Headphones;
    settings.profile.headphones.brir = Some(directory.join("moved").join("hrir.wav"));
    let (shared, mut supervisor) = start(ready_pc(), settings.clone());
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.profile.kind, ProfileKind::Headphones);
    assert!(snapshot.notes[0].starts_with("BRIR 세트를 읽지 못했습니다: "));

    let failed = supervisor.handle(Intent::Enable).unwrap_err();
    assert!(failed.message.starts_with("BRIR 세트를 읽지 못했습니다: "));
    assert_eq!(supervisor.snapshot().phase, Phase::Ready);
    assert!(shared.take_calls().is_empty());
    assert!(shared.get().launched.is_empty());
    assert_eq!(supervisor.settings(), &settings);

    let set = brir_folder(&directory, "here", &["FL", "FR"]);
    supervisor.handle(Intent::ImportBrir(set)).unwrap();
    supervisor.handle(Intent::Enable).unwrap();
    assert_eq!(launched(&shared), ["headphones here 0 dB lfe=Mute [FL FR]"]);
}

#[test]
fn a_test_tone_is_checked_against_the_profile_the_engine_was_last_given() {
    use ssr_types::DirectionId;

    let directory = TestDirectory::new("test-tone-layout");
    let studio = hesuvi(&directory, "studio");
    let (shared, mut supervisor) = running(ready_pc());
    // The engine still reports its installed stereo set while a new one is
    // queued (EngineStatus.directions lags): the cached profile, which is the
    // layout the engine checks a test tone against, decides.
    shared.get().engine_directions = vec![DirectionView {
        id: DirectionId(0),
        name: "FL".into(),
        azimuth_deg: 30.0,
        elevation_deg: 0.0,
    }];
    supervisor
        .handle(Intent::TestDirection(DirectionId(1)))
        .unwrap();
    assert_eq!(
        supervisor.handle(Intent::TestDirection(DirectionId(2))),
        Err(error("선택한 방향을 찾을 수 없습니다.", NextAction::None))
    );

    // A seven-direction set handed to the engine: its directions are good
    // at once, before the engine reports them.
    supervisor.handle(Intent::ImportBrir(studio)).unwrap();
    supervisor
        .handle(Intent::SetProfileKind(
            ssr_types::app::ProfileKind::Headphones,
        ))
        .unwrap();
    supervisor
        .handle(Intent::TestDirection(DirectionId(6)))
        .unwrap();
    assert_eq!(
        supervisor.handle(Intent::TestDirection(DirectionId(7))),
        Err(error("선택한 방향을 찾을 수 없습니다.", NextAction::None))
    );
    let applied = shared.get().applied.clone();
    assert_eq!(applied.len(), 3, "{applied:?}");
    assert_eq!(applied[0], "test 1");
    assert_eq!(applied[2], "test 6");
}

#[test]
fn needs_prepared_rebuilds_the_profile_the_engine_plays_now() {
    let directory = TestDirectory::new("needs-prepared");
    let studio = hesuvi(&directory, "studio");
    let room = brir_folder(&directory, "room", &["FL", "FR"]);
    let (shared, mut supervisor) = running(ready_pc());

    // Asked at once after the launch: the stereo profile it was launched with.
    shared
        .get()
        .engine_polls
        .push_back(Ok(vec![SessionNotice::NeedsPrepared]));
    assert!(!supervisor.needs_prepared());
    assert_eq!(
        supervisor.tick(Instant::now()),
        [SessionNotice::NeedsPrepared]
    );
    // The tick only remembers it: the caller prepares (ADR 0033).
    assert!(supervisor.needs_prepared());
    assert!(shared.get().applied.is_empty());
    let notices = supervisor.prepare_again();
    assert!(
        matches!(
            notices.as_slice(),
            [SessionNotice::Prepared {
                kind: engine::OutputKind::Speakers,
                channels: 2,
                ..
            }]
        ),
        "{notices:?}"
    );
    assert!(!supervisor.needs_prepared());
    assert_eq!(
        std::mem::take(&mut shared.get().applied),
        ["prepared Speakers 2ch [L R]"]
    );
    assert!(supervisor.prepare_again().is_empty());

    // A set imported while running is what the next re-prepare builds, not
    // the profile the engine was launched with.
    supervisor.handle(Intent::SetGain(-6.0)).unwrap();
    supervisor
        .handle(Intent::ImportBrir(studio.clone()))
        .unwrap();
    supervisor
        .handle(Intent::SetProfileKind(
            ssr_types::app::ProfileKind::Headphones,
        ))
        .unwrap();
    shared.get().applied.clear();
    shared
        .get()
        .engine_polls
        .push_back(Ok(vec![SessionNotice::NeedsPrepared]));
    supervisor.tick(Instant::now());
    // A full queue: the output is prepared again on the next call.
    shared.get().apply_errors.push_back(SessionError::Rejected);
    assert_eq!(supervisor.prepare_again(), [SessionNotice::PrepareRetry]);
    assert!(supervisor.needs_prepared());
    assert!(matches!(
        supervisor.prepare_again().as_slice(),
        [SessionNotice::Prepared {
            kind: engine::OutputKind::Headphones,
            channels: 2,
            ..
        }]
    ));
    assert_eq!(
        std::mem::take(&mut shared.get().applied),
        ["prepared Headphones 2ch [FL SL BL FC FR SR BR]"]
    );

    // A profile handed over while one is owed answers it.
    shared
        .get()
        .engine_polls
        .push_back(Ok(vec![SessionNotice::NeedsPrepared]));
    supervisor.tick(Instant::now());
    supervisor.handle(Intent::ImportBrir(room.clone())).unwrap();
    assert!(!supervisor.needs_prepared());
    assert_eq!(
        std::mem::take(&mut shared.get().applied),
        ["prepared Headphones 2ch [FL FR]"]
    );

    // A set whose files went away cannot rebuild the output: as a failed
    // start, the engine stops, the roles go back, and the app is Faulted.
    std::fs::remove_dir_all(&room).unwrap();
    shared
        .get()
        .engine_polls
        .push_back(Ok(vec![SessionNotice::NeedsPrepared]));
    supervisor.tick(Instant::now());
    assert!(supervisor.prepare_again().is_empty());
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Faulted);
    let error = snapshot.error.expect("the fault is explained");
    assert!(
        error
            .message
            .starts_with("엔진이 멈췄습니다: output preparation failed: "),
        "{error:?}"
    );
    assert_eq!(error.next, NextAction::Retry);
    assert!(!supervisor.needs_prepared());
    assert!(shared.get().applied.is_empty());
    assert_eq!(shared.get().roles, previous_roles());
}

#[test]
fn a_reprepare_is_built_once_retried_and_refreshes_the_direction_cache() {
    use ssr_types::DirectionId;

    let directory = TestDirectory::new("reprepare-retry");
    let room = brir_folder(&directory, "room", &["FL", "FR"]);
    let (shared, mut supervisor) = start(ready_pc(), dac_settings());
    supervisor.handle(Intent::ImportBrir(room.clone())).unwrap();
    supervisor
        .handle(Intent::SetProfileKind(
            ssr_types::app::ProfileKind::Headphones,
        ))
        .unwrap();
    supervisor.handle(Intent::Enable).unwrap();
    write_wav(
        &room.join("FC.wav"),
        &[vec![0.5, 0.0, 0.0], vec![0.0, 0.25, 0.0]],
    );
    shared
        .get()
        .engine_polls
        .push_back(Ok(vec![SessionNotice::NeedsPrepared]));
    supervisor.tick(Instant::now());
    assert!(supervisor.prepare_work_pending());
    shared.get().apply_errors.extend([
        SessionError::Rejected,
        SessionError::Rejected,
        SessionError::Rejected,
    ]);

    assert_eq!(supervisor.prepare_again(), [SessionNotice::PrepareRetry]);
    assert!(!supervisor.prepare_work_pending());
    // If a retry prepared again this removal would fault the supervisor. The
    // already prepared three-direction output must instead survive it.
    std::fs::remove_file(room.join("FC.wav")).unwrap();
    for _ in 0..2 {
        assert_eq!(supervisor.prepare_again(), [SessionNotice::PrepareRetry]);
        assert!(!supervisor.prepare_work_pending());
    }
    assert!(matches!(
        supervisor.prepare_again().as_slice(),
        [SessionNotice::Prepared {
            kind: engine::OutputKind::Headphones,
            channels: 2,
            ..
        }]
    ));
    assert!(!supervisor.needs_prepared());
    assert_eq!(view_names(&supervisor.snapshot()), ["FL", "FR", "FC"]);
    supervisor
        .handle(Intent::TestDirection(DirectionId(2)))
        .unwrap();
    assert_eq!(
        shared.get().applied,
        [
            "prepared Headphones 2ch [FL FR FC]".to_owned(),
            "test 2".to_owned(),
        ]
    );
}

#[test]
fn watching_profile_file_presence_unlocks_and_relocks_imports() {
    let directory = TestDirectory::new("watch-profile-file");
    let set = brir_folder(&directory, "set", &["FL", "FR"]);
    let imported = brir_folder(&directory, "imported", &["FL", "FR", "FC"]);
    let file = directory.join("profile.toml");
    std::fs::write(
        &file,
        "name = \"desk\"\ngain_db = 0.0\nlimiter_ceiling_dbfs = -1.0\n\n[brir]\nfolder = \"set\"\n",
    )
    .unwrap();
    let shared = Shared::new(ready_pc());
    let mut supervisor = Supervisor::new(
        FakeHost(shared.clone()),
        FakeLauncher(shared.clone()),
        Box::new(FakePlugins(shared.clone())),
        Config {
            profile_file: Some(file.clone()),
            ..this_build()
        },
        dac_settings(),
    );
    assert!(supervisor.snapshot().profile.from_file);

    std::fs::remove_file(&file).unwrap();
    supervisor.watch_profile_file();
    assert!(!supervisor.snapshot().profile.from_file);
    supervisor.handle(Intent::ImportBrir(imported)).unwrap();
    supervisor
        .handle(Intent::SetProfileKind(
            ssr_types::app::ProfileKind::Headphones,
        ))
        .unwrap();
    assert_eq!(view_names(&supervisor.snapshot()), ["FL", "FR", "FC"]);

    std::fs::write(
        &file,
        "name = \"desk\"\ngain_db = 0.0\nlimiter_ceiling_dbfs = -1.0\n\n[brir]\nfolder = \"set\"\n",
    )
    .unwrap();
    supervisor.watch_profile_file();
    assert!(supervisor.snapshot().profile.from_file);
    assert_eq!(view_names(&supervisor.snapshot()), ["FL", "FR"]);
    assert_eq!(
        supervisor.handle(Intent::ImportBrir(set)),
        Err(error(
            "상태 폴더의 profile.toml이 우선합니다. 그 파일을 지우거나 고치십시오.",
            NextAction::None
        ))
    );
}

#[test]
fn language_defaults_overrides_resets_and_rerenders_errors_in_every_phase() {
    let (shared, mut supervisor) = start(ready_pc(), Settings::default());
    assert_eq!(supervisor.language(), Language::Ko);
    assert_eq!(supervisor.snapshot().language, Language::Ko);

    shared.get().platform.os_build = 18_363;
    supervisor.handle(Intent::BeginSetup).unwrap_err();
    assert!(
        supervisor
            .snapshot()
            .error
            .unwrap()
            .message
            .starts_with("Windows 10 2004")
    );
    supervisor
        .handle(Intent::SetLanguage(Some(Language::En)))
        .unwrap();
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.language, Language::En);
    assert!(
        snapshot
            .error
            .unwrap()
            .message
            .starts_with("Windows 10 version 2004")
    );
    supervisor.handle(Intent::SetLanguage(None)).unwrap();
    assert_eq!(supervisor.snapshot().language, Language::Ko);

    let welcome = start(ready_pc(), Settings::default()).1;

    let mut not_test_signing_world = ready_pc();
    not_test_signing_world.platform.test_signing = false;
    let mut not_test_signing = start(not_test_signing_world, Settings::default()).1;
    not_test_signing.handle(Intent::BeginSetup).unwrap_err();

    let mut driver_world = ready_pc();
    driver_world.driver = DriverState::NotInstalled;
    let mut driver = start(driver_world, Settings::default()).1;
    driver.handle(Intent::BeginSetup).unwrap();

    let mut reboot_world = ready_pc();
    reboot_world.driver = DriverState::RebootRequired;
    let mut reboot = start(reboot_world, Settings::default()).1;
    reboot.handle(Intent::BeginSetup).unwrap_err();

    let mut choose_output = start(ready_pc(), Settings::default()).1;
    choose_output.handle(Intent::BeginSetup).unwrap();

    let ready = start(ready_pc(), dac_settings()).1;
    let running_machine = running(ready_pc()).1;

    let mut faulted_world = ready_pc();
    faulted_world
        .engine_polls
        .push_back(Err(SessionError::Rejected));
    let mut faulted = running(faulted_world).1;
    faulted.tick(Instant::now());

    let mut cases = vec![
        welcome,
        supervisor,
        not_test_signing,
        driver,
        reboot,
        choose_output,
        ready,
        running_machine,
        faulted,
    ];
    for supervisor in &mut cases {
        let phase = supervisor.snapshot().phase;
        assert_eq!(
            supervisor.handle(Intent::SetLanguage(Some(Language::En))),
            Ok(()),
            "{phase:?}"
        );
        assert_eq!(supervisor.snapshot().phase, phase);
    }

    let mut faulted = running(ready_pc()).1;
    faulted
        .handle(Intent::Disable)
        .expect("disable makes the machine Ready");
    faulted.handle(Intent::Enable).unwrap();
    faulted
        .handle(Intent::SetLanguage(Some(Language::En)))
        .unwrap();
    faulted.handle(Intent::Quit).unwrap();
    assert_eq!(
        faulted.handle(Intent::SetLanguage(Some(Language::Ko))),
        Err(error(
            "That action is not available right now.",
            NextAction::None
        ))
    );
}

#[test]
fn default_language_and_failed_profile_errors_follow_language_changes() {
    let mut config = this_build();
    config.default_language = Language::En;
    let shared = Shared::new(ready_pc());
    let mut supervisor = Supervisor::new(
        FakeHost(shared.clone()),
        FakeLauncher(shared.clone()),
        Box::new(FakePlugins(shared.clone())),
        config,
        dac_settings(),
    );
    assert_eq!(supervisor.language(), Language::En);
    assert_eq!(supervisor.snapshot().profile.name, "2.0");

    let failed = supervisor
        .handle(Intent::ImportBrir("not-a-brir.txt".into()))
        .unwrap_err();
    assert_eq!(
        failed.message,
        "Choose hesuvi.wav, hrir.wav, or a folder of per-direction WAV files."
    );
    supervisor
        .handle(Intent::SetLanguage(Some(Language::Ko)))
        .unwrap();
    assert_eq!(
        supervisor.snapshot().error.unwrap().message,
        "hesuvi.wav, hrir.wav 또는 방향별 WAV 폴더를 고르십시오."
    );
}

// ---- Upmixers (ADR 0035 with its 2026-10-16 correction, ADR 0021) ----

const PLUGIN: &str = r"C:\Program Files\Common Files\VST3\Example Upmixer.vst3";

fn choose_plugin(endpoint: u16) -> Intent {
    Intent::ChoosePlugin(ssr_types::app::PluginChoice {
        endpoint,
        path: PLUGIN.into(),
    })
}

fn set_upmixer(endpoint: u16, choice: ssr_types::app::UpmixerChoice) -> Intent {
    Intent::SetUpmixer(ssr_types::app::UpmixerSetting { endpoint, choice })
}

fn set_apply(kind: ssr_types::app::ProfileKind, on: bool) -> Intent {
    Intent::SetUpmixApply(ssr_types::app::UpmixApply { kind, on })
}

fn take_log(shared: &Shared) -> Vec<String> {
    std::mem::take(&mut shared.get().plugins.log)
}

fn take_applied(shared: &Shared) -> Vec<String> {
    std::mem::take(&mut shared.get().applied)
}

/// What the fake host's upmixer becomes in the engine at `rate`.
fn stage(endpoint: u16, rate: u32) -> String {
    format!("upmixer {endpoint} {rate} Hz [L R] -> [L R C LFE Ls Rs]")
}

/// Ready on the DAC with every endpoint explicitly off but `endpoint`, which
/// upmixes with the example plugin; the logs start empty.
fn ready_with_plugin(world: World, endpoint: u16) -> (Shared, Machine) {
    use ssr_types::app::UpmixerChoice;

    let (shared, mut supervisor) = start(world, dac_settings());
    for other in 0..3 {
        supervisor
            .handle(set_upmixer(other, UpmixerChoice::Off))
            .unwrap();
    }
    supervisor.handle(choose_plugin(endpoint)).unwrap();
    supervisor
        .handle(set_upmixer(endpoint, UpmixerChoice::Plugin))
        .unwrap();
    take_log(&shared);
    shared.take_calls();
    (shared, supervisor)
}

/// [`ready_with_plugin`], then enabled: the host is up and its stage in the
/// engine; the logs start empty.
fn running_with_plugin(world: World, endpoint: u16) -> (Shared, Machine) {
    let (shared, mut supervisor) = ready_with_plugin(world, endpoint);
    supervisor.handle(Intent::Enable).unwrap();
    assert_eq!(
        take_log(&shared),
        [format!("start {endpoint} Example Upmixer 48000")]
    );
    assert_eq!(take_applied(&shared), [stage(endpoint, 48_000)]);
    shared.take_calls();
    supervisor.take_tick_changes();
    (shared, supervisor)
}

/// `ui_contract.rs`'s `plugin_upmix`: `endpoint` goes through the example
/// plugin (stereo to 5.1, 64 frames of latency), the others are explicitly
/// off. The fixtures round the latency to 23.3 ms.
fn fixture_upmix(endpoint: usize, state: ssr_types::app::UpmixState) -> ssr_types::app::UpmixView {
    use ssr_types::app::{
        EndpointUpmixView, PluginFormat, PluginView, UpmixState, UpmixView, UpmixerChoice,
    };

    let running = matches!(state, UpmixState::Running | UpmixState::Fallback);
    let mut endpoints: Vec<_> = (0..3)
        .map(|endpoint| EndpointUpmixView {
            endpoint,
            choice: Some(UpmixerChoice::Off),
            effective: UpmixerChoice::Off,
            plugin: None,
            state: UpmixState::Stopped,
            bypass: None,
            latency_ms: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
        })
        .collect();
    let view = &mut endpoints[endpoint];
    view.choice = Some(UpmixerChoice::Plugin);
    view.effective = UpmixerChoice::Plugin;
    view.plugin = Some(PluginView {
        path: PLUGIN.to_owned(),
        format: PluginFormat::Vst3,
        name: "Example Upmixer".to_owned(),
        vendor: "Example Audio".to_owned(),
    });
    view.state = state;
    view.latency_ms = running.then_some(23.3);
    view.inputs = ["L", "R"].map(str::to_owned).to_vec();
    view.outputs = ["L", "R", "C", "LFE", "Ls", "Rs"]
        .map(str::to_owned)
        .to_vec();
    UpmixView {
        endpoints,
        headphones: true,
        speakers: true,
        first_party_available: false,
    }
}

/// The snapshot's upmix view with each latency checked against `ms` and
/// rounded as the fixtures write it.
fn upmix_shape(supervisor: &Machine, ms: f32) -> ssr_types::app::UpmixView {
    let mut view = supervisor.snapshot().upmix;
    for endpoint in &mut view.endpoints {
        if let Some(latency) = endpoint.latency_ms {
            assert!((latency - ms).abs() < 1e-3, "{latency} ms, not {ms} ms");
            endpoint.latency_ms = Some(23.3);
        }
    }
    view
}

/// Two blocks, 64 frames and 2 ms at 48 kHz.
const LATENCY_MS: f32 = (960.0 + 64.0 + 96.0) / 48.0;

fn stereo_pcm(rate: u32) -> ssr_types::SourceFormat {
    ssr_types::SourceFormat::Pcm(ssr_types::PcmFormat {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 32,
        valid_bits: 32,
        sample_type: ssr_types::SampleType::Float,
        channel_mask: 0x3,
    })
}

fn format_changed(endpoint: u16, format: ssr_types::SourceFormat) -> SessionNotice {
    SessionNotice::FormatChanged {
        endpoint,
        epoch: 1,
        format,
        unsupported: None,
    }
}

#[test]
fn upmix_settings_default_and_older_files_migrate() {
    use ssr_types::app::{PluginFormat, ProfileKind, UpmixerChoice};
    use supervisor::upmix::{DEFAULT_UPMIXER, EndpointUpmixSettings, UpmixSettings};

    assert_eq!(DEFAULT_UPMIXER, UpmixerChoice::Off);
    let defaults = UpmixSettings::default();
    assert!(defaults.headphones && defaults.speakers);
    assert!(
        defaults
            .endpoints
            .iter()
            .all(|endpoint| *endpoint == EndpointUpmixSettings::default())
    );
    assert!(defaults.applies(ProfileKind::Headphones) && defaults.applies(ProfileKind::Speakers));
    assert_eq!(defaults.effective(2), UpmixerChoice::Off);

    // Settings written before ADR 0035 have no upmix at all.
    let old: Settings = serde_json::from_str(
        r#"{"language":"ko","output":{"id":"{dac}","name":"DAC"},"profile":{"kind":"speakers"}}"#,
    )
    .unwrap();
    assert_eq!(old.upmix, UpmixSettings::default());
    // A partial table keeps what it says and defaults the rest.
    let partial: Settings = serde_json::from_str(
        r#"{"upmix":{"speakers":false,"endpoints":[{"choice":"plugin"},{},{}]}}"#,
    )
    .unwrap();
    assert!(partial.upmix.headphones);
    assert!(!partial.upmix.speakers);
    assert_eq!(
        partial.upmix.endpoints[0].choice,
        Some(UpmixerChoice::Plugin)
    );
    assert_eq!(partial.upmix.endpoints[1], EndpointUpmixSettings::default());
    // A plugin stored before its buses were known loads without them.
    let plugin: Settings = serde_json::from_str(
        r#"{"upmix":{"endpoints":[{},{"choice":"plugin","plugin":{"path":"C:\\up.dll","format":"vst2","name":"Up","vendor":"V"}},{}]}}"#,
    )
    .unwrap();
    let stored = plugin.upmix.endpoints[1].plugin.as_ref().unwrap();
    assert_eq!(stored.format, PluginFormat::Vst2);
    assert_eq!(stored.class, None);
    assert!(stored.inputs.is_empty() && stored.outputs.is_empty());

    // What the supervisor writes reads back the same.
    let (_shared, mut supervisor) = start(ready_pc(), dac_settings());
    supervisor.handle(choose_plugin(2)).unwrap();
    supervisor
        .handle(set_upmixer(2, UpmixerChoice::Plugin))
        .unwrap();
    supervisor
        .handle(set_apply(ProfileKind::Headphones, false))
        .unwrap();
    let json = serde_json::to_string(supervisor.settings()).unwrap();
    let read: Settings = serde_json::from_str(&json).unwrap();
    assert_eq!(&read, supervisor.settings());
    assert_eq!(
        read.upmix.endpoints[2].plugin.as_ref().unwrap().class,
        Some([7; 16])
    );
    assert!(!read.upmix.headphones);
}

#[test]
fn upmix_intents_refuse_endpoint_three_the_first_party_upmixer_and_the_wizard() {
    use ssr_types::app::{Language, ProfileKind, UpmixerChoice};
    use supervisor::text::Message;

    let cannot = error(
        &Message::EndpointCannotUpmix.render(Language::Ko),
        NextAction::None,
    );
    let (shared, mut supervisor) = start(ready_pc(), dac_settings());
    assert_eq!(
        supervisor.handle(set_upmixer(3, UpmixerChoice::Plugin)),
        Err(cannot.clone())
    );
    assert_eq!(supervisor.handle(choose_plugin(3)), Err(cannot.clone()));
    assert_eq!(
        supervisor.handle(set_upmixer(0, UpmixerChoice::FirstParty)),
        Err(error(
            &Message::UpmixerUnavailable.render(Language::Ko),
            NextAction::None
        ))
    );
    assert_eq!(
        supervisor.handle(Intent::OpenPluginEditor(0)),
        Err(not_allowed())
    );
    // Refusals change nothing and leave no error behind.
    assert_eq!(supervisor.settings(), &dac_settings());
    assert_eq!(supervisor.snapshot().error, None);
    assert!(take_log(&shared).is_empty());

    // The wizard comes first.
    let (_shared, mut supervisor) = start(ready_pc(), Settings::default());
    for intent in [
        set_upmixer(0, UpmixerChoice::Plugin),
        choose_plugin(0),
        Intent::OpenPluginEditor(0),
        set_apply(ProfileKind::Speakers, false),
    ] {
        assert_eq!(supervisor.handle(intent), Err(not_allowed()));
    }
    assert_eq!(supervisor.settings(), &Settings::default());
}

#[test]
fn choose_plugin_scans_and_refuses_what_it_cannot_read() {
    use ssr_types::app::{Language, PluginFormat, UpmixerChoice};
    use supervisor::text::Message;

    let scan_failed = |detail: &str| {
        error(
            &Message::PluginScanFailed {
                detail: detail.into(),
            }
            .render(Language::Ko),
            NextAction::None,
        )
    };
    let (shared, mut supervisor) = start(ready_pc(), dac_settings());

    // Neither a .vst3 nor a .dll: no scan at all.
    let refused = supervisor.handle(Intent::ChoosePlugin(ssr_types::app::PluginChoice {
        endpoint: 1,
        path: "C:/Plugins/upmixer.exe".into(),
    }));
    assert_eq!(refused, Err(scan_failed("upmixer.exe")));
    assert_eq!(supervisor.snapshot().error, refused.err());
    assert!(take_log(&shared).is_empty());

    // The scan's own error, and a file without a class.
    shared
        .get()
        .plugins
        .scans
        .push_back(Err(PluginHostError::Failed(
            "LoadLibrary failed (193)".into(),
        )));
    assert_eq!(
        supervisor.handle(choose_plugin(1)),
        Err(scan_failed("LoadLibrary failed (193)"))
    );
    shared.get().plugins.scans.push_back(Ok(Vec::new()));
    // The file's name (on Linux a backslash path has no separators).
    let name = std::path::Path::new(PLUGIN)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(supervisor.handle(choose_plugin(1)), Err(scan_failed(&name)));
    assert_eq!(supervisor.settings(), &dac_settings());

    // A VST 2 .dll keeps no class; a .vst3 keeps the first.
    shared.get().plugins.scans.push_back(Ok(vec![
        PluginClass {
            id: [1; 16],
            name: "First".into(),
            vendor: "Vendor".into(),
        },
        PluginClass {
            id: [2; 16],
            name: "Second".into(),
            vendor: "Vendor".into(),
        },
    ]));
    supervisor.handle(choose_plugin(1)).unwrap();
    assert_eq!(supervisor.snapshot().error, None);
    let stored = supervisor.settings().upmix.endpoints[1]
        .plugin
        .clone()
        .unwrap();
    assert_eq!(
        (stored.format, stored.class, stored.name.as_str()),
        (PluginFormat::Vst3, Some([1; 16]), "First")
    );
    supervisor
        .handle(Intent::ChoosePlugin(ssr_types::app::PluginChoice {
            endpoint: 0,
            path: r"C:\VstPlugins\Upmix.DLL".into(),
        }))
        .unwrap();
    let stored = supervisor.settings().upmix.endpoints[0]
        .plugin
        .clone()
        .unwrap();
    assert_eq!((stored.format, stored.class), (PluginFormat::Vst2, None));
    assert_eq!(
        take_log(&shared),
        [
            format!("scan {PLUGIN} Vst3"),
            format!("scan {PLUGIN} Vst3"),
            format!("scan {PLUGIN} Vst3"),
            r"scan C:\VstPlugins\Upmix.DLL Vst2".to_owned(),
        ]
    );
    // Choosing a plugin is not choosing to upmix with it.
    assert_eq!(supervisor.settings().upmix.effective(1), UpmixerChoice::Off);
}

#[test]
fn a_plugin_choice_starts_the_host_and_installs_the_stage_while_running() {
    use ssr_types::app::{UpmixState, UpmixerChoice};

    // Chosen while Ready: nothing starts until the engine runs.
    let (shared, mut supervisor) = ready_with_plugin(ready_pc(), 1);
    let mut view = fixture_upmix(1, UpmixState::Stopped);
    view.endpoints[1].inputs.clear();
    view.endpoints[1].outputs.clear();
    assert_eq!(
        supervisor.snapshot().upmix,
        view,
        "the buses are unknown before a run"
    );

    supervisor.handle(Intent::Enable).unwrap();
    assert_eq!(take_log(&shared), ["start 1 Example Upmixer 48000"]);
    assert_eq!(take_applied(&shared), [stage(1, 48_000)]);
    assert_eq!(
        upmix_shape(&supervisor, LATENCY_MS),
        fixture_upmix(1, UpmixState::Running)
    );
    let stored = supervisor.settings().upmix.endpoints[1]
        .plugin
        .clone()
        .unwrap();
    assert_eq!(stored.inputs, ["L", "R"]);
    assert_eq!(stored.outputs, ["L", "R", "C", "LFE", "Ls", "Rs"]);

    // A quiet tick changes nothing.
    supervisor.tick(Instant::now());
    assert!(take_log(&shared).is_empty());
    assert!(take_applied(&shared).is_empty());

    // Turned off while running: the stage leaves and the host stops.
    supervisor
        .handle(set_upmixer(1, UpmixerChoice::Off))
        .unwrap();
    assert_eq!(take_applied(&shared), ["upmixer 1 off"]);
    assert_eq!(take_log(&shared), ["shutdown 1"]);
    supervisor.tick(Instant::now());
    assert_eq!(take_log(&shared), ["exited 1"]);
    assert_eq!(
        supervisor.snapshot().upmix.endpoints[1].state,
        UpmixState::Stopped
    );

    // Chosen while running: a plugin first, then the choice.
    let (shared, mut supervisor) = running(ready_pc());
    supervisor.handle(choose_plugin(0)).unwrap();
    assert_eq!(take_log(&shared), [format!("scan {PLUGIN} Vst3")]);
    supervisor
        .handle(set_upmixer(0, UpmixerChoice::Plugin))
        .unwrap();
    assert_eq!(take_log(&shared), ["start 0 Example Upmixer 48000"]);
    assert_eq!(take_applied(&shared), [stage(0, 48_000)]);
    assert_eq!(
        supervisor.snapshot().upmix.endpoints[0].state,
        UpmixState::Running
    );

    // Another plugin on the same endpoint: its own state, a new host.
    shared.get().plugins.scans.push_back(Ok(vec![PluginClass {
        id: [9; 16],
        name: "Other Upmixer".into(),
        vendor: "Other".into(),
    }]));
    supervisor.handle(choose_plugin(0)).unwrap();
    assert_eq!(
        take_log(&shared),
        [
            format!("scan {PLUGIN} Vst3"),
            "forget 0".to_owned(),
            "shutdown 0".to_owned(),
            "start 0 Other Upmixer 48000".to_owned(),
        ]
    );
    assert_eq!(take_applied(&shared), ["upmixer 0 off", &stage(0, 48_000)]);
}

#[test]
fn each_kind_keeps_its_switch_and_the_engine_takes_the_one_in_use() {
    use ssr_types::app::{ProfileKind, UpmixBypass, UpmixState};

    // Speakers off before the engine starts: it starts without upmixing.
    let (shared, mut supervisor) = ready_with_plugin(ready_pc(), 1);
    supervisor
        .handle(set_apply(ProfileKind::Speakers, false))
        .unwrap();
    assert!(!supervisor.settings().upmix.speakers);
    assert!(take_applied(&shared).is_empty());
    supervisor.handle(Intent::Enable).unwrap();
    assert_eq!(
        take_applied(&shared),
        ["upmix apply false".to_owned(), stage(1, 48_000)]
    );
    assert_eq!(take_log(&shared), ["start 1 Example Upmixer 48000"]);
    // The host stays up; the sound goes around it.
    let view = supervisor.snapshot().upmix;
    assert!(!view.speakers && view.headphones);
    assert_eq!(view.endpoints[1].state, UpmixState::Running);
    assert_eq!(view.endpoints[1].bypass, Some(UpmixBypass::KindOff));

    // The other kind's switch is only kept.
    supervisor
        .handle(set_apply(ProfileKind::Headphones, false))
        .unwrap();
    assert!(take_applied(&shared).is_empty());
    supervisor
        .handle(set_apply(ProfileKind::Speakers, true))
        .unwrap();
    assert_eq!(take_applied(&shared), ["upmix apply true"]);
    assert_eq!(supervisor.snapshot().upmix.endpoints[1].bypass, None);

    // Switching kinds brings the other kind's switch.
    let directory = TestDirectory::new("upmix-kinds");
    let studio = hesuvi(&directory, "studio");
    supervisor.handle(Intent::ImportBrir(studio)).unwrap();
    take_applied(&shared);
    supervisor
        .handle(Intent::SetProfileKind(ProfileKind::Headphones))
        .unwrap();
    let applied = take_applied(&shared);
    assert_eq!(applied.len(), 2, "{applied:?}");
    assert!(applied[0].starts_with("prepared Headphones"), "{applied:?}");
    assert_eq!(applied[1], "upmix apply false");
    supervisor
        .handle(Intent::SetProfileKind(ProfileKind::Speakers))
        .unwrap();
    assert_eq!(take_applied(&shared)[1], "upmix apply true");
    // None of it touched the host.
    assert_eq!(take_log(&shared), Vec::<String>::new());
    assert_eq!(
        supervisor.snapshot().upmix.endpoints[1].state,
        UpmixState::Running
    );
}

#[test]
fn a_full_change_queue_hands_stages_and_switches_over_on_the_next_tick() {
    use ssr_types::app::{ProfileKind, UpmixState};

    let (shared, mut supervisor) = ready_with_plugin(ready_pc(), 1);
    shared.get().apply_errors.push_back(SessionError::Rejected);
    supervisor.handle(Intent::Enable).unwrap();
    assert!(take_applied(&shared).is_empty());
    assert_eq!(
        supervisor.snapshot().upmix.endpoints[1].state,
        UpmixState::Starting
    );
    supervisor.tick(Instant::now());
    assert_eq!(take_applied(&shared), [stage(1, 48_000)]);
    assert_eq!(
        supervisor.snapshot().upmix.endpoints[1].state,
        UpmixState::Running
    );

    shared.get().apply_errors.push_back(SessionError::Rejected);
    supervisor
        .handle(set_apply(ProfileKind::Speakers, false))
        .unwrap();
    assert!(take_applied(&shared).is_empty());
    supervisor.tick(Instant::now());
    assert_eq!(take_applied(&shared), ["upmix apply false"]);

    shared.get().apply_errors.push_back(SessionError::Rejected);
    supervisor
        .handle(set_upmixer(1, ssr_types::app::UpmixerChoice::Off))
        .unwrap();
    assert!(take_applied(&shared).is_empty());
    supervisor.tick(Instant::now());
    assert_eq!(take_applied(&shared), ["upmixer 1 off"]);
    supervisor.tick(Instant::now());
    assert!(take_applied(&shared).is_empty());
}

#[test]
fn a_rate_change_restarts_the_host_at_the_new_rate() {
    use ssr_types::app::{UpmixState, UpmixerChoice};
    use ssr_types::{PcmFormat, SampleType, SourceFormat};

    let (shared, mut supervisor) = running_with_plugin(ready_pc(), 1);
    // The same rate, or another endpoint: nothing.
    shared.get().engine_polls.push_back(Ok(vec![
        format_changed(1, stereo_pcm(48_000)),
        format_changed(0, stereo_pcm(44_100)),
    ]));
    supervisor.tick(Instant::now());
    assert!(take_log(&shared).is_empty());
    assert!(take_applied(&shared).is_empty());

    shared
        .get()
        .engine_polls
        .push_back(Ok(vec![format_changed(1, stereo_pcm(96_000))]));
    supervisor.tick(Instant::now());
    assert_eq!(take_log(&shared), ["restart 1 96000"]);
    assert_eq!(take_applied(&shared), [stage(1, 96_000)]);
    let view = &supervisor.snapshot().upmix.endpoints[1];
    assert_eq!(view.state, UpmixState::Running);
    let latency = view.latency_ms.unwrap();
    assert!(
        (latency - (1920.0 + 64.0 + 192.0) / 96.0).abs() < 1e-3,
        "{latency}"
    );

    // Native-surround PCM is never upmixed, and an unsupported format plays
    // nothing: neither moves the host.
    let surround = SourceFormat::Pcm(PcmFormat {
        channels: 6,
        sample_rate: 44_100,
        bits_per_sample: 16,
        valid_bits: 16,
        sample_type: SampleType::Int,
        channel_mask: 0x3f,
    });
    shared.get().engine_polls.push_back(Ok(vec![
        format_changed(1, surround),
        SessionNotice::FormatChanged {
            endpoint: 1,
            epoch: 2,
            format: stereo_pcm(88_200),
            unsupported: Some(ssr_types::UnsupportedReason::Format),
        },
    ]));
    supervisor.tick(Instant::now());
    assert!(take_log(&shared).is_empty());

    // A host started later starts at the endpoint's rate.
    supervisor.handle(choose_plugin(0)).unwrap();
    supervisor
        .handle(set_upmixer(0, UpmixerChoice::Plugin))
        .unwrap();
    assert_eq!(
        take_log(&shared),
        [
            format!("scan {PLUGIN} Vst3"),
            "start 0 Example Upmixer 44100".to_owned()
        ]
    );
    assert_eq!(take_applied(&shared), [stage(0, 44_100)]);
}

#[test]
fn a_respawn_reinstalls_the_stage_and_a_new_latency_restarts_the_host() {
    use ssr_types::app::UpmixState;

    let (shared, mut supervisor) = running_with_plugin(ready_pc(), 1);
    // The host died: the engine plays the fallback while the watch waits.
    shared.get().plugins.events[1].push_back(PluginEvent::Fallback(true));
    supervisor.tick(Instant::now());
    assert!(supervisor.take_tick_changes().snapshot);
    assert_eq!(
        upmix_shape(&supervisor, LATENCY_MS),
        fixture_upmix(1, UpmixState::Fallback)
    );
    shared.get().plugins.events[1].push_back(PluginEvent::NeedsRespawn);
    supervisor.tick(Instant::now());
    assert_eq!(take_log(&shared), ["restart 1 48000"]);
    assert_eq!(take_applied(&shared), [stage(1, 48_000)]);
    assert_eq!(
        upmix_shape(&supervisor, LATENCY_MS),
        fixture_upmix(1, UpmixState::Running)
    );

    // A new plugin latency: a new host and a stage with the new delay.
    shared.get().plugins.events[1].push_back(PluginEvent::LatencyChanged(128));
    supervisor.tick(Instant::now());
    assert_eq!(take_log(&shared), ["restart 1 48000"]);
    assert_eq!(take_applied(&shared), [stage(1, 48_000)]);
    let latency = supervisor.snapshot().upmix.endpoints[1].latency_ms.unwrap();
    assert!(
        (latency - (960.0 + 128.0 + 96.0) / 48.0).abs() < 1e-3,
        "{latency}"
    );

    // Blocks the engine misses read as the fallback too.
    let installed = shared.get().stages[1].as_ref().unwrap().status();
    shared.get().upmix_status[1] = Some(engine::UpmixStatus {
        upmixing: true,
        mode: engine::UpmixMode::Fallback,
        ..installed
    });
    assert_eq!(
        supervisor.snapshot().upmix.endpoints[1].state,
        UpmixState::Fallback
    );
}

#[test]
fn a_disabled_plugin_stays_failed_until_chosen_again() {
    use ssr_types::app::{Language, UpmixState, UpmixerChoice};
    use supervisor::text::Message;

    let (shared, mut supervisor) = running_with_plugin(ready_pc(), 1);
    shared.get().plugins.events[1].push_back(PluginEvent::Disabled);
    supervisor.tick(Instant::now());
    assert!(supervisor.take_tick_changes().snapshot);
    assert_eq!(take_log(&shared), ["shutdown 1"]);
    assert_eq!(take_applied(&shared), ["upmixer 1 off"]);
    let failed = UpmixState::Failed(
        Message::PluginDisabled {
            name: "Example Upmixer".into(),
        }
        .render(Language::Ko),
    );
    assert_eq!(
        supervisor.snapshot().upmix,
        fixture_upmix(1, failed.clone())
    );
    supervisor.tick(Instant::now());
    assert_eq!(take_log(&shared), ["exited 1"]);

    // Off and on again keeps it off for this session.
    supervisor.handle(Intent::Disable).unwrap();
    supervisor.handle(Intent::Enable).unwrap();
    assert!(take_log(&shared).is_empty());
    supervisor.handle(Intent::Disable).unwrap();
    // The fixture `ready-upmix-failed`.
    assert_eq!(
        supervisor.snapshot().upmix,
        fixture_upmix(1, failed.clone())
    );
    // In English too.
    supervisor
        .handle(Intent::SetLanguage(Some(Language::En)))
        .unwrap();
    assert_eq!(
        supervisor.snapshot().upmix.endpoints[1].state,
        UpmixState::Failed(
            "The upmixer plugin stopped three times within 10 minutes, so it is off until the app restarts: Example Upmixer".into()
        )
    );

    // Choosing again clears it.
    supervisor
        .handle(set_upmixer(1, UpmixerChoice::Plugin))
        .unwrap();
    assert_eq!(
        supervisor.snapshot().upmix,
        fixture_upmix(1, UpmixState::Stopped)
    );
    supervisor.handle(Intent::Enable).unwrap();
    assert_eq!(take_log(&shared), ["start 1 Example Upmixer 48000"]);
}

#[test]
fn a_host_that_fails_to_start_or_is_no_upmixer_fails_with_its_sentence() {
    use ssr_types::app::{Language, UpmixState, UpmixerChoice};
    use supervisor::text::Message;

    let mut world = ready_pc();
    world
        .plugins
        .start_errors
        .push_back(PluginHostError::Failed("the host did not say Hello".into()));
    let (shared, mut supervisor) = ready_with_plugin(world, 1);
    supervisor.handle(Intent::Enable).unwrap();
    assert_eq!(take_log(&shared), ["start 1 Example Upmixer 48000"]);
    assert!(take_applied(&shared).is_empty());
    assert_eq!(
        supervisor.snapshot().upmix.endpoints[1].state,
        UpmixState::Failed(
            Message::PluginScanFailed {
                detail: "the host did not say Hello".into()
            }
            .render(Language::Ko)
        )
    );
    // The engine still runs.
    assert_eq!(supervisor.snapshot().phase, Phase::Running);

    shared.get().plugins.not_upmixer = true;
    supervisor
        .handle(set_upmixer(1, UpmixerChoice::Plugin))
        .unwrap();
    assert_eq!(
        take_log(&shared),
        ["start 1 Example Upmixer 48000", "shutdown 1"]
    );
    assert!(take_applied(&shared).is_empty());
    assert_eq!(
        supervisor.snapshot().upmix.endpoints[1].state,
        UpmixState::Failed(
            Message::PluginNotUpmixer {
                name: "Example Upmixer".into()
            }
            .render(Language::Ko)
        )
    );

    // A host that comes up later and then fails reports it from a poll.
    shared.get().plugins.not_upmixer = false;
    shared.get().plugins.slow = true;
    supervisor
        .handle(set_upmixer(1, UpmixerChoice::Plugin))
        .unwrap();
    take_log(&shared);
    shared.get().plugins.events[1].push_back(PluginEvent::StartFailed("timed out".into()));
    supervisor.tick(Instant::now());
    assert_eq!(
        supervisor.snapshot().upmix.endpoints[1].state,
        UpmixState::Failed(
            Message::PluginScanFailed {
                detail: "timed out".into()
            }
            .render(Language::Ko)
        )
    );
}

#[test]
fn a_slow_host_is_starting_until_it_reports_running() {
    use ssr_types::app::UpmixState;

    let mut world = ready_pc();
    world.plugins.slow = true;
    let (shared, mut supervisor) = ready_with_plugin(world, 1);
    supervisor.handle(Intent::Enable).unwrap();
    assert_eq!(take_log(&shared), ["start 1 Example Upmixer 48000"]);
    assert!(take_applied(&shared).is_empty());
    assert_eq!(
        supervisor.snapshot().upmix.endpoints[1].state,
        UpmixState::Starting
    );
    // The editor waits for the host.
    assert_eq!(
        supervisor.handle(Intent::OpenPluginEditor(1)),
        Err(not_allowed())
    );
    supervisor.take_tick_changes();

    supervisor.tick(Instant::now());
    assert_eq!(supervisor.take_tick_changes(), Default::default());
    shared.get().plugins.events[1].push_back(PluginEvent::Running);
    supervisor.tick(Instant::now());
    assert_eq!(take_applied(&shared), [stage(1, 48_000)]);
    let changes = supervisor.take_tick_changes();
    assert!(changes.snapshot, "the screen hears of it");
    assert!(changes.settings, "the buses were learned");
    assert_eq!(
        upmix_shape(&supervisor, LATENCY_MS),
        fixture_upmix(1, UpmixState::Running)
    );
    supervisor.handle(Intent::OpenPluginEditor(1)).unwrap();
    assert_eq!(take_log(&shared), ["editor 1"]);
    assert_eq!(
        supervisor.handle(Intent::OpenPluginEditor(0)),
        Err(not_allowed())
    );
    assert_eq!(
        supervisor.handle(Intent::OpenPluginEditor(7)),
        Err(not_allowed())
    );
}

#[test]
fn stopping_the_engine_shuts_every_host_down() {
    use ssr_types::app::{UpmixState, UpmixerChoice};

    let (shared, mut supervisor) = running_with_plugin(ready_pc(), 0);
    supervisor.handle(choose_plugin(2)).unwrap();
    supervisor
        .handle(set_upmixer(2, UpmixerChoice::Plugin))
        .unwrap();
    take_log(&shared);
    take_applied(&shared);

    supervisor.handle(Intent::Disable).unwrap();
    assert_eq!(take_log(&shared), ["shutdown 0", "shutdown 2"]);
    // The stages went with the engine; nothing is handed to it.
    assert!(take_applied(&shared).is_empty());
    assert!(
        supervisor
            .snapshot()
            .upmix
            .endpoints
            .iter()
            .all(|view| view.state == UpmixState::Stopped && view.latency_ms.is_none())
    );
    // Ready keeps polling them until they exit.
    supervisor.tick(Instant::now());
    assert_eq!(take_log(&shared), ["exited 0", "exited 2"]);
    supervisor.tick(Instant::now());
    assert!(take_log(&shared).is_empty());

    // Quit waits for them before the app ends; a fault stops them too.
    supervisor.handle(Intent::Enable).unwrap();
    take_log(&shared);
    supervisor.handle(Intent::Quit).unwrap();
    assert_eq!(
        take_log(&shared),
        ["shutdown 0", "shutdown 2", "exited 0", "exited 2"]
    );
}

#[test]
fn an_engine_fault_shuts_the_hosts_down() {
    let (shared, mut supervisor) = running_with_plugin(ready_pc(), 1);
    shared
        .get()
        .engine_polls
        .push_back(Err(SessionError::Rejected));
    supervisor.tick(Instant::now());
    assert_eq!(supervisor.snapshot().phase, Phase::Faulted);
    assert_eq!(take_log(&shared), ["shutdown 1"]);
    supervisor.tick(Instant::now());
    assert_eq!(take_log(&shared), ["exited 1"]);
}

#[test]
fn pass_upmixes_like_the_other_endpoints() {
    use ssr_types::app::{UpmixBypass, UpmixState};

    let (shared, mut supervisor) = running_with_plugin(ready_pc(), 2);
    // The fixture `running-upmix-pass`.
    assert_eq!(
        upmix_shape(&supervisor, LATENCY_MS),
        fixture_upmix(2, UpmixState::Running)
    );
    // The fixture `running-upmix-native-surround`: the engine says why.
    let installed = shared.get().stages[2].as_ref().unwrap().status();
    shared.get().upmix_status[2] = Some(engine::UpmixStatus {
        bypass: Some(UpmixBypass::NativeSurround),
        ..installed
    });
    let mut expected = fixture_upmix(2, UpmixState::Running);
    expected.endpoints[2].bypass = Some(UpmixBypass::NativeSurround);
    assert_eq!(upmix_shape(&supervisor, LATENCY_MS), expected);

    shared
        .get()
        .engine_polls
        .push_back(Ok(vec![format_changed(2, stereo_pcm(192_000))]));
    supervisor.tick(Instant::now());
    assert_eq!(take_log(&shared), ["restart 2 192000"]);
    assert_eq!(take_applied(&shared), [stage(2, 192_000)]);
    supervisor.handle(Intent::OpenPluginEditor(2)).unwrap();
    assert_eq!(take_log(&shared), ["editor 2"]);
}

#[test]
fn a_profile_file_locks_no_upmix_intent() {
    use ssr_types::app::{ProfileKind, UpmixerChoice};

    let directory = TestDirectory::new("profile-file-upmix");
    brir_folder(&directory, "set", &["FL", "FR"]);
    let file = directory.join("profile.toml");
    std::fs::write(
        &file,
        "name = \"desk\"\ngain_db = 0.0\nlimiter_ceiling_dbfs = -1.0\n\n[brir]\nfolder = \"set\"\n",
    )
    .unwrap();
    let shared = Shared::new(ready_pc());
    let mut supervisor = Supervisor::new(
        FakeHost(shared.clone()),
        FakeLauncher(shared.clone()),
        Box::new(FakePlugins(shared.clone())),
        Config {
            profile_file: Some(file),
            ..this_build()
        },
        dac_settings(),
    );
    supervisor.handle(Intent::Enable).unwrap();
    // The file's headphones are the kind in use.
    assert_eq!(supervisor.snapshot().profile.kind, ProfileKind::Headphones);
    supervisor.handle(choose_plugin(1)).unwrap();
    supervisor
        .handle(set_upmixer(1, UpmixerChoice::Plugin))
        .unwrap();
    supervisor
        .handle(set_apply(ProfileKind::Headphones, false))
        .unwrap();
    supervisor.handle(Intent::OpenPluginEditor(1)).unwrap();
    assert_eq!(
        take_applied(&shared),
        [stage(1, 48_000), "upmix apply false".to_owned()]
    );
}
