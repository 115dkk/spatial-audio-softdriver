//! The administrator tasks `ssr-setup` runs elevated (ADR 0032), ported from
//! the test kit: `tools/ci/RootDevice.cs` and `tools/testkit/install.ps1` for
//! the install, `tools/testkit/uninstall.ps1` for the removal, and
//! `tools/ci/exclusive-policy.ps1` for the endpoints' exclusive-mode policy.
//!
//! Every Windows call goes through `win`; a failing one ends the task with
//! `"<task>: <API>: <code and text>"` and its HRESULT, Win32 error or
//! CONFIGRET as the result's `code`.

use std::fmt::Display;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use ssr_abi::exclusive_policy::{
    ALLOW_EXCLUSIVE_PID, ENDPOINT_MASK_VALUE, EXCLUSIVE_PRIORITY_PID, POLICY_FMTID, PolicyValues,
    install_mask, policy_endpoints,
};
use ssr_abi::setup_contract::{TaskLine, TaskName, TaskResult, parameters_key};
use windows::Win32::Devices::DeviceAndDriverInstallation::{DIF_REGISTERDEVICE, DIF_REMOVE};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::IMMDevice;
use windows::core::GUID;

use crate::driver_store::{self, DriverPackage};
use crate::endpoints::{OurEndpoint, our_endpoints};
use crate::identities::{self, CURRENT, Package};
use crate::removal::{
    ENDPOINT_OWNER_VALUE, MMDEVICES_RENDER_KEY, Remaining, endpoint_owned_by,
    service_delete_succeeded,
};
use crate::win::{self, CertStore, Certificate, Com, DeviceInfoSet, RegKey, WinError};

/// The stores a test-signing certificate must be in for Windows to load a
/// driver it signed (`install.ps1`).
const TRUST_STORES: [&str; 2] = ["Root", "TrustedPublisher"];

/// How long the tasks wait for the device's endpoints to turn active before
/// they apply their policy: more than 10 s on CI runners (`install.ps1`).
const ENDPOINT_WAIT: Duration = Duration::from_secs(30);
const ENDPOINT_POLL: Duration = Duration::from_secs(2);

/// `ERROR_SUCCESS_REBOOT_REQUIRED`, pnputil's exit code for a removal that
/// finishes at the next boot.
const PNPUTIL_REBOOT: i32 = 3010;

const ALLOW_EXCLUSIVE: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(POLICY_FMTID),
    pid: ALLOW_EXCLUSIVE_PID,
};
const EXCLUSIVE_PRIORITY: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(POLICY_FMTID),
    pid: EXCLUSIVE_PRIORITY_PID,
};

/// Why a task failed: the result's `error` and `code`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    pub(crate) error: String,
    pub(crate) code: Option<i32>,
}

impl Failure {
    pub(crate) fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            code: None,
        }
    }

    fn context(self, what: impl Display) -> Self {
        Self {
            error: format!("{what}: {}", self.error),
            code: self.code,
        }
    }

    fn into_result(self, task: TaskName) -> TaskResult {
        TaskResult::failure(format!("{task}: {}", self.error), self.code)
    }
}

impl From<WinError> for Failure {
    fn from(error: WinError) -> Self {
        Self {
            error: error.to_string(),
            code: Some(error.code),
        }
    }
}

/// Runs one task and answers with its result line.
pub fn run(line: &TaskLine) -> TaskResult {
    let _com = match Com::init() {
        Ok(com) => com,
        Err(error) => return Failure::from(error).into_result(line.task),
    };
    let outcome = match line.task {
        TaskName::Install => inf_of(line).and_then(|inf| install(&inf, line.cert.as_deref())),
        TaskName::Uninstall => Ok(uninstall()),
        TaskName::SetFormats => line
            .endpoint_mask
            .ok_or_else(|| Failure::new("--endpoint-mask is required"))
            .and_then(set_formats),
        TaskName::RestartDevice => restart_device(),
        TaskName::Repair => inf_of(line).and_then(|inf| repair(&inf, line.cert.as_deref())),
    };
    outcome.unwrap_or_else(|failure| failure.into_result(line.task))
}

/// The INF as a full path, which `UpdateDriverForPlugAndPlayDevicesW` needs.
fn inf_of(line: &TaskLine) -> Result<PathBuf, Failure> {
    let inf = line
        .inf
        .as_deref()
        .ok_or_else(|| Failure::new("--inf is required"))?;
    std::path::absolute(inf).map_err(|error| Failure::new(format!("{}: {error}", inf.display())))
}

/// One of our device nodes and the package it belongs to.
#[derive(Clone, Debug)]
struct DevNode {
    instance_id: String,
    devinst: u32,
    package: &'static Package,
}

/// The current package's root device nodes, with the legacy packages' when
/// `include_legacy`, present or not.
fn devnodes(include_legacy: bool) -> Result<Vec<DevNode>, Failure> {
    Ok(win::device_ids("ROOT")?
        .into_iter()
        .filter_map(|instance_id| {
            let devinst = win::locate_devnode(&instance_id)?;
            let hardware_ids = win::hardware_ids(devinst);
            let package = if include_legacy {
                identities::all().find(|package| package.owns_device(&hardware_ids))
            } else {
                CURRENT.owns_device(&hardware_ids).then_some(&CURRENT)
            }?;
            Some(DevNode {
                instance_id,
                devinst,
                package,
            })
        })
        .collect())
}

/// The current package's device node.
fn current_node() -> Result<DevNode, Failure> {
    devnodes(false)?
        .into_iter()
        .next()
        .ok_or_else(|| Failure::new("Spatial SoftReceiver is not installed"))
}

fn endpoint_list(endpoints: &[u16]) -> String {
    if endpoints.is_empty() {
        return "none".to_owned();
    }
    endpoints
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------- install, repair

fn install(inf: &Path, cert: Option<&Path>) -> Result<TaskResult, Failure> {
    if let Some(node) = devnodes(true)?.first() {
        return Err(Failure::new(format!(
            "already installed ({}); uninstall first",
            node.instance_id
        )));
    }
    if let Some(cert) = cert {
        trust_certificate(cert)?;
    }
    win::copy_oem_inf(inf)?;
    let reboot_required = create_root_device(inf)?;
    let node = devnodes(false)?
        .into_iter()
        .next()
        .ok_or_else(|| Failure::new("the root device was registered but cannot be found"))?;
    if !reboot_required {
        apply_policy_to_installed(&node)?;
    }
    // Until the reboot the endpoints do not exist; the host then reports the
    // policy missing and the supervisor asks for a repair, which applies it.
    Ok(TaskResult::success(
        format!("installed {}", node.instance_id),
        reboot_required,
    ))
}

/// Reinstalls the driver on the current device node (trusting the
/// certificate first, as `install` does) and applies the exclusive policy;
/// without a node, installs.
fn repair(inf: &Path, cert: Option<&Path>) -> Result<TaskResult, Failure> {
    let Some(node) = devnodes(false)?.into_iter().next() else {
        return install(inf, cert);
    };
    if let Some(cert) = cert {
        trust_certificate(cert)?;
    }
    let reboot_required = win::update_driver(CURRENT.hardware_id, inf)?;
    let mut message = format!("repaired {}", node.instance_id);
    if reboot_required {
        message.push_str("; the exclusive policy waits for the reboot");
    } else {
        let applied = apply_policy_to_installed(&node)?;
        message.push_str(&format!(
            "; exclusive policy on endpoints {}",
            endpoint_list(&applied)
        ));
    }
    Ok(TaskResult::success(message, reboot_required))
}

/// After an install or repair without a reboot: refuses a device with a
/// problem code, then applies the policy for the driver's `EndpointMask`.
fn apply_policy_to_installed(node: &DevNode) -> Result<Vec<u16>, Failure> {
    if let Some(problem) = win::devnode_problem(node.devinst) {
        return Err(Failure {
            error: format!("{} reports problem code {problem}", node.instance_id),
            code: i32::try_from(problem).ok(),
        });
    }
    let mask = install_mask(win::machine_dword(&parameters_key(), ENDPOINT_MASK_VALUE));
    apply_exclusive_policy(&node.instance_id, mask)
        .map_err(|failure| failure.context("exclusive policy"))
}

/// Trusts the package's test-signing certificate in [`TRUST_STORES`]. A
/// release package has no certificate file; a certificate that is not ours is
/// not trusted.
fn trust_certificate(path: &Path) -> Result<(), Failure> {
    let der = match fs::read(path) {
        Ok(der) => der,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(Failure::new(format!("{}: {error}", path.display()))),
    };
    let subject = Certificate::from_der(&der)?.subject();
    if !CURRENT.owns_certificate(&subject) {
        return Ok(());
    }
    for name in TRUST_STORES {
        CertStore::open_local_machine(name)?.add(&der)?;
    }
    Ok(())
}

/// `RootDevice.cs`: a root device with our hardware id, registered, then the
/// driver installed on it. True when Windows asks for a reboot.
fn create_root_device(inf: &Path) -> Result<bool, Failure> {
    let (class, class_name) = win::inf_class(inf)?;
    {
        let set = DeviceInfoSet::new(Some(&class))?;
        let mut device = set.create_device(&class_name, &class)?;
        set.set_hardware_id(&mut device, CURRENT.hardware_id)?;
        set.call_class_installer(
            "SetupDiCallClassInstaller(DIF_REGISTERDEVICE)",
            DIF_REGISTERDEVICE,
            &device,
        )?;
    }
    Ok(win::update_driver(CURRENT.hardware_id, inf)?)
}

// ---------------------------------------------------------------- policy

/// Waits up to [`ENDPOINT_WAIT`] for the endpoints of `mask` to turn active,
/// then applies the policy to them (it is idempotent). The endpoints that got
/// it, in order.
fn apply_exclusive_policy(instance_id: &str, mask: u8) -> Result<Vec<u16>, Failure> {
    let owners = [instance_id.to_owned()];
    let deadline = Instant::now() + ENDPOINT_WAIT;
    loop {
        let endpoints = our_endpoints(&owners)?;
        let ready = policy_endpoints(mask).all(|number| {
            endpoints
                .iter()
                .any(|endpoint| endpoint.endpoint == number && endpoint.is_active())
        });
        if ready || Instant::now() >= deadline {
            break;
        }
        thread::sleep(ENDPOINT_POLL);
    }
    set_exclusive_policy(&owners, mask)?;
    Ok(policy_endpoints(mask).collect())
}

/// `Set-SsrExclusivePolicy`: exactly one active endpoint per number of
/// `mask`, both values written and committed, then read back.
fn set_exclusive_policy(owners: &[String], mask: u8) -> Result<(), Failure> {
    let targets: Vec<OurEndpoint> = our_endpoints(owners)?
        .into_iter()
        .filter(OurEndpoint::is_active)
        .collect();
    for number in policy_endpoints(mask) {
        let matches: Vec<&OurEndpoint> = targets
            .iter()
            .filter(|target| target.endpoint == number)
            .collect();
        let [target] = matches.as_slice() else {
            return Err(Failure::new(format!(
                "expected one active endpoint {number}, found {}",
                matches.len()
            )));
        };
        let expected = PolicyValues::expected(number);
        write_policy(&target.device, expected)?;
        if read_policy(&target.device)? != expected {
            return Err(Failure::new(format!(
                "endpoint {number} ({}) policy readback differs",
                target.id
            )));
        }
    }
    Ok(())
}

/// `Clear-SsrExclusivePolicy`: both values removed from every endpoint of the
/// device, in any state, before the device is removed, so the endpoint's
/// leftover registry entry keeps no policy.
fn clear_exclusive_policy(instance_id: &str) -> Result<(), Failure> {
    for target in our_endpoints(&[instance_id.to_owned()])? {
        write_policy(&target.device, PolicyValues::CLEARED)?;
        if read_policy(&target.device)? != PolicyValues::CLEARED {
            return Err(Failure::new(format!(
                "endpoint {} ({}) policy was not cleared",
                target.endpoint, target.id
            )));
        }
    }
    Ok(())
}

fn write_policy(device: &IMMDevice, values: PolicyValues) -> Result<(), Failure> {
    let store = win::property_store(device, true)?;
    win::write_dword(&store, &ALLOW_EXCLUSIVE, values.allow_exclusive)?;
    win::write_dword(&store, &EXCLUSIVE_PRIORITY, values.exclusive_priority)?;
    win::commit(&store)?;
    Ok(())
}

fn read_policy(device: &IMMDevice) -> Result<PolicyValues, Failure> {
    let store = win::property_store(device, false)?;
    Ok(PolicyValues {
        allow_exclusive: win::read_dword(&store, &ALLOW_EXCLUSIVE)?,
        exclusive_priority: win::read_dword(&store, &EXCLUSIVE_PRIORITY)?,
    })
}

// ---------------------------------------------------------------- formats, restart

/// Writes the driver's `EndpointMask` parameter, restarts the device so the
/// driver reads it, then applies the policy to every endpoint the new mask
/// enables.
fn set_formats(endpoint_mask: u8) -> Result<TaskResult, Failure> {
    win::set_machine_dword(
        &parameters_key(),
        ENDPOINT_MASK_VALUE,
        u32::from(endpoint_mask),
    )?;
    let node = current_node()?;
    let reboot_required = restart(&node)?;
    let mut message = format!(
        "endpoint mask {endpoint_mask:#x}; restarted {}",
        node.instance_id
    );
    if reboot_required {
        message.push_str("; the exclusive policy waits for the reboot");
    } else {
        let applied = apply_exclusive_policy(&node.instance_id, endpoint_mask)
            .map_err(|failure| failure.context("exclusive policy"))?;
        message.push_str(&format!(
            "; exclusive policy on endpoints {}",
            endpoint_list(&applied)
        ));
    }
    Ok(TaskResult::success(message, reboot_required))
}

fn restart_device() -> Result<TaskResult, Failure> {
    let node = current_node()?;
    let reboot_required = restart(&node)?;
    Ok(TaskResult::success(
        format!("restarted {}", node.instance_id),
        reboot_required,
    ))
}

/// Disables and enables the device node; true when Windows needs a reboot to
/// finish.
fn restart(node: &DevNode) -> Result<bool, Failure> {
    let disable_pending = win::disable_devnode(node.devinst)?;
    let enable_pending = win::enable_devnode(node.devinst).map_err(|error| {
        Failure::from(error).context(format!("{} stays disabled", node.instance_id))
    })?;
    Ok(disable_pending || enable_pending)
}

// ---------------------------------------------------------------- uninstall

/// `uninstall.ps1`: every current and legacy device (policy first), every
/// driver package from our INFs, the orphan services, and our test-signing
/// certificates; then what stayed. A removal that only waits for a reboot
/// does not count as staying; it makes the result ask for the reboot.
fn uninstall() -> TaskResult {
    let mut removed = Vec::new();
    let mut pending = Vec::new();
    let mut failures = Vec::new();

    // Devices.
    let nodes = devnodes(true).unwrap_or_else(|failure| {
        failures.push(failure);
        Vec::new()
    });
    let owners: Vec<String> = nodes.iter().map(|node| node.instance_id.clone()).collect();
    let mut pending_devices = Vec::new();
    for node in &nodes {
        if let Err(failure) = clear_exclusive_policy(&node.instance_id) {
            failures.push(failure.context(format!("exclusive policy of {}", node.instance_id)));
        }
        match remove_device(&node.instance_id) {
            Ok(false) => removed.push(format!("device {}", node.instance_id)),
            Ok(true) => {
                pending.push(format!("device {}", node.instance_id));
                pending_devices.push(node.instance_id.clone());
            }
            Err(failure) => failures.push(failure.context(&node.instance_id)),
        }
    }

    // Driver packages.
    let mut pending_packages = Vec::new();
    match our_driver_packages() {
        Ok(packages) => {
            for package in packages {
                match delete_driver_package(&package.published) {
                    Ok(false) => removed.push(format!("driver package {}", package.published)),
                    Ok(true) => {
                        pending.push(format!("driver package {}", package.published));
                        pending_packages.push(package.published);
                    }
                    Err(failure) => failures.push(failure),
                }
            }
        }
        Err(failure) => failures.push(failure),
    }

    // Orphan services: never while one of their devices or packages stayed.
    let remaining_nodes = devnodes(true).unwrap_or_else(|failure| {
        failures.push(failure);
        Vec::new()
    });
    let remaining_packages = our_driver_packages().unwrap_or_else(|failure| {
        failures.push(failure);
        Vec::new()
    });
    for package in identities::all() {
        let device_stayed = remaining_nodes.iter().any(|node| node.package == package);
        let package_stayed = remaining_packages
            .iter()
            .any(|driver| package.owns_inf(&driver.original));
        if device_stayed || package_stayed {
            continue;
        }
        match delete_orphan_service(package) {
            Ok(true) => removed.push(format!("service {}", package.service)),
            Ok(false) => {}
            Err(failure) => failures.push(failure),
        }
    }

    // Certificates.
    for name in TRUST_STORES {
        match remove_certificates(name) {
            Ok(subjects) => removed.extend(
                subjects
                    .into_iter()
                    .map(|subject| format!("certificate {name} {subject}")),
            ),
            Err(failure) => failures.push(failure.context(name)),
        }
    }

    // What stayed, without what waits for the reboot.
    let mut remaining = Remaining {
        devices: remaining_nodes
            .iter()
            .filter(|node| !pending_devices.contains(&node.instance_id))
            .count(),
        driver_packages: remaining_packages
            .iter()
            .filter(|driver| !pending_packages.contains(&driver.published))
            .count(),
        ..Remaining::default()
    };
    for name in TRUST_STORES {
        match our_certificates(name) {
            Ok(count) => remaining.certificates += count,
            Err(failure) => failures.push(failure.context(name)),
        }
    }
    match service_keys() {
        Ok(count) => remaining.service_keys = count,
        Err(failure) => failures.push(failure),
    }
    let staying_owners: Vec<&String> = owners
        .iter()
        .filter(|owner| !pending_devices.contains(owner))
        .collect();
    match endpoint_entries(&staying_owners) {
        Ok(count) => remaining.endpoints = count,
        Err(failure) => failures.push(failure),
    }

    let mut message = format!(
        "removed: {}; remaining: {}",
        if removed.is_empty() {
            "nothing".to_owned()
        } else {
            removed.join(", ")
        },
        remaining.summary()
    );
    if !pending.is_empty() {
        message.push_str(&format!("; waiting for the reboot: {}", pending.join(", ")));
    }
    if remaining.service_keys > 0 && remaining.devices == 0 && remaining.driver_packages == 0 {
        message.push_str("; service keys can stay until the next boot");
    }
    let reboot_required = !pending.is_empty();
    if failures.is_empty() && !remaining.failed() {
        return TaskResult::success(message, reboot_required);
    }
    let error = if failures.is_empty() {
        format!("uninstall: something stayed ({})", remaining.summary())
    } else {
        let reasons: Vec<&str> = failures
            .iter()
            .map(|failure| failure.error.as_str())
            .collect();
        format!("uninstall: {}", reasons.join("; "))
    };
    TaskResult {
        ok: false,
        reboot_required,
        message,
        error: Some(error),
        code: failures.iter().find_map(|failure| failure.code),
    }
}

/// `SetupDiCallClassInstaller(DIF_REMOVE)` on the device, opened by instance
/// id; true when the removal finishes at the next boot.
fn remove_device(instance_id: &str) -> Result<bool, Failure> {
    let set = DeviceInfoSet::new(None)?;
    let device = set.open_device(instance_id)?;
    set.call_class_installer("SetupDiCallClassInstaller(DIF_REMOVE)", DIF_REMOVE, &device)?;
    Ok(set.needs_reboot(&device)?)
}

/// A program from System32, not from the search path: this process is
/// elevated.
fn system_program(name: &str, args: &[&str]) -> Result<Output, Failure> {
    let program = std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join(name))
        .unwrap_or_else(|| PathBuf::from(name));
    Command::new(&program)
        .args(args)
        .output()
        .map_err(|error| Failure::new(format!("{}: {error}", program.display())))
}

/// The driver store packages whose original INF is a current or legacy one.
fn our_driver_packages() -> Result<Vec<DriverPackage>, Failure> {
    let output = system_program("pnputil.exe", &["/enum-drivers"])?;
    if !output.status.success() {
        return Err(Failure {
            error: format!("pnputil /enum-drivers: exit {:?}", output.status.code()),
            code: output.status.code(),
        });
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(driver_store::parse_enum_drivers(&text)
        .into_iter()
        .filter(|package| identities::all().any(|ours| ours.owns_inf(&package.original)))
        .collect())
}

/// `pnputil /delete-driver <published> /uninstall /force`; true when the
/// removal finishes at the next boot.
fn delete_driver_package(published: &str) -> Result<bool, Failure> {
    let output = system_program(
        "pnputil.exe",
        &["/delete-driver", published, "/uninstall", "/force"],
    )?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(PNPUTIL_REBOOT) => Ok(true),
        code => Err(Failure {
            error: format!(
                "pnputil /delete-driver {published}: exit {code:?}: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            ),
            code,
        }),
    }
}

fn service_key(package: &Package) -> String {
    format!(r"SYSTEM\CurrentControlSet\Services\{}", package.service)
}

/// `sc.exe delete <service>` when the package's service key exists; true
/// when it ran. No direct deletion of the service's registry key.
fn delete_orphan_service(package: &Package) -> Result<bool, Failure> {
    if RegKey::open_machine(&service_key(package))?.is_none() {
        return Ok(false);
    }
    let output = system_program("sc.exe", &["delete", package.service])?;
    let code = output.status.code();
    if service_delete_succeeded(code) {
        Ok(true)
    } else {
        Err(Failure {
            error: format!(
                "sc.exe delete {}: exit {code:?}: {}",
                package.service,
                String::from_utf8_lossy(&output.stdout).trim()
            ),
            code,
        })
    }
}

/// How many current or legacy service keys exist.
fn service_keys() -> Result<usize, Failure> {
    let mut count = 0;
    for package in identities::all() {
        if RegKey::open_machine(&service_key(package))?.is_some() {
            count += 1;
        }
    }
    Ok(count)
}

/// Endpoint entries under [`MMDEVICES_RENDER_KEY`] whose saved owner is one of
/// `owners`. Reported, never deleted (`uninstall.ps1`).
fn endpoint_entries(owners: &[&String]) -> Result<usize, Failure> {
    if owners.is_empty() {
        return Ok(0);
    }
    let Some(render) = RegKey::open_machine(MMDEVICES_RENDER_KEY)? else {
        return Ok(0);
    };
    Ok(render
        .subkey_names()?
        .into_iter()
        .filter(|endpoint| {
            let properties = format!(r"{MMDEVICES_RENDER_KEY}\{endpoint}\Properties");
            win::machine_string(&properties, ENDPOINT_OWNER_VALUE)
                .is_some_and(|owner| endpoint_owned_by(&owner, owners))
        })
        .count())
}

/// Deletes our test-signing certificates from one local machine store; the
/// subjects deleted.
fn remove_certificates(store_name: &str) -> Result<Vec<String>, Failure> {
    let store = CertStore::open_local_machine(store_name)?;
    let mut removed = Vec::new();
    for certificate in store.certificates() {
        let subject = certificate.subject();
        if identities::all().any(|package| package.owns_certificate(&subject)) {
            certificate.delete_from_store()?;
            removed.push(subject);
        }
    }
    Ok(removed)
}

fn our_certificates(store_name: &str) -> Result<usize, Failure> {
    let store = CertStore::open_local_machine(store_name)?;
    Ok(store
        .certificates()
        .iter()
        .filter(|certificate| {
            let subject = certificate.subject();
            identities::all().any(|package| package.owns_certificate(&subject))
        })
        .count())
}
