//! The motion layer — chrome-only tweens (`docs/UI_CRAFT_AND_MOTION_PLAN.md` §3).
//!
//! Four pieces, and every hover/press/drawer/toast/spawn/collapse effect in
//! that plan reduces to them. An executor reaching for a fifth mechanism
//! (a clock thread, a global animation registry, a shared locked handle) is
//! off-design — see D3's named wrong turn. Ownership is per-panel state,
//! ticked by the UI
//! frame loop that already runs every vsync (`ui_root.update()` →
//! `Panel::update()`, see `panels/layer_header.rs`'s record-pulse precedent —
//! "driven by the per-frame `update()` tick + elapsed time, so it needs no
//! animation subsystem"). A node with a live tween stays dirty (keeps
//! painting) while its `tick()` returns true; a `bool any_animating` bubbles
//! up from there for the caller's own dirty-tracking. No allocations on the
//! tick path — every field below is a plain scalar or a pre-sized `Vec`
//! reused across calls, never rebuilt per frame.
//!
//! Curves: `Curve::Ease` (D1, general-purpose — cubic-bezier(.25,.1,.25,1))
//! and `Curve::Snap` (D15, magnetic-snap settle only — back-out, ~25%
//! overshoot). Durations are one of `color::MOTION_FAST_MS` / `MOTION_MED_MS`
//! / `MOTION_SLOW_MS`.
//!
//! Reduced motion: `AnimF32::set_target` checks the process-wide
//! [`reduced_motion`] flag and collapses to [`AnimF32::snap`] when set. The
//! flag itself is a bare `AtomicBool` (never a registry, never a clock) —
//! set once by whichever caller queries the OS accessibility setting.
//!
//! ## Exit-state pattern (a rule, not a type)
//! A panel deleting/removing an item does NOT delete its UI node the instant
//! the model changes. It moves the item into a panel-owned
//! `dying: Vec<(Id, Transient)>`, fires the [`Transient`] (e.g. `MOTION_MED`
//! for a collapse+fade), and keeps DRAWING the dying item from that list
//! until [`Transient::tick`] returns `false` — only then does the panel drop
//! it from `dying` for good. The data model is unaffected: the
//! `EditingService` command that actually removed the item already
//! completed; motion only delays how long the UI keeps painting a node that
//! no longer exists in the model. This is the one place motion touches UI
//! node lifetime — every P2 "delete collapse" / "group fold" effect reduces
//! to this one rule, never a bespoke per-panel deletion animation.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::color;
use crate::node::Rect;

// ── Reduced motion ───────────────────────────────────────────────────────
// A single process-wide flag, not a registry: sizes zero state per-animation,
// costs one atomic load per `set_target`. Set once at startup (and again on a
// live OS toggle) by whichever caller queries the platform accessibility
// setting — manifold-ui has no OS dependency of its own, so the query itself
// lives upstream (e.g. `NSWorkspace.accessibilityDisplayShouldReduceMotion`
// on macOS) and calls [`set_reduced_motion`].

static REDUCED_MOTION: AtomicBool = AtomicBool::new(false);

/// Whether the OS's reduced-motion accessibility setting is currently active.
pub fn reduced_motion() -> bool {
    REDUCED_MOTION.load(Ordering::Relaxed)
}

/// Set the process-wide reduced-motion flag. Every `AnimF32::set_target` call
/// after this collapses to `snap()` until cleared again.
pub fn set_reduced_motion(v: bool) {
    REDUCED_MOTION.store(v, Ordering::Relaxed);
}

/// Master on/off for the UI motion layer, kept conceptually distinct from the
/// OS accessibility [`reduced_motion`] flag it currently rides on: this is
/// "does MANIFOLD's chrome animate at all," a product choice, not an
/// accessibility one. `set_motion_enabled(false)` collapses every
/// `AnimF32`/`FlipList` tween to an instant snap to its final visible state,
/// leaving the code in place (the motion is stashed behind the flag, not
/// deleted). Flip back to `true` to restore it. Does not gate the one-shot
/// `Transient` feedbacks (value flash, undo toast) — those still fire and are
/// seen, just as designed.
pub fn set_motion_enabled(enabled: bool) {
    set_reduced_motion(!enabled);
}

// ── Curves ───────────────────────────────────────────────────────────────

/// The one curve family every tween in the app picks from. `Ease` is the
/// general-purpose D1 curve; `Snap` fires only on magnetic-snap settle
/// events (D15) — never on a hover/press/drawer tween.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Curve {
    #[default]
    Ease,
    Snap,
}

impl Curve {
    fn apply(self, t: f32) -> f32 {
        match self {
            Curve::Ease => ease_cubic_bezier(t),
            Curve::Snap => ease_out_back(t, color::EASE_SNAP_BACK_C1),
        }
    }
}

/// `cubic-bezier(0.25, 0.1, 0.25, 1)` — D1's one general-purpose ease.
/// Solves the standard "given elapsed-time fraction x, find the bezier
/// parameter t with bezier_x(t) == x, then return bezier_y(t)" via a few
/// Newton-Raphson steps (the standard CSS-timing-function technique) — no
/// crate, ~15 lines.
fn ease_cubic_bezier(x: f32) -> f32 {
    const X1: f32 = 0.25;
    const Y1: f32 = 0.1;
    const X2: f32 = 0.25;
    const Y2: f32 = 1.0;

    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }

    let bezier = |t: f32, p1: f32, p2: f32| -> f32 {
        let mt = 1.0 - t;
        3.0 * mt * mt * t * p1 + 3.0 * mt * t * t * p2 + t * t * t
    };
    let bezier_deriv = |t: f32, p1: f32, p2: f32| -> f32 {
        let mt = 1.0 - t;
        3.0 * mt * mt * p1 + 6.0 * mt * t * (p2 - p1) + 3.0 * t * t * (1.0 - p2)
    };

    let mut t = x;
    for _ in 0..8 {
        let dx = bezier(t, X1, X2) - x;
        let d = bezier_deriv(t, X1, X2);
        if d.abs() < 1e-6 {
            break;
        }
        t = (t - dx / d).clamp(0.0, 1.0);
    }
    bezier(t, Y1, Y2)
}

/// Back-out ease: overshoots past 1.0 before settling there. `c1` controls
/// the overshoot magnitude (see `EASE_SNAP_BACK_C1`).
fn ease_out_back(x: f32, c1: f32) -> f32 {
    let c3 = c1 + 1.0;
    let u = x - 1.0;
    1.0 + c3 * u * u * u + c1 * u * u
}

// ── AnimF32 ──────────────────────────────────────────────────────────────

/// A single eased scalar tween — the foundation every other piece here (and
/// every D15/D17 motion effect in the plan) is built from. Zero allocations;
/// four `f32` fields plus a `Curve` tag.
#[derive(Clone, Copy, Debug)]
pub struct AnimF32 {
    current: f32,
    target: f32,
    from: f32,
    /// Progress through the active tween, 0..1. `>= 1.0` means settled.
    t: f32,
    /// One of `color::MOTION_FAST_MS` / `MOTION_MED_MS` / `MOTION_SLOW_MS`.
    dur_ms: f32,
    curve: Curve,
}

impl AnimF32 {
    /// A settled value with no tween in flight. `dur_ms` sizes every future
    /// `set_target` call on this instance.
    pub fn new(value: f32, dur_ms: f32) -> Self {
        Self {
            current: value,
            target: value,
            from: value,
            t: 1.0,
            dur_ms,
            curve: Curve::Ease,
        }
    }

    /// Use `Curve::Snap` instead of the default `Curve::Ease` for every tween
    /// on this instance (D15 magnetic-snap settle).
    pub fn with_curve(mut self, curve: Curve) -> Self {
        self.curve = curve;
        self
    }

    /// Retarget: restarts the tween **from wherever `current` sits right
    /// now** (never from the old `from`), so a rapid hover-in/hover-out never
    /// jumps back to the start. A no-op whenever `v` is already the target —
    /// whether settled there or already heading there — which is what makes
    /// this safe to call every frame with a caller's current discrete state
    /// (the normal usage: `set_target` unconditionally each tick, not only on
    /// change). Collapses to [`AnimF32::snap`] when [`reduced_motion`] is set.
    pub fn set_target(&mut self, v: f32) {
        if self.target == v {
            return;
        }
        if reduced_motion() {
            self.snap(v);
            return;
        }
        self.from = self.current;
        self.target = v;
        self.t = 0.0;
    }

    /// Jump straight to `v` with no animation — the reduced-motion path, and
    /// the right call for first-layout initialization (no tween on spawn).
    pub fn snap(&mut self, v: f32) {
        self.current = v;
        self.target = v;
        self.from = v;
        self.t = 1.0;
    }

    /// Advance the tween by `dt_ms`. Returns `true` while still animating —
    /// the caller keeps its node dirty / keeps painting while this is `true`,
    /// and can stop once it returns `false`.
    pub fn tick(&mut self, dt_ms: f32) -> bool {
        if self.t >= 1.0 {
            return false;
        }
        self.t = if self.dur_ms <= 0.0 {
            1.0
        } else {
            (self.t + dt_ms / self.dur_ms).min(1.0)
        };
        let eased = self.curve.apply(self.t);
        self.current = self.from + (self.target - self.from) * eased;
        self.t < 1.0
    }

    /// The current eased value — what the caller paints this frame.
    pub fn value(&self) -> f32 {
        self.current
    }

    /// The value this tween is heading toward (not the eased current).
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Whether a tween is currently in flight.
    pub fn is_animating(&self) -> bool {
        self.t < 1.0
    }
}

// ── Transient ────────────────────────────────────────────────────────────

/// A one-shot timed event with no persistent target — a flash, shake, pop,
/// pulse, sweep, or toast. Unlike `AnimF32` there is no "current value"; the
/// caller reads `progress()` and derives whatever it needs (an alpha, a
/// shake offset, a color mix) from it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Transient {
    elapsed_ms: f32,
    dur_ms: f32,
    active: bool,
}

impl Transient {
    /// Start (or restart) the one-shot, lasting `dur_ms`.
    pub fn fire(&mut self, dur_ms: f32) {
        self.elapsed_ms = 0.0;
        self.dur_ms = dur_ms;
        self.active = true;
    }

    /// `Some(progress)` (0..1) while active; `None` once idle or finished.
    pub fn progress(&self) -> Option<f32> {
        if !self.active {
            return None;
        }
        if self.dur_ms <= 0.0 {
            return Some(1.0);
        }
        Some((self.elapsed_ms / self.dur_ms).min(1.0))
    }

    /// Advance by `dt_ms`. Returns `true` while still active; on the frame it
    /// finishes, marks itself idle and returns `false`.
    pub fn tick(&mut self, dt_ms: f32) -> bool {
        if !self.active {
            return false;
        }
        self.elapsed_ms += dt_ms;
        if self.elapsed_ms >= self.dur_ms {
            self.active = false;
            return false;
        }
        true
    }
}

// ── FlipList ─────────────────────────────────────────────────────────────

/// List-displacement animation (the FLIP technique — First, Last, Invert,
/// Play): capture each item's rect before a reorder, then after the reorder
/// ease each item from its old position back to its new one. Used for card
/// reorders and group-fold collapses (P2); the offsets are added to the
/// item's already-final layout position, never used to compute layout
/// itself.
#[derive(Clone, Debug, Default)]
pub struct FlipList {
    pre: Vec<Rect>,
    offsets: Vec<(AnimF32, AnimF32)>,
}

impl FlipList {
    /// Record each item's rect before the reorder/resize that's about to
    /// happen.
    pub fn capture(&mut self, rects: &[Rect]) {
        self.pre.clear();
        self.pre.extend_from_slice(rects);
    }

    /// After the reorder, build one `(dx, dy)` `AnimF32` pair per item in
    /// `rects_after`, each easing from its pre-reorder offset down to zero
    /// over `dur_ms`. An index with no matching pre-capture rect (an item was
    /// added) gets a zero offset — it simply doesn't animate in. Returns the
    /// built offsets; call [`FlipList::tick`] each frame afterward.
    pub fn play(&mut self, rects_after: &[Rect], dur_ms: f32) -> &[(AnimF32, AnimF32)] {
        self.offsets = rects_after
            .iter()
            .enumerate()
            .map(|(i, after)| {
                let (dx, dy) = self
                    .pre
                    .get(i)
                    .map(|before| (before.x - after.x, before.y - after.y))
                    .unwrap_or((0.0, 0.0));
                let mut ax = AnimF32::new(dx, dur_ms);
                let mut ay = AnimF32::new(dy, dur_ms);
                ax.set_target(0.0);
                ay.set_target(0.0);
                (ax, ay)
            })
            .collect();
        &self.offsets
    }

    /// Advance every item's offset tween by `dt_ms`. Returns `true` while any
    /// item is still animating.
    pub fn tick(&mut self, dt_ms: f32) -> bool {
        let mut any = false;
        for (dx, dy) in &mut self.offsets {
            any |= dx.tick(dt_ms);
            any |= dy.tick(dt_ms);
        }
        any
    }

    /// The current per-item `(dx, dy)` offsets, in `rects_after` order.
    pub fn offsets(&self) -> &[(AnimF32, AnimF32)] {
        &self.offsets
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `REDUCED_MOTION` is a process-wide static; one test below flips it.
    // Rust runs `#[test]` fns on separate threads by default, so every test
    // in this module takes this lock first — serializing the whole module is
    // the cheapest way to keep that one test from bleeding into the others'
    // `set_target` expectations.
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ── AnimF32 ──────────────────────────────────────────────────────────

    #[test]
    fn progress_goes_zero_to_one_and_settles() {
        let _g = lock();
        let mut a = AnimF32::new(0.0, 100.0);
        a.set_target(10.0);
        assert!(a.is_animating());
        assert_eq!(a.value(), 0.0);

        // Halfway through the duration: still animating, value has moved.
        assert!(a.tick(50.0));
        assert!(a.value() > 0.0 && a.value() < 10.0);

        // Finish it off — settles exactly at the target and stops reporting
        // "still animating".
        assert!(!a.tick(50.0));
        assert!(!a.is_animating());
        assert_eq!(a.value(), 10.0);

        // Ticking a settled anim is a cheap no-op.
        assert!(!a.tick(16.0));
        assert_eq!(a.value(), 10.0);
    }

    #[test]
    fn retarget_mid_flight_restarts_from_current_not_from_the_old_from() {
        let _g = lock();
        let mut a = AnimF32::new(0.0, 100.0);
        a.set_target(10.0);
        a.tick(50.0); // partway there
        let mid_value = a.value();
        assert!(mid_value > 0.0 && mid_value < 10.0);

        // Retarget while mid-flight: the new tween must start from wherever
        // we are right now, not snap back to the original `from` (0.0).
        a.set_target(20.0);
        assert_eq!(a.value(), mid_value, "retarget must not jump on the same frame");
        assert!(a.is_animating());

        // One more full duration later it settles at the NEW target, and the
        // value only ever moved forward from `mid_value` (never dipped back
        // toward the original start).
        assert!(a.tick(50.0));
        assert!(a.value() >= mid_value);
        a.tick(50.0);
        assert_eq!(a.value(), 20.0);
    }

    #[test]
    fn snap_is_instant_no_animation() {
        let _g = lock();
        let mut a = AnimF32::new(0.0, 240.0);
        a.snap(42.0);
        assert_eq!(a.value(), 42.0);
        assert_eq!(a.target(), 42.0);
        assert!(!a.is_animating());
        // A single tick changes nothing — there is nothing to animate.
        assert!(!a.tick(16.0));
        assert_eq!(a.value(), 42.0);
    }

    #[test]
    fn reduced_motion_collapses_set_target_to_a_snap() {
        let _g = lock();
        set_reduced_motion(true);
        let mut a = AnimF32::new(0.0, 240.0);
        a.set_target(100.0);
        assert_eq!(a.value(), 100.0, "reduced motion must snap instantly");
        assert!(!a.is_animating());
        set_reduced_motion(false);

        // With the flag cleared, the same call animates normally again.
        let mut b = AnimF32::new(0.0, 240.0);
        b.set_target(100.0);
        assert!(b.is_animating());
        assert_eq!(b.value(), 0.0);
    }

    #[test]
    fn ease_curve_is_monotonic_and_bounded() {
        let _g = lock();
        let mut a = AnimF32::new(0.0, 100.0);
        a.set_target(1.0);
        let mut last = 0.0;
        for _ in 0..20 {
            a.tick(5.0);
            let v = a.value();
            assert!((-0.001..=1.001).contains(&v), "ease must stay within [0,1]: {v}");
            assert!(v >= last - 1e-4, "ease must be monotonic");
            last = v;
        }
        assert_eq!(a.value(), 1.0);
    }

    #[test]
    fn snap_curve_overshoots_by_roughly_25_percent() {
        let _g = lock();
        let mut a = AnimF32::new(0.0, 100.0).with_curve(Curve::Snap);
        a.set_target(1.0);
        let mut peak: f32 = 0.0;
        for _ in 0..100 {
            a.tick(1.0);
            peak = peak.max(a.value());
        }
        assert!(
            (peak - 1.25).abs() < 0.02,
            "back-out peak should overshoot ~25%, got {peak}"
        );
        assert_eq!(a.value(), 1.0, "must still settle exactly at the target");
    }

    // ── Transient ────────────────────────────────────────────────────────

    #[test]
    fn transient_lifecycle_fire_progress_finished() {
        let _g = lock();
        let mut t = Transient::default();
        assert_eq!(t.progress(), None, "idle before any fire()");

        t.fire(100.0);
        assert_eq!(t.progress(), Some(0.0));

        assert!(t.tick(40.0));
        assert!((t.progress().unwrap() - 0.4).abs() < 1e-4);

        assert!(t.tick(40.0));
        assert!((t.progress().unwrap() - 0.8).abs() < 1e-4);

        // Crossing the duration finishes it: tick reports false and progress
        // goes back to None.
        assert!(!t.tick(40.0));
        assert_eq!(t.progress(), None, "finished transient reports idle");

        // Re-firing restarts cleanly.
        t.fire(50.0);
        assert_eq!(t.progress(), Some(0.0));
    }

    // ── FlipList ─────────────────────────────────────────────────────────

    #[test]
    fn flip_offsets_start_at_the_pre_reorder_delta_and_ease_to_zero() {
        let _g = lock();
        let mut flip = FlipList::default();
        let before = vec![Rect::new(0.0, 0.0, 10.0, 10.0), Rect::new(0.0, 50.0, 10.0, 10.0)];
        flip.capture(&before);

        // Item 0 moved from y=0 to y=100; item 1 stayed put.
        let after = vec![Rect::new(0.0, 100.0, 10.0, 10.0), Rect::new(0.0, 50.0, 10.0, 10.0)];
        let offsets = flip.play(&after, 100.0);
        assert_eq!(offsets.len(), 2);
        // Pre-reorder offset for item 0: before.y - after.y = 0 - 100 = -100.
        assert_eq!(offsets[0].1.value(), -100.0);
        assert_eq!(offsets[1].1.value(), 0.0, "item that didn't move has zero offset");

        assert!(flip.tick(50.0));
        let mid = flip.offsets()[0].1.value();
        assert!(mid > -100.0 && mid < 0.0, "offset eases toward zero: {mid}");

        assert!(!flip.tick(50.0));
        assert_eq!(flip.offsets()[0].1.value(), 0.0);
        assert_eq!(flip.offsets()[1].1.value(), 0.0);
    }

    #[test]
    fn flip_new_item_with_no_pre_capture_gets_zero_offset() {
        let _g = lock();
        let mut flip = FlipList::default();
        flip.capture(&[Rect::new(0.0, 0.0, 10.0, 10.0)]);
        let after = vec![Rect::new(0.0, 0.0, 10.0, 10.0), Rect::new(0.0, 20.0, 10.0, 10.0)];
        let offsets = flip.play(&after, 100.0);
        assert_eq!(offsets[1].0.value(), 0.0);
        assert_eq!(offsets[1].1.value(), 0.0);
    }
}
