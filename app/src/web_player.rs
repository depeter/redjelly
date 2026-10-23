//! VRT's own web player, for the DRM-protected VRT MAX streams mpv can't
//! play. On Windows it runs in a WebView2 (Widevine) inside the video
//! window, behind the see-through player screen, so redjelly's own controls
//! sit on top and drive the page's `<video>` through script
//! (`web_player.js`). Elsewhere there is no web player.

use serde::Deserialize;

/// What `web_player.js` reports twice a second.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PageState {
    /// "top", or the origin of the child frame holding the video.
    pub frame: String,
    pub url: String,
    pub video: bool,
    pub t: f64,
    /// Seconds; negative while unknown or live.
    pub d: f64,
    pub paused: bool,
    pub vol: f64,
    pub muted: bool,
    /// `readyState`: 3+ means frames are playing.
    pub rs: i32,
    /// Seekable range (the DVR window for live).
    pub seek: Option<(f64, f64)>,
    pub tracks: Vec<TextTrack>,
    /// Visible error / notice text on the page.
    pub msgs: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TextTrack {
    pub lang: String,
    pub label: String,
    pub on: bool,
}

pub const LOGIN_URL: &str = "https://www.vrt.be/vrtmax/sso/login";

pub fn available() -> bool {
    cfg!(windows)
}

/// A JS string literal.
pub fn js_str(s: &str) -> String {
    serde_json::Value::String(s.to_owned()).to_string()
}

#[cfg(windows)]
pub use imp::WebPlayer;

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;
    use std::num::NonZeroIsize;
    use std::path::PathBuf;
    use std::ptr::null;
    use std::rc::Rc;

    use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{BLACK_BRUSH, GetStockObject, HBRUSH};
    use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, GetClientRect, HWND_TOP, MA_NOACTIVATE, RegisterClassExW, SW_HIDE, SW_SHOWNA,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos, ShowWindow, WM_MOUSEACTIVATE, WNDCLASSEXW, WS_CHILD, WS_CLIPCHILDREN,
        WS_CLIPSIBLINGS,
    };

    use wry::WebViewBuilderExtWindows;

    const SCRIPT: &str = include_str!("web_player.js");

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// `build_as_child` wants a window handle provider.
    struct Parent(HWND);

    impl HasWindowHandle for Parent {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            let raw = Win32WindowHandle::new(NonZeroIsize::new(self.0 as isize).ok_or(HandleError::Unavailable)?);
            // SAFETY: the container window lives as long as the process.
            Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(raw)) })
        }
    }

    struct Inner {
        webview: Rc<wry::WebView>,
        /// Child of the video window, above mpv's own child window.
        container: HWND,
        parent: HWND,
        size: (i32, i32),
        _context: wry::WebContext,
    }

    pub struct WebPlayer {
        inner: RefCell<Option<Inner>>,
        /// WebView2 profile: cookies (the VRT login) persist here.
        data_dir: PathBuf,
    }

    impl WebPlayer {
        pub fn new(data_dir: PathBuf) -> Self {
            Self { inner: RefCell::new(None), data_dir }
        }

        pub fn created(&self) -> bool {
            self.inner.borrow().is_some()
        }

        /// Creates the webview inside `parent` (the video window) on first
        /// use. `on_message` gets every IPC message, on the UI thread.
        pub fn ensure(&self, parent: i64, on_message: impl Fn(String) + 'static) -> anyhow::Result<()> {
            if self.created() {
                return Ok(());
            }
            anyhow::ensure!(parent != 0, "VRT MAX needs Settings → Video output → Window");
            let parent = parent as HWND;
            // SAFETY: Win32 calls with valid arguments on the UI thread.
            let container = unsafe {
                // WebView2 needs COM; winit usually set it up already.
                let _ = CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
                let instance = GetModuleHandleW(null());
                let class = wide("redjelly-web");
                let wc = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(wnd_proc),
                    hInstance: instance,
                    hbrBackground: GetStockObject(BLACK_BRUSH) as HBRUSH,
                    lpszClassName: class.as_ptr(),
                    ..std::mem::zeroed()
                };
                RegisterClassExW(&wc);
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    null(),
                    WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
                    0,
                    0,
                    1,
                    1,
                    parent,
                    std::ptr::null_mut(),
                    instance,
                    null(),
                );
                anyhow::ensure!(!hwnd.is_null(), "CreateWindowExW failed: {}", std::io::Error::last_os_error());
                hwnd
            };
            let mut context = wry::WebContext::new(Some(self.data_dir.clone()));
            let webview = wry::WebViewBuilder::new_with_web_context(&mut context)
                .with_url("about:blank")
                .with_initialization_script_for_main_only(SCRIPT, false)
                .with_ipc_handler(move |req| on_message(req.into_body()))
                .with_autoplay(true)
                .with_focused(false)
                .with_background_color((0, 0, 0, 255))
                // Always covered by the (see-through) UI window: with native
                // occlusion tracking Chromium would think it's hidden and
                // stop presenting video. Keeps wry's own defaults too.
                .with_additional_browser_args(
                    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion \
                     --autoplay-policy=no-user-gesture-required",
                )
                // Pop-ups (ads, share links) stay out.
                .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
                .build_as_child(&Parent(container))?;
            *self.inner.borrow_mut() =
                Some(Inner { webview: Rc::new(webview), container, parent, size: (0, 0), _context: context });
            self.fit();
            Ok(())
        }

        /// The webview, outside any borrow: some calls pump messages.
        fn webview(&self) -> Option<Rc<wry::WebView>> {
            self.inner.borrow().as_ref().map(|i| i.webview.clone())
        }

        pub fn load(&self, url: &str) {
            if let Some(w) = self.webview()
                && let Err(e) = w.load_url(url)
            {
                tracing::warn!("web player: loading {url}: {e}");
            }
        }

        pub fn eval(&self, js: &str) {
            if let Some(w) = self.webview() {
                let _ = w.evaluate_script(js);
            }
        }

        /// The top-level document's URL, as WebView2 knows it (not as the
        /// page claims).
        pub fn url(&self) -> Option<String> {
            self.webview().and_then(|w| w.url().ok())
        }

        pub fn cookie(&self, url: &str, name: &str) -> Option<String> {
            let w = self.webview()?;
            let cookies = w.cookies_for_url(url).ok()?;
            cookies.into_iter().find(|c| c.name() == name).map(|c| c.value().to_owned())
        }

        /// Puts the page over mpv's output, or hides it.
        pub fn show(&self, on: bool) {
            let Some(container) = self.inner.borrow().as_ref().map(|i| i.container) else { return };
            // SAFETY: a live child window of ours.
            unsafe {
                if on {
                    SetWindowPos(container, HWND_TOP, 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOMOVE);
                    ShowWindow(container, SW_SHOWNA);
                } else {
                    ShowWindow(container, SW_HIDE);
                }
            }
            self.fit();
        }

        /// Follows the video window's size; called every tick.
        pub fn fit(&self) {
            let mut inner = self.inner.borrow_mut();
            let Some(i) = inner.as_mut() else { return };
            let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            // SAFETY: live windows of ours.
            unsafe { GetClientRect(i.parent, &mut rect) };
            let size = (rect.right - rect.left, rect.bottom - rect.top);
            if size == i.size || size.0 <= 0 || size.1 <= 0 {
                return;
            }
            i.size = size;
            unsafe { SetWindowPos(i.container, HWND_TOP, 0, 0, size.0, size.1, SWP_NOACTIVATE | SWP_NOZORDER) };
            let _ = i.webview.set_bounds(wry::Rect {
                position: wry::dpi::PhysicalPosition::new(0, 0).into(),
                size: wry::dpi::PhysicalSize::new(size.0 as u32, size.1 as u32).into(),
            });
        }

        /// Chromium's devtools for the page, to see why VRT won't play.
        pub fn devtools(&self) {
            if let Some(w) = self.webview() {
                w.open_devtools();
            }
        }
    }

    unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        unsafe {
            match msg {
                WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
    }
}

/// No web player off Windows; every call is a no-op.
#[cfg(not(windows))]
pub struct WebPlayer;

#[cfg(not(windows))]
impl WebPlayer {
    pub fn new(_data_dir: std::path::PathBuf) -> Self {
        Self
    }
    pub fn created(&self) -> bool {
        false
    }
    pub fn ensure(&self, _parent: i64, _on_message: impl Fn(String) + 'static) -> anyhow::Result<()> {
        anyhow::bail!("VRT's DRM streams only play on Windows")
    }
    pub fn load(&self, _url: &str) {}
    pub fn eval(&self, _js: &str) {}
    pub fn url(&self) -> Option<String> {
        None
    }
    pub fn cookie(&self, _url: &str, _name: &str) -> Option<String> {
        None
    }
    pub fn show(&self, _on: bool) {}
    pub fn fit(&self) {}
    pub fn devtools(&self) {}
}
