//! Win32 Raw Input window and integrity-level helpers.

use std::{ffi::c_void, mem::size_of};

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, LPARAM},
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
        UI::{
            Input::{
                GetRawInputData, HRAWINPUT, KeyboardAndMouse::GetAsyncKeyState, RAWINPUT,
                RAWINPUTDEVICE, RID_INPUT, RIDEV_INPUTSINK, RIDEV_REMOVE, RIM_TYPEKEYBOARD,
                RIM_TYPEMOUSE, RegisterRawInputDevices,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, DispatchMessageW, GetForegroundWindow,
                GetWindowTextW, GetWindowThreadProcessId, HWND_MESSAGE, MSG, PM_REMOVE,
                PeekMessageW, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT,
            },
        },
    },
    core::w,
};

use crate::Error;

#[derive(Clone, Copy)]
pub(super) enum Raw {
    Keyboard(windows::Win32::UI::Input::RAWKEYBOARD),
    Mouse(windows::Win32::UI::Input::RAWMOUSE),
}

pub(super) struct RawWindow {
    hwnd: HWND,
}

impl RawWindow {
    pub fn open() -> Result<Self, Error> {
        // SAFETY: STATIC is a process-global system class. The message-only
        // window has no menu, instance state, or creation parameter.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("SocialSpeak raw input"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
        }
        .map_err(|error| win_error(&error))?;
        let devices = [
            RAWINPUTDEVICE {
                usUsagePage: 0x01,
                usUsage: 0x06,
                dwFlags: RIDEV_INPUTSINK,
                hwndTarget: hwnd,
            },
            RAWINPUTDEVICE {
                usUsagePage: 0x01,
                usUsage: 0x02,
                dwFlags: RIDEV_INPUTSINK,
                hwndTarget: hwnd,
            },
        ];
        // SAFETY: both descriptors and the target HWND remain valid through
        // the call; Windows copies registration state.
        if let Err(error) = unsafe {
            RegisterRawInputDevices(
                &devices,
                u32::try_from(size_of::<RAWINPUTDEVICE>()).unwrap_or(u32::MAX),
            )
        } {
            // SAFETY: `hwnd` was created by this thread and has no children.
            let _ = unsafe { DestroyWindow(hwnd) };
            return Err(win_error(&error));
        }
        Ok(Self { hwnd })
    }

    pub fn poll(&self, output: &mut Vec<Raw>) -> Result<(), Error> {
        let mut message = MSG::default();
        // SAFETY: this thread owns the window and message queue. `message`
        // stays live for each Win32 call.
        while unsafe { PeekMessageW(&raw mut message, Some(self.hwnd), 0, 0, PM_REMOVE).as_bool() }
        {
            if message.message == WM_INPUT {
                output.push(read_raw(message.lParam)?);
            }
            // SAFETY: the removed message is initialized by PeekMessageW.
            unsafe {
                let _ = TranslateMessage(&raw const message);
                DispatchMessageW(&raw const message);
            }
        }
        Ok(())
    }
}

impl Drop for RawWindow {
    fn drop(&mut self) {
        let remove = [
            RAWINPUTDEVICE {
                usUsagePage: 0x01,
                usUsage: 0x06,
                dwFlags: RIDEV_REMOVE,
                hwndTarget: HWND::default(),
            },
            RAWINPUTDEVICE {
                usUsagePage: 0x01,
                usUsage: 0x02,
                dwFlags: RIDEV_REMOVE,
                hwndTarget: HWND::default(),
            },
        ];
        // SAFETY: unregistering is best-effort during teardown; this thread
        // owns the window and registration.
        unsafe {
            let _ = RegisterRawInputDevices(
                &remove,
                u32::try_from(size_of::<RAWINPUTDEVICE>()).unwrap_or(u32::MAX),
            );
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

pub(super) fn key_down(virtual_key: i32) -> bool {
    if virtual_key == 0 {
        return true;
    }
    // SAFETY: every i32 is accepted; mapped callers pass documented VK values.
    unsafe { GetAsyncKeyState(virtual_key) < 0 }
}

pub(super) fn elevated_foreground() -> Option<String> {
    // SAFETY: pseudo-handle is valid for this process and must not be closed.
    if process_elevated(unsafe { GetCurrentProcess() }).unwrap_or(false) {
        return None;
    }
    // SAFETY: read-only foreground query.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return None;
    }
    let mut process_id = 0;
    // SAFETY: output points to a live u32.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut process_id)) };
    if process_id == 0 {
        return None;
    }
    // SAFETY: process id came from the foreground HWND.
    let process =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }.ok()?;
    let elevated = process_elevated(process).unwrap_or(false);
    // SAFETY: real handle returned by OpenProcess, closed exactly once.
    let _ = unsafe { CloseHandle(process) };
    if !elevated {
        return None;
    }
    let mut title = [0_u16; 256];
    // SAFETY: HWND is still only queried; buffer is writable and bounded.
    let length = unsafe { GetWindowTextW(hwnd, &mut title) };
    let title = usize::try_from(length)
        .ok()
        .filter(|length| *length > 0)
        .map(|length| String::from_utf16_lossy(&title[..length]));
    Some(title.unwrap_or_else(|| "the foreground application".to_owned()))
}

fn read_raw(lparam: LPARAM) -> Result<Raw, Error> {
    let mut input = RAWINPUT::default();
    let mut size = u32::try_from(size_of::<RAWINPUT>()).unwrap_or(u32::MAX);
    // SAFETY: WM_INPUT lParam is an HRAWINPUT. The fixed RAWINPUT storage is
    // sufficient for keyboard and mouse records registered by this window.
    let read = unsafe {
        GetRawInputData(
            HRAWINPUT(lparam.0 as *mut c_void),
            RID_INPUT,
            Some((&raw mut input).cast()),
            &raw mut size,
            u32::try_from(size_of::<windows::Win32::UI::Input::RAWINPUTHEADER>())
                .unwrap_or(u32::MAX),
        )
    };
    if read == u32::MAX {
        return Err(Error::Unavailable("GetRawInputData failed".to_owned()));
    }
    // SAFETY: the active union arm is identified by the initialized header.
    unsafe {
        match input.header.dwType {
            value if value == RIM_TYPEKEYBOARD.0 => Ok(Raw::Keyboard(input.data.keyboard)),
            value if value == RIM_TYPEMOUSE.0 => Ok(Raw::Mouse(input.data.mouse)),
            _ => Err(Error::Unavailable("unexpected raw-input device".to_owned())),
        }
    }
}

fn process_elevated(process: HANDLE) -> windows::core::Result<bool> {
    let mut token = HANDLE::default();
    // SAFETY: process handle is valid for query; output receives a token handle.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) }?;
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0;
    // SAFETY: token is live and the output buffer matches TokenElevation.
    let result = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            u32::try_from(size_of::<TOKEN_ELEVATION>()).unwrap_or(u32::MAX),
            &raw mut returned,
        )
    };
    // SAFETY: token came from OpenProcessToken and is closed exactly once.
    let _ = unsafe { CloseHandle(token) };
    result?;
    Ok(elevation.TokenIsElevated != 0)
}

fn win_error(error: &windows::core::Error) -> Error {
    Error::Unavailable(error.to_string())
}
