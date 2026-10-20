//! Quitting asks before it throws away unsaved edits (BUG-akqgc).
//!
//! Every quit path reaches [`Application::quit`]: the main window's close
//! button (`WindowEvent::CloseRequested`), and on macOS every route into
//! `-[NSApplication terminate:]` (the MANIFOLD menu's Quit, ⌘Q, the Dock's
//! Quit, logout), which [`install`] turns into a request `about_to_wait`
//! takes. Perform mode asks too.
//!
//! With unsaved edits the user picks Save, Don't Save or Cancel. Save is the
//! normal save (Save As for an untitled project); a failed or cancelled save
//! keeps the app open, and a failed save has already shown its error. A
//! running export is cancelled first and the prompt waits until it has
//! unwound, because the content thread drops a save request mid-export. A
//! clean project quits at once, export or not: the content thread's
//! `Shutdown` cancels a running export itself.

use winit::event_loop::ActiveEventLoop;

use crate::alerts::SaveChoice;
use crate::app::Application;
use crate::content_command::ContentCommand;

impl Application {
    /// A quit from an event-loop callback: ask if needed, then end the loop.
    pub(crate) fn quit(&mut self, event_loop: &ActiveEventLoop) {
        let name = if self.local_project.project_name.is_empty() {
            "Untitled".to_owned()
        } else {
            self.local_project.project_name.clone()
        };
        if self.request_quit(|| crate::alerts::save_changes(&name)) {
            event_loop.exit();
        }
    }

    /// One quit request. `ask` runs only with unsaved edits and no export
    /// running. True once the content thread is shut down; the caller ends
    /// the event loop.
    pub(crate) fn request_quit(&mut self, ask: impl FnOnce() -> SaveChoice) -> bool {
        if self.shutting_down {
            return true;
        }
        let dirty = self.content_state.editing_is_dirty;
        if dirty && self.ws.ui_root.export_progress.is_open() {
            if self.ws.ui_root.export_progress.request_cancel() {
                self.send_content_cmd(ContentCommand::CancelExport);
                self.ws.ui_root.mark_overlay_dirty();
            }
            self.quit_after_export = true;
            return false;
        }
        self.quit_after_export = false;
        let quit = !dirty
            || match ask() {
                SaveChoice::Save => self.save_project(),
                SaveChoice::DontSave => true,
                SaveChoice::Cancel => false,
            };
        if quit {
            self.shut_down_content_thread();
        }
        quit
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

/// True once per OS quit (`terminate:`) since the last call.
pub(crate) fn take_os_quit_request() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::QUIT_REQUESTED.with(|c| c.replace(false))
    }
    #[cfg(not(target_os = "macos"))]
    false
}

/// Route `-[NSApplication terminate:]` into [`Application::quit`]. Call once,
/// on the UI thread, after winit set its application delegate. Returns false
/// when it could not be installed; ⌘Q then quits without asking.
#[cfg(target_os = "macos")]
pub(crate) fn install() -> bool {
    macos::install()
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn install() -> bool {
    false
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::ffi::{class_addMethod, class_respondsToSelector, object_getClass};
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{class, msg_send, sel};
    use std::cell::Cell;

    thread_local! {
        pub(super) static QUIT_REQUESTED: Cell<bool> = const { Cell::new(false) };
    }

    /// `NSTerminateCancel`: AppKit keeps running; the event loop asks and
    /// quits by ending the loop itself.
    const NS_TERMINATE_CANCEL: usize = 0;

    type ShouldTerminateFn = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize;

    unsafe extern "C-unwind" fn should_terminate(
        _this: *mut AnyObject,
        _sel: Sel,
        _sender: *mut AnyObject,
    ) -> usize {
        QUIT_REQUESTED.with(|c| c.set(true));
        NS_TERMINATE_CANCEL
    }

    /// winit 0.30.13's application delegate implements only
    /// `applicationDidFinishLaunching:` and `applicationWillTerminate:`, so
    /// `terminate:` ends the process without a `CloseRequested`. Add
    /// `applicationShouldTerminate:` to its class (a fresh method, not a
    /// swizzle), then set the delegate again so AppKit sees the new method.
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
            let selector = sel!(applicationShouldTerminate:);
            if class_respondsToSelector(cls, selector).as_bool() {
                log::warn!(
                    "[quit] the delegate already implements applicationShouldTerminate: — \
                     winit changed and this module needs updating; ⌘Q quits without asking"
                );
                return false;
            }
            // NSApplicationTerminateReply is an NSUInteger: "Q" on 64-bit.
            let added = class_addMethod(
                cls,
                selector,
                std::mem::transmute::<ShouldTerminateFn, Imp>(should_terminate),
                c"Q@:@".as_ptr(),
            )
            .as_bool();
            if !added {
                log::warn!("[quit] class_addMethod(applicationShouldTerminate:) failed — ⌘Q quits without asking");
                return false;
            }
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

    #[test]
    fn unsaved_edits_hold_the_shutdown_until_the_prompt_is_answered() {
        // Don't Save: nothing is sent while the prompt is up, then Shutdown.
        let (mut app, rx) = app_with_edits(true);
        let probe = rx.clone();
        let quit = app.request_quit(|| {
            assert!(probe.is_empty(), "the prompt is answered before anything is sent");
            SaveChoice::DontSave
        });
        assert!(quit, "Don't Save quits");
        assert!(app.shutting_down);
        assert_eq!(sent(&rx), ["Shutdown"]);

        // Cancel: the app keeps running with its content thread connected.
        let (mut app, rx) = app_with_edits(true);
        let quit = app.request_quit(|| SaveChoice::Cancel);
        assert!(!quit, "Cancel keeps the app open");
        assert!(!app.shutting_down);
        assert!(app.content_tx.is_some(), "the content thread is still connected");
        assert!(sent(&rx).is_empty(), "Cancel sends nothing");

        // No unsaved edits: quits at once without asking.
        let (mut app, rx) = app_with_edits(false);
        let quit = app.request_quit(|| panic!("a clean project never asks"));
        assert!(quit);
        assert_eq!(sent(&rx), ["Shutdown"]);
    }

    #[test]
    fn a_quit_during_an_export_cancels_it_and_asks_once_it_ends() {
        let (mut app, rx) = app_with_edits(true);
        app.ws.ui_root.export_progress.begin("show.mp4");
        let quit = app.request_quit(|| panic!("no prompt while the export unwinds"));
        assert!(!quit);
        assert_eq!(sent(&rx), ["CancelExport"], "the quit cancels the export and nothing else");
        assert!(!app.quit_after_export_ready(), "the prompt waits for the export to end");

        // A second quit while it unwinds sends no second cancel.
        assert!(!app.request_quit(|| panic!("still unwinding")));
        assert!(sent(&rx).is_empty());

        app.ws.ui_root.export_progress.finish();
        assert!(app.quit_after_export_ready(), "the export ended, so the quit asks now");
        assert!(app.request_quit(|| SaveChoice::DontSave));
        assert_eq!(sent(&rx), ["Shutdown"]);
        assert!(!app.quit_after_export);
    }

    #[test]
    fn a_clean_project_quits_through_a_running_export() {
        // The content thread's Shutdown cancels the export itself.
        let (mut app, rx) = app_with_edits(false);
        app.ws.ui_root.export_progress.begin("show.mp4");
        assert!(app.request_quit(|| panic!("a clean project never asks")));
        assert_eq!(sent(&rx), ["Shutdown"]);
    }
}
