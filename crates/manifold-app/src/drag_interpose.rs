//! Live pointer position during a macOS Finder file drag.
//!
//! winit 0.30.13's macOS backend registers its `NSWindow` as the drag
//! destination (`registerForDraggedTypes`, forwarding to the window's
//! delegate — see the vendored `platform_impl/macos/window_delegate.rs`), but
//! the delegate implements only `draggingEntered:`, `prepareForDragOperation:`,
//! `performDragOperation:`, `concludeDragOperation:`, and `draggingExited:`.
//! It does NOT implement `draggingUpdated:`, so the live position AppKit sends
//! on every drag movement is never seen — winit surfaces only `HoveredFile`
//! (once, on entry, no position) and the final `DroppedFile` (also no
//! position). `App::cursor_pos` is last set before the drag began and stays
//! stale for its entire duration.
//!
//! Verified 2026-07-05 (live drag test): polling
//! `mouseLocationOutsideOfEventStream` and `+[NSEvent mouseLocation]` both
//! freeze for the whole NSDragging session — AppKit simply doesn't update
//! either API while it owns the drag. Polling cannot work; do not reintroduce
//! it. The only live position during a drag is what AppKit hands the
//! destination via `draggingUpdated:` (`[sender draggingLocation]`).
//!
//! Fix: at startup, add `draggingUpdated:` to winit's window-delegate class
//! (a fresh `class_addMethod`, not a swizzle — the method doesn't exist yet)
//! and swizzle `performDragOperation:` (which DOES exist) so the drop
//! position is captured exactly even if the pointer never moves again after
//! drag-entry. Both write into a main-thread-only cell in the same logical,
//! top-left coordinate convention as `App::cursor_pos`.
//!
//! One assumption this whole mechanism rests on and cannot be verified
//! headless: that `NSWindow` forwards a dragging message to its delegate once
//! the delegate responds to it (`respondsToSelector:` is checked per-message,
//! not cached at registration time). This is documented AppKit behavior but
//! only a live drag proves it — see the P1 gate in
//! `docs/TIMELINE_INGEST_DESIGN.md`. If forwarding doesn't happen, every
//! caller here degrades to `None`, and drop arms fall back to the pre-existing
//! (stale) `cursor_pos` behavior via `.unwrap_or(self.cursor_pos)`.

use manifold_ui::node::Vec2;
use std::cell::Cell;

thread_local! {
    static DRAG_POS: Cell<Option<Vec2>> = const { Cell::new(None) };
}

/// Live pointer position during an OS file drag, in logical pixels, top-left
/// origin — the same convention as `App::cursor_pos`. `None` when no drag is
/// in flight (or interposition could not be installed, or this platform
/// doesn't implement it).
pub fn drag_position() -> Option<Vec2> {
    DRAG_POS.with(|c| c.get())
}

/// Clear the tracked position. Call on `HoveredFileCancelled` and once a
/// `DroppedFile` has been consumed.
pub fn clear_drag_position() {
    DRAG_POS.with(|c| c.set(None));
}

/// Install the `draggingUpdated:` / `performDragOperation:` interposition on
/// winit's window delegate. Call once, on the UI thread, right after the
/// window is created (before it's moved into `WindowState`). Returns `false`
/// if any step fails — callers must keep working via the `cursor_pos`
/// fallback, not treat this as fatal.
#[cfg(target_os = "macos")]
pub fn install(window: &winit::window::Window) -> bool {
    macos::install(window)
}

#[cfg(not(target_os = "macos"))]
pub fn install(_window: &winit::window::Window) -> bool {
    false
}

#[cfg(target_os = "macos")]
mod macos {
    use super::DRAG_POS;
    use manifold_ui::node::Vec2;
    use objc2::ffi::{
        class_addMethod, class_getInstanceMethod, class_respondsToSelector, method_setImplementation,
        object_getClass,
    };
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::{msg_send, sel};
    use objc2_foundation::NSPoint;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::cell::Cell;
    use std::sync::OnceLock;

    // The content NSView, captured at install time — needed to convert
    // `draggingLocation`'s window-base point into the same top-left logical
    // convention `App::cursor_pos` uses. UI-thread only, like everything else
    // in this module.
    thread_local! {
        static NS_VIEW: Cell<*mut AnyObject> = const { Cell::new(std::ptr::null_mut()) };
    }

    // `performDragOperation:` is swizzled, not replaced — the original IMP
    // (which queues winit's `DroppedFile` event) must still run. Written once
    // at install; read from the wrapper on every drop.
    static ORIGINAL_PERFORM_DRAG_IMP: OnceLock<Imp> = OnceLock::new();

    pub(super) fn install(window: &winit::window::Window) -> bool {
        let Ok(handle) = window.window_handle() else {
            log::warn!("[drag_interpose] no window handle — falling back to cursor_pos");
            return false;
        };
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
            log::warn!("[drag_interpose] not an AppKit window handle — falling back to cursor_pos");
            return false;
        };

        unsafe {
            let ns_view = appkit.ns_view.as_ptr() as *mut AnyObject;
            let ns_window: *mut AnyObject = msg_send![ns_view, window];
            if ns_window.is_null() {
                log::warn!("[drag_interpose] NSView has no NSWindow yet — falling back to cursor_pos");
                return false;
            }
            let delegate: *mut AnyObject = msg_send![ns_window, delegate];
            if delegate.is_null() {
                log::warn!("[drag_interpose] NSWindow has no delegate — falling back to cursor_pos");
                return false;
            }

            let cls = object_getClass(delegate) as *mut AnyClass;
            if cls.is_null() {
                log::warn!("[drag_interpose] could not resolve delegate class — falling back to cursor_pos");
                return false;
            }

            let dragging_updated_sel = sel!(draggingUpdated:);
            if class_respondsToSelector(cls, dragging_updated_sel).as_bool() {
                // A future winit already implements this — nothing to add.
                // We don't have a live position source in that case (we'd
                // need to swizzle instead), so decline rather than guess.
                log::warn!(
                    "[drag_interpose] delegate already implements draggingUpdated: — \
                     winit likely changed; this module needs updating. Falling back to cursor_pos"
                );
                return false;
            }

            // NSUInteger (NSDragOperation's underlying type) is `unsigned
            // long` on 64-bit Apple platforms → Objective-C type encoding
            // "L", not "Q" (unsigned long long) — verified against
            // objc2::ffi::NSUInteger = usize.
            let added: bool = class_addMethod(
                cls,
                dragging_updated_sel,
                std::mem::transmute::<DraggingUpdatedFn, Imp>(dragging_updated),
                c"L@:@".as_ptr(),
            )
            .as_bool();
            if !added {
                log::warn!("[drag_interpose] class_addMethod(draggingUpdated:) failed — falling back to cursor_pos");
                return false;
            }

            let perform_drag_sel = sel!(performDragOperation:);
            let method = class_getInstanceMethod(cls, perform_drag_sel);
            if method.is_null() {
                log::warn!(
                    "[drag_interpose] delegate has no performDragOperation: to swizzle — falling back to cursor_pos"
                );
                return false;
            }
            let new_imp = std::mem::transmute::<PerformDragOperationFn, Imp>(perform_drag_operation_wrapper);
            let Some(original_imp) = method_setImplementation(method, new_imp) else {
                log::warn!(
                    "[drag_interpose] method_setImplementation(performDragOperation:) returned no prior IMP — falling back to cursor_pos"
                );
                return false;
            };
            if ORIGINAL_PERFORM_DRAG_IMP.set(original_imp).is_err() {
                log::warn!("[drag_interpose] install() called more than once — ignoring");
                return false;
            }

            NS_VIEW.with(|c| c.set(ns_view));
        }

        log::info!("[drag_interpose] installed draggingUpdated:/performDragOperation: interposition");
        true
    }

    type DraggingUpdatedFn = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize;
    type PerformDragOperationFn =
        unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool;

    const NS_DRAG_OPERATION_COPY: usize = 1;

    unsafe extern "C-unwind" fn dragging_updated(
        _this: *mut AnyObject,
        _sel: Sel,
        sender: *mut AnyObject,
    ) -> usize {
        unsafe { stash_location(sender) };
        NS_DRAG_OPERATION_COPY
    }

    unsafe extern "C-unwind" fn perform_drag_operation_wrapper(
        this: *mut AnyObject,
        sel: Sel,
        sender: *mut AnyObject,
    ) -> Bool {
        unsafe {
            stash_location(sender);
            let original = ORIGINAL_PERFORM_DRAG_IMP
                .get()
                .expect("perform_drag_operation_wrapper called before install()");
            let original: PerformDragOperationFn = std::mem::transmute(*original);
            // `original` queues WindowEvent::DroppedFile — it does NOT deliver
            // it synchronously, winit's event loop drains the queue later. So
            // the stashed position must survive past this call: clearing here
            // would wipe it before app.rs's DroppedFile arm ever reads it.
            // The tracker clears it itself, from the UI thread, once that arm
            // has finished (DragHoverTracker::on_drag_ended).
            original(this, sel, sender)
        }
    }

    unsafe fn stash_location(sender: *mut AnyObject) {
        unsafe {
            if sender.is_null() {
                return;
            }
            let ns_view = NS_VIEW.with(|c| c.get());
            if ns_view.is_null() {
                return;
            }
            // draggingLocation is in the destination's base coordinate
            // system — here that's the WINDOW (winit registers the window,
            // not a view, as the drop destination). convertPoint:fromView:
            // with a nil view means "the point is in window base
            // coordinates."
            let window_pt: NSPoint = msg_send![sender, draggingLocation];
            let view_pt: NSPoint = msg_send![
                ns_view,
                convertPoint: window_pt,
                fromView: std::ptr::null::<AnyObject>(),
            ];
            // winit's WinitView overrides `isFlipped` to return true (its
            // origin is the upper-left corner), so `convertPoint:fromView:`
            // already yields top-left-origin coordinates in points — the same
            // convention and units as `App::cursor_pos`. No flip: flipping on
            // the view height here mirrors y around the view's vertical
            // center (the launch-day bug — ghost visible only in the mirror
            // band, drops landing on the wrong lane).
            let logical = Vec2::new(view_pt.x as f32, view_pt.y as f32);
            log::debug!("[drag_interpose] drag position: {:.1},{:.1}", logical.x, logical.y);
            DRAG_POS.with(|c| c.set(Some(logical)));
        }
    }

}
