#![allow(unsafe_code)]
//! Owned kernel resources. Handles passed to a child are explicitly selected.
use std::ffi::{OsStr, OsString};
use std::os::windows::{
    ffi::OsStrExt,
    io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use std::path::Path;
use std::time::Duration;
use windows::{
    Win32::{
        Foundation::*,
        Security::SECURITY_ATTRIBUTES,
        System::{JobObjects::*, Pipes::CreatePipe, Threading::*},
    },
    core::{PCWSTR, PWSTR},
};

pub fn suppress_error_dialogs() {
    use windows::Win32::System::Diagnostics::Debug::{
        SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
    };
    // SAFETY: called once during disposable host startup, before plugin threads exist.
    unsafe { SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX) };
}

pub fn inherited_file(value: usize) -> windows::core::Result<std::fs::File> {
    Ok(duplicate_value(value, false)?.into())
}

pub fn duplicate_value(value: usize, inheritable: bool) -> windows::core::Result<OwnedHandle> {
    let mut result = HANDLE::default();
    // SAFETY: obtains only a pseudo handle, never owned or closed.
    let current = unsafe { GetCurrentProcess() };
    // SAFETY: Windows validates the source handle; result is a fresh owned duplicate.
    unsafe {
        DuplicateHandle(
            current,
            HANDLE(value as *mut _),
            current,
            &mut result,
            0,
            inheritable,
            DUPLICATE_SAME_ACCESS,
        )
    }?;
    Ok(own(result))
}

pub fn parent_handle() -> windows::core::Result<OwnedHandle> {
    // SAFETY: pseudo handle is used only to duplicate into a real inheritable handle.
    let current = unsafe { GetCurrentProcess() };
    duplicate_value(current.0 as usize, true)
}

pub fn process_gone(handle: &OwnedHandle) -> bool {
    // SAFETY: live owned process handle, zero timeout, no pointer payload.
    unsafe { WaitForSingleObject(raw(handle), 0) == WAIT_OBJECT_0 }
}

fn own(handle: HANDLE) -> OwnedHandle {
    // SAFETY: private helper used only for newly created, uniquely owned handles.
    unsafe { OwnedHandle::from_raw_handle(handle.0) }
}
fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}
fn wide(value: &OsStr) -> windows::core::Result<Vec<u16>> {
    let mut value: Vec<u16> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(windows::core::Error::from_hresult(E_INVALIDARG));
    }
    value.push(0);
    Ok(value)
}

#[derive(Debug)]
pub struct Pipe {
    pub parent: std::fs::File,
    pub child: OwnedHandle,
}
impl Pipe {
    pub fn create(parent_reads: bool) -> windows::core::Result<Self> {
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            bInheritHandle: true.into(),
            ..Default::default()
        };
        let mut read = HANDLE::default();
        let mut write = HANDLE::default();
        // SAFETY: valid out pointers and inheritable attributes.
        unsafe { CreatePipe(&mut read, &mut write, Some(&sa), 0) }?;
        let (read, write) = (own(read), own(write));
        let (parent, child) = if parent_reads {
            (read, write)
        } else {
            (write, read)
        };
        // SAFETY: live owned handle; clear inheritance on the parent-only end.
        unsafe { SetHandleInformation(raw(&parent), HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }?;
        Ok(Self {
            parent: parent.into(),
            child,
        })
    }
    pub fn child_value(&self) -> usize {
        self.child.as_raw_handle() as usize
    }
}

struct Attributes {
    _storage: Vec<usize>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}
impl Attributes {
    fn new(handles: &[HANDLE]) -> windows::core::Result<Self> {
        let mut bytes = 0;
        // SAFETY: null list is the documented size-query call; bytes is writable.
        let _ = unsafe { InitializeProcThreadAttributeList(None, 1, Some(0), &mut bytes) };
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        // SAFETY: aligned backing storage is at least the queried size and remains live.
        unsafe { InitializeProcThreadAttributeList(Some(list), 1, Some(0), &mut bytes) }?;
        let attributes = Self {
            _storage: storage,
            list,
        };
        // SAFETY: list is initialized; handles storage remains valid until CreateProcess returns.
        unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(handles.as_ptr().cast()),
                size_of_val(handles),
                None,
                None,
            )
        }?;
        Ok(attributes)
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: initialized list is released before backing storage.
        unsafe { DeleteProcThreadAttributeList(self.list) };
    }
}

pub fn spawn_handles(
    exe: &Path,
    args: &[OsString],
    values: &[usize],
) -> windows::core::Result<ChildProcess> {
    let handles: Vec<HANDLE> = values
        .iter()
        .map(|value| HANDLE(*value as *mut _))
        .collect();
    ChildProcess::spawn(exe, args, &handles)
}

#[derive(Debug)]
pub struct ChildProcess {
    job: OwnedHandle,
    process: OwnedHandle,
    pid: u32,
}
impl ChildProcess {
    pub fn spawn(exe: &Path, args: &[OsString], inherit: &[HANDLE]) -> windows::core::Result<Self> {
        let exe = wide(exe.as_os_str())?;
        let mut command = OsString::from("\"ssr-plughost\"");
        for arg in args {
            command.push(" ");
            // Only decimal handles and switch names are used by this protocol.
            let text = arg.to_string_lossy();
            if text.is_empty() || !text.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return Err(windows::core::Error::from_hresult(E_INVALIDARG));
            }
            command.push(arg);
        }
        let mut command = wide(&command)?;
        let attributes = Attributes::new(inherit)?;
        // SAFETY: unnamed job, no inheritable attributes.
        let job = own(unsafe { CreateJobObjectW(None, None) }?);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        // SAFETY: limits matches the requested information class and byte size.
        unsafe {
            SetInformationJobObject(
                raw(&job),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of_val(&limits) as u32,
            )
        }?;
        let startup = STARTUPINFOEXW {
            StartupInfo: STARTUPINFOW {
                cb: size_of::<STARTUPINFOEXW>() as u32,
                ..Default::default()
            },
            lpAttributeList: attributes.list,
        };
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: valid terminated strings and extended startup; suspended until job assignment.
        unsafe {
            CreateProcessW(
                PCWSTR(exe.as_ptr()),
                Some(PWSTR(command.as_mut_ptr())),
                None,
                None,
                true,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED,
                None,
                None,
                &startup.StartupInfo,
                &mut info,
            )
        }?;
        let process = own(info.hProcess);
        let thread = own(info.hThread);
        let child = Self {
            job,
            process,
            pid: info.dwProcessId,
        };
        // SAFETY: child is still suspended and both handles remain owned.
        let assigned = unsafe { AssignProcessToJobObject(raw(&child.job), raw(&child.process)) };
        if let Err(error) = assigned {
            let _ = child.terminate(3);
            return Err(error);
        }
        // SAFETY: this is the live primary thread created suspended above.
        if unsafe { ResumeThread(raw(&thread)) } == u32::MAX {
            let _ = child.terminate(3);
            return Err(windows::core::Error::from_thread());
        }
        Ok(child)
    }
    pub fn pid(&self) -> u32 {
        self.pid
    }
    pub fn handle(&self) -> HANDLE {
        raw(&self.process)
    }
    pub fn duplicate_handle(&self) -> std::io::Result<OwnedHandle> {
        self.process.try_clone()
    }
    pub fn wait(&self, timeout: Duration) -> bool {
        // SAFETY: owned process handle remains valid for the whole wait.
        unsafe {
            WaitForSingleObject(
                self.handle(),
                timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32,
            ) == WAIT_OBJECT_0
        }
    }
    pub fn exit_code(&self) -> windows::core::Result<Option<u32>> {
        if !self.wait(Duration::ZERO) {
            return Ok(None);
        }
        let mut code = 0;
        // SAFETY: live process handle and valid out pointer; process has exited.
        unsafe { GetExitCodeProcess(self.handle(), &mut code) }?;
        Ok(Some(code))
    }
    pub fn terminate(&self, code: u32) -> windows::core::Result<()> {
        // SAFETY: owned child process handle carries terminate access.
        unsafe { TerminateProcess(self.handle(), code) }
    }
}

#[derive(Debug)]
pub struct Mmcss {
    handle: HANDLE,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl Mmcss {
    pub fn pro_audio() -> windows::core::Result<Self> {
        let mut index = 0;
        // SAFETY: constant terminated task name and valid task-index pointer.
        let handle =
            unsafe { AvSetMmThreadCharacteristicsW(windows::core::w!("Pro Audio"), &mut index) }?;
        Ok(Self {
            handle,
            _thread: std::marker::PhantomData,
        })
    }
}
impl Drop for Mmcss {
    fn drop(&mut self) {
        // SAFETY: registration was created on this thread and the guard is not Send.
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.handle) };
    }
}
