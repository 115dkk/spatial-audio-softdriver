use std::fs::{self, File, TryLockError};
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use ssr_sys::WindowsHost;
use ssr_sys::host::RESTORE_POINT_FILE;
use supervisor::guard::Guard;
use supervisor::restore_point::{self, RestorePoint};

use crate::marker::{self, Marker};
use crate::runner::{self, AppProcess, Exit, Platform, Timeout};
use crate::show;

/// The guard's lock in the state directory: one guard per state directory.
/// The OS releases it however the guard ends.
pub const LOCK_FILE: &str = "guard.lock";

#[derive(Debug)]
pub struct WindowsPlatform {
    state_dir: PathBuf,
    app: PathBuf,
    extra: Vec<String>,
    minimized: bool,
    /// Held for the guard's lifetime once [`WindowsPlatform::take_lock`]
    /// succeeded.
    lock: Option<File>,
}

impl WindowsPlatform {
    pub fn new(state_dir: PathBuf, app: PathBuf, extra: Vec<String>, minimized: bool) -> Self {
        Self {
            state_dir,
            app,
            extra,
            minimized,
            lock: None,
        }
    }

    /// Takes the state directory's guard lock. `Ok(false)` when another
    /// guard holds it: that guard owns the app, and this one only asks it
    /// for the window (see [`show::request`]).
    pub fn take_lock(&mut self) -> std::io::Result<bool> {
        fs::create_dir_all(&self.state_dir)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.state_dir.join(LOCK_FILE))?;
        match file.try_lock() {
            Ok(()) => {
                self.lock = Some(file);
                Ok(true)
            }
            Err(TryLockError::WouldBlock) => Ok(false),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }
}

#[derive(Debug)]
enum Output {
    Line(String),
    Eof,
}

#[derive(Debug)]
struct ChildProcess {
    child: Child,
    output: Receiver<Output>,
}

impl AppProcess for ChildProcess {
    fn next_line(&mut self, wait: Duration) -> Result<Option<String>, Timeout> {
        match self.output.recv_timeout(wait) {
            Ok(Output::Line(line)) => Ok(Some(line)),
            Ok(Output::Eof) | Err(RecvTimeoutError::Disconnected) => Ok(None),
            Err(RecvTimeoutError::Timeout) => Err(Timeout),
        }
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn id(&self) -> u32 {
        self.child.id()
    }
}

impl Platform for WindowsPlatform {
    fn spawn(&mut self, resume: bool) -> Result<Box<dyn AppProcess>, String> {
        let mut command = Command::new(&self.app);
        command
            .args(&self.extra)
            .arg("--guarded")
            .env("SSR_GUARD_STATE_DIR", &self.state_dir)
            .current_dir(&self.state_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        if resume {
            command.arg("--resume");
        }
        if self.minimized {
            command.arg("--minimized");
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("{}: {error}", self.app.display()))?;
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the app stdout pipe was not created".to_owned());
        };
        let (send, output) = mpsc::channel();
        if let Err(error) = thread::Builder::new()
            .name("ssr-guard-pipe".to_owned())
            .spawn(move || {
                let mut lines = BufReader::new(stdout).lines();
                while let Some(Ok(line)) = lines.next() {
                    if send.send(Output::Line(line)).is_err() {
                        return;
                    }
                }
                let _ = send.send(Output::Eof);
            })
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("the pipe reader did not start: {error}"));
        }
        Ok(Box::new(ChildProcess { child, output }))
    }

    fn restore(&mut self, app: u32) -> Option<Vec<String>> {
        let path = self.state_dir.join(RESTORE_POINT_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => return Some(vec![format!("복원점을 읽지 못했습니다: {error}")]),
        };
        let point = match RestorePoint::from_json(&text) {
            Ok(point) => point,
            Err(error) => return Some(vec![format!("복원점을 읽지 못했습니다: {error}")]),
        };
        if !runner::restore_allowed(point.app_pid, app) {
            log(
                &self.state_dir,
                &format!(
                    "restore: the restore point belongs to pid {}, not to the app this guard launched ({app}); left alone",
                    point.app_pid
                ),
            );
            return None;
        }
        log(
            &self.state_dir,
            &format!("restore: giving the roles back (app pid {})", point.app_pid),
        );
        Some(restore_point::restore(
            &point,
            &mut windows_host(&self.state_dir),
        ))
    }

    fn restore_point_owner(&self) -> Option<u32> {
        let text = fs::read_to_string(self.state_dir.join(RESTORE_POINT_FILE)).ok()?;
        // An unreadable restore point still means the roles were taken; the
        // restore reports what is wrong with it.
        Some(RestorePoint::from_json(&text).map_or(0, |point| point.app_pid))
    }

    fn write_marker(&mut self, value: &Marker) {
        if let Err(error) = marker::write(&self.state_dir.join(marker::FILE), value) {
            log(&self.state_dir, &format!("marker: {error}"));
        }
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Guards the app, or, when another guard already owns this state directory
/// (the user launched the app again), only asks the running app for its
/// window and ends. A guard that cannot tell (the lock file fails for
/// another reason) guards anyway: the restore point's owner rule
/// ([`runner::restore_allowed`]) keeps it from taking a running instance's
/// roles.
pub fn run(platform: &mut WindowsPlatform) -> Exit {
    match platform.take_lock() {
        Ok(true) => runner::run(platform, Guard::STALL_AFTER, runner::TAKEOVER_SILENCE),
        Ok(false) => {
            log(
                &platform.state_dir,
                "guard: another guard owns the app; asking it for the window",
            );
            if let Err(error) = show::request(&platform.state_dir) {
                log(
                    &platform.state_dir,
                    &format!("guard: window request: {error}"),
                );
            }
            Exit::Clean
        }
        Err(error) => {
            log(
                &platform.state_dir,
                &format!("guard: lock: {error}; guarding anyway"),
            );
            runner::run(platform, Guard::STALL_AFTER, runner::TAKEOVER_SILENCE)
        }
    }
}

fn windows_host(state_dir: &Path) -> WindowsHost {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    WindowsHost::new(exe_dir.join("ssr-setup.exe"), state_dir.to_path_buf())
}

pub fn log(state_dir: &Path, message: &str) {
    use std::io::Write as _;
    use std::time::{SystemTime, UNIX_EPOCH};

    let _ = fs::create_dir_all(state_dir);
    let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_dir.join("guard.log"))
    else {
        return;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let message = message.replace(['\r', '\n'], " ");
    let _ = writeln!(
        file,
        "{}.{:03} {message}",
        now.as_secs(),
        now.subsec_millis()
    );
}
