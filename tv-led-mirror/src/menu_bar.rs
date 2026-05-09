//! Menu-bar status item with a Quit action.
//!
//! Without this the app is killable only via `killall tv-led-mirror` from a
//! terminal — fine for a CLI, but the user wanted a "real app" feel. Adding
//! a menu-bar item alongside the Dock icon (Info.plist's LSUIElement is
//! disabled) gives two ways to kill: Cmd+Q from the Dock, or click the
//! status icon → Quit.
//!
//! Lifecycle: NSApplication.run() blocks forever. The menu's Quit action
//! is wired to NSApp's built-in `terminate:` selector, which exits via
//! libc::exit(). We register an atexit hook that runs the LED controller's
//! shutdown — best-effort blackout — so the strips don't hold the last
//! captured frame after the user quits.

use std::sync::Arc;

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
};
use objc2_foundation::NSString;

use crate::SharedState;

/// `NSStatusItemVariableLength` — the system "auto-size to content" length.
const VARIABLE_LENGTH: f64 = -1.0;

/// Run the menu-bar event loop. Blocks until the user quits.
pub fn run(state: Arc<SharedState>) -> ! {
    install_atexit_blackout(state);

    let mtm = MainThreadMarker::new()
        .expect("menu_bar::run must be called on the main thread (call from main())");
    let app = NSApplication::sharedApplication(mtm);
    // Regular = visible in Dock, App Switcher, has menu bar. Pairs with the
    // Info.plist no longer setting LSUIElement.
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    install_status_item(mtm);

    // Run the AppKit event loop. The capture pipeline runs on a CGDisplayStream
    // dispatch queue + a background polling thread, so this thread is purely
    // for the UI. Returns only when the user quits, at which point exit() is
    // called and our atexit handler sends a final blackout.
    let _: () = unsafe { objc2::msg_send![&app, run] };
    unreachable!("NSApplication.run() returned");
}

fn install_status_item(mtm: MainThreadMarker) {
    let bar: Retained<NSStatusBar> = NSStatusBar::systemStatusBar();
    let item: Retained<NSStatusItem> = bar.statusItemWithLength(VARIABLE_LENGTH);

    if let Some(button) = item.button(mtm) {
        let title = NSString::from_str("LED");
        button.setTitle(&title);
    }

    // Menu with one item: "Quit TVLEDMirror" → terminate:.
    let menu = NSMenu::new(mtm);
    let quit_title = NSString::from_str("Quit TVLEDMirror");
    let key_q = NSString::from_str("q");
    let quit_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &quit_title,
            Some(objc2::sel!(terminate:)),
            &key_q,
        )
    };
    menu.addItem(&quit_item);
    item.setMenu(Some(&menu));

    // Keep the status item alive for the lifetime of the process.
    // `Retained` would otherwise drop it when this fn returns.
    std::mem::forget(item);
}

// ─── atexit shutdown ─────────────────────────────────────────────────────────

use parking_lot::Mutex;
use std::sync::OnceLock;

static SHUTDOWN_STATE: OnceLock<Mutex<Option<Arc<SharedState>>>> = OnceLock::new();

fn install_atexit_blackout(state: Arc<SharedState>) {
    let slot = SHUTDOWN_STATE.get_or_init(|| Mutex::new(None));
    *slot.lock() = Some(state);

    extern "C" fn cleanup() {
        let Some(slot) = SHUTDOWN_STATE.get() else {
            return;
        };
        // Pull the Arc out so we can drop it after shutdown — running the
        // controller's shutdown holds the controller mutex briefly; we don't
        // want to deadlock if anything else also tries to acquire it.
        let Some(state) = slot.lock().take() else {
            return;
        };
        // Best-effort: the controller's shutdown sends a blackout DMX packet
        // and tears down the ArtNet socket. Ignore poisoning — at exit we
        // just want the LEDs dark.
        state.controller.lock().shutdown();
    }
    unsafe {
        libc::atexit(cleanup);
    }
}
