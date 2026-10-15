#![allow(unsafe_code)]

//! Every `unsafe` call of the crate, each in a small safe function whose
//! documentation names the Windows API it calls. `endpoints` and `tasks` are
//! written in safe Rust on top of these.
//!
//! Handles are owned by types that release them on drop (`Com`,
//! `DeviceInfoSet`, `CertStore`, `Certificate`); COM interfaces release
//! themselves. Failures keep the API's name and its HRESULT, Win32 error or
//! CONFIGRET in [`WinError`], which `tasks` reports as the result's `error`
//! and `code`.

use std::ffi::{OsStr, c_void};
use std::marker::PhantomData;
use std::mem::{ManuallyDrop, size_of, size_of_val};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::NonNull;
use std::rc::Rc;

use windows::{
    Win32::{
        Devices::{
            DeviceAndDriverInstallation::{
                CM_DEVNODE_STATUS_FLAGS, CM_DISABLE_UI_NOT_OK, CM_Disable_DevNode,
                CM_Enable_DevNode, CM_GETIDLIST_FILTER_ENUMERATOR, CM_Get_DevNode_PropertyW,
                CM_Get_DevNode_Status, CM_Get_Device_ID_List_SizeW, CM_Get_Device_ID_ListW,
                CM_LOCATE_DEVNODE_PHANTOM, CM_Locate_DevNodeW, CM_PROB, CONFIGRET, CR_BUFFER_SMALL,
                CR_NEED_RESTART, CR_SUCCESS, DI_FUNCTION, DI_NEEDREBOOT, DI_NEEDRESTART,
                DICD_GENERATE_ID, DN_HAS_PROBLEM, HDEVINFO, INSTALLFLAG_FORCE, SP_COPY_STYLE,
                SP_DEVINFO_DATA, SP_DEVINSTALL_PARAMS_W, SPDRP_HARDWAREID, SPOST_PATH,
                SetupCopyOEMInfW, SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList,
                SetupDiCreateDeviceInfoW, SetupDiDestroyDeviceInfoList,
                SetupDiGetDeviceInstallParamsW, SetupDiGetINFClassW, SetupDiOpenDeviceInfoW,
                SetupDiSetDeviceRegistryPropertyW, UpdateDriverForPlugAndPlayDevicesW,
            },
            Properties::{DEVPKEY_Device_HardwareIds, DEVPROP_TYPE_STRING_LIST, DEVPROPTYPE},
        },
        Foundation::{
            ERROR_FILE_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_NOT_FOUND,
            PROPERTYKEY, WIN32_ERROR,
        },
        Media::Audio::{
            DEVICE_STATE, DEVICE_STATEMASK_ALL, IConnector, IDeviceTopology, IMMDevice,
            IMMDeviceEnumerator, MMDeviceEnumerator, eRender,
        },
        Security::Cryptography::{
            CERT_CONTEXT, CERT_NAME_RDN_TYPE, CERT_NAME_STR_REVERSE_FLAG, CERT_OPEN_STORE_FLAGS,
            CERT_QUERY_ENCODING_TYPE, CERT_STORE_ADD_REPLACE_EXISTING, CERT_STORE_PROV_SYSTEM_W,
            CERT_SYSTEM_STORE_LOCAL_MACHINE, CERT_X500_NAME_STR, CertAddEncodedCertificateToStore,
            CertCloseStore, CertCreateCertificateContext, CertDeleteCertificateFromStore,
            CertDuplicateCertificateContext, CertEnumCertificatesInStore,
            CertFreeCertificateContext, CertGetNameStringW, CertOpenStore, HCERTSTORE,
            PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
        },
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
                CoUninitialize, STGM_READ, STGM_READWRITE,
                StructuredStorage::{
                    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
                    PropVariantClear,
                },
            },
            Registry::{
                HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_DWORD, REG_ROUTINE_FLAGS, RRF_RT_REG_DWORD,
                RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
                RegSetKeyValueW,
            },
            Variant::{VT_EMPTY, VT_UI4},
        },
        UI::Shell::PropertiesSystem::IPropertyStore,
    },
    core::{BOOL, GUID, PCWSTR, PWSTR},
};

/// A failed Windows call: the API, its code and the code's text.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{api}: {detail}")]
pub(crate) struct WinError {
    pub(crate) api: &'static str,
    /// The HRESULT (Win32 errors as `HRESULT_FROM_WIN32`) or the CONFIGRET.
    pub(crate) code: i32,
    pub(crate) detail: String,
}

impl WinError {
    pub(crate) fn hresult(api: &'static str, error: &windows::core::Error) -> Self {
        Self {
            api,
            code: error.code().0,
            detail: format!("{:#010X} {}", error.code().0, error.message()),
        }
    }

    fn win32(api: &'static str, status: WIN32_ERROR) -> Self {
        Self::hresult(api, &status.into())
    }

    fn config(api: &'static str, status: CONFIGRET) -> Self {
        Self {
            api,
            code: i32::try_from(status.0).unwrap_or(i32::MAX),
            detail: format!("CONFIGRET {:#x}", status.0),
        }
    }
}

/// NUL-terminated UTF-16 for the W functions.
pub(crate) fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

/// The strings of a REG_MULTI_SZ or DEVPROP_TYPE_STRING_LIST buffer.
fn split_multi_sz(units: &[u16]) -> Vec<String> {
    units
        .split(|&unit| unit == 0)
        .filter(|item| !item.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

// ---------------------------------------------------------------- COM

/// This thread's membership of the multithreaded apartment:
/// `CoInitializeEx(COINIT_MULTITHREADED)`, balanced by `CoUninitialize` on
/// drop. The `Rc` marker keeps it on the thread that joined; declare it before
/// any interface so the interfaces are released first.
#[derive(Debug)]
pub(crate) struct Com(PhantomData<Rc<()>>);

impl Com {
    pub(crate) fn init() -> Result<Self, WinError> {
        // SAFETY: no reserved pointer is passed; a success (S_OK or S_FALSE)
        // is balanced by Drop on this same thread.
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        result
            .ok()
            .map_err(|error| WinError::hresult("CoInitializeEx", &error))?;
        Ok(Self(PhantomData))
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: balances the successful CoInitializeEx of `init` on the
        // thread that made it (the marker keeps this value there).
        unsafe { CoUninitialize() };
    }
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

/// `CoCreateInstance(MMDeviceEnumerator)`. The caller holds a [`Com`].
pub(crate) fn device_enumerator() -> Result<IMMDeviceEnumerator, WinError> {
    // SAFETY: a live CLSID; the caller's `Com` initialized this thread, and
    // the binding asks for IMMDeviceEnumerator's IID and wraps the result.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .map_err(|error| WinError::hresult("CoCreateInstance(MMDeviceEnumerator)", &error))
}

/// `IMMDeviceEnumerator::EnumAudioEndpoints(eRender, DEVICE_STATEMASK_ALL)`,
/// then `IMMDeviceCollection::GetCount` and `Item`: every render endpoint in
/// every state. One that fails to load is left out.
pub(crate) fn render_endpoints(
    enumerator: &IMMDeviceEnumerator,
) -> Result<Vec<IMMDevice>, WinError> {
    // SAFETY: a COM call on a live enumerator with a valid flow and state mask.
    let collection =
        unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE(DEVICE_STATEMASK_ALL)) }
            .map_err(|error| WinError::hresult("EnumAudioEndpoints", &error))?;
    // SAFETY: a COM call on a live collection.
    let count = unsafe { collection.GetCount() }
        .map_err(|error| WinError::hresult("IMMDeviceCollection::GetCount", &error))?;
    Ok((0..count)
        .filter_map(|index| {
            // SAFETY: a COM call on a live collection with an index below its count.
            unsafe { collection.Item(index) }.ok()
        })
        .collect())
}

/// `IMMDevice::GetId`.
pub(crate) fn endpoint_id(device: &IMMDevice) -> Result<String, WinError> {
    // SAFETY: a COM call on a live device; the string it returns is ours.
    let id =
        unsafe { device.GetId() }.map_err(|error| WinError::hresult("IMMDevice::GetId", &error))?;
    Ok(take_com_string(id))
}

/// `IMMDevice::GetState` (DEVICE_STATE_ACTIVE is 1).
pub(crate) fn endpoint_state(device: &IMMDevice) -> Result<u32, WinError> {
    // SAFETY: a COM call on a live device.
    unsafe { device.GetState() }
        .map(|state| state.0)
        .map_err(|error| WinError::hresult("IMMDevice::GetState", &error))
}

/// `IMMDevice::Activate::<IDeviceTopology>`, then
/// `IDeviceTopology::GetConnectorCount` and `GetConnector`: the endpoint's
/// topology connectors.
pub(crate) fn connectors(device: &IMMDevice) -> Result<Vec<IConnector>, WinError> {
    // SAFETY: a COM activation on a live device; the binding asks for
    // IDeviceTopology's IID and passes no activation parameters.
    let topology: IDeviceTopology = unsafe { device.Activate(CLSCTX_ALL, None) }
        .map_err(|error| WinError::hresult("IMMDevice::Activate(IDeviceTopology)", &error))?;
    // SAFETY: a COM call on a live topology.
    let count = unsafe { topology.GetConnectorCount() }
        .map_err(|error| WinError::hresult("IDeviceTopology::GetConnectorCount", &error))?;
    (0..count)
        .map(|index| {
            // SAFETY: a COM call on a live topology with an index below its count.
            unsafe { topology.GetConnector(index) }
                .map_err(|error| WinError::hresult("IDeviceTopology::GetConnector", &error))
        })
        .collect()
}

/// `IConnector::GetDeviceIdConnectedTo`; `None` for E_NOTFOUND (the connector
/// is not connected).
pub(crate) fn connected_device_id(connector: &IConnector) -> Result<Option<String>, WinError> {
    // SAFETY: a COM call on a live connector; the string it returns is ours.
    match unsafe { connector.GetDeviceIdConnectedTo() } {
        Ok(id) => Ok(Some(take_com_string(id))),
        Err(error) if error.code() == ERROR_NOT_FOUND.to_hresult() => Ok(None),
        Err(error) => Err(WinError::hresult(
            "IConnector::GetDeviceIdConnectedTo",
            &error,
        )),
    }
}

/// `IMMDevice::OpenPropertyStore`, STGM_READWRITE when `write` (which needs
/// an administrator) and STGM_READ otherwise.
pub(crate) fn property_store(device: &IMMDevice, write: bool) -> Result<IPropertyStore, WinError> {
    let access = if write { STGM_READWRITE } else { STGM_READ };
    // SAFETY: a COM call on a live device with a valid access mode.
    unsafe { device.OpenPropertyStore(access) }
        .map_err(|error| WinError::hresult("IMMDevice::OpenPropertyStore", &error))
}

/// `IPropertyStore::GetValue` of a DWORD property: `None` when it is empty
/// (VT_EMPTY), an error when it holds another type. The value is released
/// with `PropVariantClear`.
pub(crate) fn read_dword(
    store: &IPropertyStore,
    key: &PROPERTYKEY,
) -> Result<Option<u32>, WinError> {
    // SAFETY: a COM call on a live store with a live key; the value it
    // returns is ours and cleared below.
    let mut value = unsafe { store.GetValue(key) }
        .map_err(|error| WinError::hresult("IPropertyStore::GetValue", &error))?;
    let read = {
        // SAFETY: every PROPVARIANT begins with this member (tag and value);
        // the DECIMAL member only overlays it.
        let inner: &PROPVARIANT_0_0 = unsafe { &value.Anonymous.Anonymous };
        if inner.vt == VT_EMPTY {
            Ok(None)
        } else if inner.vt == VT_UI4 {
            // SAFETY: the VT_UI4 tag says the union holds ulVal.
            Ok(Some(unsafe { inner.Anonymous.ulVal }))
        } else {
            Err(WinError {
                api: "IPropertyStore::GetValue",
                code: 0,
                detail: format!("unexpected variant type {}", inner.vt.0),
            })
        }
    };
    // SAFETY: `value` came from GetValue, is owned here and cleared once.
    let _ = unsafe { PropVariantClear(&mut value) };
    read
}

/// `IPropertyStore::SetValue` with a VT_UI4 (`Some`) or VT_EMPTY (`None`).
pub(crate) fn write_dword(
    store: &IPropertyStore,
    key: &PROPERTYKEY,
    value: Option<u32>,
) -> Result<(), WinError> {
    let value = PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: if value.is_some() { VT_UI4 } else { VT_EMPTY },
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 {
                    ulVal: value.unwrap_or(0),
                },
            }),
        },
    };
    // SAFETY: a COM call on a live store; `key` and `value` (a plain VT_UI4 or
    // VT_EMPTY that owns nothing) outlive the call, which copies the value.
    unsafe { store.SetValue(key, &value) }
        .map_err(|error| WinError::hresult("IPropertyStore::SetValue", &error))
}

/// `IPropertyStore::Commit`.
pub(crate) fn commit(store: &IPropertyStore) -> Result<(), WinError> {
    // SAFETY: a COM call on a live store.
    unsafe { store.Commit() }.map_err(|error| WinError::hresult("IPropertyStore::Commit", &error))
}

// ---------------------------------------------------------------- cfgmgr32

/// `CM_Get_Device_ID_List_SizeW` and `CM_Get_Device_ID_ListW` with
/// CM_GETIDLIST_FILTER_ENUMERATOR: every device instance id of `enumerator`,
/// present or not.
pub(crate) fn device_ids(enumerator: &str) -> Result<Vec<String>, WinError> {
    let filter = wide(enumerator);
    let mut status = CR_SUCCESS;
    // A device that arrives between the two calls makes the list too small;
    // ask again a few times.
    for _ in 0..3 {
        let mut length = 0_u32;
        // SAFETY: `length` is a live u32 and `filter` is NUL-terminated.
        status = unsafe {
            CM_Get_Device_ID_List_SizeW(
                &mut length,
                PCWSTR(filter.as_ptr()),
                CM_GETIDLIST_FILTER_ENUMERATOR,
            )
        };
        if status != CR_SUCCESS {
            return Err(WinError::config("CM_Get_Device_ID_List_SizeW", status));
        }
        if length == 0 {
            return Ok(Vec::new());
        }
        let mut list = vec![0_u16; length as usize];
        // SAFETY: `filter` is NUL-terminated; the binding passes the buffer's
        // length with it, and the buffer is writable for that length.
        status = unsafe {
            CM_Get_Device_ID_ListW(
                PCWSTR(filter.as_ptr()),
                &mut list,
                CM_GETIDLIST_FILTER_ENUMERATOR,
            )
        };
        match status {
            CR_SUCCESS => return Ok(split_multi_sz(&list)),
            CR_BUFFER_SMALL => continue,
            _ => break,
        }
    }
    Err(WinError::config("CM_Get_Device_ID_ListW", status))
}

/// `CM_Locate_DevNodeW` with CM_LOCATE_DEVNODE_PHANTOM: installed nodes that
/// are not present are found too.
pub(crate) fn locate_devnode(instance_id: &str) -> Option<u32> {
    let id = wide(instance_id);
    let mut devinst = 0_u32;
    // SAFETY: `devinst` is a live u32 and `id` is NUL-terminated.
    let status =
        unsafe { CM_Locate_DevNodeW(&mut devinst, PCWSTR(id.as_ptr()), CM_LOCATE_DEVNODE_PHANTOM) };
    (status == CR_SUCCESS).then_some(devinst)
}

/// `CM_Get_DevNode_PropertyW(DEVPKEY_Device_HardwareIds)`: empty when the
/// node has none or the read fails.
pub(crate) fn hardware_ids(devinst: u32) -> Vec<String> {
    let mut buffer: Vec<u16> = Vec::new();
    // The first pass learns the size (CR_BUFFER_SMALL), the second reads.
    for _ in 0..2 {
        let mut kind = DEVPROPTYPE::default();
        let mut size = u32::try_from(size_of::<u16>() * buffer.len()).unwrap_or(u32::MAX);
        let pointer = (!buffer.is_empty()).then(|| buffer.as_mut_ptr().cast::<u8>());
        // SAFETY: the key, `kind` and `size` are live; `pointer` is None (size
        // 0) or writable for `size` bytes, and the buffer outlives the call.
        let status = unsafe {
            CM_Get_DevNode_PropertyW(
                devinst,
                &DEVPKEY_Device_HardwareIds,
                &mut kind,
                pointer,
                &mut size,
                0,
            )
        };
        match status {
            CR_SUCCESS if kind == DEVPROP_TYPE_STRING_LIST && !buffer.is_empty() => {
                let units = (size as usize / 2).min(buffer.len());
                return split_multi_sz(&buffer[..units]);
            }
            CR_BUFFER_SMALL if size > 0 => buffer = vec![0; (size as usize).div_ceil(2)],
            _ => break,
        }
    }
    Vec::new()
}

/// `CM_Get_DevNode_Status`: the node's problem number when it has one
/// (DN_HAS_PROBLEM), `None` when it has none or is not present.
pub(crate) fn devnode_problem(devinst: u32) -> Option<u32> {
    let mut status = CM_DEVNODE_STATUS_FLAGS::default();
    let mut problem = CM_PROB::default();
    // SAFETY: both outputs are live and writable for the call.
    let result = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, devinst, 0) };
    (result == CR_SUCCESS && status.0 & DN_HAS_PROBLEM.0 != 0).then_some(problem.0)
}

/// `CM_Disable_DevNode(CM_DISABLE_UI_NOT_OK)`; true when Windows needs a
/// restart (CR_NEED_RESTART) to finish.
pub(crate) fn disable_devnode(devinst: u32) -> Result<bool, WinError> {
    // SAFETY: plain values; an unknown devinst is reported as a CONFIGRET.
    let status = unsafe { CM_Disable_DevNode(devinst, CM_DISABLE_UI_NOT_OK) };
    restart_needed("CM_Disable_DevNode", status)
}

/// `CM_Enable_DevNode`; true when Windows needs a restart to finish.
pub(crate) fn enable_devnode(devinst: u32) -> Result<bool, WinError> {
    // SAFETY: plain values; an unknown devinst is reported as a CONFIGRET.
    let status = unsafe { CM_Enable_DevNode(devinst, 0) };
    restart_needed("CM_Enable_DevNode", status)
}

fn restart_needed(api: &'static str, status: CONFIGRET) -> Result<bool, WinError> {
    match status {
        CR_SUCCESS => Ok(false),
        CR_NEED_RESTART => Ok(true),
        other => Err(WinError::config(api, other)),
    }
}

// ---------------------------------------------------------------- SetupAPI

/// `SetupDiGetINFClassW`: the class GUID and class name an INF installs into.
pub(crate) fn inf_class(inf: &Path) -> Result<(GUID, String), WinError> {
    let inf = wide(inf);
    let mut class = GUID::zeroed();
    let mut name = [0_u16; 64];
    // SAFETY: `inf` is NUL-terminated; `class` is a writable GUID and the
    // binding passes the name buffer's length with it.
    unsafe { SetupDiGetINFClassW(PCWSTR(inf.as_ptr()), &mut class, &mut name, None) }
        .map_err(|error| WinError::hresult("SetupDiGetINFClassW", &error))?;
    let length = name
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(name.len());
    Ok((class, String::from_utf16_lossy(&name[..length])))
}

/// A device information set (`SetupDiCreateDeviceInfoList`), destroyed with
/// `SetupDiDestroyDeviceInfoList` on drop.
#[derive(Debug)]
pub(crate) struct DeviceInfoSet(HDEVINFO);

impl DeviceInfoSet {
    /// An empty set, for `class` or for any class.
    pub(crate) fn new(class: Option<&GUID>) -> Result<Self, WinError> {
        // SAFETY: `class` is None or a live GUID; no parent window.
        unsafe { SetupDiCreateDeviceInfoList(class.map(|class| class as *const GUID), None) }
            .map(Self)
            .map_err(|error| WinError::hresult("SetupDiCreateDeviceInfoList", &error))
    }

    /// `SetupDiCreateDeviceInfoW(DICD_GENERATE_ID)`: a new root device
    /// `ROOT\<class name>\NNNN`, not yet registered.
    pub(crate) fn create_device(
        &self,
        class_name: &str,
        class: &GUID,
    ) -> Result<SP_DEVINFO_DATA, WinError> {
        let name = wide(class_name);
        let mut device = SP_DEVINFO_DATA {
            cbSize: u32::try_from(size_of::<SP_DEVINFO_DATA>()).expect("a small structure"),
            ..Default::default()
        };
        // SAFETY: the set is live; `name` is NUL-terminated, `class` a live
        // GUID, and `device` a writable SP_DEVINFO_DATA with cbSize set.
        unsafe {
            SetupDiCreateDeviceInfoW(
                self.0,
                PCWSTR(name.as_ptr()),
                class,
                PCWSTR::null(),
                None,
                DICD_GENERATE_ID,
                Some(&mut device),
            )
        }
        .map_err(|error| WinError::hresult("SetupDiCreateDeviceInfoW", &error))?;
        Ok(device)
    }

    /// `SetupDiSetDeviceRegistryPropertyW(SPDRP_HARDWAREID)` with one id as
    /// REG_MULTI_SZ (the id, then an empty string).
    pub(crate) fn set_hardware_id(
        &self,
        device: &mut SP_DEVINFO_DATA,
        hardware_id: &str,
    ) -> Result<(), WinError> {
        let bytes: Vec<u8> = hardware_id
            .encode_utf16()
            .chain([0, 0])
            .flat_map(u16::to_le_bytes)
            .collect();
        // SAFETY: the set is live and `device` belongs to it; the binding
        // passes the byte slice's length with it.
        unsafe { SetupDiSetDeviceRegistryPropertyW(self.0, device, SPDRP_HARDWAREID, Some(&bytes)) }
            .map_err(|error| {
                WinError::hresult(
                    "SetupDiSetDeviceRegistryPropertyW(SPDRP_HARDWAREID)",
                    &error,
                )
            })
    }

    /// `SetupDiCallClassInstaller(function)` for one device of the set.
    pub(crate) fn call_class_installer(
        &self,
        api: &'static str,
        function: DI_FUNCTION,
        device: &SP_DEVINFO_DATA,
    ) -> Result<(), WinError> {
        // SAFETY: the set is live and `device` belongs to it.
        unsafe { SetupDiCallClassInstaller(function, self.0, Some(device)) }
            .map_err(|error| WinError::hresult(api, &error))
    }

    /// `SetupDiGetDeviceInstallParamsW`: whether the last class installer
    /// call left the device waiting for a reboot (DI_NEEDREBOOT or
    /// DI_NEEDRESTART).
    pub(crate) fn needs_reboot(&self, device: &SP_DEVINFO_DATA) -> Result<bool, WinError> {
        let mut params = SP_DEVINSTALL_PARAMS_W {
            cbSize: u32::try_from(size_of::<SP_DEVINSTALL_PARAMS_W>()).expect("a small structure"),
            ..Default::default()
        };
        // SAFETY: the set is live and `device` belongs to it; `params` is a
        // writable SP_DEVINSTALL_PARAMS_W with cbSize set.
        unsafe { SetupDiGetDeviceInstallParamsW(self.0, Some(device), &mut params) }
            .map_err(|error| WinError::hresult("SetupDiGetDeviceInstallParamsW", &error))?;
        Ok(params.Flags.0 & (DI_NEEDREBOOT.0 | DI_NEEDRESTART.0) != 0)
    }

    /// `SetupDiOpenDeviceInfoW`: adds an existing device, by instance id.
    pub(crate) fn open_device(&self, instance_id: &str) -> Result<SP_DEVINFO_DATA, WinError> {
        let id = wide(instance_id);
        let mut device = SP_DEVINFO_DATA {
            cbSize: u32::try_from(size_of::<SP_DEVINFO_DATA>()).expect("a small structure"),
            ..Default::default()
        };
        // SAFETY: the set is live, `id` is NUL-terminated, and `device` is a
        // writable SP_DEVINFO_DATA with cbSize set.
        unsafe { SetupDiOpenDeviceInfoW(self.0, PCWSTR(id.as_ptr()), None, 0, Some(&mut device)) }
            .map_err(|error| WinError::hresult("SetupDiOpenDeviceInfoW", &error))?;
        Ok(device)
    }
}

impl Drop for DeviceInfoSet {
    fn drop(&mut self) {
        // SAFETY: this value owns the set and destroys it exactly once.
        let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

/// `UpdateDriverForPlugAndPlayDevicesW(INSTALLFLAG_FORCE)`: installs `inf` on
/// every device with `hardware_id`; true when Windows asks for a reboot.
pub(crate) fn update_driver(hardware_id: &str, inf: &Path) -> Result<bool, WinError> {
    let hardware_id = wide(hardware_id);
    let inf = wide(inf);
    let mut reboot = BOOL::default();
    // SAFETY: both strings are NUL-terminated and `reboot` is a writable BOOL;
    // no parent window.
    unsafe {
        UpdateDriverForPlugAndPlayDevicesW(
            None,
            PCWSTR(hardware_id.as_ptr()),
            PCWSTR(inf.as_ptr()),
            INSTALLFLAG_FORCE,
            Some(&mut reboot),
        )
    }
    .map_err(|error| WinError::hresult("UpdateDriverForPlugAndPlayDevicesW", &error))?;
    Ok(reboot.as_bool())
}

/// `SetupCopyOEMInfW(SPOST_PATH)`: stages the package in the driver store.
/// A package that is already there (ERROR_FILE_EXISTS) is fine.
pub(crate) fn copy_oem_inf(inf: &Path) -> Result<(), WinError> {
    let inf = wide(inf);
    // SAFETY: `inf` is NUL-terminated; no media location, destination buffer
    // or out-pointers are passed.
    match unsafe {
        SetupCopyOEMInfW(
            PCWSTR(inf.as_ptr()),
            PCWSTR::null(),
            SPOST_PATH,
            SP_COPY_STYLE(0),
            None,
            None,
            None,
        )
    } {
        Ok(()) => Ok(()),
        Err(error) if error.code() == ERROR_FILE_EXISTS.to_hresult() => Ok(()),
        Err(error) => Err(WinError::hresult("SetupCopyOEMInfW", &error)),
    }
}

// ---------------------------------------------------------------- certificates

/// A system store of the local machine (`CertOpenStore(CERT_STORE_PROV_SYSTEM_W,
/// CERT_SYSTEM_STORE_LOCAL_MACHINE)`), closed with `CertCloseStore` on drop.
#[derive(Debug)]
pub(crate) struct CertStore(HCERTSTORE);

impl CertStore {
    /// `name` is `Root` or `TrustedPublisher`.
    pub(crate) fn open_local_machine(name: &str) -> Result<Self, WinError> {
        let name = wide(name);
        // SAFETY: the system provider takes the store name as a NUL-terminated
        // UTF-16 string, which outlives the call; no crypto provider.
        unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_W,
                CERT_QUERY_ENCODING_TYPE(0),
                None,
                CERT_OPEN_STORE_FLAGS(CERT_SYSTEM_STORE_LOCAL_MACHINE),
                Some(name.as_ptr().cast::<c_void>()),
            )
        }
        .map(Self)
        .map_err(|error| WinError::hresult("CertOpenStore", &error))
    }

    /// `CertAddEncodedCertificateToStore(CERT_STORE_ADD_REPLACE_EXISTING)`.
    pub(crate) fn add(&self, der: &[u8]) -> Result<(), WinError> {
        // SAFETY: the store is live; the binding passes the DER slice's length
        // with it, and no context is returned.
        unsafe {
            CertAddEncodedCertificateToStore(
                Some(self.0),
                X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                der,
                CERT_STORE_ADD_REPLACE_EXISTING,
                None,
            )
        }
        .map_err(|error| WinError::hresult("CertAddEncodedCertificateToStore", &error))
    }

    /// `CertEnumCertificatesInStore`: every certificate in the store, each
    /// duplicated (`CertDuplicateCertificateContext`) so it outlives the
    /// enumeration.
    pub(crate) fn certificates(&self) -> Vec<Certificate> {
        let mut certificates = Vec::new();
        let mut previous: Option<*const CERT_CONTEXT> = None;
        loop {
            // SAFETY: the store is live; `previous` is None or the context the
            // previous call returned, which this call frees.
            let next = unsafe { CertEnumCertificatesInStore(self.0, previous) };
            if next.is_null() {
                return certificates;
            }
            // SAFETY: `next` is a live context; the duplicate is a reference of
            // its own, which the Certificate frees.
            let duplicate = unsafe { CertDuplicateCertificateContext(Some(next.cast_const())) };
            certificates.extend(NonNull::new(duplicate).map(Certificate));
            previous = Some(next.cast_const());
        }
    }
}

impl Drop for CertStore {
    fn drop(&mut self) {
        // SAFETY: this value owns the store handle and closes it exactly once;
        // contexts still alive keep their own reference to the store.
        let _ = unsafe { CertCloseStore(Some(self.0), 0) };
    }
}

/// A certificate context this crate owns, freed with
/// `CertFreeCertificateContext` on drop.
#[derive(Debug)]
pub(crate) struct Certificate(NonNull<CERT_CONTEXT>);

impl Certificate {
    /// `CertCreateCertificateContext` from a DER file's bytes.
    pub(crate) fn from_der(der: &[u8]) -> Result<Self, WinError> {
        // SAFETY: the binding passes the slice's length with it; the context
        // copies the bytes.
        let context =
            unsafe { CertCreateCertificateContext(X509_ASN_ENCODING | PKCS_7_ASN_ENCODING, der) };
        NonNull::new(context).map(Self).ok_or_else(|| {
            WinError::hresult(
                "CertCreateCertificateContext",
                &windows::core::Error::from_thread(),
            )
        })
    }

    /// `CertGetNameStringW(CERT_NAME_RDN_TYPE, CERT_X500_NAME_STR |
    /// CERT_NAME_STR_REVERSE_FLAG)`: the subject written `CN=…` first, the way
    /// .NET's `X509Certificate.Subject` (and so the PowerShell scripts) see it.
    pub(crate) fn subject(&self) -> String {
        let string_type = CERT_X500_NAME_STR.0 | CERT_NAME_STR_REVERSE_FLAG;
        let length = self.name_string(&string_type, None);
        let mut name = vec![0_u16; length as usize];
        let written = self.name_string(&string_type, Some(&mut name)) as usize;
        let end = written.saturating_sub(1).min(name.len());
        String::from_utf16_lossy(&name[..end])
    }

    fn name_string(&self, string_type: &u32, buffer: Option<&mut [u16]>) -> u32 {
        // SAFETY: the context is live; `string_type` is the DWORD this name
        // type reads, and the binding passes the buffer's length with it.
        unsafe {
            CertGetNameStringW(
                self.0.as_ptr(),
                CERT_NAME_RDN_TYPE,
                0,
                Some((string_type as *const u32).cast()),
                buffer,
            )
        }
    }

    /// `CertDeleteCertificateFromStore`: removes the certificate from the
    /// store it was enumerated from. The call frees the context, even when it
    /// fails, so it is not freed again.
    pub(crate) fn delete_from_store(self) -> Result<(), WinError> {
        let context = ManuallyDrop::new(self);
        // SAFETY: the context is live and owned here; the call frees it and
        // ManuallyDrop keeps Drop from freeing it a second time.
        unsafe { CertDeleteCertificateFromStore(context.0.as_ptr()) }
            .map_err(|error| WinError::hresult("CertDeleteCertificateFromStore", &error))
    }
}

impl Drop for Certificate {
    fn drop(&mut self) {
        // SAFETY: this value owns one reference to the context and frees it
        // exactly once.
        let _ = unsafe { CertFreeCertificateContext(Some(self.0.as_ptr().cast_const())) };
    }
}

// ---------------------------------------------------------------- registry

/// `RegSetKeyValueW(REG_DWORD)` under HKEY_LOCAL_MACHINE, which creates the
/// subkey when it is missing.
pub(crate) fn set_machine_dword(subkey: &str, name: &str, value: u32) -> Result<(), WinError> {
    let subkey = wide(subkey);
    let name = wide(name);
    let data = value.to_ne_bytes();
    // SAFETY: both names are NUL-terminated; `data` holds the four bytes the
    // call reads.
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(name.as_ptr()),
            REG_DWORD.0,
            Some(data.as_ptr().cast()),
            4,
        )
    };
    status
        .ok()
        .map_err(|_| WinError::win32("RegSetKeyValueW", status))
}

/// `RegGetValueW` under HKEY_LOCAL_MACHINE into `data`, for the value types
/// `flags` accepts; the bytes written.
fn read_machine_value(
    subkey: &str,
    name: &str,
    flags: REG_ROUTINE_FLAGS,
    data: &mut [u16],
) -> Result<usize, WIN32_ERROR> {
    let subkey = wide(subkey);
    let name = wide(name);
    let mut size = u32::try_from(size_of_val(data)).unwrap_or(u32::MAX);
    // SAFETY: both names are NUL-terminated; `data` is writable for `size`
    // bytes, and `size` is a live u32 the call updates.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(name.as_ptr()),
            flags,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.is_ok() {
        Ok(size as usize)
    } else {
        Err(status)
    }
}

/// A REG_DWORD under HKEY_LOCAL_MACHINE; `None` when the value is missing or
/// not a DWORD.
pub(crate) fn machine_dword(subkey: &str, name: &str) -> Option<u32> {
    let mut data = [0_u16; 2];
    let bytes = read_machine_value(subkey, name, RRF_RT_REG_DWORD, &mut data).ok()?;
    (bytes == size_of::<u32>()).then(|| u32::from(data[0]) | u32::from(data[1]) << 16)
}

/// A REG_SZ under HKEY_LOCAL_MACHINE (up to 1023 characters); `None` when the
/// value is missing, not a string or longer.
pub(crate) fn machine_string(subkey: &str, name: &str) -> Option<String> {
    let mut data = vec![0_u16; 1024];
    let bytes = read_machine_value(subkey, name, RRF_RT_REG_SZ, &mut data).ok()?;
    let units = &data[..(bytes / 2).min(data.len())];
    let end = units
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(units.len());
    Some(String::from_utf16_lossy(&units[..end]))
}

/// A registry key under HKEY_LOCAL_MACHINE opened for reading
/// (`RegOpenKeyExW`), closed with `RegCloseKey` on drop.
#[derive(Debug)]
pub(crate) struct RegKey(HKEY);

impl RegKey {
    /// `None` when the key does not exist.
    pub(crate) fn open_machine(subkey: &str) -> Result<Option<Self>, WinError> {
        let subkey = wide(subkey);
        let mut key = HKEY(std::ptr::null_mut());
        // SAFETY: the name is NUL-terminated and `key` is a writable HKEY.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(subkey.as_ptr()),
                None,
                KEY_READ,
                &mut key,
            )
        };
        if status.is_ok() {
            Ok(Some(Self(key)))
        } else if status == ERROR_FILE_NOT_FOUND {
            Ok(None)
        } else {
            Err(WinError::win32("RegOpenKeyExW", status))
        }
    }

    /// `RegEnumKeyExW`: the names of the key's subkeys (at most 255
    /// characters each, the registry's limit).
    pub(crate) fn subkey_names(&self) -> Result<Vec<String>, WinError> {
        let mut names = Vec::new();
        for index in 0_u32.. {
            let mut name = [0_u16; 256];
            let mut length = u32::try_from(name.len()).expect("a small buffer");
            // SAFETY: the key is open; `name` is writable for `length`
            // characters and `length` is a live u32 the call updates; no
            // class or time is asked for.
            let status = unsafe {
                RegEnumKeyExW(
                    self.0,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut length,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            if status.is_err() {
                return Err(WinError::win32("RegEnumKeyExW", status));
            }
            let length = (length as usize).min(name.len());
            names.push(String::from_utf16_lossy(&name[..length]));
        }
        Ok(names)
    }
}

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: this value owns the open key and closes it exactly once.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}
