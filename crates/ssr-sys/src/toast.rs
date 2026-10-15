//! Best-effort desktop notifications (ADR 0033). Registration supplies the
//! per-user display metadata; the installer must also put this AUMID on the
//! app's Start menu shortcut. No activation handler is registered here.

use std::path::Path;
use std::process::{Command, Stdio};

use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::core::HSTRING;

use crate::devices::ComGuard;
use crate::toast_xml;

/// Matches the identifier in the shell's tauri.conf.json and installer shortcut.
pub const APP_USER_MODEL_ID: &str = "io.github.115dkk.spatial-softreceiver";

#[derive(Debug, thiserror::Error)]
pub enum ToastError {
    #[error("toast registration: {0}")]
    Registration(#[from] std::io::Error),
    #[error("toast registration command failed: {0}")]
    RegistrationFailed(std::process::ExitStatus),
    #[error("toast COM apartment: {0}")]
    Apartment(String),
    #[error("toast WinRT: {0}")]
    Windows(#[from] windows::core::Error),
}

/// Registers display metadata in HKCU without elevation, idempotently.
///
/// This does not create the required AUMID-bearing Start menu shortcut; the
/// installer owns that step. `icon` is the installed icon or executable path.
pub fn register(display_name: &str, icon: Option<&Path>) -> Result<(), ToastError> {
    set_registration_value("DisplayName", display_name.as_ref())?;
    if let Some(icon) = icon {
        set_registration_value("IconUri", icon.as_os_str())?;
    }
    Ok(())
}

fn set_registration_value(name: &str, value: &std::ffi::OsStr) -> Result<(), ToastError> {
    use std::os::windows::process::CommandExt as _;

    // reg.exe keeps registry FFI outside our unsafe budget. Arguments are
    // passed directly, never through a shell, and stdout must not reach the
    // guarded app's heartbeat pipe. Use the system copy, not PATH lookup.
    let root = std::env::var_os("SystemRoot").ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "SystemRoot is not set")
    })?;
    let status = Command::new(Path::new(&root).join("System32").join("reg.exe"))
        .arg("add")
        .arg(format!(
            "HKCU\\Software\\Classes\\AppUserModelId\\{APP_USER_MODEL_ID}"
        ))
        .args(["/v", name, "/t", "REG_SZ", "/d"])
        .arg(value)
        .arg("/f")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .status()?;
    if !status.success() {
        return Err(ToastError::RegistrationFailed(status));
    }
    Ok(())
}

/// Submits a title and body without waiting for dismissal or activation.
/// Call on a non-UI thread that can join the multithreaded COM apartment.
/// A successful submission does not guarantee display (Windows can mute it).
pub fn show(title: &str, body: &str) -> Result<(), ToastError> {
    let _com = ComGuard::new().map_err(|error| ToastError::Apartment(error.to_string()))?;
    let xml = XmlDocument::new()?;
    xml.LoadXml(&HSTRING::from(toast_xml::document(title, body)))?;
    let toast = ToastNotification::CreateToastNotification(&xml)?;
    let notifier =
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_USER_MODEL_ID))?;
    notifier.Show(&toast)?;
    Ok(())
}
