#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ssr_guard::heartbeat;
use ssr_guard::marker::{self, Marker, MarkerReason};
use ssr_guard::runner::{self, AppProcess, Exit, Platform, Timeout, restore_allowed};
use ssr_guard::show;
use ssr_types::app::Heartbeat;

const STALL: Duration = Duration::from_secs(2);
const TAKEOVER: Duration = Duration::from_secs(15);

#[derive(Clone)]
enum Event {
    Line(&'static str, Duration),
    Timeout(Duration),
    Eof(Duration),
}

struct FakeProcess {
    pid: u32,
    events: VecDeque<Event>,
    clock: Rc<RefCell<Instant>>,
    killed: Rc<RefCell<usize>>,
}

impl AppProcess for FakeProcess {
    fn next_line(&mut self, _wait: Duration) -> Result<Option<String>, Timeout> {
        match self.events.pop_front().expect("a scripted process event") {
            Event::Line(line, elapsed) => {
                *self.clock.borrow_mut() += elapsed;
                Ok(Some(line.to_owned()))
            }
            Event::Timeout(elapsed) => {
                *self.clock.borrow_mut() += elapsed;
                Err(Timeout)
            }
            Event::Eof(elapsed) => {
                *self.clock.borrow_mut() += elapsed;
                Ok(None)
            }
        }
    }

    fn kill(&mut self) {
        *self.killed.borrow_mut() += 1;
    }

    fn id(&self) -> u32 {
        self.pid
    }
}

/// Spawned processes get the pids 101, 102, ...
struct FakePlatform {
    clock: Rc<RefCell<Instant>>,
    processes: VecDeque<Result<VecDeque<Event>, String>>,
    /// What each restore gives back, in order, once the owner rule allows it.
    restores: VecDeque<Option<Vec<String>>>,
    /// The restore point's `app_pid`, when a restore point exists.
    owner: Option<u32>,
    /// The restore point appears this long after the start (the app takes
    /// the roles late).
    owner_after: Duration,
    start: Instant,
    restore_calls: Vec<u32>,
    markers: Vec<Marker>,
    spawns: Vec<bool>,
    killed: Rc<RefCell<usize>>,
}

impl FakePlatform {
    fn new(processes: Vec<Vec<Event>>, restores: Vec<Option<Vec<String>>>) -> Self {
        Self::with_spawns(processes.into_iter().map(Ok).collect(), restores)
    }

    fn with_spawns(
        processes: Vec<Result<Vec<Event>, String>>,
        restores: Vec<Option<Vec<String>>>,
    ) -> Self {
        let start = Instant::now();
        Self {
            clock: Rc::new(RefCell::new(start)),
            processes: processes
                .into_iter()
                .map(|result| result.map(VecDeque::from))
                .collect(),
            restores: restores.into(),
            owner: None,
            owner_after: Duration::ZERO,
            start,
            restore_calls: Vec::new(),
            markers: Vec::new(),
            spawns: Vec::new(),
            killed: Rc::new(RefCell::new(0)),
        }
    }

    fn owned_by(mut self, pid: u32) -> Self {
        self.owner = Some(pid);
        self
    }

    fn roles_taken_after(mut self, delay: Duration) -> Self {
        self.owner_after = delay;
        self
    }

    fn reasons(&self) -> Vec<MarkerReason> {
        self.markers.iter().map(|marker| marker.reason).collect()
    }
}

impl Platform for FakePlatform {
    fn spawn(&mut self, resume: bool) -> Result<Box<dyn AppProcess>, String> {
        self.spawns.push(resume);
        let events = self.processes.pop_front().expect("a scripted spawn")?;
        Ok(Box::new(FakeProcess {
            pid: 100 + self.spawns.len() as u32,
            events,
            clock: Rc::clone(&self.clock),
            killed: Rc::clone(&self.killed),
        }))
    }

    fn restore(&mut self, app: u32) -> Option<Vec<String>> {
        self.restore_calls.push(app);
        if let Some(owner) = self.owner
            && !restore_allowed(owner, app)
        {
            return None;
        }
        self.restores.pop_front().unwrap_or(None)
    }

    fn restore_point_owner(&self) -> Option<u32> {
        if self.now().saturating_duration_since(self.start) < self.owner_after {
            return None;
        }
        self.owner
    }

    fn write_marker(&mut self, marker: &Marker) {
        self.markers.push(marker.clone());
    }

    fn now(&self) -> Instant {
        *self.clock.borrow()
    }
}

const RUNNING_1: &str = r#"{"running":true,"output_periods":1,"capture_reads":1}"#;
const RUNNING_2: &str = r#"{"running":true,"output_periods":2,"capture_reads":2}"#;
const NOT_RUNNING: &str = r#"{"running":false,"output_periods":0,"capture_reads":0}"#;

fn line(text: &'static str) -> Event {
    Event::Line(text, Duration::ZERO)
}

fn timeout() -> Event {
    Event::Timeout(Duration::from_secs(1))
}

fn timeouts(count: usize) -> Vec<Event> {
    vec![timeout(); count]
}

fn run(platform: &mut FakePlatform) -> Exit {
    runner::run(platform, STALL, TAKEOVER)
}

#[test]
fn a_clean_exit_restores_nothing() {
    let mut platform = FakePlatform::new(
        vec![vec![line(RUNNING_1), Event::Eof(Duration::ZERO)]],
        vec![None],
    );
    assert_eq!(run(&mut platform), Exit::Clean);
    assert!(platform.markers.is_empty());
    assert_eq!(platform.spawns, vec![false]);
}

#[test]
fn an_exit_with_a_restore_point_restores_and_marks() {
    let mut platform = FakePlatform::new(
        vec![vec![line(RUNNING_1), Event::Eof(Duration::ZERO)]],
        vec![Some(vec!["복원 메모".to_owned()])],
    )
    .owned_by(101);
    assert_eq!(run(&mut platform), Exit::Restored(MarkerReason::AppExited));
    assert_eq!(platform.markers[0].reason, MarkerReason::AppExited);
    assert_eq!(platform.markers[0].notes, ["복원 메모"]);
    assert_eq!(platform.restore_calls, [101]);
}

#[test]
fn a_restore_point_of_another_instance_is_left_alone_on_exit() {
    // The guard died earlier and the app lived on; this new guard's app was
    // turned away by the single instance rule and exited. The restore point
    // names the live app, so nothing is restored.
    let mut platform = FakePlatform::new(
        vec![vec![Event::Eof(Duration::from_millis(500))]],
        vec![Some(vec!["되돌리면 안 됨".to_owned()])],
    )
    .owned_by(4242);
    assert_eq!(run(&mut platform), Exit::Clean);
    assert!(platform.markers.is_empty());
    assert_eq!(platform.restore_calls, [101]);
}

#[test]
fn a_guard_that_launched_nothing_restores_nothing() {
    // The app did not start at all; whatever restore point exists is not
    // this guard's to tell apart from a live instance's.
    let mut platform = FakePlatform::with_spawns(
        vec![Err("no such file".to_owned())],
        vec![Some(vec!["되돌리면 안 됨".to_owned()])],
    )
    .owned_by(4242);
    assert_eq!(
        run(&mut platform),
        Exit::Restored(MarkerReason::RestartFailed)
    );
    assert!(platform.markers.is_empty());
    assert!(platform.restore_calls.is_empty());
}

#[test]
fn a_stall_restarts_once_then_restores() {
    let mut platform = FakePlatform::new(
        vec![
            vec![line(RUNNING_1), timeout(), timeout()],
            vec![line(RUNNING_1), timeout(), timeout()],
        ],
        vec![Some(Vec::new()), Some(vec!["두 번째 복원".to_owned()])],
    );
    assert_eq!(
        run(&mut platform),
        Exit::Restored(MarkerReason::RestartFailed)
    );
    assert_eq!(platform.spawns, vec![false, true]);
    assert_eq!(*platform.killed.borrow(), 2);
    assert_eq!(
        platform.reasons(),
        [MarkerReason::StallRestarted, MarkerReason::RestartFailed]
    );
    assert_eq!(platform.restore_calls, [101, 102]);
}

#[test]
fn a_failed_respawn_keeps_the_restoration_notes() {
    let mut platform = FakePlatform::with_spawns(
        vec![
            Ok(vec![line(RUNNING_1), timeout(), timeout()]),
            Err("restart failed".to_owned()),
        ],
        vec![Some(vec!["복원 메모".to_owned()])],
    );
    assert_eq!(
        run(&mut platform),
        Exit::Restored(MarkerReason::RestartFailed)
    );
    assert_eq!(platform.markers[1].reason, MarkerReason::RestartFailed);
    assert_eq!(platform.markers[1].notes, ["복원 메모"]);
    // Both restores were on behalf of the app that was killed.
    assert_eq!(platform.restore_calls, [101, 101]);
}

#[test]
fn lines_while_not_running_are_not_a_stall() {
    let mut platform = FakePlatform::new(
        vec![vec![
            line(NOT_RUNNING),
            Event::Line(NOT_RUNNING, Duration::from_secs(3)),
            Event::Eof(Duration::ZERO),
        ]],
        vec![None],
    );
    assert_eq!(run(&mut platform), Exit::Clean);
    assert_eq!(platform.spawns, vec![false]);
}

#[test]
fn silence_while_running_is_a_stall() {
    let mut platform = FakePlatform::new(
        vec![
            vec![line(RUNNING_1), timeout(), timeout()],
            vec![line(RUNNING_1), line(RUNNING_2), Event::Eof(Duration::ZERO)],
        ],
        vec![None, None],
    );
    assert_eq!(run(&mut platform), Exit::Clean);
    assert_eq!(platform.spawns, vec![false, true]);
}

#[test]
fn a_slow_start_after_the_restart_is_not_a_stall() {
    let mut platform = FakePlatform::new(
        vec![
            vec![line(RUNNING_1), timeout(), timeout()],
            // The new app says nothing for three seconds, then runs.
            vec![
                timeout(),
                timeout(),
                timeout(),
                line(RUNNING_1),
                line(RUNNING_2),
                Event::Eof(Duration::ZERO),
            ],
        ],
        vec![None, None],
    );
    assert_eq!(run(&mut platform), Exit::Clean);
    assert_eq!(platform.spawns, vec![false, true]);
    assert_eq!(*platform.killed.borrow(), 1);
}

#[test]
fn a_stall_after_the_app_was_off_still_restores() {
    let mut platform = FakePlatform::new(
        vec![
            vec![line(RUNNING_1), timeout(), timeout()],
            // Restarted, then turned off, then on again, then stalled: the
            // one restart is spent.
            vec![
                line(RUNNING_1),
                line(NOT_RUNNING),
                Event::Line(NOT_RUNNING, Duration::from_secs(5)),
                line(RUNNING_1),
                timeout(),
                timeout(),
            ],
        ],
        vec![Some(Vec::new()), Some(vec!["두 번째 복원".to_owned()])],
    );
    assert_eq!(
        run(&mut platform),
        Exit::Restored(MarkerReason::RestartFailed)
    );
    assert_eq!(platform.spawns, vec![false, true]);
    assert_eq!(platform.markers[1].reason, MarkerReason::RestartFailed);
    assert_eq!(platform.markers[1].notes, ["두 번째 복원"]);
}

#[test]
fn a_slow_start_without_the_roles_is_waited_for() {
    // No restore point: however long the app takes to say anything, the
    // roles are not ours and nothing is decided.
    let mut events = timeouts(40);
    events.extend([line(RUNNING_1), line(RUNNING_2), Event::Eof(Duration::ZERO)]);
    let mut platform = FakePlatform::new(vec![events], vec![None]);
    assert_eq!(run(&mut platform), Exit::Clean);
    assert!(platform.markers.is_empty());
    assert_eq!(*platform.killed.borrow(), 0);
}

#[test]
fn an_app_that_holds_the_roles_but_never_runs_is_restored_without_a_restart() {
    // The app took the roles (its restore point names it) and its control
    // thread hung before the first Running heartbeat.
    let mut events = vec![line(NOT_RUNNING)];
    events.extend(timeouts(16));
    let mut platform =
        FakePlatform::new(vec![events], vec![Some(vec!["복원 메모".to_owned()])]).owned_by(101);
    assert_eq!(run(&mut platform), Exit::Restored(MarkerReason::Stuck));
    assert_eq!(platform.spawns, vec![false]);
    assert_eq!(*platform.killed.borrow(), 1);
    assert_eq!(platform.markers[0].reason, MarkerReason::Stuck);
    assert_eq!(platform.markers[0].notes, ["복원 메모"]);
    // The takeover silence is counted from the first silent look after the
    // last heartbeat: 15 timeouts of a second do not reach it, the 16th
    // does.
    assert_eq!(platform.restore_calls, [101]);
}

#[test]
fn the_takeover_clock_starts_when_the_roles_are_seen_held() {
    // Enable spent 16 s before taking the roles (a slow device query); the
    // app then gets the whole takeover silence from that point, not from
    // its last heartbeat.
    let mut events = vec![line(NOT_RUNNING)];
    events.extend(timeouts(40));
    let mut platform = FakePlatform::new(vec![events], vec![Some(Vec::new())])
        .owned_by(101)
        .roles_taken_after(Duration::from_secs(16));
    let start = platform.now();
    assert_eq!(run(&mut platform), Exit::Restored(MarkerReason::Stuck));
    assert_eq!(platform.now(), start + Duration::from_secs(31));
    assert_eq!(*platform.killed.borrow(), 1);
}

#[test]
fn a_stale_restore_point_of_another_app_does_not_start_the_takeover_clock() {
    // A restore point another process left behind is for that app's own
    // startup to deal with; this guard's slow app is not killed for it.
    let mut events = timeouts(20);
    events.extend([line(RUNNING_1), Event::Eof(Duration::ZERO)]);
    let mut platform = FakePlatform::new(vec![events], vec![None]).owned_by(4242);
    assert_eq!(run(&mut platform), Exit::Clean);
    assert!(platform.markers.is_empty());
    assert_eq!(*platform.killed.borrow(), 0);
}

#[test]
fn garbage_lines_are_ignored() {
    let mut platform = FakePlatform::new(
        vec![vec![
            line(RUNNING_1),
            Event::Line("not json", Duration::from_secs(1)),
            line(RUNNING_2),
            Event::Eof(Duration::ZERO),
        ]],
        vec![None],
    );
    assert_eq!(run(&mut platform), Exit::Clean);
    assert!(platform.markers.is_empty());
}

#[test]
fn the_owner_rule() {
    assert!(restore_allowed(0, 5));
    assert!(restore_allowed(5, 5));
    assert!(!restore_allowed(6, 5));
}

#[test]
fn the_marker_round_trips() {
    let dir = std::env::temp_dir().join(format!("ssr-guard-marker-{}", std::process::id()));
    let path = dir.join(marker::FILE);
    let wanted = Marker::new(
        MarkerReason::StallRestarted,
        123,
        vec!["복원했습니다.".to_owned()],
    );
    marker::write(&path, &wanted).expect("the marker is written");
    assert_eq!(marker::read(&path).expect("the marker is read"), wanted);
    let replaced = Marker::new(MarkerReason::Stuck, 456, Vec::new());
    marker::write(&path, &replaced).expect("the marker is replaced");
    assert_eq!(
        marker::read(&path).expect("the replacement marker is read"),
        replaced
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_window_request_is_taken_once() {
    let dir = std::env::temp_dir().join(format!("ssr-guard-show-{}", std::process::id()));
    assert!(!show::take(&dir));
    show::request(&dir).expect("the request is written");
    assert!(show::take(&dir));
    assert!(!show::take(&dir));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn heartbeat_lines_parse() {
    assert_eq!(
        heartbeat::parse(RUNNING_1),
        Some(Heartbeat {
            running: true,
            output_periods: 1,
            capture_reads: 1,
        })
    );
    assert_eq!(heartbeat::parse("garbage"), None);
}
