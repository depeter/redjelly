//! mpv's OpenGL render API, driven from the host toolkit's GL context.
//!
//! The host calls [`VideoRenderer::new`] once its GL context exists and
//! [`VideoRenderer::render`] before drawing its own UI each frame, so the UI
//! is composited on top of the video (SPEC §2.1).

use std::ffi::{CStr, CString, c_void};

use libmpv2::Mpv;
use libmpv2::render::{OpenGLInitParams, RenderContext, RenderParam, RenderParamApiType};

use crate::MpvError;

/// The host's `get_proc_address`, valid only while `VideoRenderer::new`
/// runs — mpv resolves every GL function during context creation.
type HostGpa = *const (dyn Fn(&CStr) -> *const c_void + 'static);

fn get_proc_address(gpa: &HostGpa, name: &str) -> *mut c_void {
    let Ok(name) = CString::new(name) else { return std::ptr::null_mut() };
    // SAFETY: only called by mpv inside `create_render_context`, while the
    // borrowed closure behind the pointer is alive (see `VideoRenderer::new`).
    unsafe { (**gpa)(&name) as *mut c_void }
}

pub struct VideoRenderer {
    ctx: RenderContext<'static>,
}

impl VideoRenderer {
    /// Must be called on the thread that owns the current GL context.
    /// `on_update` fires on an mpv thread when a new frame is ready; it must
    /// only schedule a redraw, never call mpv.
    pub fn new(
        mpv: &'static Mpv,
        gpa: &dyn Fn(&CStr) -> *const c_void,
        on_update: impl Fn() + Send + 'static,
    ) -> Result<Self, MpvError> {
        // SAFETY: erases the borrow's lifetime. mpv only calls back through
        // this pointer during `create_render_context` below, i.e. while `gpa`
        // is still borrowed by this function.
        let host: HostGpa = unsafe { std::mem::transmute(gpa as *const dyn Fn(&CStr) -> *const c_void) };
        let mut ctx = mpv.create_render_context([
            RenderParam::ApiType(RenderParamApiType::OpenGl),
            RenderParam::InitParams(OpenGLInitParams { get_proc_address, ctx: host }),
        ])?;
        ctx.set_update_callback(on_update);
        Ok(Self { ctx })
    }

    /// Draws the current video frame into the default framebuffer.
    ///
    /// No `report_swap`: Slint has no hook after the real buffer swap, and
    /// reporting at the wrong time (or for UI-only repaints) skews mpv's
    /// vsync estimate so it drops frames that weren't late — choppy motion.
    /// So mpv's default `BLOCK_FOR_TARGET_TIME` stays on: a new frame's
    /// render waits until the frame is due and the vsync'd swap then shows
    /// it on the next refresh, which keeps an even cadence without swap
    /// feedback. That only holds if the host redraws as soon as
    /// `on_update` fires, not on a timer of its own.
    pub fn render(&self, width: i32, height: i32) -> Result<(), MpvError> {
        // Clears the update flag; the frame is drawn every time the host
        // repaints so the UI layer never shows a stale background.
        let _ = self.ctx.update();
        self.ctx.render::<HostGpa>(0, width, height, true)
    }
}
