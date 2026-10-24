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
use crate::id::LayerId;

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

/// Where a param's spec came from (SCENE_SURFACES_DESIGN.md D2 (one live
/// lookup)). In memory only: the join sets it when the manifest is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpecSource {
    /// The live lookup described it; the code is the authority.
    #[default]
    Lookup,
    /// No template or node declares it (composite, fan-out, graph-local or
    /// user-added), so its saved spec is the user's own.
    Authored,
    /// A lookup key exists but did not answer (type missing, registry not
    /// filled yet). The saved spec is kept word for word until it resolves.
    Unresolved,
}

/// Per-control display choices a project saves (SCENE_SURFACES_DESIGN.md D7
/// (one saved choice set)). The calibration wire block loads into `slider`,
/// `curve` and `invert`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParamChoices {
    pub rename: Option<String>,
    /// The user's slider span. Display only (D15): never the reshape or the
    /// wrap period.
    pub slider: Option<(f32, f32)>,
    pub curve: Option<crate::macro_bank::MacroCurve>,
    pub invert: Option<bool>,
    /// `Some(None)` clears the section the lookup gives.
    pub section: Option<Option<String>>,
    pub pin: Option<manifold_foundation::scene_surface::CardPin>,
}

impl ParamChoices {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// True when the user set a slider range, curve or invert.
    pub fn calibrated(&self) -> bool {
        self.slider.is_some() || self.curve.is_some() || self.invert.is_some()
    }

    /// Lay these choices over a described spec. The slider range is not laid
    /// over it: `spec.min`/`max` stay the declared range, which wraps, the
    /// `Slider` hard limit and the reshape read (D15). Span readers use
    /// [`Param::slider_span`].
    pub fn apply(&self, spec: &mut ParamSpecDef) {
        if let Some(name) = &self.rename {
            spec.name = name.clone();
        }
        if let Some(section) = &self.section {
            spec.section = section.clone();
        }
        if let Some(curve) = self.curve {
            spec.curve = curve;
        }
        if let Some(invert) = self.invert {
            spec.invert = invert;
        }
    }
}

/// Which layer supplies clip-trigger events for this parameter.
///
/// `OwnLayer` is the default and means that the parameter follows the layer
/// containing its effect or generator. The selected lane is kept as a stable
/// [`LayerId`] even when that layer is currently absent from the project, so
/// loading and reconciling a project never silently retargets the parameter.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ClipTriggerSource {
    #[default]
    OwnLayer,
    Disabled,
    Lane {
        #[serde(rename = "layerId")]
        layer_id: LayerId,
    },
}

impl ClipTriggerSource {
    #[inline]
    pub(crate) fn is_own_layer(&self) -> bool {
        matches!(self, Self::OwnLayer)
    }
}

/// One parameter: descriptor + live state, one struct, id as identity (D1).
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// Descriptor — reuses the existing wire struct ([`ParamSpecDef`]).
    /// The joined spec with [`Param::choices`] applied on top.
    pub spec: ParamSpecDef,
    pub origin: ParamOrigin,
    /// Where `spec` came from; never serialized.
    pub source: SpecSource,
    /// The user's display choices on this control (SCENE_SURFACES_DESIGN.md
    /// D7 (one saved choice set)). Already applied to `spec`.
    pub choices: ParamChoices,
    /// Effective (post-modulation) value — what the renderer reads.
    pub value: f32,
    /// User-intended base (pre-modulation) value. Modulation reads `base`,
    /// computes the effective, writes it to `value`; `reset_param_effectives`
    /// copies `base` back into `value` each frame before re-applying
    /// modulation. Whether `base` is *tracked* (emitted to the wire) is the
    /// per-instance `PresetInstance.base_tracked` bit.
    pub base: f32,
    pub exposed: bool,
    /// Persisted clip-trigger routing for this parameter. The default keeps
    /// the parameter on its containing layer; it is independent of audio
    /// modulation source selection.
    pub clip_trigger_source: ClipTriggerSource,
    /// Runtime-only identity of the selected authored clip pattern, refreshed
    /// by playback reconciliation. Standalone presets have no project pattern.
    /// Prepared physics controls consume this without borrowing the project.
    pub clip_control_digest: Option<[u8; 32]>,
    /// Runtime-only automation-latch flag (see `AUTOMATION_LANES_DESIGN.md`
    /// section 4). Set by the single `set_base_param` funnel so the automation
    /// evaluator can detect "a hand touched this since I last looked". Never
    /// serialized.
    pub touched: bool,
    /// Runtime-only: this param's automation lane wrote its base on the
    /// last automation pass (not latched, recorded or held by a hand), so
    /// the lane is its authored value at any time. The physics replay reads
    /// the lane at its own sample times instead of holding the frame's
    /// value (LIVE_SIM_CLOCK_DESIGN.md D4 (preserve control semantics)).
    /// Never serialized.
    pub automated: bool,
}

impl Param {
    /// A user's section edit. On a control the lookup describes, the saved
    /// section is ignored on load, so the edit is kept as a section choice
    /// (SCENE_SURFACES_DESIGN.md D7 (one saved choice set)). Returns the prior
    /// choice when one was recorded, for undo.
    pub fn choose_section(&mut self, section: Option<String>) -> Option<Option<Option<String>>> {
        self.spec.section = section.clone();
        (self.source == SpecSource::Lookup).then(|| self.choices.section.replace(section))
    }

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
            source: match origin {
                ParamOrigin::Bundled => SpecSource::Lookup,
                ParamOrigin::UserAdded => SpecSource::Authored,
            },
            choices: ParamChoices::default(),
            value: default,
            base: default,
            exposed: true,
            clip_trigger_source: ClipTriggerSource::OwnLayer,
            clip_control_digest: None,
            touched: false,
            automated: false,
        }
    }

    #[inline]
    pub fn id(&self) -> &str {
        &self.spec.id
    }

    /// True when the parameter is stepped (integral, labelled or a toggle) —
    /// the modulation evaluators round the final value and automation authors
    /// Hold steps when this is set. Comes off the `spec` uniformly for
    /// bundled and user-added params (D3/D6: the spec is the single authority;
    /// there is no separate `ParamConvert`-derived whole-number source now).
    #[inline]
    pub fn whole_numbers(&self) -> bool {
        self.spec.whole_numbers || self.spec.is_toggle || !self.spec.value_labels.is_empty()
    }

    /// The slider range: the user's choice when one is saved, else the
    /// declared range. What the slider spans, what OSC, MIDI and Ableton 0–1
    /// map over, and what modulation depth is measured over. Never a limit,
    /// the reshape or the wrap period (SCENE_SURFACES_DESIGN.md D15 (slider
    /// range and hard limit)).
    #[inline]
    pub fn slider_span(&self) -> (f32, f32) {
        self.choices
            .slider
            .unwrap_or((self.spec.min, self.spec.max))
    }

    /// The value automation and modulation may write (SCENE_SURFACES_DESIGN.md
    /// D15 (slider range and hard limit)). A periodic param wraps over the
    /// declared range. Otherwise the output stays inside the user's slider
    /// range when one is set, else the declared range: only a hand drag or a
    /// typed value goes past the slider's ends.
    #[inline]
    pub fn driven_value(&self, v: f32) -> f32 {
        match self.choices.slider {
            Some((a, b)) if !(self.spec.wraps && self.spec.max > self.spec.min) => {
                self.spec.clamp_input(v.clamp(a.min(b), a.max(b)))
            }
            _ => self.spec.clamp_value(v),
        }
    }

    /// The hard limit with `Slider` read as the declared range: what a drag
    /// may reach past the slider's end.
    pub fn drag_limit(&self) -> manifold_foundation::param_range::HardLimit {
        use manifold_foundation::param_range::HardLimit;
        match self.spec.limit {
            HardLimit::Slider => HardLimit::Between(self.spec.min, self.spec.max),
            limit => limit,
        }
    }

    /// True when this param is periodic (BUG-039) — modulation wraps its
    /// effective value over the declared range
    /// ([`ParamSpecDef::clamp_value`]) instead of clamping.
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

    /// Value/state writes only (value, base, exposed, touched, choices).
    /// Does NOT bump topology.
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
            tooltip: None,
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
            material_role: None,
            limit: Default::default(),
            unit: None,
            scene: None,
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
        assert!(!p.choices.calibrated());
        assert_eq!(p.clip_trigger_source, ClipTriggerSource::OwnLayer);
        assert_eq!(p.id(), "amount");
        assert_eq!(p.origin, ParamOrigin::Bundled);
    }

    #[test]
    fn trigger_source_defaults_to_own_layer_and_uses_stable_wire_names() {
        let own = ClipTriggerSource::default();
        assert_eq!(own, ClipTriggerSource::OwnLayer);
        assert_eq!(
            serde_json::to_value(&own).unwrap(),
            serde_json::json!({"kind": "ownLayer"})
        );

        let lane = ClipTriggerSource::Lane {
            layer_id: LayerId::new("missing-layer"),
        };
        assert_eq!(
            serde_json::to_value(&lane).unwrap(),
            serde_json::json!({"kind": "lane", "layerId": "missing-layer"})
        );
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
        let mut s3 = spec("on", 0.0);
        s3.is_toggle = true;
        assert!(Param::bundled(s3).whole_numbers(), "toggles are stepped");
    }

    #[test]
    fn wraps_from_spec() {
        let mut s = spec("rotation", 0.0);
        assert!(!Param::bundled(s.clone()).wraps(), "false by default");
        s.wraps = true;
        assert!(Param::bundled(s).wraps());
    }

    // ─── Slider range and hard limit (D15, BUG-039 wraps) ──────────────

    fn ranged(min: f32, max: f32, wraps: bool, limit: HardLimit) -> ParamSpecDef {
        ParamSpecDef {
            min,
            max,
            wraps,
            limit,
            ..spec("p", min)
        }
    }

    use manifold_foundation::param_range::HardLimit;

    #[test]
    fn clamp_value_clamps_to_the_declared_range_under_slider() {
        let s = ranged(0.0, 360.0, false, HardLimit::Slider);
        assert_eq!(s.clamp_value(400.0), 360.0);
        assert_eq!(s.clamp_value(-10.0), 0.0);
        assert_eq!(s.clamp_value(180.0), 180.0);
    }

    #[test]
    fn clamp_value_wraps_a_saw_both_ways() {
        let s = ranged(0.0, 360.0, true, HardLimit::Slider);
        assert_eq!(s.clamp_value(370.0), 10.0);
        assert_eq!(s.clamp_value(360.0), 0.0);
        assert_eq!(s.clamp_value(725.0), 5.0);
        assert_eq!(s.clamp_value(-10.0), 350.0);
        assert_eq!(s.clamp_value(-370.0), 350.0);
        assert_eq!(s.clamp_value(90.0), 90.0);
        let offset = ranged(-180.0, 180.0, true, HardLimit::Slider);
        assert_eq!(offset.clamp_value(190.0), -170.0);
        assert_eq!(offset.clamp_value(-190.0), 170.0);
    }

    #[test]
    fn clamp_value_degenerate_range_falls_back_to_clamp() {
        let v = ranged(3.0, 3.0, true, HardLimit::Slider).clamp_value(5.0);
        assert_eq!(v, 3.0);
    }

    /// INV-S18: typing passes the slider end; only a declared limit stops it.
    #[test]
    fn typed_value_clamps_to_limit_not_slider() {
        let mut p = Param::bundled(ranged(0.0, 10.0, false, HardLimit::AtLeast(0.0)));
        p.choices.slider = Some((0.0, 1.0));
        assert_eq!(
            p.spec.clamp_input(25.0),
            25.0,
            "past the user's slider and the declared range"
        );
        assert_eq!(p.spec.clamp_input(-3.0), 0.0, "the declared limit holds");
        let hint = ranged(0.0, 1.0, false, HardLimit::Slider);
        assert_eq!(
            hint.clamp_input(7.0),
            7.0,
            "a range hint never restricts typing"
        );
    }

    /// INV-S18: a drag carries past the slider end up to the hard limit.
    #[test]
    fn drag_past_slider_end_reaches_limit() {
        let mut p = Param::bundled(ranged(-5.0, 5.0, false, HardLimit::Slider));
        p.choices.slider = Some((-1.0, 1.0));
        assert_eq!(p.slider_span(), (-1.0, 1.0));
        assert_eq!(
            p.drag_limit().clamp(3.0, (p.spec.min, p.spec.max)),
            3.0,
            "past the user's slider end"
        );
        assert_eq!(
            p.drag_limit().clamp(9.0, (p.spec.min, p.spec.max)),
            5.0,
            "stops at the declared range"
        );
        p.spec.limit = HardLimit::Unlimited;
        assert_eq!(p.drag_limit().clamp(900.0, (p.spec.min, p.spec.max)), 900.0);
    }

    /// INV-S18: modulation and automation clamp to the hard limit, never the
    /// user's slider range.
    #[test]
    fn automation_respects_the_slider_range() {
        let mut p = Param::bundled(ranged(0.0, 1.0, false, HardLimit::Between(0.0, 4.0)));
        assert_eq!(p.driven_value(3.0), 3.0, "no user range: the hard limit");
        assert_eq!(p.driven_value(6.0), 4.0);
        p.choices.slider = Some((0.0, 0.5));
        assert_eq!(
            p.driven_value(0.8),
            0.5,
            "the user's slider range caps a driven value"
        );
        assert_eq!(p.driven_value(-1.0), 0.0);
        let mut wide = Param::bundled(ranged(0.0, 1.0, false, HardLimit::AtMost(2.0)));
        wide.choices.slider = Some((0.0, 3.0));
        assert_eq!(
            wide.driven_value(5.0),
            2.0,
            "a declared limit still wins over a wider slider"
        );
        let mut spin = Param::bundled(ranged(0.0, 360.0, true, HardLimit::Slider));
        spin.choices.slider = Some((0.0, 90.0));
        assert_eq!(
            spin.driven_value(370.0),
            10.0,
            "wraps stay on the declared range"
        );
    }

    /// INV-S18: wraps use the declared range, never the user's slider.
    #[test]
    fn wraps_use_declared_range() {
        let mut spec = ranged(0.0, 360.0, true, HardLimit::Slider);
        let choices = ParamChoices {
            slider: Some((0.0, 90.0)),
            ..ParamChoices::default()
        };
        choices.apply(&mut spec);
        let mut p = Param::bundled(spec);
        p.choices = choices;
        assert_eq!(
            (p.spec.min, p.spec.max),
            (0.0, 360.0),
            "the choice never lands in the spec"
        );
        assert_eq!(p.slider_span(), (0.0, 90.0));
        assert_eq!(p.spec.clamp_value(370.0), 10.0);
    }

    /// New-storage replacement for the deleted `bench_old_resolve_param_in_baseline`
    /// (PARAM_STORAGE_DESIGN section 5). Measures `ParamManifest::get(id)` worst-case: a
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
            .map(|i| {
                param(
                    &format!("preset.instance.param.slot_{i:02}"),
                    i as f32 / N as f32,
                )
            })
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
