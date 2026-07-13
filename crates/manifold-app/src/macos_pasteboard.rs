//! General NSPasteboard access — the sole module allowed to name
//! `NSPasteboard`/`NSPasteboardItem` (docs/TIMELINE_INGEST_DESIGN.md §4/P3).
//!
//! Backs the D4 Finder-paste arbitration: `general_change_count()` gives
//! AppKit's own recency oracle (snapshotted at internal `copy_clips` time,
//! compared again at Cmd+V), and `file_urls_on_general_pasteboard()` reads
//! only `file://` URLs — Finder file copies, never arbitrary pasteboard
//! text (D4/D5 forbid treating pasted text as a path).

use std::path::PathBuf;

use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypeString};
use objc2_foundation::{NSString, NSURL};

/// AppKit's `NSPasteboard.generalPasteboard.changeCount` — increments every
/// time anything (this app or another) writes to the general pasteboard.
/// The D4 arbitration snapshots this at internal copy time and compares it
/// again at paste time to tell whether a Finder copy happened since.
pub fn general_change_count() -> i64 {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.changeCount() as i64
}

/// File URLs currently on the general pasteboard (e.g. files copied in
/// Finder with Cmd+C). Only `file://` URLs are read — a `NSPasteboardType
/// FileURL` item that isn't a file URL, or any other pasteboard content
/// (text, images, internal drag payloads), is skipped. Never parses
/// pasteboard *text* as a path.
pub fn file_urls_on_general_pasteboard() -> Vec<PathBuf> {
    let pasteboard = NSPasteboard::generalPasteboard();
    let Some(items) = pasteboard.pasteboardItems() else {
        return Vec::new();
    };

    let mut paths = Vec::with_capacity(items.len());
    for item in items.iter() {
        // SAFETY: NSPasteboardTypeFileURL is a valid `&'static NSPasteboardType`
        // static provided by AppKit; dereferencing it to pass by reference is
        // the documented usage in objc2-app-kit's own examples.
        let url_type = unsafe { NSPasteboardTypeFileURL };
        let Some(url_string) = item.stringForType(url_type) else {
            continue;
        };
        let Some(url) = NSURL::URLWithString(&url_string) else {
            continue;
        };
        if !url.isFileURL() {
            continue;
        }
        if let Some(path_ns) = url.path() {
            paths.push(PathBuf::from(path_ns.to_string()));
        }
    }
    paths
}

/// The general pasteboard's plain-text string, if any (UI_WIDGET_UNIFICATION
/// P5b — Cmd+C/X/V for `TextEditModel`-backed sessions). `None` when the
/// pasteboard holds no string content (e.g. only a file copy).
pub fn general_pasteboard_string() -> Option<String> {
    let pasteboard = NSPasteboard::generalPasteboard();
    // SAFETY: NSPasteboardTypeString is a valid `&'static NSPasteboardType`
    // static provided by AppKit — same pattern as NSPasteboardTypeFileURL
    // above.
    let string_type = unsafe { NSPasteboardTypeString };
    pasteboard
        .stringForType(string_type)
        .map(|s| s.to_string())
}

/// Replaces the general pasteboard's contents with `s` as plain text
/// (Cmd+C/X). Clears any other content the pasteboard was holding — the
/// same one-owner-at-a-time semantics as the system Cmd+C.
pub fn set_general_pasteboard_string(s: &str) {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    let ns = NSString::from_str(s);
    // SAFETY: see `general_pasteboard_string` above.
    let string_type = unsafe { NSPasteboardTypeString };
    pasteboard.setString_forType(&ns, string_type);
}
