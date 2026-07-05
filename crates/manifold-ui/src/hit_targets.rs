//! `HitTargets` — the automation-visible mirror of a custom hit-test surface.
//!
//! `UITree::hit_test` cannot see inside the graph canvas, a timeline lane's
//! clip body, or an automation lane's strip — those surfaces run their own
//! hit-testing (`graph_canvas/hit.rs`, `clip_hit_tester.rs`,
//! `automation_hit_tester.rs`). The rule (`UI_AUTOMATION_DESIGN.md` D5):
//! whatever a surface can hit-test, it must enumerate here, so the automation
//! dump (§3) can address it by identity instead of raw coordinates.
//!
//! Enumeration is on-demand — called only when the headless harness (or a
//! future live automation door) builds a dump. Zero hot-path cost: nothing
//! here runs on the render/input path.

use crate::node::Rect;

/// Implemented by every surface that answers its own hit-testing. The
/// enumeration is the automation-visible mirror of `hit_test`: every kind of
/// thing `hit_test` can return appears here with its current rect and a
/// stable label. Committed shape — `UI_AUTOMATION_DESIGN.md` §5.
pub trait HitTargets {
    fn surface_id(&self) -> &'static str;
    fn enumerate(&self, out: &mut Vec<HitTargetEntry>);
}

/// One addressable thing a custom surface can hit-test: a node, a port, a
/// wire, a clip, an automation breakpoint, … `rect` is always the current
/// on-screen rect (post camera/scroll transform) so a script can resolve it
/// without knowing the surface's internal coordinate system; `payload` is the
/// stable domain id (a clip id, a `(scope_path, node id)` pair, …) an
/// automation script keys off for exactness once `label`/`kind` narrowed the
/// candidates.
pub struct HitTargetEntry {
    pub kind: &'static str,
    pub label: String,
    pub rect: Rect,
    pub payload: String,
}
