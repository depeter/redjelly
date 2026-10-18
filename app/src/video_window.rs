//! Windows video output: mpv draws into its own native window (D3D11,
//! gpu-next, display-synced), kept directly behind the Slint window, which
//! is transparent on the player screen. Standalone-mpv frame timing instead
//! of whatever the UI toolkit's redraw loop allows.
//!
//! The video window is a no-activate tool window (no taskbar button, not in
//! Alt-Tab, never takes focus or input); the Slint window stays the app's
//! real window. A subclass on the Slint window mirrors every move, resize,
//! z-order change and minimize onto the video window.

use std::ptr::null;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{BLACK_BRUSH, ClientToScreen, GetStockObject, HBRUSH};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, IsIconic, IsWindowVisible, MA_NOACTIVATE, RegisterClassExW,
    SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowPos, ShowWindow, WM_MOUSEACTIVATE, WM_NCDESTROY,
    WM_WINDOWPOSCHANGED, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};

const SUBCLASS_ID: usize = 0x726a_7664; // "rjvd"

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub struct VideoWindow {
    hwnd: HWND,
}

impl VideoWindow {
    /// Creates the (hidden) video window. Call on the UI thread once the
    /// event loop runs, so the process is already per-monitor DPI aware and
    /// winit's message loop pumps this window too.
    pub fn new() -> anyhow::Result<Self> {
        // SAFETY: plain Win32 calls with valid, NUL-terminated arguments.
        unsafe {
            let instance = GetModuleHandleW(null());
            let class = wide("redjelly-video");
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wnd_proc),
                hInstance: instance,
                // Black until mpv's own child window covers it.
                hbrBackground: GetStockObject(BLACK_BRUSH) as HBRUSH,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            // Fails harmlessly if already registered.
            RegisterClassExW(&wc);
            let title = wide("redjelly video");
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                null(),
            );
            if hwnd.is_null() {
                anyhow::bail!("CreateWindowExW failed: {}", std::io::Error::last_os_error());
            }
            Ok(Self { hwnd })
        }
    }

    /// The handle to give mpv as `wid`.
    pub fn handle(&self) -> i64 {
        self.hwnd as i64
    }

    /// Keeps this window behind `ui`'s client area from now on.
    pub fn follow(&self, ui: HWND) -> anyhow::Result<()> {
        // SAFETY: both are live windows owned by this (the UI) thread; the
        // video window is never destroyed, so the ref data stays valid.
        unsafe {
            if SetWindowSubclass(ui, Some(ui_subclass), SUBCLASS_ID, self.hwnd as usize) == 0 {
                anyhow::bail!("SetWindowSubclass failed: {}", std::io::Error::last_os_error());
            }
            sync(ui, self.hwnd);
        }
        Ok(())
    }
}

/// Puts `video` directly behind `ui`, over its client area, or hides it
/// while `ui` is minimized or hidden.
unsafe fn sync(ui: HWND, video: HWND) {
    unsafe {
        if IsIconic(ui) != 0 || IsWindowVisible(ui) == 0 {
            ShowWindow(video, SW_HIDE);
            return;
        }
        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetClientRect(ui, &mut rect);
        let mut origin = POINT { x: 0, y: 0 };
        ClientToScreen(ui, &mut origin);
        // hWndInsertAfter = ui: placed right below it in the z-order. This
        // only reorders the video window, so `ui` gets no message back.
        SetWindowPos(video, ui, origin.x, origin.y, rect.right, rect.bottom, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
}

unsafe extern "system" fn ui_subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    video: usize,
) -> LRESULT {
    unsafe {
        let result = DefSubclassProc(hwnd, msg, wparam, lparam);
        match msg {
            // Covers moves, resizes, fullscreen, minimize/restore and the
            // window coming to the front.
            WM_WINDOWPOSCHANGED => sync(hwnd, video as HWND),
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(ui_subclass), SUBCLASS_ID);
                ShowWindow(video as HWND, SW_HIDE);
            }
            _ => {}
        }
        result
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            // Never steal activation from the UI window, even if clicked.
            WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
