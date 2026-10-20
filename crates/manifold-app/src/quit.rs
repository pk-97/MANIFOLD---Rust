//! Quitting asks before it throws away unsaved edits (BUG-akqgc).
//!
//! Every quit path reaches [`Application::quit`]: the main window's close
//! button (`WindowEvent::CloseRequested`), and on macOS every route into
//! `-[NSApplication terminate:]` (the MANIFOLD menu's Quit, ⌘Q, the Dock's
//! Quit, logout, restart, shutdown). [`install`] answers `terminate:` with
//! "later" and flags it; `about_to_wait` takes the flag and calls `quit`, in
//! editor and perform mode alike, and the decision goes back to AppKit as
//! `replyToApplicationShouldTerminate:`. A clean project replies yes on the
//! next loop pass, so logout and restart never stall on MANIFOLD.
//!
//! With unsaved edits the user picks Save, Don't Save or Cancel. Save is the
//! normal save (Save As for an untitled project); a failed or cancelled save
//! keeps the app open, and a failed save has already shown its error. A
//! running export is cancelled first and the prompt waits until it has
//! unwound, because the content thread drops a save request mid-export. A
//! clean project quits at once, export or not: the content thread's
//! `Shutdown` cancels a running export itself. While the prompt (and any
//! save dialog after it) is up, full-screen output windows drop below it.

use std::sync::Arc;

use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::alerts::SaveChoice;
use crate::app::Application;
use crate::content_command::ContentCommand;
use crate::window_registry::WindowRole;

/// What one quit request decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuitDecision {
    /// The content thread is shut down; the app ends.
    Quit,
    /// Cancelled, or the save failed or was cancelled: the app keeps running.
    Stay,
    /// The running export was cancelled; the prompt shows once it unwinds.
    AwaitExport,
}

impl QuitDecision {
    /// The answer to a pending `terminate:`: yes to quit, no to stay, none
    /// yet while the export unwinds (AppKit keeps waiting).
    pub(crate) fn terminate_reply(self) -> Option<bool> {
        match self {
            QuitDecision::Quit => Some(true),
            QuitDecision::Stay => Some(false),
            QuitDecision::AwaitExport => None,
        }
    }
}

impl Application {
    /// A quit from an event-loop callback: ask if needed, then end the app.
    pub(crate) fn quit(&mut self, event_loop: &ActiveEventLoop) {
        let name = if self.local_project.project_name.is_empty() {
            "Untitled".to_owned()
        } else {
            self.local_project.project_name.clone()
        };
        let outputs: Vec<Arc<Window>> = self
            .window_registry
            .iter()
            .filter(|(_, ws)| matches!(ws.role, WindowRole::Output { .. }))
            .map(|(_, ws)| ws.window.clone())
            .collect();
        // Held from the prompt through the save it may lead to.
        let mut lowered = None;
        let decision = self.request_quit(
            || {
                lowered = Some(OutputsBelowDialogs::lower(&outputs));
                crate::alerts::save_changes(&name)
            },
            Self::save_project,
        );
        drop(lowered);
        if terminate_pending() {
            if let Some(yes) = decision.terminate_reply() {
                reply_to_terminate(yes);
            }
        } else if decision == QuitDecision::Quit {
            event_loop.exit();
        }
    }

    /// One quit request. `ask` runs only with unsaved edits and no export
    /// running; `save` only when the answer is Save, true once on disk.
    pub(crate) fn request_quit(
        &mut self,
        ask: impl FnOnce() -> SaveChoice,
        save: impl FnOnce(&mut Self) -> bool,
    ) -> QuitDecision {
        if self.shutting_down {
            return QuitDecision::Quit;
        }
        let dirty = self.content_state.editing_is_dirty;
        if dirty && self.ws.ui_root.export_progress.is_open() {
            if self.ws.ui_root.export_progress.request_cancel() {
                self.send_content_cmd(ContentCommand::CancelExport);
                self.ws.ui_root.mark_overlay_dirty();
            }
            self.quit_after_export = true;
            return QuitDecision::AwaitExport;
        }
        self.quit_after_export = false;
        let quit = !dirty
            || match ask() {
                SaveChoice::Save => save(self),
                SaveChoice::DontSave => true,
                SaveChoice::Cancel => false,
            };
        if !quit {
            return QuitDecision::Stay;
        }
        self.shut_down_content_thread();
        QuitDecision::Quit
    }

    /// A quit that waited on a cancelled export may ask now: the export has
    /// unwound and the UI holds the content thread's state again, so a save
    /// is not refused as mid-export.
    pub(crate) fn quit_after_export_ready(&self) -> bool {
        self.quit_after_export
            && !self.ws.ui_root.export_progress.is_open()
            && !self.content_state.is_exporting
    }

    fn shut_down_content_thread(&mut self) {
        self.shutting_down = true;
        // Stop display links FIRST — their callbacks may call
        // nextDrawable() or request_redraw(). CVDisplayLinkStop blocks until
        // the in-flight callback finishes, so this must happen before we
        // destroy windows or block on joins.
        #[cfg(target_os = "macos")]
        {
            self.ws.ui_display_link = None;
        }
        if let Some(tx) = self.content_tx.take() {
            let _ = tx.send(ContentCommand::Shutdown);
        }
        if let Some(handle) = self.content_thread_handle.take() {
            let _ = handle.join();
            log::info!("[ContentThread] joined");
        }
    }
}

/// Output windows sit above the menu bar (level 25) when full screen, which
/// is above every alert and file dialog. Drops them to the normal level and
/// puts them back on drop.
struct OutputsBelowDialogs {
    #[cfg(target_os = "macos")]
    restore: Vec<(Arc<Window>, i64)>,
}

impl OutputsBelowDialogs {
    #[cfg(target_os = "macos")]
    fn lower(outputs: &[Arc<Window>]) -> Self {
        let mut restore = Vec::new();
        for window in outputs {
            if let Some(level) = crate::edr_surface::window_level(window)
                && level > 0
            {
                crate::edr_surface::set_window_level(window, 0);
                restore.push((window.clone(), level));
            }
        }
        Self { restore }
    }

    #[cfg(not(target_os = "macos"))]
    fn lower(_outputs: &[Arc<Window>]) -> Self {
        Self {}
    }
}

impl Drop for OutputsBelowDialogs {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        for (window, level) in &self.restore {
            crate::edr_surface::set_window_level(window, *level);
        }
    }
}

/// True once per OS quit (`terminate:`) since the last call.
pub(crate) fn take_os_quit_request() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::REQUESTED.with(|c| c.replace(false))
    }
    #[cfg(not(target_os = "macos"))]
    false
}

/// A `terminate:` is waiting for its reply.
fn terminate_pending() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::REPLY_PENDING.with(|c| c.get())
    }
    #[cfg(not(target_os = "macos"))]
    false
}

/// Answer the pending `terminate:` once.
fn reply_to_terminate(yes: bool) {
    #[cfg(target_os = "macos")]
    macos::reply(yes);
    #[cfg(not(target_os = "macos"))]
    let _ = yes;
}

/// Route `-[NSApplication terminate:]` into [`Application::quit`]. Call on
/// the main thread after winit's `EventLoop::new` set its delegate and
/// before the loop runs, so no delegate callback is in flight. Runs once.
pub(crate) fn install() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        #[cfg(target_os = "macos")]
        macos::install();
    });
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::ffi::{class_addMethod, class_respondsToSelector, object_getClass};
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::{class, msg_send, sel};
    use std::cell::Cell;

    thread_local! {
        /// A `terminate:` arrived and `about_to_wait` has not taken it yet.
        pub(super) static REQUESTED: Cell<bool> = const { Cell::new(false) };
        /// AppKit is waiting for `replyToApplicationShouldTerminate:`.
        pub(super) static REPLY_PENDING: Cell<bool> = const { Cell::new(false) };
    }

    /// `NSTerminateLater`: AppKit waits, running the run loop in modal-panel
    /// mode. winit's observers sit in the common modes, so `about_to_wait`
    /// keeps running and takes the request.
    const NS_TERMINATE_LATER: usize = 2;

    type ShouldTerminateFn = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize;
    type ReplyFn = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject);

    unsafe extern "C-unwind" fn should_terminate(
        _this: *mut AnyObject,
        _sel: Sel,
        _sender: *mut AnyObject,
    ) -> usize {
        REQUESTED.with(|c| c.set(true));
        REPLY_PENDING.with(|c| c.set(true));
        NS_TERMINATE_LATER
    }

    /// Runs from the run loop, outside every winit handler: a yes makes AppKit
    /// post `applicationWillTerminate:`, which winit answers by dispatching
    /// `LoopExiting`, and winit panics if its handler is already running.
    unsafe extern "C-unwind" fn deliver_reply(_this: *mut AnyObject, _sel: Sel, quit: *mut AnyObject) {
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![app, replyToApplicationShouldTerminate: Bool::new(!quit.is_null())];
        }
    }

    /// Queue the reply for the next run-loop pass (common modes, so it runs
    /// while AppKit waits in modal-panel mode).
    pub(super) fn reply(yes: bool) {
        if !REPLY_PENDING.with(|c| c.replace(false)) {
            return;
        }
        // A terminate: that arrived while a prompt was up is answered too.
        REQUESTED.with(|c| c.set(false));
        log::info!("[quit] terminate: answered {}", if yes { "yes" } else { "no" });
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let delegate: *mut AnyObject = msg_send![app, delegate];
            if delegate.is_null() {
                let _: () = msg_send![app, replyToApplicationShouldTerminate: Bool::new(yes)];
                return;
            }
            let flag: *mut AnyObject = if yes { app } else { std::ptr::null_mut() };
            let _: () = msg_send![
                delegate,
                performSelectorOnMainThread: sel!(manifoldDeliverTerminateReply:),
                withObject: flag,
                waitUntilDone: Bool::NO,
            ];
        }
    }

    /// winit 0.30.13's application delegate implements only
    /// `applicationDidFinishLaunching:` and `applicationWillTerminate:`, so
    /// `terminate:` ends the process without a `CloseRequested`. Add
    /// `applicationShouldTerminate:` and the reply trampoline to its class
    /// (fresh methods, not swizzles), then set the delegate again so AppKit
    /// sees the new method.
    pub(super) fn install() -> bool {
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            if app.is_null() {
                log::warn!("[quit] no NSApplication — ⌘Q quits without asking");
                return false;
            }
            let delegate: *mut AnyObject = msg_send![app, delegate];
            if delegate.is_null() {
                log::warn!("[quit] NSApplication has no delegate — ⌘Q quits without asking");
                return false;
            }
            let cls = object_getClass(delegate) as *mut AnyClass;
            if cls.is_null() {
                log::warn!("[quit] could not resolve the delegate class — ⌘Q quits without asking");
                return false;
            }
            let should = sel!(applicationShouldTerminate:);
            if class_respondsToSelector(cls, should).as_bool() {
                log::warn!(
                    "[quit] the delegate already implements applicationShouldTerminate: — \
                     winit changed and this module needs updating; ⌘Q quits without asking"
                );
                return false;
            }
            // The trampoline goes first: without it a "later" could never be answered.
            let trampoline = class_addMethod(
                cls,
                sel!(manifoldDeliverTerminateReply:),
                std::mem::transmute::<ReplyFn, Imp>(deliver_reply),
                c"v@:@".as_ptr(),
            )
            .as_bool();
            if !trampoline {
                log::warn!("[quit] class_addMethod(manifoldDeliverTerminateReply:) failed — ⌘Q quits without asking");
                return false;
            }
            // NSApplicationTerminateReply is an NSUInteger: "Q" on 64-bit.
            let added = class_addMethod(
                cls,
                should,
                std::mem::transmute::<ShouldTerminateFn, Imp>(should_terminate),
                c"Q@:@".as_ptr(),
            )
            .as_bool();
            if !added {
                log::warn!("[quit] class_addMethod(applicationShouldTerminate:) failed — ⌘Q quits without asking");
                return false;
            }
            // winit's EventLoop retains the delegate; NSApplication holds it weakly.
            let none: *mut AnyObject = std::ptr::null_mut();
            let _: () = msg_send![app, setDelegate: none];
            let _: () = msg_send![app, setDelegate: delegate];
        }
        log::info!("[quit] terminate: now asks before quitting");
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::{Receiver, unbounded};

    /// A headless app with unsaved edits and a stub content channel.
    fn app_with_edits(dirty: bool) -> (Application, Receiver<ContentCommand>) {
        let mut app = Application::new();
        let (tx, rx) = unbounded();
        app.content_tx = Some(tx);
        app.content_state.editing_is_dirty = dirty;
        (app, rx)
    }

    fn sent(rx: &Receiver<ContentCommand>) -> Vec<&'static str> {
        rx.try_iter()
            .map(|c| match c {
                ContentCommand::Shutdown => "Shutdown",
                ContentCommand::CancelExport => "CancelExport",
                _ => "another command",
            })
            .collect()
    }

    fn no_save(_: &mut Application) -> bool {
        panic!("only a Save answer saves")
    }

    #[test]
    fn unsaved_edits_hold_the_shutdown_until_the_prompt_is_answered() {
        // Don't Save: nothing is sent while the prompt is up, then Shutdown.
        let (mut app, rx) = app_with_edits(true);
        let probe = rx.clone();
        let decision = app.request_quit(
            || {
                assert!(probe.is_empty(), "the prompt is answered before anything is sent");
                SaveChoice::DontSave
            },
            no_save,
        );
        assert_eq!(decision, QuitDecision::Quit, "Don't Save quits");
        assert!(app.shutting_down);
        assert_eq!(sent(&rx), ["Shutdown"]);

        // Cancel: the app keeps running with its content thread connected.
        let (mut app, rx) = app_with_edits(true);
        let decision = app.request_quit(|| SaveChoice::Cancel, no_save);
        assert_eq!(decision, QuitDecision::Stay, "Cancel keeps the app open");
        assert!(!app.shutting_down);
        assert!(app.content_tx.is_some(), "the content thread is still connected");
        assert!(sent(&rx).is_empty(), "Cancel sends nothing");

        // No unsaved edits: quits at once without asking.
        let (mut app, rx) = app_with_edits(false);
        let decision = app.request_quit(|| panic!("a clean project never asks"), no_save);
        assert_eq!(decision, QuitDecision::Quit);
        assert_eq!(sent(&rx), ["Shutdown"]);
    }

    #[test]
    fn the_terminate_reply_follows_the_decision() {
        // Clean: yes.
        let (mut app, _rx) = app_with_edits(false);
        let decision = app.request_quit(|| panic!("a clean project never asks"), no_save);
        assert_eq!(decision.terminate_reply(), Some(true));

        // Cancel: no.
        let (mut app, _rx) = app_with_edits(true);
        let decision = app.request_quit(|| SaveChoice::Cancel, no_save);
        assert_eq!(decision.terminate_reply(), Some(false));

        // Save that fails (or a Save As that is cancelled): no, nothing sent.
        let (mut app, rx) = app_with_edits(true);
        let decision = app.request_quit(|| SaveChoice::Save, |_| false);
        assert_eq!(decision.terminate_reply(), Some(false));
        assert!(!app.shutting_down);
        assert!(sent(&rx).is_empty(), "a failed save keeps the content thread");

        // Save that lands: yes.
        let (mut app, rx) = app_with_edits(true);
        let decision = app.request_quit(|| SaveChoice::Save, |_| true);
        assert_eq!(decision.terminate_reply(), Some(true));
        assert_eq!(sent(&rx), ["Shutdown"]);

        // An export unwinding: no reply yet, AppKit keeps waiting.
        let (mut app, _rx) = app_with_edits(true);
        app.ws.ui_root.export_progress.begin("show.mp4");
        let decision = app.request_quit(|| panic!("no prompt mid-export"), no_save);
        assert_eq!(decision.terminate_reply(), None);
    }

    #[test]
    fn a_quit_during_an_export_cancels_it_and_asks_once_it_ends() {
        let (mut app, rx) = app_with_edits(true);
        app.ws.ui_root.export_progress.begin("show.mp4");
        let decision = app.request_quit(|| panic!("no prompt while the export unwinds"), no_save);
        assert_eq!(decision, QuitDecision::AwaitExport);
        assert_eq!(sent(&rx), ["CancelExport"], "the quit cancels the export and nothing else");
        assert!(!app.quit_after_export_ready(), "the prompt waits for the export to end");

        // A second quit while it unwinds sends no second cancel.
        let decision = app.request_quit(|| panic!("still unwinding"), no_save);
        assert_eq!(decision, QuitDecision::AwaitExport);
        assert!(sent(&rx).is_empty());

        app.ws.ui_root.export_progress.finish();
        assert!(app.quit_after_export_ready(), "the export ended, so the quit asks now");
        assert_eq!(app.request_quit(|| SaveChoice::DontSave, no_save), QuitDecision::Quit);
        assert_eq!(sent(&rx), ["Shutdown"]);
        assert!(!app.quit_after_export);
    }

    #[test]
    fn a_clean_project_quits_through_a_running_export() {
        // The content thread's Shutdown cancels the export itself.
        let (mut app, rx) = app_with_edits(false);
        app.ws.ui_root.export_progress.begin("show.mp4");
        assert_eq!(app.request_quit(|| panic!("a clean project never asks"), no_save), QuitDecision::Quit);
        assert_eq!(sent(&rx), ["Shutdown"]);
    }
}
