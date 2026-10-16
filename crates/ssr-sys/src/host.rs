#![allow(unsafe_code)]

//! The Windows adapter of the supervisor's `Host` port (ADR 0013, ADR 0032).
//!
//! Everything here runs with the user's rights: platform facts, our device
//! node's state, the render endpoints, the default device of each role, the
//! restore point file, and one elevated `ssr-setup` run per administrator task
//! (the tasks themselves live in `ssr-setup-sys`, which the app never links).
//!
//! * Endpoints are enumerated in every state through `IMMDeviceEnumerator`
//!   (`wasapi` lists active ones only); `wasapi::Device` then reads each one's
//!   id, state and friendly name. Our endpoint number comes from the KS filter
//!   the endpoint's topology connects to (`ssr_abi::ks_path::map_filter`),
//!   owned by a device node whose hardware ids include
//!   `ssr_abi::setup_contract::PACKAGE_HARDWARE_ID`; display names never
//!   select an endpoint.
//! * The driver state carries the exclusive-mode policy (ADR 0028): our
//!   active endpoints' property stores, opened read-only, judged by
//!   `ssr_abi::exclusive_policy::policy_state`. Only `ssr-setup` writes it.
//! * Default devices change through `IPolicyConfig`, which Windows does not
//!   document. It is the interface every default-device switcher uses
//!   (EarTrumpet, SoundSwitch and AudioDeviceCmdlets among the open ones):
//!   CLSID `870af99c-171d-4f9e-af0d-e63df40c2bc9` (CPolicyConfigClient), IID
//!   `f8679f50-850a-41cf-9c72-430f290290c8`, Windows 7 and later. Its vtable is
//!   IUnknown, then GetMixFormat, GetDeviceFormat, ResetDeviceFormat,
//!   SetDeviceFormat, GetProcessingPeriod, SetProcessingPeriod, GetShareMode,
//!   SetShareMode, GetPropertyValue, SetPropertyValue, SetDefaultEndpoint
//!   (`HRESULT (LPCWSTR id, ERole role)`) and SetEndpointVisibility. Only
//!   SetDefaultEndpoint is declared as a function pointer, so no other slot can
//!   be called. A changed interface would come with a new IID, so creating
//!   this one fails (E_NOINTERFACE) instead of calling a moved slot, and the
//!   supervisor keeps the roles (ADR 0032).

use std::ffi::{OsStr, c_void};
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::mem::{size_of, size_of_val};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use ssr_abi::exclusive_policy::{
    ALLOW_EXCLUSIVE_PID, ENDPOINT_MASK_VALUE, EXCLUSIVE_PRIORITY_PID, POLICY_FMTID, PolicyValues,
    install_mask, policy_state,
};
use ssr_abi::ks_path::map_filter;
use ssr_abi::setup_contract::{
    PACKAGE_HARDWARE_ID, RESULT_FILE, TaskLine, TaskResult, parameters_key,
};
use ssr_types::ports::PolicyState;
use ssr_types::{
    Arch, DefaultRoles, DeviceId, DeviceState, DriverState, Host, HostError, OutputDevice,
    OutputLayout, Platform, Role, SecureBoot, SetupOutcome, SetupTask,
};
use windows::{
    Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemCodeIntegrityInformation},
    Win32::{
        Devices::{
            DeviceAndDriverInstallation::{
                CM_DEVNODE_STATUS_FLAGS, CM_GETIDLIST_FILTER_ENUMERATOR, CM_Get_DevNode_PropertyW,
                CM_Get_DevNode_Status, CM_Get_Device_ID_List_SizeW, CM_Get_Device_ID_ListW,
                CM_LOCATE_DEVNODE_PHANTOM, CM_Locate_DevNodeW, CM_PROB, CM_PROB_PHANTOM, CONFIGRET,
                CR_BUFFER_SMALL, CR_SUCCESS, DN_HAS_PROBLEM, DN_NEED_RESTART,
            },
            Properties::{DEVPKEY_Device_HardwareIds, DEVPROP_TYPE_STRING_LIST, DEVPROPTYPE},
        },
        Foundation::{
            CloseHandle, DEVPROPKEY, ERROR_CANCELLED, ERROR_NOT_FOUND, HANDLE, PROPERTYKEY,
            WAIT_OBJECT_0, WIN32_ERROR,
        },
        Media::Audio::{
            DEVICE_STATE, DEVICE_STATEMASK_ALL, ERole, IConnector, IDeviceTopology, IMMDevice,
            IMMDeviceEnumerator, MMDeviceEnumerator, eCommunications, eConsole, eMultimedia,
            eRender,
        },
        System::{
            Com::{
                CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ,
                StructuredStorage::{PROPVARIANT_0_0, PropVariantClear},
            },
            Registry::{
                HKEY_LOCAL_MACHINE, REG_ROUTINE_FLAGS, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
                RegGetValueW,
            },
            Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
            Variant::{VT_EMPTY, VT_UI4},
            WindowsProgramming::{CODEINTEGRITY_OPTION_TESTSIGN, SYSTEM_CODEINTEGRITY_INFORMATION},
        },
        UI::Shell::{
            PropertiesSystem::IPropertyStore, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC,
            SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
        },
    },
    core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, PCWSTR, PWSTR},
};

use crate::devices::ComGuard;
use crate::driver_ring::DriverRing;

/// The restore point file in the state directory (`supervisor::restore_point`,
/// read by `ssr-guard`).
pub const RESTORE_POINT_FILE: &str = "restore-point.json";

/// `SW_HIDE`: `ssr-setup` is a console program; its window is not shown.
const SW_HIDE: i32 = 0;

const CURRENT_VERSION_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
const SECURE_BOOT_KEY: &str = r"SYSTEM\CurrentControlSet\Control\SecureBoot\State";

/// The supervisor's `Host` on Windows.
///
/// Its endpoint and role calls join the multithreaded apartment
/// (`ComGuard`), which a thread already in a single-threaded apartment cannot
/// join, so it must be driven from a thread that is not the UI thread: the
/// shell runs the supervisor on its own thread. On such a thread the lists
/// come back empty and the role change fails.
#[derive(Clone, Debug)]
pub struct WindowsHost {
    setup_exe: PathBuf,
    state_dir: PathBuf,
}

impl WindowsHost {
    /// `setup_exe` is `ssr-setup.exe` next to the app (the driver package lies
    /// beside it); `state_dir` is `%LOCALAPPDATA%\Spatial SoftReceiver`, which
    /// holds the restore point and `ssr-setup`'s result file.
    pub fn new(setup_exe: PathBuf, state_dir: PathBuf) -> Self {
        Self {
            setup_exe,
            state_dir,
        }
    }
}

impl Host for WindowsHost {
    fn platform(&mut self) -> Platform {
        Platform {
            os_build: os_build(),
            arch: native_arch(),
            test_signing: test_signing(),
            secure_boot: secure_boot(),
        }
    }

    fn driver(&mut self) -> DriverState {
        let nodes = our_devnodes();
        let Some(node) = nodes.first() else {
            return DriverState::NotInstalled;
        };
        let Ok((status, problem)) = devnode_status(node.devinst) else {
            // Located only as a phantom: the node exists but is not present.
            return DriverState::Installed {
                problem_code: CM_PROB_PHANTOM.0,
                service_running: false,
                exclusive_policy: PolicyState::Unknown,
            };
        };
        if status.0 & DN_NEED_RESTART.0 != 0 {
            return DriverState::RebootRequired;
        }
        let problem_code = if status.0 & DN_HAS_PROBLEM.0 != 0 {
            problem.0
        } else {
            0
        };
        let owners: Vec<String> = nodes.into_iter().map(|node| node.instance_id).collect();
        DriverState::Installed {
            problem_code,
            service_running: DriverRing::probe().is_ok(),
            exclusive_policy: exclusive_policy(&owners),
        }
    }

    fn outputs(&mut self) -> Vec<OutputDevice> {
        let owners: Vec<String> = our_devnodes()
            .into_iter()
            .map(|node| node.instance_id)
            .collect();
        let Ok(_com) = ComGuard::new() else {
            return Vec::new();
        };
        let Ok(devices) = render_devices() else {
            return Vec::new();
        };
        devices
            .into_iter()
            .filter_map(|device| describe(device, &owners))
            .collect()
    }

    fn default_roles(&mut self) -> DefaultRoles {
        let mut roles = DefaultRoles::default();
        let Ok(_com) = ComGuard::new() else {
            return roles;
        };
        let Ok(enumerator) = wasapi::DeviceEnumerator::new() else {
            return roles;
        };
        for role in Role::ALL {
            // E_NOTFOUND: Windows has no default device for the role.
            let id = enumerator
                .get_default_device_for_role(&wasapi::Direction::Render, &wasapi_role(role))
                .and_then(|device| device.get_id())
                .ok();
            roles.set(role, id.map(DeviceId));
        }
        roles
    }

    fn set_default_role(&mut self, role: Role, device: &DeviceId) -> Result<(), HostError> {
        if device.0.is_empty() || device.0.contains('\0') {
            return Err(HostError::Failed(format!(
                "invalid endpoint id {:?}",
                device.0
            )));
        }
        let _com = ComGuard::new().map_err(|error| HostError::Failed(error.to_string()))?;
        let policy: PolicyConfig = create_instance(&POLICY_CONFIG_CLIENT)
            .map_err(|error| HostError::Failed(format!("IPolicyConfig: {error}")))?;
        policy.set_default_endpoint(&device.0, endpoint_role(role))
    }

    fn supports_layout(
        &mut self,
        output: &DeviceId,
        layout: OutputLayout,
    ) -> Result<bool, HostError> {
        let _com = ComGuard::new().map_err(|error| HostError::Failed(error.to_string()))?;
        let enumerator = wasapi::DeviceEnumerator::new()
            .map_err(|error| HostError::Failed(error.to_string()))?;
        let devices = enumerator
            .get_device_collection(&wasapi::Direction::Render)
            .map_err(|error| HostError::Failed(error.to_string()))?;
        for index in 0..devices
            .get_nbr_devices()
            .map_err(|error| HostError::Failed(error.to_string()))?
        {
            let device = devices
                .get_device_at_index(index)
                .map_err(|error| HostError::Failed(error.to_string()))?;
            let id = device
                .get_id()
                .map_err(|error| HostError::Failed(error.to_string()))?;
            if id == output.0 {
                return Ok(crate::devices::supports_layout(&device, layout));
            }
        }
        Err(HostError::Failed(format!(
            "render endpoint {:?} was not found",
            output.0
        )))
    }

    fn run_setup(&mut self, task: &SetupTask) -> Result<SetupOutcome, HostError> {
        let package_dir = self.setup_exe.parent().ok_or_else(|| {
            HostError::Failed(format!("{} has no directory", self.setup_exe.display()))
        })?;
        fs::create_dir_all(&self.state_dir).map_err(|error| io_failure(&self.state_dir, error))?;
        let result_path = self.state_dir.join(RESULT_FILE);
        remove_if_present(&result_path).map_err(|error| io_failure(&result_path, error))?;
        let line = TaskLine::from_setup_task(task, package_dir, result_path.clone());
        let parameters = line
            .to_command_line()
            .map_err(|error| HostError::Failed(error.to_string()))?;

        let exit = {
            let _com = ComGuard::new().map_err(|error| HostError::Failed(error.to_string()))?;
            run_elevated(&self.setup_exe, &parameters)?
        };

        let text = fs::read_to_string(&result_path);
        let _ = remove_if_present(&result_path);
        let no_result = || HostError::Setup(format!("no result from ssr-setup (exit {exit})"));
        let result = text
            .ok()
            .and_then(|text| TaskResult::from_json(&text).ok())
            .ok_or_else(no_result)?;
        result.into_outcome()
    }

    fn write_restore_point(&mut self, json: Option<&str>) -> Result<(), HostError> {
        let path = self.state_dir.join(RESTORE_POINT_FILE);
        match json {
            Some(json) => replace_file(&path, json),
            None => remove_if_present(&path),
        }
        .map_err(|error| io_failure(&path, error))
    }
}

fn io_failure(path: &Path, error: io::Error) -> HostError {
    HostError::Failed(format!("{}: {error}", path.display()))
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Writes `text` to a temporary file beside `path`, then renames it over
/// `path`, so a reader sees the old file or the new one, never a torn one.
fn replace_file(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    {
        let mut file = File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)
}

/// NUL-terminated UTF-16 for the W functions.
fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

/// The UTF-16 strings of a REG_MULTI_SZ or DEVPROP_TYPE_STRING_LIST buffer.
fn split_multi_sz(units: &[u16]) -> Vec<String> {
    units
        .split(|&unit| unit == 0)
        .filter(|item| !item.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

// ---------------------------------------------------------------- platform

/// `RegGetValueW` under HKEY_LOCAL_MACHINE into `data`; the bytes written.
fn read_machine_value(
    subkey: &str,
    value: &str,
    flags: REG_ROUTINE_FLAGS,
    data: &mut [u16],
) -> Result<usize, WIN32_ERROR> {
    let subkey = wide(subkey);
    let value = wide(value);
    let mut bytes = u32::try_from(size_of_val(data)).unwrap_or(u32::MAX);
    // SAFETY: both names are NUL-terminated and outlive the call; `data` is
    // writable for `bytes` bytes, and `bytes` is a live u32 the call updates.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            flags,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    if status.is_ok() {
        Ok(bytes as usize)
    } else {
        Err(status)
    }
}

fn read_machine_dword(subkey: &str, value: &str) -> Option<u32> {
    let mut data = [0_u16; 2];
    let bytes = read_machine_value(subkey, value, RRF_RT_REG_DWORD, &mut data).ok()?;
    (bytes == size_of::<u32>()).then(|| u32::from(data[0]) | u32::from(data[1]) << 16)
}

/// `CurrentBuildNumber` (REG_SZ); 0 when unreadable.
fn os_build() -> u32 {
    let mut text = [0_u16; 32];
    let Ok(bytes) = read_machine_value(
        CURRENT_VERSION_KEY,
        "CurrentBuildNumber",
        RRF_RT_REG_SZ,
        &mut text,
    ) else {
        return 0;
    };
    let units = &text[..(bytes / 2).min(text.len())];
    String::from_utf16_lossy(units)
        .trim_end_matches('\0')
        .trim()
        .parse()
        .unwrap_or(0)
}

fn secure_boot() -> SecureBoot {
    match read_machine_dword(SECURE_BOOT_KEY, "UEFISecureBootEnabled") {
        Some(1) => SecureBoot::Enabled,
        Some(0) => SecureBoot::Disabled,
        _ => SecureBoot::Unsupported,
    }
}

/// The machine's architecture, not this process's: a process under WOW64 sees
/// its own in `PROCESSOR_ARCHITECTURE` and the machine's in
/// `PROCESSOR_ARCHITEW6432`.
fn native_arch() -> Arch {
    let name = std::env::var("PROCESSOR_ARCHITEW6432")
        .or_else(|_| std::env::var("PROCESSOR_ARCHITECTURE"))
        .unwrap_or_default();
    arch_named(&name)
}

fn arch_named(name: &str) -> Arch {
    if name.eq_ignore_ascii_case("AMD64") {
        Arch::X64
    } else if name.eq_ignore_ascii_case("ARM64") {
        Arch::Arm64
    } else {
        Arch::Other
    }
}

/// CODEINTEGRITY_OPTION_TESTSIGN from SystemCodeIntegrityInformation, which
/// needs no privilege; false when the query fails.
fn test_signing() -> bool {
    let length = u32::try_from(size_of::<SYSTEM_CODEINTEGRITY_INFORMATION>())
        .expect("a two-field structure");
    let mut info = SYSTEM_CODEINTEGRITY_INFORMATION {
        Length: length,
        CodeIntegrityOptions: 0,
    };
    let mut returned = 0_u32;
    // SAFETY: `info` is a live, writable SYSTEM_CODEINTEGRITY_INFORMATION whose
    // Length is its size, as this class requires; `returned` is a live u32.
    let status = unsafe {
        NtQuerySystemInformation(
            SystemCodeIntegrityInformation,
            (&raw mut info).cast(),
            length,
            &mut returned,
        )
    };
    status.is_ok() && info.CodeIntegrityOptions & CODEINTEGRITY_OPTION_TESTSIGN != 0
}

// ---------------------------------------------------------------- device node

/// One of our device nodes: a root-enumerated instance whose hardware ids
/// include `PACKAGE_HARDWARE_ID`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DevNode {
    instance_id: String,
    devinst: u32,
}

/// Our device nodes, present or not, in the order cfgmgr32 lists them.
fn our_devnodes() -> Vec<DevNode> {
    root_instance_ids()
        .into_iter()
        .filter_map(|instance_id| {
            let devinst = locate_devnode(&instance_id)?;
            hardware_ids(devinst)
                .iter()
                .any(|id| id.eq_ignore_ascii_case(PACKAGE_HARDWARE_ID))
                .then_some(DevNode {
                    instance_id,
                    devinst,
                })
        })
        .collect()
}

/// Every device instance the ROOT enumerator has, by
/// `CM_Get_Device_ID_List_SizeW` and `CM_Get_Device_ID_ListW`.
fn root_instance_ids() -> Vec<String> {
    let filter = wide("ROOT");
    // A device that arrives between the two calls makes the list too small;
    // ask again a few times.
    for _ in 0..3 {
        let mut length = 0_u32;
        // SAFETY: `length` is a live u32 and `filter` is NUL-terminated.
        let status = unsafe {
            CM_Get_Device_ID_List_SizeW(
                &mut length,
                PCWSTR(filter.as_ptr()),
                CM_GETIDLIST_FILTER_ENUMERATOR,
            )
        };
        if status != CR_SUCCESS || length == 0 {
            return Vec::new();
        }
        let mut list = vec![0_u16; length as usize];
        // SAFETY: `filter` is NUL-terminated; the binding passes the buffer's
        // length with it, and the buffer is writable for that length.
        let status = unsafe {
            CM_Get_Device_ID_ListW(
                PCWSTR(filter.as_ptr()),
                &mut list,
                CM_GETIDLIST_FILTER_ENUMERATOR,
            )
        };
        match status {
            CR_SUCCESS => return split_multi_sz(&list),
            CR_BUFFER_SMALL => continue,
            _ => return Vec::new(),
        }
    }
    Vec::new()
}

/// `CM_Locate_DevNodeW`, phantoms included: a node that is installed but not
/// present still blocks a second install.
fn locate_devnode(instance_id: &str) -> Option<u32> {
    let id = wide(instance_id);
    let mut devinst = 0_u32;
    // SAFETY: `devinst` is a live u32 and `id` is NUL-terminated.
    let status =
        unsafe { CM_Locate_DevNodeW(&mut devinst, PCWSTR(id.as_ptr()), CM_LOCATE_DEVNODE_PHANTOM) };
    (status == CR_SUCCESS).then_some(devinst)
}

/// `CM_Get_DevNode_PropertyW`: the property's type and size, and its value in
/// `buffer` when one is given and large enough.
fn devnode_property(
    devinst: u32,
    key: &DEVPROPKEY,
    buffer: Option<&mut [u16]>,
) -> (CONFIGRET, DEVPROPTYPE, u32) {
    let mut kind = DEVPROPTYPE::default();
    let mut size = buffer.as_ref().map_or(0, |buffer| {
        u32::try_from(size_of_val(&**buffer)).unwrap_or(u32::MAX)
    });
    let pointer = buffer.map(|buffer| buffer.as_mut_ptr().cast::<u8>());
    // SAFETY: `key`, `kind` and `size` are live; `pointer` is None (size 0) or
    // writable for `size` bytes, and the buffer outlives the call.
    let status =
        unsafe { CM_Get_DevNode_PropertyW(devinst, key, &mut kind, pointer, &mut size, 0) };
    (status, kind, size)
}

fn hardware_ids(devinst: u32) -> Vec<String> {
    let (_, _, size) = devnode_property(devinst, &DEVPKEY_Device_HardwareIds, None);
    if size == 0 {
        return Vec::new();
    }
    let mut buffer = vec![0_u16; (size as usize).div_ceil(2)];
    let (status, kind, size) =
        devnode_property(devinst, &DEVPKEY_Device_HardwareIds, Some(&mut buffer));
    if status != CR_SUCCESS || kind != DEVPROP_TYPE_STRING_LIST {
        return Vec::new();
    }
    let units = (size as usize / 2).min(buffer.len());
    split_multi_sz(&buffer[..units])
}

/// `CM_Get_DevNode_Status`: the node's status flags and problem number.
fn devnode_status(devinst: u32) -> Result<(CM_DEVNODE_STATUS_FLAGS, CM_PROB), CONFIGRET> {
    let mut status = CM_DEVNODE_STATUS_FLAGS::default();
    let mut problem = CM_PROB::default();
    // SAFETY: both outputs are live and writable for the call.
    let result = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, devinst, 0) };
    if result == CR_SUCCESS {
        Ok((status, problem))
    } else {
        Err(result)
    }
}

// ---------------------------------------------------------------- endpoints

/// `CoCreateInstance` of `clsid` for the interface `T`, in-process or local.
fn create_instance<T: Interface>(clsid: &GUID) -> windows::core::Result<T> {
    // SAFETY: `clsid` is a live GUID, the caller holds a `ComGuard` on this
    // thread, and the binding asks for T's IID and wraps the result as T.
    unsafe { CoCreateInstance(clsid, None, CLSCTX_ALL) }
}

/// Every render endpoint in every state (`EnumAudioEndpoints(eRender,
/// DEVICE_STATEMASK_ALL)`); one that fails to load is left out.
fn render_devices() -> windows::core::Result<Vec<IMMDevice>> {
    let enumerator: IMMDeviceEnumerator = create_instance(&MMDeviceEnumerator)?;
    // SAFETY: a COM call on a live enumerator with a valid flow and state mask.
    let collection =
        unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE(DEVICE_STATEMASK_ALL)) }?;
    // SAFETY: a COM call on a live collection.
    let count = unsafe { collection.GetCount() }?;
    Ok((0..count)
        .filter_map(|index| {
            // SAFETY: a COM call on a live collection with an index below its count.
            unsafe { collection.Item(index) }.ok()
        })
        .collect())
}

/// One endpoint as the supervisor sees it; `None` when Windows will not give
/// its id or state.
fn describe(device: IMMDevice, owners: &[String]) -> Option<OutputDevice> {
    let endpoint = if owners.is_empty() {
        None
    } else {
        our_endpoint(&device, owners)
    };
    let device = wasapi::Device::from_immdevice(device).ok()?;
    let id = device.get_id().ok()?;
    let state = match device.get_state().ok()? {
        wasapi::DeviceState::Active => DeviceState::Active,
        wasapi::DeviceState::Disabled => DeviceState::Disabled,
        wasapi::DeviceState::NotPresent => DeviceState::NotPresent,
        wasapi::DeviceState::Unplugged => DeviceState::Unplugged,
    };
    Some(OutputDevice {
        id: DeviceId(id),
        name: device.get_friendlyname().unwrap_or_default(),
        endpoint,
        state,
    })
}

/// Our endpoint number from the KS filters the endpoint's topology connects
/// to (`tools/ci/endpoints.ps1`, `Enumerate`). A topology that does not
/// activate or a connector that fails gives `None`, and so does a device whose
/// connectors name two different numbers: never a guess.
fn our_endpoint(device: &IMMDevice, owners: &[String]) -> Option<u16> {
    // SAFETY: a COM activation on a live device; the binding asks for
    // IDeviceTopology's IID and passes no activation parameters.
    let topology: IDeviceTopology = unsafe { device.Activate(CLSCTX_ALL, None) }.ok()?;
    // SAFETY: a COM call on a live topology.
    let count = unsafe { topology.GetConnectorCount() }.ok()?;
    let mut found = None;
    for index in 0..count {
        // SAFETY: a COM call on a live topology with an index below its count.
        let connector = unsafe { topology.GetConnector(index) }.ok()?;
        let path = match connected_device_id(&connector) {
            Ok(path) => path,
            // E_NOTFOUND: the connector is not connected.
            Err(error) if error.code() == ERROR_NOT_FOUND.to_hresult() => continue,
            Err(_) => return None,
        };
        let Some(endpoint) = map_filter(&path, owners) else {
            continue;
        };
        match found {
            None => found = Some(endpoint),
            Some(previous) if previous != endpoint => return None,
            Some(_) => {}
        }
    }
    found
}

/// `IConnector::GetDeviceIdConnectedTo`.
fn connected_device_id(connector: &IConnector) -> windows::core::Result<String> {
    // SAFETY: a COM call on a live connector; the string it returns is ours.
    let id = unsafe { connector.GetDeviceIdConnectedTo() }?;
    Ok(take_com_string(id))
}

/// Copies a string COM allocated for the caller, then frees it with
/// `CoTaskMemFree`.
fn take_com_string(text: PWSTR) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: COM returned a NUL-terminated string that stays valid until it is
    // freed below.
    let value = unsafe { text.to_hstring() };
    // SAFETY: the string was allocated with CoTaskMemAlloc for this caller and
    // is freed exactly once, after its last read.
    unsafe { CoTaskMemFree(Some(text.0.cast_const().cast())) };
    value.to_string_lossy()
}

fn wasapi_role(role: Role) -> wasapi::Role {
    match role {
        Role::Console => wasapi::Role::Console,
        Role::Multimedia => wasapi::Role::Multimedia,
        Role::Communications => wasapi::Role::Communications,
    }
}

fn endpoint_role(role: Role) -> ERole {
    match role {
        Role::Console => eConsole,
        Role::Multimedia => eMultimedia,
        Role::Communications => eCommunications,
    }
}

// ---------------------------------------------------------------- exclusive policy

const ALLOW_EXCLUSIVE: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(POLICY_FMTID),
    pid: ALLOW_EXCLUSIVE_PID,
};
const EXCLUSIVE_PRIORITY: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(POLICY_FMTID),
    pid: EXCLUSIVE_PRIORITY_PID,
};

/// The exclusive-mode policy as our active endpoints' property stores hold
/// it, judged by `ssr_abi::exclusive_policy::policy_state` for the driver's
/// `EndpointMask`. Read-only: only `ssr-setup` writes it.
fn exclusive_policy(owners: &[String]) -> PolicyState {
    let Ok(_com) = ComGuard::new() else {
        return PolicyState::Unknown;
    };
    let Ok(devices) = render_devices() else {
        return PolicyState::Unknown;
    };
    let mask = install_mask(read_machine_dword(&parameters_key(), ENDPOINT_MASK_VALUE));
    let active: Vec<(u16, Option<PolicyValues>)> = devices
        .iter()
        .filter_map(|device| {
            let endpoint = our_endpoint(device, owners)?;
            let state = wasapi::Device::from_immdevice(device.clone())
                .and_then(|device| device.get_state());
            match state {
                Ok(wasapi::DeviceState::Active) => Some((endpoint, read_policy(device))),
                Ok(_) => None,
                // A state that cannot be read is a failed read.
                Err(_) => Some((endpoint, None)),
            }
        })
        .collect();
    policy_state(mask, &active)
}

/// Both policy values from the endpoint's property store, opened read-only;
/// `None` when a read fails.
fn read_policy(device: &IMMDevice) -> Option<PolicyValues> {
    // SAFETY: a COM call on a live device with read-only access.
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok()?;
    Some(PolicyValues {
        allow_exclusive: read_dword(&store, &ALLOW_EXCLUSIVE)?,
        exclusive_priority: read_dword(&store, &EXCLUSIVE_PRIORITY)?,
    })
}

/// `IPropertyStore::GetValue` of a DWORD property: `Some(None)` when it is
/// empty (VT_EMPTY), `None` when the read fails or it holds another type. The
/// value is released with `PropVariantClear`.
fn read_dword(store: &IPropertyStore, key: &PROPERTYKEY) -> Option<Option<u32>> {
    // SAFETY: a COM call on a live store with a live key; the value it
    // returns is ours and cleared below.
    let mut value = unsafe { store.GetValue(key) }.ok()?;
    let read = {
        // SAFETY: every PROPVARIANT begins with this member (tag and value);
        // the DECIMAL member only overlays it.
        let inner: &PROPVARIANT_0_0 = unsafe { &value.Anonymous.Anonymous };
        if inner.vt == VT_EMPTY {
            Some(None)
        } else if inner.vt == VT_UI4 {
            // SAFETY: the VT_UI4 tag says the union holds ulVal.
            Some(Some(unsafe { inner.Anonymous.ulVal }))
        } else {
            None
        }
    };
    // SAFETY: `value` came from GetValue, is owned here and cleared once.
    let _ = unsafe { PropVariantClear(&mut value) };
    read
}

// ---------------------------------------------------------------- IPolicyConfig

/// CPolicyConfigClient (see the module documentation).
const POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// An `IPolicyConfig` pointer, released on drop like any interface.
#[repr(transparent)]
#[derive(Clone, Debug)]
struct PolicyConfig(IUnknown);

/// The start of IPolicyConfig's vtable; only SetDefaultEndpoint is callable.
#[repr(C)]
#[allow(dead_code, reason = "layout only: these slots are never called")]
struct IPolicyConfigVtbl {
    base: IUnknown_Vtbl,
    /// GetMixFormat, GetDeviceFormat, ResetDeviceFormat, SetDeviceFormat,
    /// GetProcessingPeriod, SetProcessingPeriod, GetShareMode, SetShareMode,
    /// GetPropertyValue, SetPropertyValue.
    _slots: [usize; 10],
    set_default_endpoint: unsafe extern "system" fn(*mut c_void, PCWSTR, i32) -> HRESULT,
    set_endpoint_visibility: usize,
}

// SAFETY: PolicyConfig is a transparent IUnknown (one non-null interface
// pointer), IID is IPolicyConfig's, so CoCreateInstance returns that
// interface, and IPolicyConfigVtbl repeats the start of its vtable: IUnknown,
// ten methods, SetDefaultEndpoint.
unsafe impl Interface for PolicyConfig {
    type Vtable = IPolicyConfigVtbl;
    const IID: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);
}

impl PolicyConfig {
    fn set_default_endpoint(&self, id: &str, role: ERole) -> Result<(), HostError> {
        let id = wide(id);
        // SAFETY: self is a live IPolicyConfig; the slot is
        // SetDefaultEndpoint(this, LPCWSTR, ERole), and `id` is NUL-terminated
        // and outlives the call.
        let result = unsafe {
            (self.vtable().set_default_endpoint)(self.as_raw(), PCWSTR(id.as_ptr()), role.0)
        };
        if result.is_ok() {
            Ok(())
        } else {
            Err(HostError::Failed(format!(
                "IPolicyConfig::SetDefaultEndpoint: {:#010X}",
                result.0
            )))
        }
    }
}

// ---------------------------------------------------------------- ssr-setup

/// Runs `program` elevated (`ShellExecuteExW`, verb `runas`) and waits for its
/// exit code. A declined UAC prompt is [`HostError::Declined`].
fn run_elevated(program: &Path, parameters: &str) -> Result<u32, HostError> {
    let verb = wide("runas");
    let file = wide(program);
    let parameters = wide(parameters);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: u32::try_from(size_of::<SHELLEXECUTEINFOW>()).expect("a small structure"),
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE,
        ..Default::default()
    };
    // SAFETY: `info` is a live SHELLEXECUTEINFOW with cbSize set and every
    // other field zero; its strings are NUL-terminated and outlive the call.
    if let Err(error) = unsafe { ShellExecuteExW(&mut info) } {
        return Err(if error.code() == ERROR_CANCELLED.to_hresult() {
            HostError::Declined
        } else {
            HostError::Failed(format!("ShellExecuteEx: {error}"))
        });
    }
    let process = info.hProcess;
    if process.is_invalid() {
        return Err(HostError::Failed(
            "ShellExecuteEx started no process".into(),
        ));
    }
    let exit = wait_for_exit(process);
    // SAFETY: SEE_MASK_NOCLOSEPROCESS handed this function the process handle;
    // it is closed exactly once, here, after the last use.
    let _ = unsafe { CloseHandle(process) };
    exit
}

/// `WaitForSingleObject(INFINITE)`, then `GetExitCodeProcess`.
fn wait_for_exit(process: HANDLE) -> Result<u32, HostError> {
    // SAFETY: `process` is a live process handle the caller owns.
    let waited = unsafe { WaitForSingleObject(process, INFINITE) };
    if waited != WAIT_OBJECT_0 {
        return Err(HostError::Failed(format!(
            "WaitForSingleObject(ssr-setup): {:#x}",
            waited.0
        )));
    }
    let mut code = 0_u32;
    // SAFETY: `process` is a live process handle and `code` a writable u32.
    unsafe { GetExitCodeProcess(process, &mut code) }
        .map_err(|error| HostError::Failed(format!("GetExitCodeProcess: {error}")))?;
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn architecture_names_map_to_arch() {
        assert_eq!(arch_named("AMD64"), Arch::X64);
        assert_eq!(arch_named("amd64"), Arch::X64);
        assert_eq!(arch_named("ARM64"), Arch::Arm64);
        assert_eq!(arch_named("x86"), Arch::Other);
        assert_eq!(arch_named(""), Arch::Other);
    }

    #[test]
    fn multi_strings_split_on_nul() {
        let units: Vec<u16> = "ROOT\\A\0root\\b\0\0".encode_utf16().collect();
        assert_eq!(split_multi_sz(&units), ["ROOT\\A", "root\\b"]);
        assert!(split_multi_sz(&[0, 0]).is_empty());
    }

    #[test]
    fn restore_point_is_replaced_and_removed() {
        let dir = std::env::temp_dir().join(format!("ssr-host-test-{}", std::process::id()));
        let mut host = WindowsHost::new(dir.join("ssr-setup.exe"), dir.clone());
        host.write_restore_point(Some("{\"version\":1}")).unwrap();
        host.write_restore_point(Some("{\"version\":2}")).unwrap();
        let path = dir.join(RESTORE_POINT_FILE);
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"version\":2}");
        host.write_restore_point(None).unwrap();
        assert!(!path.exists());
        host.write_restore_point(None).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}
