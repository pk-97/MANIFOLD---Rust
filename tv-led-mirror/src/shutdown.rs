//! Process-exit blackout hook.
//!
//! All exit paths — window close, Cmd+Q, Ctrl+C, killall — eventually call
//! libc's `exit()`. This module installs an `atexit` cleanup that grabs the
//! `SharedState` and runs the LED controller's shutdown so the strips don't
//! latch on the last captured frame after we're gone.
//!
//! Keeping the `Arc<SharedState>` in a `OnceLock<Mutex<Option<...>>>` lets
//! the cleanup function find the state even though `extern "C"` callbacks
//! can't capture environment.

use std::sync::Arc;
use std::sync::OnceLock;

use parking_lot::Mutex;

use crate::SharedState;

static SHUTDOWN_STATE: OnceLock<Mutex<Option<Arc<SharedState>>>> = OnceLock::new();

pub fn install_atexit_blackout(state: Arc<SharedState>) {
    let slot = SHUTDOWN_STATE.get_or_init(|| Mutex::new(None));
    *slot.lock() = Some(state);

    extern "C" fn cleanup() {
        let Some(slot) = SHUTDOWN_STATE.get() else {
            return;
        };
        // Take the Arc out so we can drop it after running shutdown — the
        // controller's shutdown holds its own mutex, and we don't want
        // anything else to deadlock on it during exit.
        let Some(state) = slot.lock().take() else {
            return;
        };
        state.controller.lock().shutdown();
    }
    unsafe {
        libc::atexit(cleanup);
    }
}
