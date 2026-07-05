//! General NSPasteboard access — the sole module allowed to name
//! `NSPasteboard`/`NSPasteboardItem` (docs/TIMELINE_INGEST_DESIGN.md §4/P3).
//!
//! Backs the D4 Finder-paste arbitration: `general_change_count()` gives
//! AppKit's own recency oracle (snapshotted at internal `copy_clips` time,
//! compared again at Cmd+V), and `file_urls_on_general_pasteboard()` reads
//! only `file://` URLs — Finder file copies, never arbitrary pasteboard
//! text (D4/D5 forbid treating pasted text as a path).

use std::path::PathBuf;

use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL};
use objc2_foundation::NSURL;

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
