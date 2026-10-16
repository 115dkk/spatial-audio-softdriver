#![allow(unsafe_code)]
use windows::{
    Win32::{Foundation::*, UI::WindowsAndMessaging::*},
    core::{PCWSTR, w},
};

unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_CLOSE {
        // SAFETY: live callback window; hiding defers destruction until effEditClose.
        let _ = unsafe { ShowWindow(window, SW_HIDE) };
        return LRESULT(0);
    }
    // SAFETY: parameters are forwarded unchanged from Windows' window procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

unsafe extern "system" fn close_matching(window: HWND, pid: LPARAM) -> windows::core::BOOL {
    let mut owner = 0;
    // SAFETY: enumeration supplies a live window and owner is a writable pid output.
    unsafe { GetWindowThreadProcessId(window, Some(&mut owner)) };
    if owner == pid.0 as u32 {
        // SAFETY: posts a pointer-free close message to a window in the specified test process.
        let _ = unsafe { PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
    true.into()
}

#[doc(hidden)]
pub fn close_process_editor(pid: u32) -> windows::core::Result<()> {
    // SAFETY: callback is static; lparam carries only an integer process id.
    unsafe { EnumWindows(Some(close_matching), LPARAM(pid as isize)) }
}

#[derive(Debug)]
pub struct EditorWindow {
    window: HWND,
}
impl EditorWindow {
    pub fn open(title: &str, width: i32, height: i32) -> windows::core::Result<Self> {
        // SAFETY: null module name requests the current executable's live module.
        let module = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }?;
        let instance = HINSTANCE(module.0);
        let class = WNDCLASSW {
            hInstance: instance,
            lpfnWndProc: Some(procedure),
            lpszClassName: w!("PluginHostEditor"),
            ..Default::default()
        };
        // SAFETY: class callback is static and class-name storage is permanent.
        let _ = unsafe { RegisterClassW(&class) };
        let name: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut rectangle = RECT {
            left: 0,
            top: 0,
            right: width.clamp(1, 8192),
            bottom: height.clamp(1, 8192),
        };
        // SAFETY: writable rectangle and valid top-level window style.
        unsafe { AdjustWindowRectEx(&mut rectangle, style, false, WINDOW_EX_STYLE(0)) }?;
        // SAFETY: registered class, terminated title, valid bounded dimensions, no borrowed creation data.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("PluginHostEditor"),
                PCWSTR(name.as_ptr()),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                rectangle.right - rectangle.left,
                rectangle.bottom - rectangle.top,
                None,
                None,
                Some(instance),
                None,
            )
        }?;
        // SAFETY: freshly created window belongs to the current thread.
        let _ = unsafe { ShowWindow(window, SW_SHOW) };
        Ok(Self { window })
    }
    pub fn handle_value(&self) -> usize {
        self.window.0 as usize
    }
    pub fn pump(&self) -> bool {
        let mut message = MSG::default();
        // SAFETY: writable message, only this thread's messages, removing each consumed message.
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            // SAFETY: message was obtained from PeekMessageW.
            let _ = unsafe { TranslateMessage(&message) };
            // SAFETY: message was obtained from PeekMessageW and handlers remain live.
            unsafe { DispatchMessageW(&message) };
        }
        // SAFETY: owned window remains live until Drop.
        !unsafe { IsWindowVisible(self.window) }.as_bool()
    }
    pub fn request_close(&self) -> windows::core::Result<()> {
        // SAFETY: asynchronous close carries no pointers to borrowed memory.
        unsafe { PostMessageW(Some(self.window), WM_CLOSE, WPARAM(0), LPARAM(0)) }
    }
    pub fn resize(&self, width: i32, height: i32) -> windows::core::Result<()> {
        let mut rectangle = RECT {
            left: 0,
            top: 0,
            right: width.clamp(1, 8192),
            bottom: height.clamp(1, 8192),
        };
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        // SAFETY: writable client rectangle and matching non-resizable style.
        unsafe { AdjustWindowRectEx(&mut rectangle, style, false, WINDOW_EX_STYLE(0)) }?;
        // SAFETY: owned window, bounded dimensions, no z-order or position change.
        unsafe {
            SetWindowPos(
                self.window,
                None,
                0,
                0,
                rectangle.right - rectangle.left,
                rectangle.bottom - rectangle.top,
                SWP_NOMOVE | SWP_NOZORDER,
            )
        }
    }
}
impl Drop for EditorWindow {
    fn drop(&mut self) {
        // SAFETY: window is owned and destroyed on the creating main thread, after editor close.
        let _ = unsafe { DestroyWindow(self.window) };
    }
}
