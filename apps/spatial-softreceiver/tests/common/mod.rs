//! Fakes for the control thread's tests: a host whose PC is ready for the app,
//! an engine that runs quietly, a recording emitter and settings kept in
//! memory. Each shares its state with the test through an `Arc`, since the
//! control thread owns the fakes.

use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use spatial_softreceiver::control::{Emitter, SettingsStore};
use ssr_types::app::{OutputChoice, Snapshot};
use ssr_types::ports::PolicyState;
use ssr_types::{
    Arch, DefaultRoles, DeviceId, DeviceState, DriverState, EngineStatus, Host, HostError,
    OutputDevice, Platform, Role, SecureBoot, SetupOutcome, SetupTask,
};
use supervisor::machine::Config;
use supervisor::{EngineHandle, Launcher, SessionError, SessionNotice, Settings};

/// The user's output.
pub const DEV_A: &str = "dev-a";
pub const DEV_A_NAME: &str = "스피커";

pub fn id(text: &str) -> DeviceId {
    DeviceId(text.to_owned())
}

/// The PC is x64 whatever this test runs on.
pub fn config() -> Config {
    Config {
        arch: Arch::X64,
        ..Config::default()
    }
}

/// The host's state and every mutating call, in call order.
#[derive(Debug)]
pub struct World {
    pub roles: DefaultRoles,
    pub restore_point: Option<String>,
    pub calls: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct FakeHost(pub Arc<Mutex<World>>);

impl FakeHost {
    /// Every role on `dev-a`, no restore point.
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(World {
            roles: DefaultRoles {
                console: Some(id(DEV_A)),
                multimedia: Some(id(DEV_A)),
                communications: Some(id(DEV_A)),
            },
            restore_point: None,
            calls: Vec::new(),
        })))
    }

    pub fn world(&self) -> MutexGuard<'_, World> {
        self.0.lock().expect("the host's state")
    }
}

impl Host for FakeHost {
    /// A supported, test-signing PC.
    fn platform(&mut self) -> Platform {
        Platform {
            os_build: 26_100,
            arch: Arch::X64,
            test_signing: true,
            secure_boot: SecureBoot::Disabled,
        }
    }

    fn driver(&mut self) -> DriverState {
        DriverState::Installed {
            problem_code: 0,
            service_running: true,
            exclusive_policy: PolicyState::Applied,
        }
    }

    /// The user's speakers and our three endpoints, all active.
    fn outputs(&mut self) -> Vec<OutputDevice> {
        let device = |device_id: &str, name: &str, endpoint: Option<u16>| OutputDevice {
            id: id(device_id),
            name: name.to_owned(),
            endpoint,
            state: DeviceState::Active,
        };
        vec![
            device(DEV_A, DEV_A_NAME, None),
            device("ours-0", "Spatial SoftReceiver", Some(0)),
            device("ours-1", "Spatial SoftReceiver Music", Some(1)),
            device("ours-2", "Spatial SoftReceiver PASS", Some(2)),
        ]
    }

    fn default_roles(&mut self) -> DefaultRoles {
        self.world().roles.clone()
    }

    fn set_default_role(&mut self, role: Role, device: &DeviceId) -> Result<(), HostError> {
        let mut world = self.world();
        world.calls.push(format!("set {role:?} {}", device.0));
        world.roles.set(role, Some(device.clone()));
        Ok(())
    }

    fn run_setup(&mut self, task: &SetupTask) -> Result<SetupOutcome, HostError> {
        self.world().calls.push(format!("setup {task:?}"));
        Ok(SetupOutcome {
            reboot_required: false,
            message: String::new(),
        })
    }

    fn write_restore_point(&mut self, json: Option<&str>) -> Result<(), HostError> {
        let mut world = self.world();
        world.calls.push(
            match json {
                Some(_) => "write restore point",
                None => "remove restore point",
            }
            .to_owned(),
        );
        world.restore_point = json.map(str::to_owned);
        Ok(())
    }
}

/// Starts an engine that has nothing to say.
#[derive(Debug)]
pub struct FakeLauncher;

impl Launcher for FakeLauncher {
    fn launch(&mut self, _output: &OutputDevice) -> Result<Box<dyn EngineHandle>, SessionError> {
        Ok(Box::new(QuietEngine))
    }
}

/// Panics where the engine would start, after the supervisor took the roles.
#[derive(Debug)]
pub struct PanickingLauncher;

impl Launcher for PanickingLauncher {
    fn launch(&mut self, _output: &OutputDevice) -> Result<Box<dyn EngineHandle>, SessionError> {
        panic!("a scripted panic in the launcher");
    }
}

struct QuietEngine;

impl EngineHandle for QuietEngine {
    fn poll(&mut self) -> Result<Vec<SessionNotice>, SessionError> {
        Ok(Vec::new())
    }

    fn status(&self) -> EngineStatus {
        EngineStatus::default()
    }

    fn stop(self: Box<Self>) -> Result<(), SessionError> {
        Ok(())
    }
}

/// What the control thread sent.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Snapshot(Box<Snapshot>),
    Meters,
    Quit,
}

#[derive(Clone, Debug, Default)]
pub struct Recorder(pub Arc<Mutex<Vec<Event>>>);

impl Recorder {
    pub fn events(&self) -> Vec<Event> {
        self.0.lock().expect("the recorded events").clone()
    }

    pub fn snapshots(&self) -> Vec<Snapshot> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                Event::Snapshot(snapshot) => Some(*snapshot),
                _ => None,
            })
            .collect()
    }

    pub fn meters(&self) -> usize {
        self.count(&Event::Meters)
    }

    pub fn quits(&self) -> usize {
        self.count(&Event::Quit)
    }

    fn count(&self, wanted: &Event) -> usize {
        self.events()
            .iter()
            .filter(|event| *event == wanted)
            .count()
    }

    /// Waits up to `limit` for `done` to hold over the snapshots sent so far.
    pub fn wait_for_snapshots(&self, limit: Duration, done: impl Fn(&[Snapshot]) -> bool) -> bool {
        wait_until(limit, || done(&self.snapshots()))
    }
}

impl Emitter for Recorder {
    fn snapshot(&self, snapshot: &Snapshot) {
        self.push(Event::Snapshot(Box::new(snapshot.clone())));
    }

    fn meters(&self, _status: &EngineStatus) {
        self.push(Event::Meters);
    }

    fn quit(&self) {
        self.push(Event::Quit);
    }
}

impl Recorder {
    fn push(&self, event: Event) {
        self.0.lock().expect("the recorded events").push(event);
    }
}

/// Settings kept in memory, with the number of saves.
#[derive(Clone, Debug, Default)]
pub struct MemoryStore(pub Arc<Mutex<(Settings, usize)>>);

impl MemoryStore {
    pub fn with_output(id_text: &str, name: &str) -> Self {
        let store = Self::default();
        store.0.lock().expect("the stored settings").0 = Settings {
            output: Some(OutputChoice {
                id: id(id_text),
                name: name.to_owned(),
            }),
        };
        store
    }

    pub fn settings(&self) -> Settings {
        self.0.lock().expect("the stored settings").0.clone()
    }
}

impl SettingsStore for MemoryStore {
    fn load(&self) -> Settings {
        self.settings()
    }

    fn save(&self, settings: &Settings) {
        let mut stored = self.0.lock().expect("the stored settings");
        stored.0 = settings.clone();
        stored.1 += 1;
    }
}

/// Polls `done` every 2 ms for up to `limit`.
pub fn wait_until(limit: Duration, done: impl Fn() -> bool) -> bool {
    let started = Instant::now();
    loop {
        if done() {
            return true;
        }
        if started.elapsed() >= limit {
            return false;
        }
        thread::sleep(Duration::from_millis(2));
    }
}
