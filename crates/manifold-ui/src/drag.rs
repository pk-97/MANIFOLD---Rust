//! Generic drag controller — the one grab→track→release state machine.
//!
//! Today the codebase has five separate drag state machines (`SliderDragState`,
//! per-panel `dragging` bools, `UIState` timeline drag, `InteractionOverlay::
//! DragMode`, canvas `DragMode`). They share one shape and diverge in detail —
//! a bug farm whenever a new surface reinvents grab/track/release.
//!
//! `DragController<T>` is that shape, once: a drag is *active or not*; while
//! active it carries a typed payload `T` (what is being dragged plus any
//! grab-time context), the grab position, and the live pointer position.
//!
//! It deliberately does **not** interpret the motion. Turning a delta into a
//! slider value, a timeline beat, or a canvas graph-space offset is the
//! caller's job, because that mapping is different on every surface. The
//! controller owns only the lifecycle and the geometry; the meaning stays with
//! whoever started the drag.
//!
//! `SliderDragState` is reimplemented on top of this (its `dragging` flag *is*
//! a `DragController`) as the proof that the core carries a real consumer. The
//! slider is the degenerate case — no payload (`T = ()`), absolute-position
//! tracking — so it exercises the skeleton; the timeline and canvas wrappers
//! exercise the typed payload and the delta.
//!
//! P7 migration progress (`docs/UI_WIDGET_UNIFICATION_DESIGN.md`): the audit
//! at P7's start found `UIState` no longer owns a separate timeline-drag
//! copy — it was already folded into `InteractionOverlay`'s `drag_mode`
//! before this phase began (`ui_state.rs`: "Drag/trim lifecycle ... lives on
//! InteractionOverlay — the single owner"), so that item in the five-machine
//! list above is historical, not a live fifth machine. Migrated onto
//! `DragController<T>` so far: `AudioTriggerSection::dragging_shape` (one of
//! the per-panel `dragging` bools). Still open: the remaining per-panel ad
//! hoc drag state (`param_slider_shared::ParamDragState`'s six drag slots),
//! `InteractionOverlay::DragMode`, and `graph_canvas::DragMode`.

use crate::node::Vec2;

/// One in-flight drag: the typed payload plus where it started and where the
/// pointer is now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DragSession<T> {
    /// What is being dragged, captured at grab time. Anything the caller needs
    /// to remember for the life of the drag — a clip id, a node id and grab
    /// offset, the value at grab time for escape-to-restore.
    pub payload: T,
    /// Pointer position when the drag began.
    pub start: Vec2,
    /// Latest tracked pointer position.
    pub current: Vec2,
}

impl<T> DragSession<T> {
    /// Pointer travel since grab. The most common thing a caller wants.
    #[inline]
    pub fn delta(&self) -> Vec2 {
        self.current - self.start
    }
}

/// Owns the grab→track→release lifecycle for one draggable thing.
///
/// `None` session means idle. The state machine is total: `start` arms it,
/// `track` advances it (no-op when idle), `release` disarms it and hands back
/// the payload so the caller can commit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DragController<T> {
    session: Option<DragSession<T>>,
}

impl<T> Default for DragController<T> {
    fn default() -> Self {
        Self { session: None }
    }
}

impl<T> DragController<T> {
    /// An idle controller.
    pub fn new() -> Self {
        Self::default()
    }

    /// Is a drag in flight?
    #[inline]
    pub fn is_active(&self) -> bool {
        self.session.is_some()
    }

    /// The live session, if any.
    #[inline]
    pub fn session(&self) -> Option<&DragSession<T>> {
        self.session.as_ref()
    }

    /// The payload being dragged, if any.
    #[inline]
    pub fn payload(&self) -> Option<&T> {
        self.session.as_ref().map(|s| &s.payload)
    }

    /// Begin a drag. Replaces any in-flight session (a fresh grab always wins).
    pub fn start(&mut self, payload: T, pos: Vec2) {
        self.session = Some(DragSession {
            payload,
            start: pos,
            current: pos,
        });
    }

    /// Advance the drag to a new pointer position. Returns the live session
    /// (so the caller can read the delta in one call) or `None` if idle.
    pub fn track(&mut self, pos: Vec2) -> Option<&DragSession<T>> {
        let s = self.session.as_mut()?;
        s.current = pos;
        Some(&*s)
    }

    /// Release. Returns the payload if a drag was in flight — the signal the
    /// caller uses to emit a commit — or `None` if there was nothing to release.
    pub fn release(&mut self) -> Option<T> {
        self.session.take().map(|s| s.payload)
    }

    /// Drop the drag without returning the payload (teardown / rebuild). Unlike
    /// `release`, this does not signal a commit.
    pub fn cancel(&mut self) {
        self.session = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_by_default() {
        let d: DragController<()> = DragController::new();
        assert!(!d.is_active());
        assert!(d.session().is_none());
        assert!(d.payload().is_none());
    }

    #[test]
    fn lifecycle_grab_track_release() {
        let mut d: DragController<()> = DragController::new();

        // track while idle is a no-op
        assert!(d.track(Vec2::new(5.0, 5.0)).is_none());
        assert!(!d.is_active());

        d.start((), Vec2::new(10.0, 20.0));
        assert!(d.is_active());
        let s = d.session().unwrap();
        assert_eq!(s.start, Vec2::new(10.0, 20.0));
        assert_eq!(s.current, Vec2::new(10.0, 20.0));
        assert_eq!(s.delta(), Vec2::ZERO);

        let s = d.track(Vec2::new(13.0, 24.0)).unwrap();
        assert_eq!(s.current, Vec2::new(13.0, 24.0));
        assert_eq!(s.delta(), Vec2::new(3.0, 4.0));

        // release hands back the payload exactly once
        assert!(d.release().is_some());
        assert!(!d.is_active());
        assert!(d.release().is_none());
    }

    #[test]
    fn typed_payload_round_trips() {
        // A non-() payload proves the genericity the slider's () case doesn't.
        #[derive(Debug, PartialEq, Clone, Copy)]
        struct Grab {
            clip: u32,
            offset_beats: f32,
        }
        let mut d: DragController<Grab> = DragController::new();
        let grab = Grab {
            clip: 7,
            offset_beats: 1.5,
        };
        d.start(grab, Vec2::new(0.0, 0.0));
        assert_eq!(d.payload(), Some(&grab));
        d.track(Vec2::new(100.0, 0.0));
        assert_eq!(d.payload().unwrap().clip, 7);
        assert_eq!(d.release(), Some(grab));
    }

    #[test]
    fn fresh_grab_replaces_in_flight() {
        let mut d: DragController<u32> = DragController::new();
        d.start(1, Vec2::ZERO);
        d.start(2, Vec2::new(9.0, 9.0));
        assert_eq!(d.payload(), Some(&2));
        assert_eq!(d.session().unwrap().start, Vec2::new(9.0, 9.0));
    }

    #[test]
    fn cancel_drops_without_payload() {
        let mut d: DragController<u32> = DragController::new();
        d.start(42, Vec2::ZERO);
        d.cancel();
        assert!(!d.is_active());
        // a release after cancel signals nothing
        assert!(d.release().is_none());
    }
}
