//! The `PresetInstance` aggregate: its struct, every inherent impl, and the
//! `ParamSource` impl. One type's whole contract in one file (D4 rejected a
//! per-facet split). Extracted from effects.rs (P2-E). Its custom wire-format
//! serde lives in `instance_serde.rs`.

use super::*;
use super::automation::{prune_automation_by_ids, take_automation_by_ids};
use super::instance_serde::{
    build_param_manifest, template_known_for, GraphWithDerivedParams, ManifestSer,
    ParamEntryWire,
};

// ─── Effect Instance ───


/// A single effect applied to a clip, layer, or master chain.
///
/// Serialization (custom impls below):
///
/// - `params` is one id-keyed V1.4 map (PARAM_STORAGE_DESIGN.md §4):
///   `{ id: { value, exposed, base? } }`, `base` present iff
///   `base_tracked`. This is the ONLY wire shape the typed (de)serialize
///   understands — the historical positional/keyed value duo (a values
///   container plus a parallel pre-modulation-base container) is gone;
///   `manifold-io`'s `migrations::param_storage_v14` converts every
///   legacy shape to `params` before typed deserialization ever runs.
/// - `build_effect_param_values` places incoming `params` entries onto
///   `[static prefix | user tail]` positional slots on load;
///   `ParamsSer` walks the same order in reverse on save.
///
/// In-memory storage stays positional (`Vec<ParamSlot>`) — the hot
/// path reads/writes by index. The Map form only exists on the wire.
/// See `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 step 12.
#[derive(Debug, Clone)]
pub struct PresetInstance {
    /// Whether this instance is an effect (transforms an input) or a
    /// generator (produces from nothing). The single discriminator that
    /// carries every effect/generator behavioral difference — kind-aware
    /// serde, the generator registry clamp on `set_param*`, and the
    /// generator-only fields below. Effects ignore `legacy_param_version`;
    /// generators ignore `id`/`enabled`/`collapsed`/`group_id` — those are
    /// chain-membership fields with no generator meaning. (`envelopes` and
    /// `graph` are now shared homes for both kinds — envelope-home + graph-home
    /// unification.)
    pub kind: crate::preset_def::PresetKind,
    /// Unique identifier for this effect instance. Synthetic for generators
    /// (a layer has one generator, addressed by `LayerId`).
    pub id: EffectId,
    pub(super) effect_type: PresetTypeId,
    pub enabled: bool,
    pub collapsed: bool,
    /// Id-keyed parameter manifest (PARAM_STORAGE_DESIGN.md D1): descriptor
    /// and live state in one [`crate::params::Param`] per parameter, keyed by
    /// id, with insertion order as card display order. Replaces the former
    /// positional `Vec<ParamSlot>` plus three id→index resolvers — there is no
    /// positional identity anymore, so nothing between creation and disk
    /// resolves a param through an index or the registry. Address a param by
    /// its stable id (`params.get(id)` / `params.get_mut(id)`); the renderer's
    /// `EffectSlot.bindings` read the same manifest by `source_id`.
    pub params: crate::params::ParamManifest,
    /// Whether the pre-modulation base (`ParamSlot.base`) is *tracked* — the
    /// single presence bit that gates the `base` key on each V1.4 `params`
    /// entry. Set on load whenever ANY incoming entry carried `base`, and by
    /// any base write (`set_base_param` / `reset_param_effectives` /
    /// `init_defaults_for_type`). While `false`, `get_base_param` falls
    /// through to the effective value, so per-slot `base` is allowed to be
    /// stale; the field is also the serialize-time gate for whether `base`
    /// appears on the wire at all. Not serialized; derived on load.
    pub base_tracked: bool,
    /// Raw V1.4 wire entries, held between deserialize and the loader's
    /// reconcile pass (`PARAM_STORAGE_BOUNDARIES_DESIGN.md` D1/D3). The
    /// custom `Serialize` impl above never reads this field, so it never
    /// rides the wire. `Some` from the moment `Deserialize`/`into_instance`
    /// builds the manifest until [`Self::reconcile_manifest`] rebuilds it
    /// against a registry that has since resolved the template — cleared at
    /// that point (the common case: one load, one reconcile). Stays `Some`
    /// across a reconcile call that still can't resolve a template (the
    /// keep-don't-drop path — BUG-036), so a *later* reconcile — after the
    /// registry catches up — can retry. `None` on a freshly-constructed
    /// instance (`new`/`new_generator`), which never had a wire to stash.
    pub(super) pending_wire: Option<std::collections::BTreeMap<String, ParamEntryWire>>,
    pub drivers: Option<Vec<ParameterDriver>>,
    /// Per-instance ADSR/Random envelopes, keyed by `param_id`. Envelope-home
    /// unification: both effects and generators store their envelopes here (the
    /// instance the envelope sits on is the target). `None` when unused.
    pub envelopes: Option<Vec<ParamEnvelope>>,
    pub ableton_mappings: Option<Vec<crate::ableton_mapping::AbletonParamMapping>>,
    /// Per-instance audio modulations, keyed by `param_id`. The fourth
    /// modulation source, parallel to `drivers`/`envelopes`/`ableton_mappings`:
    /// drives a param from a named audio send. `None` when unused. See
    /// `docs/AUDIO_MODULATION_DESIGN.md`.
    pub audio_mods: Option<Vec<crate::audio_mod::ParameterAudioMod>>,
    /// Per-instance timeline automation, keyed by `param_id` — a lane is a
    /// beat-indexed base writer sampled each tick (a tier-1 "hand"), riding
    /// on top of the same modulation pipeline audio_mods/drivers/envelopes
    /// feed. `None` when unused. See `docs/AUTOMATION_LANES_DESIGN.md`.
    pub automation_lanes: Option<Vec<AutomationLane>>,
    pub group_id: Option<EffectGroupId>,

    /// Per-instance graph topology override. `None` means "use the
    /// catalog default graph for this effect type" — every shipping
    /// fixture today loads with `graph: None` and round-trips
    /// byte-identically (the field is skipped when serializing).
    /// `Some(def)` carries a full graph definition that the renderer
    /// hydrates from instead of calling the catalog `build_*` helper.
    /// Phase 1 of the per-card-divergence work — see
    /// `docs/NODE_GRAPH_SYSTEM.md`.
    pub graph: Option<EffectGraphDef>,

    /// Monotonically bumped each time `graph` is replaced. Renderer
    /// caches the last seen version per instance, rebuilds the runtime
    /// `Graph` + plan + render state when it differs. Not serialized;
    /// resets to 0 on load — the renderer's `u32::MAX` sentinel forces
    /// a first-frame hydration whenever the loaded instance has a
    /// `Some(graph)`.
    pub graph_version: u32,

    /// Monotonically bumped only when `graph`'s **structure** changes —
    /// a node added/removed, a wire connected/disconnected, the whole def
    /// reverted. NOT bumped by a value-only edit (an inner param tweak) or a
    /// pure-layout edit (dragging a node), both of which still bump
    /// `graph_version` for the UI snapshot. The renderer hashes *this* into
    /// its rebuild key, so value/position edits refresh in place (state
    /// preserved) while only a real topology change forces a chain rebuild.
    /// Not serialized; resets to 0 on load.
    pub graph_structure_version: u32,

    // Legacy flat param fields (V1.0.0 format).
    pub legacy_param0: Option<f32>,
    pub legacy_param1: Option<f32>,
    pub legacy_param2: Option<f32>,
    pub legacy_param3: Option<f32>,

    /// Generator-only legacy flat field from V1.0.0 (before genParams
    /// nesting); serialized as `genParamVersion` for generator kind.
    pub legacy_param_version: Option<i32>,

    /// The "3D Shading" compile-level toggle (`docs/DEPTH_RELIGHT_DESIGN.md`
    /// D2, phase P5). `false` (default) is the exact graph that ships today,
    /// byte-identical — every existing project loads unchanged. `true` makes
    /// the renderer splice `relight_augment`'s D3 template before
    /// `final_output` on next rebuild. Shared by both kinds (effects and
    /// generators are both `PresetInstance`s), so one flag + one command path
    /// covers both cards. `PresetInstance` has custom Serialize/Deserialize
    /// impls below (not derived) — this field's wire handling lives there,
    /// not in a field attribute.
    pub relight: bool,
    /// The D3 relight-stage knobs. Always present (see
    /// [`RelightParams`]'s doc) — the toggle gates whether they're wired
    /// into the compiled graph, not whether they exist. Same custom-impl
    /// note as `relight` above.
    pub relight_params: RelightParams,
}


impl PresetInstance {
    /// Serialize a generator-kind instance in the legacy `PresetInstance`
    /// wire shape (so generator fixtures round-trip byte-identically). Ported
    /// from the former `impl Serialize for PresetInstance`.
    pub(super) fn serialize_as_generator<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let mut field_count = 2; // generatorType + params
        if self.drivers.is_some() {
            field_count += 1;
        }
        if self.envelopes.is_some() {
            field_count += 1;
        }
        if self.ableton_mappings.is_some() {
            field_count += 1;
        }
        if self.audio_mods.is_some() {
            field_count += 1;
        }
        if self.automation_lanes.is_some() {
            field_count += 1;
        }
        if self.graph.is_some() {
            field_count += 1;
        }
        if self.legacy_param_version.is_some() {
            field_count += 1;
        }
        if self.relight {
            field_count += 1;
        }
        if !self.relight_params.is_default() {
            field_count += 1;
        }

        let mut s = serializer.serialize_struct("PresetInstance", field_count)?;
        s.serialize_field("generatorType", &self.effect_type)?;
        s.serialize_field(
            "params",
            &ManifestSer {
                manifest: &self.params,
                base_tracked: self.base_tracked,
            },
        )?;
        if let Some(d) = &self.drivers {
            s.serialize_field("drivers", d)?;
        }
        if let Some(e) = &self.envelopes {
            s.serialize_field("envelopes", e)?;
        }
        if let Some(m) = &self.ableton_mappings {
            s.serialize_field("abletonMappings", m)?;
        }
        if let Some(a) = &self.audio_mods {
            s.serialize_field("audioMods", a)?;
        }
        if let Some(a) = &self.automation_lanes {
            s.serialize_field("automationLanes", a)?;
        }
        if let Some(g) = &self.graph {
            // `params` on the wrapper is derived from the live manifest
            // (D12) — see `GraphWithDerivedParams`.
            s.serialize_field(
                "graph",
                &GraphWithDerivedParams { graph: g, manifest: &self.params },
            )?;
        }
        if let Some(v) = self.legacy_param_version {
            s.serialize_field("genParamVersion", &v)?;
        }
        if self.relight {
            s.serialize_field("relight", &self.relight)?;
        }
        if !self.relight_params.is_default() {
            s.serialize_field("relightParams", &self.relight_params)?;
        }
        s.end()
    }
}


impl PresetInstance {
    /// Whether the relight stage should actually be compiled/rendered for this
    /// instance. Gated by [`manifold_foundation::RELIGHT_FEATURE_ENABLED`] so
    /// the disabled feature is inert even for projects that saved
    /// `relight: true`. All renderer compile/fusion/hash decisions consult
    /// this, never the raw field.
    #[inline]
    pub fn relight_active(&self) -> bool {
        manifold_foundation::RELIGHT_FEATURE_ENABLED && self.relight
    }

    /// The per-instance graph override (`None` ⇒ use the catalog default).
    /// One home for both effects and generators after the graph-home
    /// unification (the generator graph used to live on `Layer`).
    #[inline]
    pub fn graph_def(&self) -> &Option<EffectGraphDef> {
        &self.graph
    }

    /// Mutable handle to the per-instance graph override, for the graph
    /// editing commands.
    #[inline]
    pub fn graph_def_mut(&mut self) -> &mut Option<EffectGraphDef> {
        &mut self.graph
    }

    /// Bump the graph version so the renderer re-snapshots the graph for the
    /// UI. Bumped by *every* graph edit (value, position, structure). Does NOT
    /// on its own force a chain rebuild — see [`Self::bump_graph_structure_version`].
    #[inline]
    pub fn bump_graph_version(&mut self) {
        self.graph_version = self.graph_version.wrapping_add(1);
    }

    /// Bump the *structure* version — and the snapshot version with it — for an
    /// edit that changes the graph's topology (node/wire add or remove, a full
    /// revert). The renderer keys its rebuild on the structure version, so this
    /// is the only path that forces a chain rebuild (and the state reset that
    /// comes with it). Value/position-only edits call
    /// [`Self::bump_graph_version`] instead, so they refresh in place.
    #[inline]
    pub fn bump_graph_structure_version(&mut self) {
        self.graph_structure_version = self.graph_structure_version.wrapping_add(1);
        self.graph_version = self.graph_version.wrapping_add(1);
    }

    /// Write the user-set base value (pre-modulation) for a `param_id`.
    /// Returns `true` if the id resolved. Thin id-forwarding wrapper kept for
    /// the editing commands that drive a card param through a [`GraphTarget`];
    /// [`Self::set_base_param`] is now itself id-keyed and returns the same
    /// bool.
    pub fn set_base_param_by_id(&mut self, param_id: &str, value: f32) -> bool {
        self.set_base_param(param_id, value)
    }

    /// Create a new effect-kind PresetInstance with the given type.
    pub fn new(effect_type: PresetTypeId) -> Self {
        Self {
            kind: crate::preset_def::PresetKind::Effect,
            id: generate_effect_id(),
            effect_type,
            enabled: true,
            collapsed: false,
            params: crate::params::ParamManifest::default(),
            base_tracked: false,
            pending_wire: None,
            drivers: None,
            envelopes: None,
            ableton_mappings: None,
            audio_mods: None,
            automation_lanes: None,
            group_id: None,
            graph: None,
            graph_version: 0,
            graph_structure_version: 0,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
            legacy_param_version: None,
            relight: false,
            relight_params: RelightParams::default(),
        }
    }

    /// Create a new generator-kind PresetInstance, fully initialized to the
    /// generator type's registry defaults. The generator mirror of [`Self::new`]
    /// (ported from the former `PresetInstance::new`).
    pub fn new_generator(generator_type: PresetTypeId) -> Self {
        let mut s = Self {
            kind: crate::preset_def::PresetKind::Generator,
            id: generate_effect_id(),
            effect_type: generator_type,
            enabled: true,
            collapsed: false,
            params: crate::params::ParamManifest::default(),
            base_tracked: false,
            pending_wire: None,
            drivers: None,
            envelopes: None,
            ableton_mappings: None,
            audio_mods: None,
            automation_lanes: None,
            group_id: None,
            graph: None,
            graph_version: 0,
            graph_structure_version: 0,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
            legacy_param_version: None,
            relight: false,
            relight_params: RelightParams::default(),
        };
        s.init_defaults();
        s
    }

    /// Rebuild this instance's `ParamManifest` from its stashed wire entries
    /// (D1) against the CURRENT registry — called by
    /// [`crate::project::Project::reconcile_param_manifests`] once the
    /// project's own embedded presets have been installed. No-op if there is
    /// no stash (a freshly-constructed instance, or one already resolved).
    ///
    /// Idempotent, and safe to call more than once: if this pass still can't
    /// resolve a template for the instance (`template_known_for` false —
    /// BUG-036's class, e.g. a project-local preset not registered yet), the
    /// stash is kept rather than cleared, so a *later* reconcile call — once
    /// the registry catches up — can retry from the same wire entries. Once
    /// a pass resolves a real template, the stash is cleared; this is the
    /// common case (one load, one reconcile).
    pub(crate) fn reconcile_manifest(&mut self) {
        let Some(wire) = self.pending_wire.clone() else {
            return;
        };
        let (params, base_tracked) =
            build_param_manifest(self.is_generator(), &self.effect_type, &self.graph, Some(wire));
        self.params = params;
        self.base_tracked = base_tracked;
        if template_known_for(self.is_generator(), &self.effect_type, &self.graph) {
            self.pending_wire = None;
        }
    }

    /// Rebuild this instance's `ParamManifest` from the CURRENT `graph`
    /// metadata (BUG-295) — the live twin of [`Self::reconcile_manifest`],
    /// for a structural scene edit (e.g. `AddSceneFogCommand`) that stamps a
    /// freshly-minted node's exposures into `graph.preset_metadata.params`
    /// at runtime. `reconcile_manifest` only fires from a `pending_wire`
    /// stash set at load/deserialize time, so a live structural edit has no
    /// path back into `self.params` without this: the new/changed rows stay
    /// invisible to the panel until a save+reload round trip re-derives the
    /// manifest from the file's wire.
    ///
    /// Value preservation: the CURRENT manifest is round-tripped through the
    /// same wire encoding the file serializer uses
    /// ([`ParamEntryWire::from_param`], `base_tracked` included) instead of
    /// `reconcile_manifest`'s saved-file wire — this is exactly the encode
    /// half of the save/load cycle that already preserves state on disk, run
    /// in-memory instead. `build_param_manifest` then overlays that wire onto
    /// descriptors freshly gathered from `graph` (§`gather_known_params`):
    /// existing params keep their live value/base/exposed/calibration by id;
    /// a param whose backing node was newly stamped appears with its spec
    /// default; a param whose backing node is no longer in
    /// `graph.preset_metadata.params` simply isn't re-seeded, so it drops.
    ///
    /// Does NOT touch `pending_wire` — that stash is load-path-only and
    /// orthogonal to this method (`manifest_provisional` reads only
    /// `pending_wire`, so this never flips it).
    pub fn refresh_manifest_from_graph(&mut self) {
        let wire: std::collections::BTreeMap<String, ParamEntryWire> = self
            .params
            .iter()
            .map(|p| (p.id().to_string(), ParamEntryWire::from_param(p, self.base_tracked)))
            .collect();
        let (params, base_tracked) =
            build_param_manifest(self.is_generator(), &self.effect_type, &self.graph, Some(wire));
        self.params = params;
        self.base_tracked = base_tracked;
    }

    /// Whether this instance's preset def failed to resolve at load (BUG-036's
    /// class) — no registry template AND no inline generator `meta.params`,
    /// so its saved params are kept on a placeholder spec rather than dropped
    /// (`build_param_manifest`'s keep-don't-drop branch). Used by
    /// `Project::reconcile_param_manifests` (BUG-079) to count how many
    /// instances need the "opened with repairs" toast to name them — the
    /// same condition `PresetRuntime::try_build` hits later at chain-build
    /// time when it can't find a `LoadedPresetView` for the effect type and
    /// falls back to source passthrough (`preset_runtime.rs`).
    pub fn template_unresolved(&self) -> bool {
        !template_known_for(self.is_generator(), &self.effect_type, &self.graph)
    }

    /// `true` when this instance's manifest was built against an incomplete
    /// registry and hasn't been reconciled yet (BUG-080). `pending_wire` is
    /// the structural marker for "provisional" — set at deserialize-time
    /// build, cleared by [`Self::reconcile_manifest`] once a real template
    /// resolves. A provisional manifest reaching a runtime seam (chain build,
    /// UI row translation) means a load/ingest path skipped the reconcile
    /// call; see `docs/PARAM_MANIFEST_GATE_DESIGN.md` D1.
    pub fn manifest_provisional(&self) -> bool {
        self.pending_wire.is_some()
    }

    #[inline]
    pub fn is_generator(&self) -> bool {
        matches!(self.kind, crate::preset_def::PresetKind::Generator)
    }

    /// Read-only access to the effect type.
    #[inline]
    pub fn effect_type(&self) -> &PresetTypeId {
        &self.effect_type
    }

    /// Retarget this instance at a different preset id WITHOUT resetting params
    /// (unlike `change_type`). Used by the fork flow: a fork is a copy of the
    /// same preset under a new id, so the existing param values stay valid.
    #[inline]
    pub fn set_preset_id(&mut self, id: PresetTypeId) {
        self.effect_type = id;
    }

    /// Read-only access to the generator type (alias of [`Self::effect_type`]
    /// — the `effect_type` field holds the preset type for both kinds).
    #[inline]
    pub fn generator_type(&self) -> &PresetTypeId {
        &self.effect_type
    }

    /// Has any drivers? Unity PresetInstance.cs line 28.
    pub fn has_drivers(&self) -> bool {
        self.drivers.as_ref().is_some_and(|d| !d.is_empty())
    }

    pub fn clone_deep(&self) -> Self {
        self.clone()
    }

    /// Assign a fresh EffectId (used when deep-cloning a layer or effect chain).
    pub fn regenerate_id(&mut self) {
        self.id = EffectId::new(crate::math::short_id());
    }

    /// A fresh, independent copy of this instance for duplication / paste.
    ///
    /// Mints a new [`EffectId`] (so the copy is a distinct identity, not a
    /// reference to the original) and applies the "fresh copy" carry-rule:
    /// hardware/external bindings are dropped — `ableton_mappings` and
    /// `audio_mods` are cleared, so a pasted card is NOT mapped to the same
    /// Ableton control or audio send as its source. Per-instance modulation
    /// that has no external binding (`drivers`, `envelopes`) is kept.
    ///
    /// `group_id` is left untouched — the caller decides: a cross-chain paste
    /// drops it (the source group doesn't exist in the destination); a
    /// whole-layer duplicate remaps it to the duplicated group.
    pub fn duplicated(&self) -> Self {
        let mut copy = self.clone();
        copy.regenerate_id();
        copy.ableton_mappings = None;
        copy.audio_mods = None;
        copy
    }

    /// Number of parameters on this instance (manifest length).
    pub fn param_count(&self) -> usize {
        self.params.len()
    }

    /// Read the effective (modulated) value of param `id`. Unknown id → 0.0.
    pub fn get_param(&self, id: &str) -> f32 {
        self.params.get(id).map(|p| p.value).unwrap_or(0.0)
    }

    /// Whether param `id` is exposed (a visible card slider). Unknown id →
    /// `true`, the conservative always-visible default.
    pub fn is_param_exposed(&self, id: &str) -> bool {
        self.params.get(id).map(|p| p.exposed).unwrap_or(true)
    }

    /// Toggle a param's exposure flag. No-op if `id` is unknown.
    pub fn set_param_exposed(&mut self, id: &str, exposed: bool) {
        if let Some(p) = self.params.get_mut(id) {
            p.exposed = exposed;
        }
    }

    /// Write the effective (modulated) value of param `id`. No-op if `id` is
    /// unknown — a value write can't create a param (the manifest is seeded at
    /// instantiation/load; there is no positional grow). No registry clamp
    /// (the renderer reshape is the single place range is enforced toward the
    /// inner node).
    pub fn set_param(&mut self, id: &str, value: f32) {
        if let Some(p) = self.params.get_mut(id) {
            p.value = value;
        }
    }

    /// Read the user-set base value (before modulation) of param `id`. While
    /// base isn't tracked, `Param.base` may be stale, so fall through to the
    /// effective value.
    pub fn get_base_param(&self, id: &str) -> f32 {
        if self.base_tracked
            && let Some(p) = self.params.get(id)
        {
            return p.base;
        }
        self.get_param(id)
    }

    /// Set the user-intended base value of param `id`. **The single funnel**
    /// every live hand (UI slider, Ableton macro, OSC, macro bank, editing
    /// commands) writes through — marks the param `touched` so the
    /// automation-lane override latch (`docs/AUTOMATION_LANES_DESIGN.md` §4) can
    /// detect it. Returns whether `id` resolved. System-level seeding that is
    /// NOT a live gesture uses [`Self::write_base_param`] /
    /// [`Self::set_base_param_from_automation`] (no `touched`).
    pub fn set_base_param(&mut self, id: &str, value: f32) -> bool {
        if !self.write_base_param(id, value) {
            return false;
        }
        if let Some(p) = self.params.get_mut(id) {
            p.touched = true;
        }
        true
    }

    /// Automation-lane-only sibling of [`Self::set_base_param`]: writes
    /// `base`/`value` identically but does **not** set `touched` (using
    /// `set_base_param` from the automation evaluator would self-latch the lane
    /// as overridden the next frame). See `docs/AUTOMATION_LANES_DESIGN.md` §4.
    pub fn set_base_param_from_automation(&mut self, id: &str, value: f32) {
        self.write_base_param(id, value);
    }

    /// Shared base writer: `ensure_base_values` then write base + effective on
    /// param `id`. Does NOT set `touched` (callers decide). Returns whether
    /// `id` resolved. No generator migrate-on-touch: the manifest is fully
    /// seeded at instantiation/load, so there is no lazy positional tail to
    /// grow.
    pub(crate) fn write_base_param(&mut self, id: &str, value: f32) -> bool {
        self.ensure_base_values();
        match self.params.get_mut(id) {
            Some(p) => {
                // Setting base also sets the effective; modulation later
                // overrides value.
                p.base = value;
                p.value = value;
                true
            }
            None => false,
        }
    }

    /// Reset every param's effective value from its base value.
    pub fn reset_param_effectives(&mut self) {
        self.ensure_base_values();
        for p in self.params.iter_mut() {
            p.value = p.base;
        }
    }

    /// Begin tracking base: capture each param's current effective value as its
    /// base (when not already tracked) so subsequent modulation reads a stable
    /// pre-mod value.
    pub fn ensure_base_values(&mut self) {
        if !self.base_tracked {
            for p in self.params.iter_mut() {
                p.base = p.value;
            }
            self.base_tracked = true;
        }
    }

    pub fn find_driver(&self, param_id: &str) -> Option<&ParameterDriver> {
        self.drivers
            .as_ref()?
            .iter()
            .find(|d| d.param_id == param_id)
    }

    /// Get drivers list reference (may be None).
    pub fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>> {
        self.drivers.as_ref()
    }

    /// Create a driver for a param id.
    pub fn create_driver(&mut self, param_id: ParamId) -> &ParameterDriver {
        let driver = ParameterDriver::new(param_id, BeatDivision::Quarter, DriverWaveform::Sine);
        self.drivers_mut().push(driver);
        self.drivers.as_ref().unwrap().last().unwrap()
    }

    /// Remove driver by param id.
    pub fn remove_driver(&mut self, param_id: &str) {
        if let Some(drivers) = &mut self.drivers {
            drivers.retain(|d| d.param_id != param_id);
        }
    }

    /// The per-instance graph's user-added [`BindingDef`]s, in declaration
    /// order — the **single source of truth** for this effect's
    /// user-exposed parameters after the binding-storage unification
    /// (`PRESET_UNIFICATION_PLAN.md` step 3). User bindings no longer live
    /// in a parallel `PresetInstance.user_param_bindings` Vec; they are the
    /// `user_added` entries of `graph.preset_metadata.bindings`, exactly
    /// like the generator side. Empty when the effect has no per-instance
    /// graph or no user-added bindings. Order matches the `param_values`
    /// user-tail order (registry `param_count + j`).
    pub fn user_added_bindings(&self) -> impl Iterator<Item = &crate::effect_graph_def::BindingDef> {
        self.graph
            .as_ref()
            .and_then(|g| g.preset_metadata.as_ref())
            .into_iter()
            .flat_map(|m| m.bindings.iter())
            .filter(|b| b.user_added)
    }

    /// Number of user-exposed parameter bindings on this instance.
    pub fn user_param_count(&self) -> usize {
        self.user_added_bindings().count()
    }

    /// Synthesize the in-memory [`UserParamBinding`] view for each
    /// user-added binding. Routing + affine (`scale`/`offset`) come from the
    /// `user_added` [`BindingDef`]; the reshape (range / invert / curve /
    /// `is_angle`) comes from the matching `ParamSpecDef` — the single source
    /// after the per-instance reshape note was deleted. `is_angle` now has a
    /// home on the spec (seeded at expose from the inner `ParamType::Angle`),
    /// so it round-trips instead of being dead-fed `false`.
    ///
    /// Allocates a `Vec`; callers (renderer rebuild, state-sync, panels)
    /// hit this only on the boundary path (binding edit / card build), not
    /// the per-frame hot path, so the allocation is acceptable.
    pub fn user_param_bindings(&self) -> Vec<UserParamBinding> {
        self.user_added_bindings()
            .map(|b| self.synth_user_binding(b))
            .collect()
    }

    /// The REAL binding id (`inst.params` key) for the inner-graph param
    /// `(node_doc_id, param_key)`, if any binding — bundled or user-added —
    /// targets it. BUG-249: scene panel rows are keyed by synthesized
    /// `scene.{doc}.{param}` ids the modulation runtime can never resolve;
    /// every modulation write/read for a scene row must translate through
    /// this to the exposed param the runtime actually evaluates
    /// (`modulation.rs` resolves via `inst.params.get_mut(param_id)`).
    /// Node identity follows the expose command's own convention: match the
    /// binding's `node_id` against the node's stable id, falling back to
    /// the handle-minted id when the stable id is empty (bundled nodes).
    /// Returns `None` when the param isn't exposed (no binding yet).
    /// **Instance-graph only:** an instance that still TRACKS its catalog
    /// preset (`graph: None` — every freshly imported model layer) resolves
    /// nothing here; callers holding the effective def must fall back to
    /// [`binding_id_for_node_param_in`] with it, or bound rows on tracking
    /// instances silently miss (the importer-camera deadness, 2026-07-18).
    pub fn binding_id_for_node_param(&self, node_doc_id: u32, param_key: &str) -> Option<String> {
        binding_id_for_node_param_in(self.graph.as_ref()?, node_doc_id, param_key)
    }

    /// Build one [`UserParamBinding`] from a `user_added` [`BindingDef`]
    /// plus its matching `ParamSpecDef` reshape. Shared by
    /// [`Self::user_param_bindings`] and the single-binding lookups.
    fn synth_user_binding(&self, b: &crate::effect_graph_def::BindingDef) -> UserParamBinding {
        use crate::effect_graph_def::BindingTarget;
        let (node_id, inner_param) = match &b.target {
            BindingTarget::Node { node_id, param } => (node_id.clone(), param.clone()),
            BindingTarget::Composite { outer_name } => {
                (NodeId::default(), outer_name.clone())
            }
        };
        // The full slider surface (range + curve + invert + label) is the
        // manifest entry's live `spec` — so a recalibrated user param's range
        // reaches the renderer (PARAM_STORAGE_DESIGN.md D6). scale/offset come
        // from the binding recipe. Identity fallback when no manifest entry.
        let param = self.params.get(&b.id);
        let spec = param.map(|p| &p.spec);
        UserParamBinding {
            id: b.id.clone(),
            label: spec.map(|s| s.name.clone()).unwrap_or_else(|| b.label.clone()),
            node_id,
            legacy_node_handle: None,
            inner_param,
            min: spec.map(|s| s.min).unwrap_or(0.0),
            max: spec.map(|s| s.max).unwrap_or(1.0),
            default_value: b.default_value,
            convert: b.convert,
            is_angle: spec.map(|s| s.is_angle).unwrap_or(false),
            invert: spec.map(|s| s.invert).unwrap_or(false),
            curve: spec.map(|s| s.curve).unwrap_or_default(),
            scale: b.scale,
            offset: b.offset,
            value_labels: spec.map(|s| s.value_labels.clone()).unwrap_or_default(),
            section: spec.and_then(|s| s.section.clone()),
        }
    }

    /// Position of a user binding by stable id within the user-added tail, or
    /// `None` if not found. Index is relative to the user tail. (For the
    /// manifest entry, use [`crate::params::ParamManifest::get`] by id.)
    pub fn user_binding_index(&self, id: &str) -> Option<usize> {
        self.user_added_bindings().position(|b| b.id == id)
    }

    // `static_param_count`, `param_id_to_value_index`, and `resolve_param` are
    // DELETED (PARAM_STORAGE_DESIGN.md D3): there is no positional slot to
    // resolve an id to, and no static/user split with addressing meaning. A
    // consumer that needs a param's value/range/whole-number data reads
    // `self.params.get(id)` and takes it off the entry
    // (`.value`, `.spec.min`/`.spec.max`, `.whole_numbers()`).

    /// Append a user-exposed binding and reserve its `param_values`
    /// (and `base_param_values`, if present) slot at the tail.
    ///
    /// New slot defaults come from the binding's `default_value`.
    /// Bumps `user_param_bindings_version` so the renderer's
    /// per-FX scratch rehydrates on the next frame.
    ///
    /// Calls [`Self::align_to_definition`] first so the static
    /// prefix matches the registry's `param_count` before the new
    /// user-tail slot is pushed. Without this, an effect freshly
    /// constructed via [`Self::new`] (with empty `param_values`)
    /// would land its first user-binding value at index 0 — wrong
    /// if the registry says `n_static > 0`. Align is cheap
    /// (single Vec resize); user-binding mutations are rare (one
    /// per checkbox click), so the cost is negligible.
    ///
    /// Caller is responsible for ensuring `binding.id` is unique
    /// within this instance — `generate_user_param_id` (in
    /// `manifold-editing`) provides the canonical collision-free
    /// shape.
    pub fn append_user_binding(&mut self, binding: UserParamBinding) {
        use crate::effect_graph_def::{
            BindingDef, BindingTarget, EffectGraphDef, ParamSpecDef, PresetMetadata,
        };
        let whole_numbers = matches!(
            binding.convert,
            ParamConvert::IntRound | ParamConvert::EnumRound | ParamConvert::Trigger
        );
        // The param descriptor: the manifest holds the live copy (the runtime
        // authority), and `meta.params` keeps a consistent shadow so the graph
        // def stays uniform with a bundled preset JSON.
        let spec = ParamSpecDef {
            id: binding.id.clone(),
            name: binding.label.clone(),
            min: binding.min,
            max: binding.max,
            default_value: binding.default_value,
            whole_numbers,
            is_toggle: matches!(binding.convert, ParamConvert::BoolThreshold),
            is_trigger: matches!(binding.convert, ParamConvert::Trigger),
            value_labels: binding.value_labels.clone(),
            format_string: None,
            osc_suffix: String::new(),
            curve: binding.curve,
            invert: binding.invert,
            // Captured from the inner param's `ParamType::Angle` at expose time
            // (rides `UserParamBinding.is_angle`). The spec is now the single
            // home for the flag, so the card reads it straight off the manifest.
            is_angle: binding.is_angle,
            // A user-exposed inner-graph param is never the trigger-gate card
            // (that's the preset-authored `clip_trigger` outer card only).
            is_trigger_gate: false,
            wraps: false,
            // Section seeding from the innermost enclosing group's display
            // name (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2 D5) — resolved
            // by the expose command (`mirror_effect_side`) and carried on
            // the `UserParamBinding` this fn receives.
            section: binding.section.clone(),
        };

        // The per-instance graph is the single binding-storage list.
        // The live expose command lifts the canonical graph before this
        // runs; for graph-less callers (storage unit tests) we synthesize
        // a metadata-only graph so the binding still has a home.
        let graph = self.graph.get_or_insert_with(|| EffectGraphDef {
            version: 0,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: Vec::new(),
            wires: Vec::new(),
        });
        let meta = graph.preset_metadata.get_or_insert_with(|| PresetMetadata {
            id: PresetTypeId::new(""),
            display_name: String::new(),
            category: String::new(),
            osc_prefix: String::new(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            params: Vec::new(),
            bindings: Vec::new(),
            skip_mode: Default::default(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        });
        meta.params.push(spec.clone());
        meta.bindings.push(BindingDef {
            id: binding.id.clone(),
            label: binding.label.clone(),
            default_value: binding.default_value,
            target: BindingTarget::Node {
                node_id: binding.node_id.clone(),
                param: binding.inner_param.clone(),
            },
            convert: binding.convert,
            user_added: true,
            scale: binding.scale,
            offset: binding.offset,
        });

        // The manifest entry (id as identity, order = card order). `push`
        // bumps topology (D8). base + value both seed from the spec default.
        self.params.push(crate::params::Param::user_added(spec));
    }

    /// Remove a user-exposed binding by id and drop its `param_values`
    /// (and `base_param_values`) slot. Returns the removed binding view.
    ///
    /// Pulls the `user_added` `BindingDef` + its `ParamSpecDef` from the
    /// per-instance graph's `preset_metadata` (the single storage list)
    /// and its reshape note. Restores undo-state via the returned
    /// `UserParamBinding` plus the value caller saved before the call
    /// (use [`Self::get_param`] / [`Self::get_base_param`] at the slot
    /// returned by [`Self::param_id_to_value_index`]).
    pub fn remove_user_binding_by_id(&mut self, id: &str) -> Option<UserParamBinding> {
        let j = self.user_binding_index(id)?;

        // Synthesize the removed view BEFORE mutating the graph (it reads
        // the binding + the manifest spec).
        let removed = {
            let b = self.user_added_bindings().nth(j)?;
            self.synth_user_binding(b)
        };

        // Pull the binding + shadow spec from the graph metadata.
        if let Some(meta) = self
            .graph
            .as_mut()
            .and_then(|g| g.preset_metadata.as_mut())
        {
            if let Some(bi) = meta.bindings.iter().position(|b| b.user_added && b.id == id) {
                meta.bindings.remove(bi);
            }
            if let Some(si) = meta.params.iter().position(|p| p.id == id) {
                meta.params.remove(si);
            }
        }

        // Drop the manifest entry (id as identity; bumps topology).
        self.params.remove(id);
        Some(removed)
    }

    /// Re-insert a previously-removed user binding at its original
    /// user-tail `position`, restoring the graph metadata (binding +
    /// spec), the reshape note (if the binding carried one), and the
    /// `param_values` (+ `base_param_values`) slot. Undo counterpart to
    /// [`Self::remove_user_binding_by_id`]; keeps the user-tail order so
    /// sibling user bindings keep their slot indices.
    pub fn restore_user_binding_at(
        &mut self,
        binding: UserParamBinding,
        position: usize,
        param: crate::params::Param,
    ) {
        use crate::effect_graph_def::{
            BindingDef, BindingTarget, EffectGraphDef, ParamSpecDef, PresetMetadata,
        };

        let graph = self.graph.get_or_insert_with(|| EffectGraphDef {
            version: 0,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: Vec::new(),
            wires: Vec::new(),
        });
        let meta = graph.preset_metadata.get_or_insert_with(|| PresetMetadata {
            id: PresetTypeId::new(""),
            display_name: String::new(),
            category: String::new(),
            osc_prefix: String::new(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            params: Vec::new(),
            bindings: Vec::new(),
            skip_mode: Default::default(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        });

        // Absolute binding index of the `position`-th user-added entry
        // (or end). Static (bundled) bindings sit before / among them in
        // declaration order; we count only the user-added ones.
        let abs_binding_idx = meta
            .bindings
            .iter()
            .enumerate()
            .filter(|(_, b)| b.user_added)
            .nth(position)
            .map(|(i, _)| i)
            .unwrap_or(meta.bindings.len());
        let whole_numbers = matches!(
            binding.convert,
            ParamConvert::IntRound | ParamConvert::EnumRound | ParamConvert::Trigger
        );
        meta.bindings.insert(
            abs_binding_idx,
            BindingDef {
                id: binding.id.clone(),
                label: binding.label.clone(),
                default_value: binding.default_value,
                target: BindingTarget::Node {
                    node_id: binding.node_id.clone(),
                    param: binding.inner_param.clone(),
                },
                convert: binding.convert,
                user_added: true,
                scale: binding.scale,
                offset: binding.offset,
            },
        );
        // Spec list: append (its absolute position isn't load-bearing for
        // effects — the registry drives the static prefix; the user tail
        // is keyed by id).
        meta.params.push(ParamSpecDef {
            id: binding.id.clone(),
            name: binding.label.clone(),
            min: binding.min,
            max: binding.max,
            default_value: binding.default_value,
            whole_numbers,
            is_toggle: matches!(binding.convert, ParamConvert::BoolThreshold),
            is_trigger: matches!(binding.convert, ParamConvert::Trigger),
            value_labels: binding.value_labels.clone(),
            format_string: None,
            osc_suffix: String::new(),
            curve: binding.curve,
            invert: binding.invert,
            // Position-aware reinstate (undo of an unexpose): preserve the
            // angle flag off the captured binding, same as `append_user_binding`.
            is_angle: binding.is_angle,
            is_trigger_gate: false,
            wraps: false,
            // Same carry-through as `append_user_binding` — this shadow entry
            // is inert anyway (D12 re-derives `meta.params` from the live
            // manifest at serialize time), but keeping it consistent avoids a
            // misleading `None` sitting next to a real section value on the
            // manifest entry `self.params.insert_at` restores below.
            section: binding.section.clone(),
        });

        // Re-insert the manifest entry at its original display position among
        // the user tail: the bundled prefix (unchanged by a user-param
        // removal) plus `position`. `insert_at` clamps + bumps topology (D10).
        let bundled = self
            .params
            .iter()
            .filter(|p| matches!(p.origin, crate::params::ParamOrigin::Bundled))
            .count();
        self.params.insert_at(bundled + position, param);
    }

    // `align_to_definition` is DELETED (PARAM_STORAGE_DESIGN.md D3). It existed
    // to resize the positional `param_values` array to the registry/graph param
    // count after a load or a binding edit — there is no positional array to
    // resize now. The manifest is coherent by construction: it is seeded whole
    // at instantiation/load (`build_param_manifest`) and mutated by
    // `push`/`remove`/`insert_at`, so there is never a length to reconcile.

    /// Snapshot this instance's current base (pre-modulation) param values into
    /// `def`'s preset metadata as the new defaults, so the def becomes a frozen
    /// copy of the configured card rather than a stock template. This is what
    /// makes Make Unique / Export carry the look you set: `param_values` is the
    /// live instrument and stays on the instance, but the def now defaults to
    /// the same values, so a later add/import/load reproduces them through the
    /// normal defaults path. Matched by param id; a metadata param the instance
    /// doesn't expose keeps its existing default. No-op without metadata.
    pub fn snapshot_values_into_def(&self, def: &mut EffectGraphDef) {
        let Some(meta) = def.preset_metadata.as_mut() else {
            return;
        };
        for p in meta.params.iter_mut() {
            if self.params.get(&p.id).is_some() {
                p.default_value = self.get_base_param(&p.id);
            }
        }
        for b in meta.bindings.iter_mut() {
            if self.params.get(&b.id).is_some() {
                b.default_value = self.get_base_param(&b.id);
            }
        }
    }

    /// Replace `param_values` with fresh exposed slots seeded from `def`'s
    /// preset metadata defaults (in declaration order — the same layout the
    /// registry produces). Used when retargeting to an *imported* preset, whose
    /// param structure differs from the instance's prior one: the old positional
    /// `param_values` no longer line up with the new bindings, so reusing them
    /// feeds garbage (or zero) into the graph. Re-seeding from the def both
    /// applies the imported preset's saved values and restores correct
    /// alignment. No-op without metadata.
    pub fn reseed_param_values_from_def(&mut self, def: &EffectGraphDef) {
        if let Some(meta) = def.preset_metadata.as_ref() {
            let entries = meta
                .params
                .iter()
                .map(|p| {
                    let user = meta.bindings.iter().any(|b| b.user_added && b.id == p.id);
                    if user {
                        crate::params::Param::user_added(p.clone())
                    } else {
                        crate::params::Param::bundled(p.clone())
                    }
                })
                .collect();
            self.params = crate::params::ParamManifest::from_params(entries);
        }
    }

    /// Remove every exposed card param bound to `node_id` — its binding, param
    /// spec, value slot, and any drivers / Ableton mappings / envelopes that
    /// referenced it — and return a capture for [`Self::restore_exposures`].
    /// Empty (and a no-op) when nothing was bound to the node. Used when a graph
    /// edit removes the node a slider drove, so the slider doesn't linger as a
    /// dead control. Operates on the per-instance `graph.preset_metadata`, so
    /// the caller must have lifted the graph first (the graph editor always has).
    pub fn remove_exposures_for_node(&mut self, node_id: &NodeId) -> Vec<RemovedExposure> {
        use crate::effect_graph_def::BindingTarget;
        let Some(meta) = self.graph.as_ref().and_then(|g| g.preset_metadata.as_ref()) else {
            return Vec::new();
        };
        // Capture phase (immutable): everything we'll remove, with positions.
        let mut captured: Vec<RemovedExposure> = Vec::new();
        for (bi, b) in meta.bindings.iter().enumerate() {
            let BindingTarget::Node { node_id: nid, .. } = &b.target else {
                continue;
            };
            if nid != node_id {
                continue;
            }
            let id = b.id.as_str();
            let param_position = self.params.index_of(id);
            let param = self.params.get(id).cloned();
            let drivers = self
                .drivers
                .iter()
                .flatten()
                .filter(|d| d.param_id == id)
                .cloned()
                .collect();
            let ableton_mappings = self
                .ableton_mappings
                .iter()
                .flatten()
                .filter(|m| m.param_id == id)
                .cloned()
                .collect();
            let envelopes = self
                .envelopes
                .iter()
                .flatten()
                .filter(|e| e.param_id == id)
                .cloned()
                .collect();
            let audio_mods = self
                .audio_mods
                .iter()
                .flatten()
                .filter(|a| a.param_id == id)
                .cloned()
                .collect();
            captured.push(RemovedExposure {
                param_position,
                binding_index: bi,
                param,
                binding: b.clone(),
                drivers,
                ableton_mappings,
                envelopes,
                audio_mods,
            });
        }
        if captured.is_empty() {
            return captured;
        }
        let ids: std::collections::HashSet<&str> =
            captured.iter().map(|c| c.binding.id.as_str()).collect();
        // Remove the descriptor shadow (`meta.params`) + the bindings, and the
        // manifest entries — all keyed by id, no positional indices.
        if let Some(meta) = self.graph.as_mut().and_then(|g| g.preset_metadata.as_mut()) {
            meta.params.retain(|p| !ids.contains(p.id.as_str()));
            let mut bidx: Vec<usize> = captured.iter().map(|c| c.binding_index).collect();
            bidx.sort_unstable_by(|a, b| b.cmp(a));
            for i in bidx {
                if i < meta.bindings.len() {
                    meta.bindings.remove(i);
                }
            }
        }
        for &id in &ids {
            self.params.remove(id);
        }
        prune_automation_by_ids(&mut self.drivers, &ids, |d| &*d.param_id);
        prune_automation_by_ids(&mut self.ableton_mappings, &ids, |m| &*m.param_id);
        prune_automation_by_ids(&mut self.envelopes, &ids, |e| &*e.param_id);
        prune_automation_by_ids(&mut self.audio_mods, &ids, |a| &*a.param_id);
        captured
    }

    /// Re-insert exposures removed by [`Self::remove_exposures_for_node`] at
    /// their original positions, restoring bindings, param specs, value slots,
    /// and automation. The undo half — exact inverse of the removal.
    pub fn restore_exposures(&mut self, removed: Vec<RemovedExposure>) {
        if removed.is_empty() {
            return;
        }
        // Insert in ascending original-index order so each lands where it was.
        if let Some(meta) = self.graph.as_mut().and_then(|g| g.preset_metadata.as_mut()) {
            // Restore the descriptor shadow from each removed entry's spec.
            for r in &removed {
                if let Some(p) = &r.param
                    && !meta.params.iter().any(|s| s.id == p.spec.id)
                {
                    meta.params.push(p.spec.clone());
                }
            }
            let mut binds: Vec<(usize, crate::effect_graph_def::BindingDef)> = removed
                .iter()
                .map(|r| (r.binding_index, r.binding.clone()))
                .collect();
            binds.sort_by_key(|(i, _)| *i);
            for (i, b) in binds {
                let i = i.min(meta.bindings.len());
                meta.bindings.insert(i, b);
            }
        }
        // Re-insert manifest entries at their captured display positions,
        // ascending so each lands where it was (D10). `insert_at` clamps.
        let mut params: Vec<(usize, crate::params::Param)> = removed
            .iter()
            .filter_map(|r| Some((r.param_position?, r.param.clone()?)))
            .collect();
        params.sort_by_key(|(i, _)| *i);
        for (i, param) in params {
            self.params.insert_at(i, param);
        }
        for r in &removed {
            if !r.drivers.is_empty() {
                self.drivers
                    .get_or_insert_with(Vec::new)
                    .extend(r.drivers.iter().cloned());
            }
            if !r.ableton_mappings.is_empty() {
                self.ableton_mappings
                    .get_or_insert_with(Vec::new)
                    .extend(r.ableton_mappings.iter().cloned());
            }
            if !r.envelopes.is_empty() {
                self.envelopes
                    .get_or_insert_with(Vec::new)
                    .extend(r.envelopes.iter().cloned());
            }
            if !r.audio_mods.is_empty() {
                self.audio_mods
                    .get_or_insert_with(Vec::new)
                    .extend(r.audio_mods.iter().cloned());
            }
        }
    }

    /// Drop any driver / Ableton mapping / envelope whose `param_id` no longer
    /// resolves to a live param on this instance, returning the removed rows for
    /// undo. The integrity sweep for structural edits that wipe params without
    /// going through the precise per-param prune — e.g. a Revert that clears the
    /// graph (and with it the user-added bindings automation was hung on). Rows
    /// are keyed by id and order-independent, so the restore just re-appends.
    pub fn prune_orphaned_automation(&mut self) -> RemovedAutomation {
        let mut orphans: std::collections::HashSet<String> = std::collections::HashSet::new();
        for d in self.drivers.iter().flatten() {
            if self.params.get(&d.param_id).is_none() {
                orphans.insert(d.param_id.to_string());
            }
        }
        for m in self.ableton_mappings.iter().flatten() {
            if self.params.get(&m.param_id).is_none() {
                orphans.insert(m.param_id.to_string());
            }
        }
        for e in self.envelopes.iter().flatten() {
            if self.params.get(&e.param_id).is_none() {
                orphans.insert(e.param_id.to_string());
            }
        }
        for a in self.audio_mods.iter().flatten() {
            if self.params.get(&a.param_id).is_none() {
                orphans.insert(a.param_id.to_string());
            }
        }
        for l in self.automation_lanes.iter().flatten() {
            if self.params.get(&l.param_id).is_none() {
                orphans.insert(l.param_id.to_string());
            }
        }
        if orphans.is_empty() {
            return RemovedAutomation::default();
        }
        RemovedAutomation {
            drivers: take_automation_by_ids(&mut self.drivers, &orphans, |d| &d.param_id),
            ableton_mappings: take_automation_by_ids(&mut self.ableton_mappings, &orphans, |m| {
                &m.param_id
            }),
            envelopes: take_automation_by_ids(&mut self.envelopes, &orphans, |e| &e.param_id),
            audio_mods: take_automation_by_ids(&mut self.audio_mods, &orphans, |a| &a.param_id),
            automation_lanes: take_automation_by_ids(&mut self.automation_lanes, &orphans, |l| {
                &l.param_id
            }),
        }
    }

    /// Re-attach automation removed by [`Self::prune_orphaned_automation`]. The
    /// undo half — append-restore, since automation rows carry no positional
    /// meaning (they're keyed by `param_id`).
    pub fn restore_automation(&mut self, removed: RemovedAutomation) {
        if !removed.drivers.is_empty() {
            self.drivers.get_or_insert_with(Vec::new).extend(removed.drivers);
        }
        if !removed.ableton_mappings.is_empty() {
            self.ableton_mappings
                .get_or_insert_with(Vec::new)
                .extend(removed.ableton_mappings);
        }
        if !removed.envelopes.is_empty() {
            self.envelopes
                .get_or_insert_with(Vec::new)
                .extend(removed.envelopes);
        }
        if !removed.audio_mods.is_empty() {
            self.audio_mods
                .get_or_insert_with(Vec::new)
                .extend(removed.audio_mods);
        }
        if !removed.automation_lanes.is_empty() {
            self.automation_lanes
                .get_or_insert_with(Vec::new)
                .extend(removed.automation_lanes);
        }
    }

    /// Get the drivers list, creating it if None.
    pub fn drivers_mut(&mut self) -> &mut Vec<ParameterDriver> {
        if self.drivers.is_none() {
            self.drivers = Some(Vec::new());
        }
        self.drivers.as_mut().unwrap()
    }

    /// Get the audio-mods list, creating it if None.
    pub fn audio_mods_mut(&mut self) -> &mut Vec<crate::audio_mod::ParameterAudioMod> {
        if self.audio_mods.is_none() {
            self.audio_mods = Some(Vec::new());
        }
        self.audio_mods.as_mut().unwrap()
    }

    pub fn find_audio_mod(&self, param_id: &str) -> Option<&crate::audio_mod::ParameterAudioMod> {
        self.audio_mods
            .as_ref()
            .and_then(|v| v.iter().find(|a| a.param_id == param_id))
    }

    pub fn has_audio_mods(&self) -> bool {
        self.audio_mods.as_ref().is_some_and(|v| !v.is_empty())
    }

    /// Whether this instance's clip-launch edge should count toward its own
    /// Trigger response (§9 U3, supersedes the deleted `AudioTriggerMod::
    /// clip_edge_enabled`). Finds an ENABLED `audio_mods` entry targeting a
    /// trigger-gate param (`spec.is_trigger_gate`); none found means no audio
    /// config exists for this gate, so the clip edge is unconditionally on
    /// (the pre-§8 behavior, unchanged). A DISABLED mod is semantically
    /// absent — the same "disabled means absent" rule the old per-instance
    /// config used to own (the bug that shipped a disarmed Transient config
    /// silently killing clip triggers on reload), now expressed with zero
    /// trigger-specific storage: a fire-mode mod is just a normal audio mod.
    pub fn clip_edge_enabled(&self) -> bool {
        let Some(mods) = self.audio_mods.as_ref() else {
            return true;
        };
        let gate_mod = mods.iter().find(|m| {
            m.enabled
                && self
                    .params
                    .get(m.param_id.as_ref())
                    .is_some_and(|p| p.spec.is_trigger_gate)
        });
        match gate_mod {
            None => true,
            Some(m) => m
                .trigger_mode
                .unwrap_or(crate::audio_trigger::TriggerFireMode::Both)
                .wants_clip_edge(),
        }
    }
}


/// Implement ParamSource for PresetInstance.
/// Port of Unity PresetInstance : IParamSource.
/// Generator-kind methods, ported from the former `PresetInstance`. They
/// read the generator registry via `self.effect_type` (which holds the preset
/// type for both kinds). Only ever called on generator-kind instances.
impl PresetInstance {
    // `migrate_to_registry_length` is DELETED (PARAM_STORAGE_DESIGN.md D3):
    // there is no lazy positional tail to pad. A generator's manifest is seeded
    // whole from the template at instantiation (`init_defaults_for_type`) and at
    // load (`build_param_manifest`).

    /// Generator-only home.
    pub fn find_envelope(&self, param_id: &str) -> Option<&ParamEnvelope> {
        self.envelopes.as_ref()?.iter().find(|e| e.param_id == param_id)
    }

    /// No-alloc check.
    pub fn has_envelopes(&self) -> bool {
        self.envelopes.as_ref().is_some_and(|e| !e.is_empty())
    }

    /// The instance's envelope list, auto-created on first access.
    /// Envelope-home unification: effect and generator envelopes both live
    /// here, keyed by `param_id` (no `target_effect_type` — the instance the
    /// envelope sits on is the target).
    pub fn envelopes_mut(&mut self) -> &mut Vec<ParamEnvelope> {
        if self.envelopes.is_none() {
            self.envelopes = Some(Vec::new());
        }
        self.envelopes.as_mut().unwrap()
    }

    /// The instance's automation-lane list, auto-created on first access.
    /// Sibling of [`Self::drivers_mut`] / [`Self::envelopes_mut`] /
    /// [`Self::audio_mods_mut`] — same per-param-id-row pattern. See
    /// `docs/AUTOMATION_LANES_DESIGN.md` §2.
    pub fn automation_lanes_mut(&mut self) -> &mut Vec<AutomationLane> {
        if self.automation_lanes.is_none() {
            self.automation_lanes = Some(Vec::new());
        }
        self.automation_lanes.as_mut().unwrap()
    }

    /// No-alloc check, mirroring [`Self::has_envelopes`] / [`Self::has_audio_mods`].
    pub fn has_automation_lanes(&self) -> bool {
        self.automation_lanes.as_ref().is_some_and(|v| !v.is_empty())
    }

    /// Reset effective values to base — ONLY for params with active drivers or
    /// envelopes (generator semantics).
    pub fn reset_effectives(&mut self) {
        if self.params.is_empty() {
            return;
        }
        self.ensure_base_values();
        // Collect the ids of params with an active driver or envelope first
        // (disjoint from the `self.params` mutation below).
        let mut ids: Vec<String> = Vec::new();
        for d in self.drivers.iter().flatten() {
            if d.enabled {
                ids.push(d.param_id.to_string());
            }
        }
        for e in self.envelopes.iter().flatten() {
            if e.enabled {
                ids.push(e.param_id.to_string());
            }
        }
        for id in ids {
            if let Some(p) = self.params.get_mut(&id) {
                p.value = p.base;
            }
        }
    }

    /// Change generator type, re-initializing to the new type's defaults and
    /// clearing drivers/envelopes.
    pub fn change_type(&mut self, new_type: PresetTypeId) {
        if new_type == PresetTypeId::NONE {
            return;
        }
        self.effect_type = new_type.clone();
        self.init_defaults_for_type(new_type);
        if let Some(drivers) = &mut self.drivers {
            drivers.clear();
        }
        if let Some(envelopes) = &mut self.envelopes {
            envelopes.clear();
        }
    }

    /// Initialize base + effective arrays from the generator registry defaults,
    /// setting the type.
    pub fn init_defaults_for_type(&mut self, gen_type: PresetTypeId) {
        if let Some(def) = crate::preset_definition_registry::try_get(&gen_type) {
            self.effect_type = gen_type;
            // Seed the manifest whole from the registry template; each bundled
            // Param seeds base = value = default.
            let entries = def
                .param_defs
                .iter()
                .map(|pd| crate::params::Param::bundled(pd.spec.clone()))
                .collect();
            self.params = crate::params::ParamManifest::from_params(entries);
            self.base_tracked = true;
        }
    }

    /// Legacy init_defaults (no parameter). Uses the current generator type.
    pub fn init_defaults(&mut self) {
        let gt = self.effect_type.clone();
        self.init_defaults_for_type(gt);
    }

    /// Snapshot current base param values (for undo). When base is tracked,
    /// snapshots each slot's `base`; otherwise the effective value (the former
    /// `base_param_values: None` fall-through).
    pub fn snapshot_params(&self) -> Vec<f32> {
        if self.base_tracked {
            self.params.iter().map(|p| p.base).collect()
        } else {
            self.params.iter().map(|p| p.value).collect()
        }
    }

    /// Snapshot current drivers (for undo).
    pub fn snapshot_drivers(&self) -> Option<Vec<ParameterDriver>> {
        self.drivers
            .as_ref()
            .and_then(|d| if d.is_empty() { None } else { Some(d.clone()) })
    }

    /// Snapshot current generator envelopes (for undo).
    pub fn snapshot_envelopes(&self) -> Option<Vec<ParamEnvelope>> {
        self.envelopes
            .as_ref()
            .and_then(|e| if e.is_empty() { None } else { Some(e.clone()) })
    }

    /// Restore from a snapshot (used by undo).
    pub fn restore(
        &mut self,
        gen_type: PresetTypeId,
        params: Vec<f32>,
        drivers: Option<Vec<ParameterDriver>>,
        envelopes: Option<Vec<ParamEnvelope>>,
    ) {
        // Re-seed the manifest descriptors from the registry template, then
        // honor the snapshot's arity and overlay the snapshotted base values in
        // manifest (card) order. The undo snapshot is the authoritative param
        // set — a curated instance can carry fewer params than the current
        // registry template — so trim the reseeded template to the snapshot
        // length; without this, restoring a shorter snapshot leaves the
        // registry-default tail entries appended past the snapshot
        // (PARAM_STORAGE_DESIGN.md D-storage).
        self.init_defaults_for_type(gen_type);
        self.params.truncate(params.len());
        for (p, v) in self.params.iter_mut().zip(params.iter()) {
            p.base = *v;
            p.value = *v;
        }
        self.base_tracked = true;
        if let Some(d) = &mut self.drivers {
            d.clear();
        }
        if let Some(snapshot_drivers) = drivers {
            self.drivers_mut().extend(snapshot_drivers);
        }
        if let Some(e) = &mut self.envelopes {
            e.clear();
        }
        if let Some(snapshot_envelopes) = envelopes {
            self.envelopes_mut().extend(snapshot_envelopes);
        }
    }
}

impl ParamSource for PresetInstance {
    fn display_name(&self) -> &str {
        // The registry hands back an owned `Arc<PresetDef>` (hot-reloadable
        // since step 10), so the name is interned to `&'static str` to
        // satisfy the trait's borrowed return without rippling `String`
        // through every `ParamSource` caller. See
        // `preset_definition_registry::intern_display_name`.
        match crate::preset_definition_registry::try_get(&self.effect_type) {
            Some(def) => crate::preset_definition_registry::intern_display_name(&def.display_name),
            // Unknown type: kind-appropriate fallback label.
            None if self.is_generator() => "Generator",
            None => "?",
        }
    }

    fn param_count(&self) -> usize {
        self.params.len()
    }

    fn get_param_def(&self, id: &str) -> crate::effect_graph_def::ParamSpecDef {
        // The manifest entry's `spec` is the descriptor for every param
        // (bundled + user-added, calibrated in place). Unknown id → default.
        self.params
            .get(id)
            .map(|p| p.spec.clone())
            .unwrap_or_default()
    }

    fn get_param(&self, id: &str) -> f32 {
        PresetInstance::get_param(self, id)
    }

    fn set_param(&mut self, id: &str, value: f32) {
        PresetInstance::set_param(self, id, value);
    }

    fn get_base_param(&self, id: &str) -> f32 {
        PresetInstance::get_base_param(self, id)
    }

    fn set_base_param(&mut self, id: &str, value: f32) {
        PresetInstance::set_base_param(self, id, value);
    }

    fn find_driver(&self, param_id: &str) -> Option<&ParameterDriver> {
        PresetInstance::find_driver(self, param_id)
    }

    fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>> {
        PresetInstance::get_drivers_list(self)
    }

    fn create_driver(&mut self, param_id: ParamId) -> &ParameterDriver {
        PresetInstance::create_driver(self, param_id)
    }

    fn remove_driver(&mut self, param_id: &str) {
        PresetInstance::remove_driver(self, param_id);
    }
}
