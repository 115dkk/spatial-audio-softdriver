//! The JSON documents `ssr-live host` prints (`tools/ci/host_check.py` and
//! `tools/ci/host_role_check.py` read them).
//!
//! The Windows host adapter only runs on Windows, but the shapes here take the
//! plain `ssr_types` values, so every spelling and every null is tested on
//! all OSes. The spellings are part of the CI checkers' contract:
//!
//! * `platform`: `os_build`, `arch` (`x64`, `arm64`, `other`), `test_signing`,
//!   `secure_boot` (`enabled`, `disabled`, `unsupported`).
//! * `driver`: `state` (`not-installed`, `installed`, `reboot-required`);
//!   only `installed` adds `problem_code`, `service_running` and
//!   `exclusive_policy` (`applied`, `missing`, `unknown`).
//! * `outputs`: `id`, `name`, `endpoint` (our endpoint number or null),
//!   `state` (`active`, `disabled`, `not-present`, `unplugged`).
//! * `default_roles`: `console`, `multimedia`, `communications`, each an
//!   endpoint id or null.

use std::fmt::Write as _;

use serde_json::{Value, json};
use ssr_types::{
    Arch, DefaultRoles, DeviceId, DeviceState, DriverState, HostError, OutputDevice, Platform,
    PolicyState, Role, SecureBoot, SetupOutcome,
};

pub fn arch_name(arch: Arch) -> &'static str {
    match arch {
        Arch::X64 => "x64",
        Arch::Arm64 => "arm64",
        Arch::Other => "other",
    }
}

pub fn secure_boot_name(secure_boot: SecureBoot) -> &'static str {
    match secure_boot {
        SecureBoot::Enabled => "enabled",
        SecureBoot::Disabled => "disabled",
        SecureBoot::Unsupported => "unsupported",
    }
}

pub fn policy_name(policy: PolicyState) -> &'static str {
    match policy {
        PolicyState::Applied => "applied",
        PolicyState::Missing => "missing",
        PolicyState::Unknown => "unknown",
    }
}

pub fn device_state_name(state: DeviceState) -> &'static str {
    match state {
        DeviceState::Active => "active",
        DeviceState::Disabled => "disabled",
        DeviceState::NotPresent => "not-present",
        DeviceState::Unplugged => "unplugged",
    }
}

/// The role's name on the command line and in `default_roles`.
pub fn role_name(role: Role) -> &'static str {
    match role {
        Role::Console => "console",
        Role::Multimedia => "multimedia",
        Role::Communications => "communications",
    }
}

pub fn platform_json(platform: &Platform) -> Value {
    json!({
        "os_build": platform.os_build,
        "arch": arch_name(platform.arch),
        "test_signing": platform.test_signing,
        "secure_boot": secure_boot_name(platform.secure_boot),
    })
}

/// `state`, plus the installed node's fields only when it is installed.
pub fn driver_json(driver: &DriverState) -> Value {
    match driver {
        DriverState::NotInstalled => json!({ "state": "not-installed" }),
        DriverState::RebootRequired => json!({ "state": "reboot-required" }),
        DriverState::Installed {
            problem_code,
            service_running,
            exclusive_policy,
        } => json!({
            "state": "installed",
            "problem_code": problem_code,
            "service_running": service_running,
            "exclusive_policy": policy_name(*exclusive_policy),
        }),
    }
}

pub fn output_json(output: &OutputDevice) -> Value {
    json!({
        "id": output.id.0,
        "name": output.name,
        "endpoint": output.endpoint,
        "state": device_state_name(output.state),
    })
}

pub fn default_roles_json(roles: &DefaultRoles) -> Value {
    let mut object = serde_json::Map::new();
    for role in Role::ALL {
        let id = roles.get(role).map(|id| Value::from(id.0.as_str()));
        object.insert(role_name(role).to_owned(), id.unwrap_or(Value::Null));
    }
    Value::Object(object)
}

/// `host show`.
pub fn show_json(
    platform: &Platform,
    driver: &DriverState,
    outputs: &[OutputDevice],
    roles: &DefaultRoles,
) -> Value {
    json!({
        "platform": platform_json(platform),
        "driver": driver_json(driver),
        "outputs": outputs.iter().map(output_json).collect::<Vec<_>>(),
        "default_roles": default_roles_json(roles),
    })
}

/// What a `host` subcommand prints on stdout, and the message for stderr when
/// it failed (exit 1). A failure may still print a document (a declined
/// administrator prompt).
#[derive(Debug, PartialEq)]
pub struct Answer {
    pub stdout: Option<Value>,
    pub error: Option<String>,
}

impl Answer {
    fn printed(document: Value) -> Self {
        Self {
            stdout: Some(document),
            error: None,
        }
    }

    fn failed(error: &HostError) -> Self {
        Self {
            stdout: None,
            error: Some(error.to_string()),
        }
    }
}

/// `host set-default`: `{"ok": true, "role": …, "id": …}`, or the host's error.
pub fn set_default_answer(role: Role, id: &DeviceId, result: Result<(), HostError>) -> Answer {
    match result {
        Ok(()) => Answer::printed(json!({
            "ok": true,
            "role": role_name(role),
            "id": id.0,
        })),
        Err(error) => Answer::failed(&error),
    }
}

/// `host setup`: `{"ok": true, "reboot_required": …, "message": …}`; a
/// declined prompt prints `{"ok": false, "declined": true}` and still fails;
/// any other error only fails.
pub fn setup_answer(result: Result<SetupOutcome, HostError>) -> Answer {
    match result {
        Ok(outcome) => Answer::printed(json!({
            "ok": true,
            "reboot_required": outcome.reboot_required,
            "message": outcome.message,
        })),
        Err(HostError::Declined) => Answer {
            stdout: Some(json!({ "ok": false, "declined": true })),
            error: Some(HostError::Declined.to_string()),
        },
        Err(error) => Answer::failed(&error),
    }
}

/// Pretty JSON with every character outside ASCII written as a `\u` escape
/// (UTF-16 surrogate pairs above the BMP), so localized endpoint names reach
/// the reader intact through any console code page or shell redirection. JSON
/// text outside strings is ASCII, so escaping every non-ASCII character keeps
/// the document valid and equal.
pub fn to_ascii_json(value: &Value) -> String {
    let text = serde_json::to_string_pretty(value).expect("a serde_json::Value always serializes");
    let mut ascii = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii() {
            ascii.push(character);
        } else {
            let mut units = [0_u16; 2];
            for unit in character.encode_utf16(&mut units) {
                let _ = write!(ascii, "\\u{unit:04x}");
            }
        }
    }
    ascii
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform() -> Platform {
        Platform {
            os_build: 26_100,
            arch: Arch::X64,
            test_signing: true,
            secure_boot: SecureBoot::Disabled,
        }
    }

    fn output(id: &str, endpoint: Option<u16>, state: DeviceState) -> OutputDevice {
        OutputDevice {
            id: DeviceId(id.to_owned()),
            name: "공간 소프트리시버".to_owned(),
            endpoint,
            state,
        }
    }

    #[test]
    fn platform_spells_every_arch_and_secure_boot_state() {
        assert_eq!(
            platform_json(&platform()),
            json!({"os_build": 26_100, "arch": "x64", "test_signing": true, "secure_boot": "disabled"})
        );
        for (arch, name) in [
            (Arch::X64, "x64"),
            (Arch::Arm64, "arm64"),
            (Arch::Other, "other"),
        ] {
            let value = platform_json(&Platform { arch, ..platform() });
            assert_eq!(value["arch"], name);
        }
        for (secure_boot, name) in [
            (SecureBoot::Enabled, "enabled"),
            (SecureBoot::Disabled, "disabled"),
            (SecureBoot::Unsupported, "unsupported"),
        ] {
            let value = platform_json(&Platform {
                secure_boot,
                test_signing: false,
                ..platform()
            });
            assert_eq!(value["secure_boot"], name);
            assert_eq!(value["test_signing"], false);
        }
    }

    #[test]
    fn driver_carries_node_fields_only_when_installed() {
        assert_eq!(
            driver_json(&DriverState::NotInstalled),
            json!({"state": "not-installed"})
        );
        assert_eq!(
            driver_json(&DriverState::RebootRequired),
            json!({"state": "reboot-required"})
        );
        for (policy, name) in [
            (PolicyState::Applied, "applied"),
            (PolicyState::Missing, "missing"),
            (PolicyState::Unknown, "unknown"),
        ] {
            assert_eq!(
                driver_json(&DriverState::Installed {
                    problem_code: 52,
                    service_running: false,
                    exclusive_policy: policy,
                }),
                json!({
                    "state": "installed",
                    "problem_code": 52,
                    "service_running": false,
                    "exclusive_policy": name,
                })
            );
        }
    }

    #[test]
    fn outputs_spell_every_state_and_null_endpoints() {
        for (state, name) in [
            (DeviceState::Active, "active"),
            (DeviceState::Disabled, "disabled"),
            (DeviceState::NotPresent, "not-present"),
            (DeviceState::Unplugged, "unplugged"),
        ] {
            assert_eq!(output_json(&output("{a}", Some(2), state))["state"], name);
        }
        assert_eq!(
            output_json(&output(
                "{0.0.0.00000000}.{a}",
                Some(0),
                DeviceState::Active
            )),
            json!({
                "id": "{0.0.0.00000000}.{a}",
                "name": "공간 소프트리시버",
                "endpoint": 0,
                "state": "active",
            })
        );
        assert_eq!(
            output_json(&output("{b}", None, DeviceState::Active))["endpoint"],
            Value::Null
        );
    }

    #[test]
    fn default_roles_name_all_three_and_null_the_missing_ones() {
        let roles = DefaultRoles {
            console: Some(DeviceId("{a}".to_owned())),
            multimedia: Some(DeviceId("{b}".to_owned())),
            communications: None,
        };
        assert_eq!(
            default_roles_json(&roles),
            json!({"console": "{a}", "multimedia": "{b}", "communications": null})
        );
        assert_eq!(
            default_roles_json(&DefaultRoles::default()),
            json!({"console": null, "multimedia": null, "communications": null})
        );
        for (role, name) in [
            (Role::Console, "console"),
            (Role::Multimedia, "multimedia"),
            (Role::Communications, "communications"),
        ] {
            assert_eq!(role_name(role), name);
        }
    }

    #[test]
    fn show_holds_the_four_sections() {
        let outputs = [
            output("{a}", Some(0), DeviceState::Active),
            output("{b}", None, DeviceState::Unplugged),
        ];
        let roles = DefaultRoles {
            console: Some(DeviceId("{a}".to_owned())),
            ..DefaultRoles::default()
        };
        let show = show_json(&platform(), &DriverState::NotInstalled, &outputs, &roles);
        assert_eq!(show["platform"], platform_json(&platform()));
        assert_eq!(show["driver"], json!({"state": "not-installed"}));
        assert_eq!(show["outputs"].as_array().unwrap().len(), 2);
        assert_eq!(show["outputs"][1]["state"], "unplugged");
        assert_eq!(show["default_roles"]["console"], "{a}");
        assert_eq!(show["default_roles"]["communications"], Value::Null);
        assert_eq!(
            show_json(&platform(), &DriverState::NotInstalled, &[], &roles)["outputs"],
            json!([])
        );
    }

    #[test]
    fn set_default_prints_the_role_or_fails_with_the_host_error() {
        let id = DeviceId("{a}".to_owned());
        assert_eq!(
            set_default_answer(Role::Communications, &id, Ok(())),
            Answer {
                stdout: Some(json!({"ok": true, "role": "communications", "id": "{a}"})),
                error: None,
            }
        );
        let failed = set_default_answer(
            Role::Console,
            &id,
            Err(HostError::Failed("IPolicyConfig: E_NOINTERFACE".to_owned())),
        );
        assert_eq!(failed.stdout, None);
        assert_eq!(
            failed.error.as_deref(),
            Some("the host refused: IPolicyConfig: E_NOINTERFACE")
        );
    }

    #[test]
    fn setup_prints_the_outcome_a_decline_or_only_fails() {
        assert_eq!(
            setup_answer(Ok(SetupOutcome {
                reboot_required: true,
                message: "installed".to_owned(),
            })),
            Answer {
                stdout: Some(json!({"ok": true, "reboot_required": true, "message": "installed"})),
                error: None,
            }
        );
        let declined = setup_answer(Err(HostError::Declined));
        assert_eq!(
            declined.stdout,
            Some(json!({"ok": false, "declined": true}))
        );
        assert!(declined.error.is_some());
        for error in [
            HostError::Setup("pnputil: 5".to_owned()),
            HostError::Failed("no directory".to_owned()),
            HostError::Unsupported,
        ] {
            let answer = setup_answer(Err(error.clone()));
            assert_eq!(answer.stdout, None);
            assert_eq!(answer.error, Some(error.to_string()));
        }
    }

    #[test]
    fn ascii_json_escapes_non_ascii_and_parses_back_equal() {
        let value = json!({"name": "공간 소프트리시버 🎧", "id": "{a}", "endpoint": null});
        let text = to_ascii_json(&value);
        assert!(text.is_ascii());
        assert!(text.contains("\\uacf5"));
        assert!(text.contains("\\ud83c\\udfa7"));
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), value);
    }
}
