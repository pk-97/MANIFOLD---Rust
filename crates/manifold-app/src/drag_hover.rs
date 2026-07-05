//! Tracks an in-flight Finder file-drag hover. UI thread only, no shared
//! state — the content thread learns about drops the same way it always has
//! (commands), after the drop is fully resolved here.
//!
//! One tracker serves every dropped file type (audio, MIDI, image, glTF):
//! each `DroppedFile` arm resolves its target via
//! `tracker.drop_position().unwrap_or(self.cursor_pos)` instead of reading
//! `cursor_pos` directly, so all of them get the live position from
//! `drag_interpose` where the platform supports it, and gracefully fall back
//! to the last known cursor position otherwise (non-macOS, or the one AppKit
//! forwarding assumption in `drag_interpose` not holding — see its doc
//! comment).

use crate::drag_interpose;
use manifold_ui::node::Vec2;
use std::path::PathBuf;

#[derive(Default)]
pub struct DragHoverTracker {
    hovered_files: Vec<PathBuf>,
    /// Probed once, from the FIRST hovered file only, at hover-start — not
    /// per frame. Drives the P2 ghost-clip preview (full source length at
    /// the would-be drop beat). `None` until a file has hovered, or if the
    /// first file isn't audio.
    first_hovered_audio_seconds: Option<manifold_core::Seconds>,
}

impl DragHoverTracker {
    pub fn on_hovered_file(&mut self, path: PathBuf) {
        if self.hovered_files.is_empty()
            && crate::project_io::is_supported_audio_extension(&path)
        {
            self.first_hovered_audio_seconds =
                Some(crate::project_io::audio_source_duration(&path.to_string_lossy()));
        }
        self.hovered_files.push(path);
    }

    /// Call once a drop has been consumed, or on `HoveredFileCancelled`.
    pub fn on_drag_ended(&mut self) {
        self.hovered_files.clear();
        self.first_hovered_audio_seconds = None;
        drag_interpose::clear_drag_position();
    }

    pub fn is_active(&self) -> bool {
        !self.hovered_files.is_empty()
    }

    /// Live pointer position for the in-flight drag, logical pixels,
    /// top-left origin (same convention as `App::cursor_pos`). `None` if no
    /// drag is active or the platform/interposition can't supply one.
    pub fn drop_position(&self) -> Option<Vec2> {
        drag_interpose::drag_position()
    }

    pub fn hovered_files(&self) -> &[PathBuf] {
        &self.hovered_files
    }

    /// Full source length of the first hovered file, if it's audio. `Some`
    /// only while a single audio file is being dragged — the P2 ghost
    /// preview shows this file's whole length at the would-be start beat.
    pub fn first_hovered_audio_seconds(&self) -> Option<manifold_core::Seconds> {
        self.first_hovered_audio_seconds
    }
}
