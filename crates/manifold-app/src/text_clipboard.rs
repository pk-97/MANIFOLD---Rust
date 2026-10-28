//! The plain-text clipboard behind text-field copy, cut and paste and the
//! copy actions. One handle lives on the UI thread: on X11 and Wayland the
//! owner serves the copied text, so a handle dropped right after a copy would
//! take the text with it.

use std::cell::RefCell;

thread_local! {
    static CLIPBOARD: RefCell<Option<arboard::Clipboard>> =
        RefCell::new(match arboard::Clipboard::new() {
            Ok(clipboard) => Some(clipboard),
            Err(err) => {
                log::warn!("[Clipboard] unavailable: {err}");
                None
            }
        });
}

/// The clipboard's text, if it holds any.
pub(crate) fn text() -> Option<String> {
    CLIPBOARD.with_borrow_mut(|clipboard| clipboard.as_mut()?.get_text().ok())
}

/// Replace the clipboard's contents with `text`.
pub(crate) fn set_text(text: &str) {
    CLIPBOARD.with_borrow_mut(|clipboard| {
        if let Some(clipboard) = clipboard
            && let Err(err) = clipboard.set_text(text)
        {
            log::warn!("[Clipboard] copy failed: {err}");
        }
    });
}
