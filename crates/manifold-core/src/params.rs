//! Per-instance parameter manifest — id-keyed storage that replaces the
//! positional `Vec<ParamSlot>` + three-resolver design.
//!
//! `docs/PARAM_STORAGE_DESIGN.md` is the contract. The governing rule (D1):
//! **one struct, one list.** [`Param`] fuses descriptor (`spec`) and live
//! state (`value`/`base`/`exposed`/`touched`) in a single struct; identity is
//! the id (`spec.id`); insertion order IS card display order. There is no
//! positional identity, no `id → index` resolver, no registry consultation for
//! a live instance's params. A positional view is a *transient boundary
//! computation* (transport blocks — P3) guarded by [`ParamManifest::topology`],
//! never stored: a stored index is the exact bug class this module deletes
//! (`e226be46`, the driver-misroute).
//!
//! Owned by `manifold-core`; lives inside `Project`, so it is content-thread
//! resident and the UI only ever sees it through `Arc<Project>` snapshots. No
//! shared state, no locks (D-invariants).

use crate::effect_graph_def::ParamSpecDef;

/// Why this param exists on the instance. Behavioral tag only — origin has no
/// addressing consequence (D3): a `Bundled` param that is unexposed hides its
/// card; a `UserAdded` param that is removed deletes its entry; `UserAdded`
/// specs serialize inline while `Bundled` specs track the template. None of
/// that touches how the param is *addressed* (always by id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamOrigin {
    /// Seeded from the preset template at instantiation.
    Bundled,
    /// Added by the user (graph-editor expose). Its `spec` serializes inline.
    UserAdded,
}

/// One parameter: descriptor + live state, one struct, id as identity (D1).
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// Descriptor — reuses the existing wire struct ([`ParamSpecDef`]).
    /// Calibration edits `spec.min`/`spec.max`/`spec.curve`/`spec.invert` in
    /// place and sets [`Param::calibrated`] (D6).
    pub spec: ParamSpecDef,
    pub origin: ParamOrigin,
    /// True once calibration has diverged this param's `spec` from the
    /// template. Serialization gate for the `calibration` wire block; cleared
    /// by a calibration reset (which re-reads the template range).
    pub calibrated: bool,
    /// Effective (post-modulation) value — what the renderer reads.
    pub value: f32,
    /// User-intended base (pre-modulation) value. Modulation reads `base`,
    /// computes the effective, writes it to `value`; `reset_param_effectives`
    /// copies `base` back into `value` each frame before re-applying
    /// modulation. Whether `base` is *tracked* (emitted to the wire) is the
    /// per-instance `PresetInstance.base_tracked` bit.
    pub base: f32,
    pub exposed: bool,
    /// Runtime-only automation-latch flag (see `AUTOMATION_LANES_DESIGN.md`
    /// §4). Set by the single `set_base_param` funnel so the automation
    /// evaluator can detect "a hand touched this since I last looked". Never
    /// serialized.
    pub touched: bool,
}

impl Param {
    /// A bundled (template-seeded) param, value + base at the spec default,
    /// exposed, untouched, uncalibrated.
    pub fn bundled(spec: ParamSpecDef) -> Self {
        Self::seeded(spec, ParamOrigin::Bundled)
    }

    /// A user-added param, value + base at the spec default.
    pub fn user_added(spec: ParamSpecDef) -> Self {
        Self::seeded(spec, ParamOrigin::UserAdded)
    }

    fn seeded(spec: ParamSpecDef, origin: ParamOrigin) -> Self {
        let default = spec.default_value;
        Self {
            spec,
            origin,
            calibrated: false,
            value: default,
            base: default,
            exposed: true,
            touched: false,
        }
    }

    #[inline]
    pub fn id(&self) -> &str {
        &self.spec.id
    }

    /// True when the parameter is integral — the modulation evaluators round
    /// the final value when this is set. Comes off the `spec` uniformly for
    /// bundled and user-added params (D3/D6: the spec is the single authority;
    /// there is no separate `ParamConvert`-derived whole-number source now).
    #[inline]
    pub fn whole_numbers(&self) -> bool {
        self.spec.whole_numbers || !self.spec.value_labels.is_empty()
    }

    /// True when this param is periodic (BUG-039) — modulation constrains
    /// its effective value with [`constrain_to_range`]'s wrap arm instead of
    /// clamping. Reads straight off `spec.wraps` (D1: the spec is the single
    /// authority), mirroring [`Self::whole_numbers`]'s shape.
    #[inline]
    pub fn wraps(&self) -> bool {
        self.spec.wraps
    }

    /// True when this param should appear on the outer CARD (scene-panel
    /// exposure convergence). Reads straight off `spec.card_visible` (D1:
    /// the spec is the single authority) — the scene panel's own section
    /// query ignores this and always keeps every stamped param.
    #[inline]
    pub fn card_visible(&self) -> bool {
        self.spec.card_visible
    }
}

/// Constrain a modulation-computed value to a param's `[min, max]` range —
/// the single post-process point every driver/automation write funnels
/// through (BUG-039). `wraps` params rem_euclid back into range instead of
/// clamping, so a saw LFO or an automation ramp sweeping past `max` (or
/// below `min`) continues spinning instead of hitching at the rail:
/// `min + (value - min).rem_euclid(max - min)`. `rem_euclid` (not `%`) so a
/// downward-sweeping (negative) saw still lands on the geometrically
/// correct in-range value rather than a negative one. Falls back to a plain
/// clamp when `wraps` is false, or when the range is degenerate
/// (`max <= min`, nothing to wrap around).
#[inline]
pub fn constrain_to_range(value: f32, min: f32, max: f32, wraps: bool) -> f32 {
    if wraps && max > min {
        min + (value - min).rem_euclid(max - min)
    } else {
        value.clamp(min, max)
    }
}

/// The per-instance parameter manifest. Insertion order = card display order;
/// identity is the id; nothing is derived (D1).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParamManifest {
    entries: Vec<Param>,
    /// Bumped on every add / remove / reorder — NOT on value/state writes.
    /// Transport blocks and any transient positional view guard on this (D8):
    /// a same-length reorder that the old `len == len` guard missed changes
    /// this, so the guard now catches it.
    topology: u32,
}

impl ParamManifest {
    /// Build a manifest from an ordered list of params (topology starts at 0).
    /// The single constructor the loader and instantiation use — seeds/overlays
    /// happen on the `Vec<Param>` before it becomes a manifest.
    pub fn from_params(entries: Vec<Param>) -> Self {
        debug_assert!(
            {
                let mut ids: Vec<&str> = entries.iter().map(|p| p.id()).collect();
                ids.sort_unstable();
                let before = ids.len();
                ids.dedup();
                before == ids.len()
            },
            "ParamManifest built with duplicate param ids"
        );
        Self {
            entries,
            topology: 0,
        }
    }

    pub fn get(&self, id: &str) -> Option<&Param> {
        self.entries.iter().find(|p| p.spec.id == id)
    }

    /// Value/state writes only (value, base, exposed, touched, in-place
    /// calibration of `spec.min`/`max`). Does NOT bump topology.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Param> {
        self.entries.iter_mut().find(|p| p.spec.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Param> {
        self.entries.iter()
    }

    /// Whole-manifest state writes (e.g. `reset_param_effectives`,
    /// per-frame modulation apply). Order-stable; does NOT bump topology —
    /// callers must not add/remove/reorder through it (they can't: they only
    /// get `&mut Param`, never the `Vec`).
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Param> {
        self.entries.iter_mut()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    pub fn topology(&self) -> u32 {
        self.topology
    }

    pub fn contains(&self, id: &str) -> bool {
        self.entries.iter().any(|p| p.spec.id == id)
    }

    /// Display position of `id`, or `None`. Used to capture undo restore
    /// position (D10) and by boundary code that needs the transient index.
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.entries.iter().position(|p| p.spec.id == id)
    }

    /// Append a param at the end (card order). Bumps topology.
    /// `debug_assert`s id uniqueness within the manifest (the invariant
    /// `generate_user_param_id` already provides, now enforced at storage).
    pub fn push(&mut self, p: Param) {
        debug_assert!(
            !self.contains(p.id()),
            "ParamManifest::push duplicate id {:?}",
            p.id()
        );
        self.entries.push(p);
        self.topology = self.topology.wrapping_add(1);
    }

    /// Remove by id, returning the removed entry for undo capture. Bumps
    /// topology.
    pub fn remove(&mut self, id: &str) -> Option<Param> {
        let i = self.index_of(id)?;
        self.topology = self.topology.wrapping_add(1);
        Some(self.entries.remove(i))
    }

    /// Undo restore at a captured display position (D10). `index` is clamped
    /// to the current length. Bumps topology.
    pub fn insert_at(&mut self, index: usize, p: Param) {
        debug_assert!(
            !self.contains(p.id()),
            "ParamManifest::insert_at duplicate id {:?}",
            p.id()
        );
        let i = index.min(self.entries.len());
        self.entries.insert(i, p);
        self.topology = self.topology.wrapping_add(1);
    }

    /// Truncate to the first `len` entries (used only where the old
    /// positional path truncated a junk tail at load). Bumps topology iff it
    /// actually removed something.
    pub fn truncate(&mut self, len: usize) {
        if len < self.entries.len() {
            self.entries.truncate(len);
            self.topology = self.topology.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str, default: f32) -> ParamSpecDef {
        ParamSpecDef {
            id: id.to_string(),
            name: id.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: default,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: crate::macro_bank::MacroCurve::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
            card_visible: true,
        }
    }

    fn param(id: &str, default: f32) -> Param {
        Param::bundled(spec(id, default))
    }

    #[test]
    fn seeded_param_defaults_value_and_base() {
        let p = param("amount", 0.7);
        assert_eq!(p.value, 0.7);
        assert_eq!(p.base, 0.7);
        assert!(p.exposed);
        assert!(!p.touched);
        assert!(!p.calibrated);
        assert_eq!(p.id(), "amount");
        assert_eq!(p.origin, ParamOrigin::Bundled);
    }

    #[test]
    fn get_and_get_mut_by_id() {
        let mut m = ParamManifest::from_params(vec![param("a", 0.1), param("b", 0.2)]);
        assert_eq!(m.get("b").unwrap().value, 0.2);
        assert!(m.get("nope").is_none());
        m.get_mut("a").unwrap().value = 0.9;
        assert_eq!(m.get("a").unwrap().value, 0.9);
    }

    #[test]
    fn value_write_does_not_bump_topology() {
        let mut m = ParamManifest::from_params(vec![param("a", 0.1)]);
        let t = m.topology();
        m.get_mut("a").unwrap().value = 0.5;
        m.get_mut("a").unwrap().exposed = false;
        for p in m.iter_mut() {
            p.base = 0.0;
        }
        assert_eq!(m.topology(), t, "value/state writes must not bump topology");
    }

    #[test]
    fn push_remove_insert_bump_topology_and_preserve_order() {
        let mut m = ParamManifest::from_params(vec![param("a", 0.1), param("b", 0.2)]);
        let t0 = m.topology();
        m.push(param("c", 0.3));
        assert_eq!(m.topology(), t0 + 1);
        assert_eq!(
            m.iter().map(|p| p.id().to_string()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        let removed = m.remove("b").unwrap();
        assert_eq!(removed.id(), "b");
        assert_eq!(m.topology(), t0 + 2);
        assert_eq!(m.index_of("b"), None);
        // Undo restore at captured position 1.
        m.insert_at(1, removed);
        assert_eq!(m.topology(), t0 + 3);
        assert_eq!(
            m.iter().map(|p| p.id().to_string()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn same_length_reorder_changes_topology() {
        // The exact case the old `param_values.len() == len` transport guard
        // missed (D8): membership identical, order different.
        let mut m = ParamManifest::from_params(vec![param("a", 0.1), param("b", 0.2)]);
        let before = m.topology();
        let a = m.remove("a").unwrap();
        m.push(a); // now [b, a] — same length, different order
        assert_ne!(m.topology(), before, "a reorder must change topology");
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn insert_at_clamps_out_of_range_index() {
        let mut m = ParamManifest::from_params(vec![param("a", 0.1)]);
        m.insert_at(99, param("z", 0.9));
        assert_eq!(
            m.iter().map(|p| p.id().to_string()).collect::<Vec<_>>(),
            vec!["a", "z"]
        );
    }

    #[test]
    fn whole_numbers_from_spec() {
        let mut s = spec("count", 1.0);
        s.whole_numbers = true;
        let p = Param::bundled(s);
        assert!(p.whole_numbers());
        let mut s2 = spec("mode", 0.0);
        s2.value_labels = vec!["off".into(), "on".into()];
        assert!(Param::bundled(s2).whole_numbers());
    }

    #[test]
    fn wraps_from_spec() {
        let mut s = spec("rotation", 0.0);
        assert!(!Param::bundled(s.clone()).wraps(), "false by default");
        s.wraps = true;
        assert!(Param::bundled(s).wraps());
    }

    // ─── constrain_to_range (BUG-039) ──────────────────────────────────

    #[test]
    fn constrain_to_range_clamps_when_not_wrapping() {
        assert_eq!(constrain_to_range(400.0, 0.0, 360.0, false), 360.0);
        assert_eq!(constrain_to_range(-10.0, 0.0, 360.0, false), 0.0);
        assert_eq!(constrain_to_range(180.0, 0.0, 360.0, false), 180.0);
    }

    #[test]
    fn constrain_to_range_wraps_a_positive_saw_past_max() {
        // A saw LFO / automation ramp overshooting max continues spinning
        // instead of plateauing at the rail.
        assert_eq!(constrain_to_range(370.0, 0.0, 360.0, true), 10.0);
        assert_eq!(constrain_to_range(360.0, 0.0, 360.0, true), 0.0);
        assert_eq!(constrain_to_range(720.0, 0.0, 360.0, true), 0.0);
        assert_eq!(constrain_to_range(725.0, 0.0, 360.0, true), 5.0);
    }

    #[test]
    fn constrain_to_range_wraps_a_negative_saw_below_min() {
        // A downward-sweeping saw (or a reversed automation ramp) must land
        // on the geometrically correct in-range value, not a negative one —
        // this is exactly why the fix uses `rem_euclid`, not `%`.
        assert_eq!(constrain_to_range(-10.0, 0.0, 360.0, true), 350.0);
        assert_eq!(constrain_to_range(-360.0, 0.0, 360.0, true), 0.0);
        assert_eq!(constrain_to_range(-370.0, 0.0, 360.0, true), 350.0);
    }

    #[test]
    fn constrain_to_range_wraps_an_offset_range() {
        // min != 0 (e.g. a -180..180 rotation card) still wraps correctly.
        assert_eq!(constrain_to_range(190.0, -180.0, 180.0, true), -170.0);
        assert_eq!(constrain_to_range(-190.0, -180.0, 180.0, true), 170.0);
    }

    #[test]
    fn constrain_to_range_in_range_value_is_untouched() {
        // No-op for values already inside range, wrapping or not — existing
        // shows stay byte-identical.
        assert_eq!(constrain_to_range(90.0, 0.0, 360.0, true), 90.0);
        assert_eq!(constrain_to_range(90.0, 0.0, 360.0, false), 90.0);
    }

    #[test]
    fn constrain_to_range_degenerate_range_falls_back_to_clamp() {
        // max <= min: nothing to wrap around, must not divide by zero / NaN.
        let v = constrain_to_range(5.0, 3.0, 3.0, true);
        assert_eq!(v, 3.0);
        assert!(v.is_finite());
    }

    /// New-storage replacement for the deleted `bench_old_resolve_param_in_baseline`
    /// (PARAM_STORAGE_DESIGN §5). Measures `ParamManifest::get(id)` worst-case: a
    /// 40-param manifest, the target id LAST, and every id sharing a long common
    /// prefix so each non-matching compare does realistic work (real ids look like
    /// `user.mix.amount.1`). The old positional resolver (`resolve_param_in`, which
    /// also scanned + consulted the frozen registry) measured 135.73 ns/op; the
    /// design ceiling is 2× = 271.5 ns/op. `get` is a bare scan with no registry
    /// consult, so it should beat the baseline outright. Min-of-N rounds so a
    /// transient scheduler stall on a loaded machine can't flake the default
    /// `cargo test --workspace` sweep. Wall-clock ceilings still flake under
    /// nextest's parallel pool (BUG-113), so this only runs under
    /// `--features bench-timing`. Run with `-- --nocapture` to see the number.
    #[test]
    #[cfg(feature = "bench-timing")]
    fn bench_resolve() {
        const N: usize = 40;
        const ITERS: u64 = 1_000_000;
        const ROUNDS: usize = 5;

        let params: Vec<Param> = (0..N)
            .map(|i| param(&format!("preset.instance.param.slot_{i:02}"), i as f32 / N as f32))
            .collect();
        let manifest = ParamManifest::from_params(params);
        // Worst case: the id the scan reaches last.
        let worst_id = format!("preset.instance.param.slot_{:02}", N - 1);

        // Warm up (page-in, branch predictor) — not timed.
        let mut acc = 0.0f32;
        for _ in 0..ITERS {
            acc += manifest
                .get(std::hint::black_box(&worst_id))
                .map(|p| p.value)
                .unwrap_or(0.0);
        }
        std::hint::black_box(acc);

        let mut best_ns_per_op = f64::INFINITY;
        for _ in 0..ROUNDS {
            let start = std::time::Instant::now();
            let mut acc = 0.0f32;
            for _ in 0..ITERS {
                acc += manifest
                    .get(std::hint::black_box(&worst_id))
                    .map(|p| p.value)
                    .unwrap_or(0.0);
            }
            std::hint::black_box(acc);
            let ns_per_op = start.elapsed().as_nanos() as f64 / ITERS as f64;
            best_ns_per_op = best_ns_per_op.min(ns_per_op);
        }

        println!(
            "ParamManifest::get worst-case ({N} params, id last): {best_ns_per_op:.2} ns/op \
             (old resolve_param_in baseline 135.73 ns/op; ceiling 271.5)"
        );
        assert!(
            best_ns_per_op <= 271.5,
            "ParamManifest::get regressed past the 2× ceiling: {best_ns_per_op:.2} ns/op > 271.5"
        );
    }
}
