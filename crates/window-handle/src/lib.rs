//! A copy of a window's native handle, for code that must run while GPUI's `App` is not borrowed.
//!
//! Creating a WebView2 runs a nested Win32 message loop. GPUI only lends its `Window` inside
//! `update` (App borrowed), and any GPUI task that runs during that loop then panics on the
//! borrow. So the mail webview is created from a GPUI task, between `update`s, with this handle
//! copied beforehand (see `mailbox-ui` `mail/webview.rs`). This crate holds the workspace's only
//! `unsafe` (this handle, and [`host_child_windows`] setting an environment variable).

use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};

/// The native handle of a window, copied out of its borrow.
#[derive(Debug, Clone, Copy)]
pub struct ParentWindow(RawWindowHandle);

impl ParentWindow {
    /// Copies `window`'s handle.
    pub fn of(window: &impl HasWindowHandle) -> Result<Self, HandleError> {
        Ok(Self(window.window_handle()?.as_raw()))
    }
}

impl HasWindowHandle for ParentWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // SAFETY: the handle comes from a live GPUI window. Callers use it right away, on the
        // window's thread, to parent a child window; if the window was closed meanwhile, Win32
        // fails the call on the stale handle instead of touching freed memory.
        #[allow(unsafe_code)]
        Ok(unsafe { WindowHandle::borrow_raw(self.0) })
    }
}

/// Makes GPUI draw each window into a plain swap chain instead of a topmost DirectComposition
/// layer, which would cover the mail webview (a child window): the HTML body showed as a blank
/// white rectangle. The windows are opaque, so nothing is lost. An explicit
/// `GPUI_DISABLE_DIRECT_COMPOSITION` (e.g. `0`) is respected.
///
/// Call first thing in `main`, before any thread starts (it changes the environment).
pub fn host_child_windows() {
    const VAR: &str = "GPUI_DISABLE_DIRECT_COMPOSITION";
    if std::env::var_os(VAR).is_none() {
        // SAFETY: called at the start of `main`, while the process has a single thread, so no
        // other thread can read the environment at the same time.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var(VAR, "1")
        };
    }
}
