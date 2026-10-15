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
use ssr_types::app::{Intent, NextAction, OutputChoice, Phase, SetupStep, UserError};
use ssr_types::ports::PolicyState;
use ssr_types::{
    Arch, DefaultRoles, DeviceId, DeviceState, DriverState, EngineFault, EngineStatus, Host,
    HostError, OutputDevice, Platform, Role, SecureBoot, SetupOutcome, SetupTask,
};
use supervisor::machine::Config;
use supervisor::restore_point::{self, RestorePoint, RestorePointError};
use supervisor::{
    EngineHandle, Launcher, Session, SessionError, SessionNotice, Settings, Supervisor,
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
    calls: Vec<String>,
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
    fn launch(&mut self, output: &OutputDevice) -> Result<Box<dyn EngineHandle>, SessionError> {
        let mut world = self.0.get();
        world.calls.push(format!("launch {}", output.id.0));
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
        EngineStatus::default()
    }

    fn stop(self: Box<Self>) -> Result<(), SessionError> {
        self.0.get().calls.push("stop engine".into());
        Ok(())
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
        calls: Vec::new(),
    }
}

fn this_build() -> Config {
    Config {
        requires_test_signing: true,
        min_os_build: 19_041,
        arch: Arch::X64,
        fault_grace: Config::FAULT_GRACE,
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
        output: Some(dac_choice()),
    }
}

type Machine = Supervisor<FakeHost, FakeLauncher>;

fn start(world: World, settings: Settings) -> (Shared, Machine) {
    let shared = Shared::new(world);
    let supervisor = Supervisor::new(
        FakeHost(shared.clone()),
        FakeLauncher(shared.clone()),
        this_build(),
        settings,
    );
    (shared, supervisor)
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
            "드라이버 서명을 받아들이지 않았습니다(문제 코드 52). \
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
            "드라이버 서비스가 돌고 있지 않습니다. 복구를 실행하십시오.",
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
            "이 빌드는 x64용이지만 이 PC는 Arm64입니다.",
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
            "이 빌드는 x64용이지만 이 PC는 다른 아키텍처입니다.",
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
            output: Some(OutputChoice {
                id: id("{dac-before-reboot}"),
                name: DAC_NAME.into(),
            }),
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
    let lost = SessionNotice::Fault(EngineFault::OutputLost("the DAC was removed".into()));
    shared.get().engine_polls.push_back(Ok(vec![lost.clone()]));
    assert_eq!(supervisor.tick(start), [lost]);
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Running);
    assert_eq!(
        snapshot.error,
        Some(error(
            "출력 장치와의 연결이 끊어져 다시 연결하는 중입니다: the DAC was removed",
            NextAction::None
        ))
    );
    assert!(snapshot.roles_taken);
    assert!(snapshot.engine.is_some());
    assert!(supervisor.tick(start + Duration::from_secs(1)).is_empty());
    assert_eq!(supervisor.snapshot().phase, Phase::Running);

    // The engine reconnects 3 s later.
    shared.get().engine_polls.push_back(Ok(vec![
        SessionNotice::OutputReconnected,
        SessionNotice::FaultCleared,
    ]));
    assert_eq!(supervisor.tick(start + Duration::from_secs(3)).len(), 2);
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Running);
    assert_eq!(snapshot.error, None);
    assert_eq!(snapshot.notes, ["다시 연결했습니다."]);
    assert!(snapshot.roles_taken);

    // The grace no longer runs.
    supervisor.tick(start + Duration::from_secs(20));
    assert_eq!(supervisor.snapshot().phase, Phase::Running);

    // A lost capture says so in its own words.
    shared
        .get()
        .engine_polls
        .push_back(Ok(vec![SessionNotice::Fault(EngineFault::CaptureLost(
            "the ring closed".into(),
        ))]));
    supervisor.tick(start + Duration::from_secs(21));
    let snapshot = supervisor.snapshot();
    assert_eq!(snapshot.phase, Phase::Running);
    assert_eq!(
        snapshot.error,
        Some(error(
            "캡처 장치와의 연결이 끊어져 다시 연결하는 중입니다.",
            NextAction::None
        ))
    );

    // Nothing was stopped and no role moved.
    assert!(shared.take_calls().is_empty());
    assert_eq!(shared.get().roles, taken_roles());
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
    assert_eq!(
        supervisor.handle(Intent::SetGain(-3.0)),
        Err(error("아직 지원하지 않는 동작입니다.", NextAction::None))
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
fn unlisted_and_unwired_intents_leave_the_state_alone() {
    let (shared, mut supervisor) = start(ready_pc(), dac_settings());
    for intent in [Intent::BeginSetup, Intent::Recheck, Intent::Disable] {
        assert_eq!(supervisor.handle(intent), Err(not_allowed()));
        assert_eq!(supervisor.snapshot().phase, Phase::Ready);
    }
    for intent in [
        Intent::ImportBrir("room.wav".into()),
        Intent::ImportHeadphoneFir("hp.wav".into()),
        Intent::SwitchProfile("speakers".into()),
        Intent::SetGain(-3.0),
        Intent::TestDirection(ssr_types::DirectionId(0)),
        Intent::StartDiagnosticRecording,
    ] {
        assert_eq!(
            supervisor.handle(intent),
            Err(error("아직 지원하지 않는 동작입니다.", NextAction::None))
        );
        assert_eq!(supervisor.snapshot().phase, Phase::Ready);
    }

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
            "주 엔드포인트가 보이지 않습니다. 드라이버 상태를 확인하십시오.",
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
        output: Some(OutputChoice {
            id: id("{old-dac}"),
            name: "Old DAC".into(),
        }),
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
    fn launch(&mut self, _output: &OutputDevice) -> Result<Box<dyn EngineHandle>, SessionError> {
        let session = Session::start(
            common::config(),
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
