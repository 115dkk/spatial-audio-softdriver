//! The `ssr-setup` contract (ADR 0032): the task line the app runs elevated and
//! the one JSON line the task answers with.
//!
//! The app (`ssr-sys::host`) builds a [`TaskLine`] from the supervisor's
//! [`SetupTask`], quotes it into the parameters of one elevated launch, and
//! reads the [`TaskResult`] that `ssr-setup` writes to `--result`, since an
//! elevated process's stdout cannot reach its parent. `ssr-setup` parses the
//! same line with clap and prints the same JSON. Both ends build on this module,
//! so it holds every name the two must agree on. It is pure and runs on every
//! OS; the JSON is a flat object, read and written here without a JSON crate so
//! the driver contract crate keeps its dependencies.

use std::ffi::OsString;
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};

use ssr_types::{HostError, SetupOutcome, SetupTask};

/// The hardware id of our root-enumerated device
/// (`tools/ci/package-identities.ps1`, `HardwareId`). The app finds its device
/// node by it, never by a display name.
pub const PACKAGE_HARDWARE_ID: &str = r"ROOT\SpatialSoftReceiver";

/// The driver's service (`tools/ci/package-identities.ps1`, `Service`).
pub const PACKAGE_SERVICE: &str = "SpatialSoftReceiver";

/// The driver's registry parameters under HKEY_LOCAL_MACHINE, where
/// `exclusive_policy::ENDPOINT_MASK_VALUE` lives.
pub fn parameters_key() -> String {
    format!(r"SYSTEM\CurrentControlSet\Services\{PACKAGE_SERVICE}\Parameters")
}

/// The driver package's INF, next to `ssr-setup.exe`
/// (`tools/ci/package-identities.ps1`, `Inf`).
pub const PACKAGE_INF: &str = "spatialsoftreceiver.inf";

/// The test-signing certificate a test-signed package carries next to its INF.
/// A release package has none, and `ssr-setup` then installs no certificate.
pub const PACKAGE_CERTIFICATE: &str = "spatialsoftreceiver-test.cer";

/// The file in the app's state directory that `ssr-setup` answers in.
pub const RESULT_FILE: &str = "setup-result.json";

/// The one task an `ssr-setup` run performs, named as its subcommand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TaskName {
    Install,
    Uninstall,
    SetFormats,
    RestartDevice,
    Repair,
}

impl TaskName {
    /// Every task, in the order `ssr-setup --help` lists them.
    pub const ALL: [TaskName; 5] = [
        TaskName::Install,
        TaskName::Uninstall,
        TaskName::SetFormats,
        TaskName::RestartDevice,
        TaskName::Repair,
    ];

    /// The subcommand name.
    pub const fn as_str(self) -> &'static str {
        match self {
            TaskName::Install => "install",
            TaskName::Uninstall => "uninstall",
            TaskName::SetFormats => "set-formats",
            TaskName::RestartDevice => "restart-device",
            TaskName::Repair => "repair",
        }
    }
}

impl fmt::Display for TaskName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `ssr-setup <task> [--inf P] [--cert P] [--endpoint-mask N] --result P`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskLine {
    pub task: TaskName,
    /// The driver package INF (`install`, `repair`).
    pub inf: Option<PathBuf>,
    /// The test-signing certificate (`install`, `repair`); a missing file
    /// means a release package.
    pub cert: Option<PathBuf>,
    /// The driver's `EndpointMask` parameter (`set-formats`).
    pub endpoint_mask: Option<u8>,
    /// Where `ssr-setup` writes its JSON line.
    pub result: PathBuf,
}

impl TaskLine {
    /// The line for `task`, with the package files taken from `package_dir`
    /// (the directory of `ssr-setup.exe`).
    pub fn from_setup_task(task: &SetupTask, package_dir: &Path, result: PathBuf) -> Self {
        let package = |name: &str| Some(package_dir.join(name));
        let (task, inf, cert, endpoint_mask) = match *task {
            SetupTask::Install => (
                TaskName::Install,
                package(PACKAGE_INF),
                package(PACKAGE_CERTIFICATE),
                None,
            ),
            SetupTask::Uninstall => (TaskName::Uninstall, None, None, None),
            SetupTask::SetFormats { endpoint_mask } => {
                (TaskName::SetFormats, None, None, Some(endpoint_mask))
            }
            SetupTask::RestartDevice => (TaskName::RestartDevice, None, None, None),
            SetupTask::Repair => (
                TaskName::Repair,
                package(PACKAGE_INF),
                package(PACKAGE_CERTIFICATE),
                None,
            ),
        };
        Self {
            task,
            inf,
            cert,
            endpoint_mask,
            result,
        }
    }

    /// The arguments after the program name, in the order `ssr-setup` lists
    /// them.
    pub fn to_args(&self) -> Vec<OsString> {
        let mut args = vec![OsString::from(self.task.as_str())];
        if let Some(inf) = &self.inf {
            args.extend([OsString::from("--inf"), inf.clone().into_os_string()]);
        }
        if let Some(cert) = &self.cert {
            args.extend([OsString::from("--cert"), cert.clone().into_os_string()]);
        }
        if let Some(mask) = self.endpoint_mask {
            args.extend([
                OsString::from("--endpoint-mask"),
                OsString::from(mask.to_string()),
            ]);
        }
        args.extend([
            OsString::from("--result"),
            self.result.clone().into_os_string(),
        ]);
        args
    }

    /// [`to_args`](Self::to_args) as one Windows command line (the
    /// `lpParameters` of an elevated launch), each argument quoted by
    /// [`quote_windows_arg`].
    pub fn to_command_line(&self) -> Result<String, ContractError> {
        let mut line = String::new();
        for arg in self.to_args() {
            let text = arg
                .to_str()
                .ok_or_else(|| ContractError::NotUnicode(arg.to_string_lossy().into_owned()))?;
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&quote_windows_arg(text));
        }
        Ok(line)
    }
}

/// Quotes one argument so `CommandLineToArgvW` and the MSVC runtime (and so
/// Rust's `std::env::args`) read it back unchanged: arguments without blanks
/// or quotes stay as they are; others are enclosed in quotes, with the
/// backslashes before a quote or the closing quote doubled.
pub fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.to_owned();
    }
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut backslashes = 0_usize;
    for character in arg.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            other => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(other);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

/// The JSON line `ssr-setup` prints and writes to `--result`:
/// `{"ok":…,"reboot_required":…,"message":"…"}`, plus `"error"` and `"code"`
/// when the task failed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskResult {
    pub ok: bool,
    pub reboot_required: bool,
    pub message: String,
    /// What failed, `"<step>: <code and text>"`.
    pub error: Option<String>,
    /// The failing step's HRESULT, CONFIGRET or exit code.
    pub code: Option<i32>,
}

impl TaskResult {
    pub fn success(message: impl Into<String>, reboot_required: bool) -> Self {
        Self {
            ok: true,
            reboot_required,
            message: message.into(),
            error: None,
            code: None,
        }
    }

    pub fn failure(error: impl Into<String>, code: Option<i32>) -> Self {
        Self {
            ok: false,
            reboot_required: false,
            message: String::new(),
            error: Some(error.into()),
            code,
        }
    }

    /// One line, ASCII only (other characters as `\u` escapes), no newline.
    pub fn to_json(&self) -> String {
        let mut line = String::from("{\"ok\":");
        line.push_str(if self.ok { "true" } else { "false" });
        line.push_str(",\"reboot_required\":");
        line.push_str(if self.reboot_required {
            "true"
        } else {
            "false"
        });
        line.push_str(",\"message\":");
        push_json_string(&mut line, &self.message);
        if let Some(error) = &self.error {
            line.push_str(",\"error\":");
            push_json_string(&mut line, error);
        }
        if let Some(code) = self.code {
            let _ = write!(line, ",\"code\":{code}");
        }
        line.push('}');
        line
    }

    /// Reads one result line. `ok` is required; `reboot_required` and
    /// `message` default to false and empty, `error` and `code` may be absent
    /// or null, and unknown fields are skipped.
    pub fn from_json(text: &str) -> Result<Self, ContractError> {
        let fields = Parser::new(text).object()?;
        let mut ok = None;
        let mut result = Self::default();
        for (name, value) in fields {
            match name.as_str() {
                "ok" => ok = Some(value.boolean("ok")?),
                "reboot_required" => result.reboot_required = value.boolean("reboot_required")?,
                "message" => result.message = value.string("message")?,
                "error" => result.error = value.optional_string("error")?,
                "code" => result.code = value.optional_code()?,
                _ => {}
            }
        }
        result.ok = ok.ok_or(ContractError::Missing("ok"))?;
        Ok(result)
    }

    /// What the supervisor's `Host::run_setup` returns for this line.
    pub fn into_outcome(self) -> Result<SetupOutcome, HostError> {
        if self.ok {
            return Ok(SetupOutcome {
                reboot_required: self.reboot_required,
                message: self.message,
            });
        }
        let error = match self.error {
            Some(error) if !error.is_empty() => error,
            _ if !self.message.is_empty() => self.message,
            _ => "ssr-setup reported a failure without a reason".to_owned(),
        };
        Err(HostError::Setup(error))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    #[error("the result line is not a JSON object: {0}")]
    Json(String),
    #[error("the result line has no \"{0}\"")]
    Missing(&'static str),
    #[error("\"{field}\" in the result line must be {expected}")]
    Type {
        field: &'static str,
        expected: &'static str,
    },
    #[error("the argument {0:?} is not valid Unicode")]
    NotUnicode(String),
}

fn push_json_string(out: &mut String, text: &str) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(character),
            other => {
                for unit in other.encode_utf16(&mut [0; 2]) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

/// A field value as far as the contract cares.
enum Value {
    Null,
    Bool(bool),
    Str(String),
    /// The number as written.
    Number(String),
    /// An array or object, skipped.
    Composite,
}

impl Value {
    fn boolean(self, field: &'static str) -> Result<bool, ContractError> {
        match self {
            Value::Bool(value) => Ok(value),
            _ => Err(ContractError::Type {
                field,
                expected: "true or false",
            }),
        }
    }

    fn string(self, field: &'static str) -> Result<String, ContractError> {
        match self {
            Value::Str(value) => Ok(value),
            _ => Err(ContractError::Type {
                field,
                expected: "a string",
            }),
        }
    }

    fn optional_string(self, field: &'static str) -> Result<Option<String>, ContractError> {
        match self {
            Value::Null => Ok(None),
            other => other.string(field).map(Some),
        }
    }

    fn optional_code(self) -> Result<Option<i32>, ContractError> {
        let wrong = ContractError::Type {
            field: "code",
            expected: "a 32-bit integer or null",
        };
        match self {
            Value::Null => Ok(None),
            Value::Number(text) => text.parse::<i32>().map(Some).map_err(|_| wrong),
            _ => Err(wrong),
        }
    }
}

/// Composite values nested deeper than this are refused rather than skipped.
const MAX_DEPTH: usize = 32;

struct Parser<'a> {
    text: &'a str,
    position: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, position: 0 }
    }

    fn error(&self, what: &str) -> ContractError {
        ContractError::Json(format!("{what} at byte {}", self.position))
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.position).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.position += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), ContractError> {
        self.skip_whitespace();
        if self.peek() == Some(byte) {
            self.position += 1;
            Ok(())
        } else {
            Err(self.error(&format!("expected '{}'", char::from(byte))))
        }
    }

    /// The whole text as one object of named values.
    fn object(mut self) -> Result<Vec<(String, Value)>, ContractError> {
        self.expect(b'{')?;
        let mut fields = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.position += 1;
        } else {
            loop {
                self.skip_whitespace();
                if self.peek() != Some(b'"') {
                    return Err(self.error("expected a field name"));
                }
                let name = self.string()?;
                self.expect(b':')?;
                let value = self.value(1)?;
                fields.push((name, value));
                self.skip_whitespace();
                match self.peek() {
                    Some(b',') => self.position += 1,
                    Some(b'}') => {
                        self.position += 1;
                        break;
                    }
                    _ => return Err(self.error("expected ',' or '}'")),
                }
            }
        }
        self.skip_whitespace();
        if self.position != self.text.len() {
            return Err(self.error("unexpected text after the object"));
        }
        Ok(fields)
    }

    fn value(&mut self, depth: usize) -> Result<Value, ContractError> {
        self.skip_whitespace();
        match self.peek() {
            Some(b'"') => self.string().map(Value::Str),
            Some(b't') => self.literal("true").map(|()| Value::Bool(true)),
            Some(b'f') => self.literal("false").map(|()| Value::Bool(false)),
            Some(b'n') => self.literal("null").map(|()| Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number().map(Value::Number),
            Some(open @ (b'[' | b'{')) => {
                if depth >= MAX_DEPTH {
                    return Err(self.error("nested too deeply"));
                }
                self.composite(open, depth + 1).map(|()| Value::Composite)
            }
            _ => Err(self.error("expected a value")),
        }
    }

    fn literal(&mut self, word: &str) -> Result<(), ContractError> {
        if self.text[self.position..].starts_with(word) {
            self.position += word.len();
            Ok(())
        } else {
            Err(self.error("unknown literal"))
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.position;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.position += 1;
        }
        self.position - start
    }

    fn number(&mut self) -> Result<String, ContractError> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        match self.peek() {
            Some(b'0') => self.position += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(self.error("expected a digit")),
        }
        if self.peek() == Some(b'.') {
            self.position += 1;
            if self.digits() == 0 {
                return Err(self.error("expected a fraction digit"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            if self.digits() == 0 {
                return Err(self.error("expected an exponent digit"));
            }
        }
        Ok(self.text[start..self.position].to_owned())
    }

    fn hex4(&mut self) -> Result<u16, ContractError> {
        let digits = self
            .text
            .get(self.position..self.position + 4)
            .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| self.error("expected four hex digits"))?;
        self.position += 4;
        u16::from_str_radix(digits, 16).map_err(|_| self.error("expected four hex digits"))
    }

    fn string(&mut self) -> Result<String, ContractError> {
        self.position += 1; // the opening quote
        let mut value = String::new();
        loop {
            let Some(character) = self.text[self.position..].chars().next() else {
                return Err(self.error("unterminated string"));
            };
            self.position += character.len_utf8();
            match character {
                '"' => return Ok(value),
                '\\' => {
                    let Some(escape) = self.peek() else {
                        return Err(self.error("unterminated escape"));
                    };
                    self.position += 1;
                    match escape {
                        b'"' => value.push('"'),
                        b'\\' => value.push('\\'),
                        b'/' => value.push('/'),
                        b'b' => value.push('\u{8}'),
                        b'f' => value.push('\u{c}'),
                        b'n' => value.push('\n'),
                        b'r' => value.push('\r'),
                        b't' => value.push('\t'),
                        b'u' => {
                            let first = self.hex4()?;
                            let unit = if (0xd800..0xdc00).contains(&first) {
                                if !self.text[self.position..].starts_with("\\u") {
                                    return Err(self.error("unpaired surrogate"));
                                }
                                self.position += 2;
                                let second = self.hex4()?;
                                if !(0xdc00..0xe000).contains(&second) {
                                    return Err(self.error("unpaired surrogate"));
                                }
                                0x10000
                                    + ((u32::from(first) - 0xd800) << 10)
                                    + (u32::from(second) - 0xdc00)
                            } else {
                                u32::from(first)
                            };
                            value.push(
                                char::from_u32(unit)
                                    .ok_or_else(|| self.error("unpaired surrogate"))?,
                            );
                        }
                        _ => return Err(self.error("unknown escape")),
                    }
                }
                control if control < ' ' => {
                    return Err(self.error("control character in a string"));
                }
                other => value.push(other),
            }
        }
    }

    /// Skips an array or object whose opening bracket is at the position.
    fn composite(&mut self, open: u8, depth: usize) -> Result<(), ContractError> {
        let close = if open == b'[' { b']' } else { b'}' };
        self.position += 1;
        self.skip_whitespace();
        if self.peek() == Some(close) {
            self.position += 1;
            return Ok(());
        }
        loop {
            if open == b'{' {
                self.skip_whitespace();
                if self.peek() != Some(b'"') {
                    return Err(self.error("expected a field name"));
                }
                self.string()?;
                self.expect(b':')?;
            }
            self.value(depth)?;
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(byte) if byte == close => {
                    self.position += 1;
                    return Ok(());
                }
                _ => return Err(self.error("expected ',' or a closing bracket")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(text: &str) -> OsString {
        OsString::from(text)
    }

    #[test]
    fn task_names_display_as_the_subcommands() {
        let names: Vec<String> = TaskName::ALL.iter().map(ToString::to_string).collect();
        assert_eq!(
            names,
            [
                "install",
                "uninstall",
                "set-formats",
                "restart-device",
                "repair"
            ]
        );
    }

    #[test]
    fn task_line_arguments_for_each_task() {
        let package = Path::new("C:/Program Files/Spatial SoftReceiver");
        let result = PathBuf::from("C:/state/setup-result.json");
        let inf = package.join(PACKAGE_INF).into_os_string();
        let cert = package.join(PACKAGE_CERTIFICATE).into_os_string();
        let result_arg = result.clone().into_os_string();
        let cases = [
            (
                SetupTask::Install,
                vec![
                    os("install"),
                    os("--inf"),
                    inf.clone(),
                    os("--cert"),
                    cert.clone(),
                    os("--result"),
                    result_arg.clone(),
                ],
            ),
            (
                SetupTask::Uninstall,
                vec![os("uninstall"), os("--result"), result_arg.clone()],
            ),
            (
                SetupTask::SetFormats { endpoint_mask: 5 },
                vec![
                    os("set-formats"),
                    os("--endpoint-mask"),
                    os("5"),
                    os("--result"),
                    result_arg.clone(),
                ],
            ),
            (
                SetupTask::RestartDevice,
                vec![os("restart-device"), os("--result"), result_arg.clone()],
            ),
            (
                SetupTask::Repair,
                vec![
                    os("repair"),
                    os("--inf"),
                    inf,
                    os("--cert"),
                    cert,
                    os("--result"),
                    result_arg,
                ],
            ),
        ];
        for (task, expected) in cases {
            let line = TaskLine::from_setup_task(&task, package, result.clone());
            assert_eq!(line.to_args(), expected, "{task:?}");
            assert_eq!(line.result, result);
        }
    }

    #[test]
    fn parameters_key_is_the_services_parameters() {
        assert_eq!(
            parameters_key(),
            r"SYSTEM\CurrentControlSet\Services\SpatialSoftReceiver\Parameters"
        );
    }

    #[test]
    fn task_line_quotes_paths_with_spaces_into_one_command_line() {
        let line = TaskLine {
            task: TaskName::SetFormats,
            inf: None,
            cert: None,
            endpoint_mask: Some(7),
            result: PathBuf::from(
                r"C:\Users\a b\AppData\Local\Spatial SoftReceiver\setup-result.json",
            ),
        };
        assert_eq!(
            line.to_command_line().unwrap(),
            r#"set-formats --endpoint-mask 7 --result "C:\Users\a b\AppData\Local\Spatial SoftReceiver\setup-result.json""#
        );
    }

    #[test]
    fn windows_arguments_are_quoted_for_command_line_to_argv() {
        for (arg, quoted) in [
            ("plain", "plain"),
            (r"C:\dir\file.inf", r"C:\dir\file.inf"),
            ("", r#""""#),
            ("a b", r#""a b""#),
            ("tab\there", "\"tab\there\""),
            (r"C:\dir with space\", r#""C:\dir with space\\""#),
            (r#"say "hi""#, r#""say \"hi\"""#),
            (r#"a\"b"#, r#""a\\\"b""#),
            (r"a\\b c", r#""a\\b c""#),
        ] {
            assert_eq!(quote_windows_arg(arg), quoted, "{arg:?}");
        }
    }

    #[test]
    fn task_result_round_trips_through_one_json_line() {
        let results = [
            TaskResult::success(r"installed ROOT\MEDIA\0000", true),
            TaskResult::success("", false),
            TaskResult {
                ok: false,
                reboot_required: false,
                message: "removed: device \"x\"\n\ttab".into(),
                error: Some(
                    "SetupDiCallClassInstaller(DIF_REMOVE): 0x800F0203 공간 소프트리시버 🎧".into(),
                ),
                code: Some(0x800F_0203_u32 as i32),
            },
            TaskResult::failure("no device", None),
            TaskResult::failure("ctrl \u{1} \u{7f}", Some(i32::MAX)),
        ];
        for result in results {
            let line = result.to_json();
            assert!(line.is_ascii() && !line.contains('\n'), "{line}");
            assert_eq!(TaskResult::from_json(&line).unwrap(), result, "{line}");
        }
    }

    #[test]
    fn task_result_writes_error_and_code_only_on_failure() {
        assert_eq!(
            TaskResult::success("installed", false).to_json(),
            r#"{"ok":true,"reboot_required":false,"message":"installed"}"#
        );
        assert_eq!(
            TaskResult::failure("restart: 0x00000017", Some(23)).to_json(),
            r#"{"ok":false,"reboot_required":false,"message":"","error":"restart: 0x00000017","code":23}"#
        );
    }

    #[test]
    fn from_json_rejects_a_missing_ok() {
        assert_eq!(
            TaskResult::from_json(r#"{"reboot_required":true,"message":"x"}"#),
            Err(ContractError::Missing("ok"))
        );
        assert_eq!(
            TaskResult::from_json("{}"),
            Err(ContractError::Missing("ok"))
        );
    }

    #[test]
    fn from_json_reads_the_short_lines_and_skips_unknown_fields() {
        let result =
            TaskResult::from_json(r#"{"ok":false,"error":"ssr-setup runs on Windows","code":2}"#)
                .unwrap();
        assert_eq!(
            result,
            TaskResult::failure("ssr-setup runs on Windows", Some(2))
        );
        let result = TaskResult::from_json(
            " {\"version\":1.5e3, \"ok\" : true ,\"extra\":[1,{\"a\":[null,\"]\"]}],\"error\":null,\"code\":null}\r\n",
        )
        .unwrap();
        assert_eq!(result, TaskResult::success("", false));
        let result =
            TaskResult::from_json(r#"{"ok":true,"message":"a\/b \u00e9\ud83c\udfa7"}"#).unwrap();
        assert_eq!(result.message, "a/b é🎧");
    }

    #[test]
    fn from_json_rejects_malformed_lines_and_wrong_types() {
        for line in [
            "",
            "[]",
            r#"{"ok":true"#,
            r#"{"ok":true,}"#,
            r#"{"ok":true} x"#,
            r#"{"ok":true}{"ok":true}"#,
            r#"{ok:true}"#,
            r#"{"ok":tru}"#,
            r#"{"ok":true,"message":"a"#,
            "{\"ok\":true,\"message\":\"a\nb\"}",
            r#"{"ok":true,"message":"\q"}"#,
            r#"{"ok":true,"message":"\ud800"}"#,
            r#"{"ok":true,"message":"\udc00"}"#,
            r#"{"ok":true,"x":01}"#,
            r#"{"ok":true,"x":1.}"#,
            r#"{"ok":true,"x":-}"#,
            r#"{"ok":true,"x":[1,]}"#,
        ] {
            assert!(
                matches!(TaskResult::from_json(line), Err(ContractError::Json(_))),
                "{line:?}"
            );
        }
        for (line, field) in [
            (r#"{"ok":"yes"}"#, "ok"),
            (r#"{"ok":true,"reboot_required":1}"#, "reboot_required"),
            (r#"{"ok":true,"message":null}"#, "message"),
            (r#"{"ok":false,"error":3}"#, "error"),
            (r#"{"ok":false,"code":1.5}"#, "code"),
            (r#"{"ok":false,"code":2147483648}"#, "code"),
            (r#"{"ok":false,"code":"2"}"#, "code"),
        ] {
            assert!(
                matches!(
                    TaskResult::from_json(line),
                    Err(ContractError::Type { field: f, .. }) if f == field
                ),
                "{line}"
            );
        }
        let deep = format!(r#"{{"ok":true,"x":{}{}}}"#, "[".repeat(40), "]".repeat(40));
        assert!(matches!(
            TaskResult::from_json(&deep),
            Err(ContractError::Json(_))
        ));
    }

    #[test]
    fn task_results_become_the_hosts_outcome() {
        assert_eq!(
            TaskResult::success("installed", true).into_outcome(),
            Ok(SetupOutcome {
                reboot_required: true,
                message: "installed".into()
            })
        );
        assert_eq!(
            TaskResult::failure("install: already installed", Some(1)).into_outcome(),
            Err(HostError::Setup("install: already installed".into()))
        );
        let without_error = TaskResult {
            message: "removed nothing".into(),
            ..TaskResult::default()
        };
        assert_eq!(
            without_error.into_outcome(),
            Err(HostError::Setup("removed nothing".into()))
        );
        assert!(matches!(
            TaskResult::default().into_outcome(),
            Err(HostError::Setup(reason)) if !reason.is_empty()
        ));
    }
}
