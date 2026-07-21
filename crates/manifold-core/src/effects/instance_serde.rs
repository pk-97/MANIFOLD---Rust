//! Custom wire-format serialization for `PresetInstance` and its param
//! manifest: wire structs, `build_param_manifest` + helpers, the hand-written
//! `Serialize`/`Deserialize` impls, `migrate_legacy_audio_trigger`, and the
//! generator-instance deserialize entry points. Extracted from effects.rs
//! (P2-E, design D4). Serde attributes are byte-identical to the original.

use super::*;

// ─── Wire-format helpers for `params` (V1.4) ───
//
// PARAM_STORAGE_DESIGN.md D4/D12: the typed loader understands ONLY the V1.4
// id-keyed `params` shape, and the manifest is the single authority. The four
// historical positional/keyed value shapes are gone — `manifold-io`'s
// `migrations::param_storage_v14` converts every preset instance to the V1.4
// shape BEFORE typed deserialization runs (V1 JSON + V2 ZIP), so that module is
// the only place positional param knowledge survives.
//
// Save is trivial: iterate the manifest, emit each entry by its own id
// ([`ManifestSer`]). Load is the §4 reconcile: seed bundled + user-added
// descriptors from the template/graph, overlay the file's state + calibration
// by id, append self-describing inline-`spec` entries ([`build_param_manifest`]).
// `meta.params` is READ at load only to reconstruct pre-P2 descriptors; it is
// NOT re-derived on save (a user param's spec rides the wire's inline `spec`,
// D12 §4 step 3; a bundled param's range edit rides the `calibration` block,
// D6). This keeps `meta.params` byte-stable across a round-trip and keeps the
// manifest the sole runtime authority.

/// The per-entry calibration block: the recalibrated range (and curve/invert
/// when non-default) a chevron popover wrote onto a *bundled* param. Present on
/// the wire iff [`crate::params::Param::calibrated`]; a bundled param without
/// it tracks the template (D6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CalibrationWire {
    min: f32,
    max: f32,
    #[serde(default, skip_serializing_if = "curve_is_linear")]
    curve: crate::macro_bank::MacroCurve,
    #[serde(default, skip_serializing_if = "is_false")]
    invert: bool,
}

/// One entry in `PresetInstance.params` — the id is the map key. `base` iff
/// `base_tracked` (D5), `calibration` iff calibrated (D6), `spec` inline iff
/// the param is user-added (D12). `exposed` always serializes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct ParamEntryWire {
    value: f32,
    #[serde(default = "default_true")]
    exposed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    calibration: Option<CalibrationWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spec: Option<crate::effect_graph_def::ParamSpecDef>,
}

impl ParamEntryWire {
    /// Wire entry for a manifest param.
    pub(super) fn from_param(p: &crate::params::Param, base_tracked: bool) -> Self {
        Self {
            value: p.value,
            exposed: p.exposed,
            base: base_tracked.then_some(p.base),
            calibration: p.calibrated.then_some(CalibrationWire {
                min: p.spec.min,
                max: p.spec.max,
                curve: p.spec.curve,
                invert: p.spec.invert,
            }),
            spec: matches!(p.origin, crate::params::ParamOrigin::UserAdded)
                .then(|| p.spec.clone()),
        }
    }

    /// Overlay this file entry onto a manifest param already seeded from the
    /// template. A self-describing inline `spec` (user-added) replaces the
    /// descriptor first; then value/base/exposed; then a `calibration` block
    /// overrides the range (setting `calibrated`). Returns whether the entry
    /// carried a `base` (folds into the instance `base_tracked` bit).
    fn apply_to(&self, p: &mut crate::params::Param) -> bool {
        if let Some(spec) = &self.spec {
            p.spec = spec.clone();
        }
        p.value = self.value;
        p.base = self.base.unwrap_or(self.value);
        p.exposed = self.exposed;
        if let Some(c) = &self.calibration {
            p.spec.min = c.min;
            p.spec.max = c.max;
            p.spec.curve = c.curve;
            p.spec.invert = c.invert;
            p.calibrated = true;
        }
        self.base.is_some()
    }
}

/// Serialize a `PresetInstance`'s `params` — the single V1.4 id-keyed map for
/// BOTH kinds (D12). Emits each manifest entry by its own id in card order; no
/// registry lookup, no positional prefix/tail split.
pub(super) struct ManifestSer<'a> {
    pub(super) manifest: &'a crate::params::ParamManifest,
    pub(super) base_tracked: bool,
}

impl Serialize for ManifestSer<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.manifest.len()))?;
        for p in self.manifest.iter() {
            if p.id().is_empty() {
                continue;
            }
            map.serialize_entry(p.id(), &ParamEntryWire::from_param(p, self.base_tracked))?;
        }
        map.end()
    }
}

/// Serializes the instance's graph override with `preset_metadata.params`
/// rewritten from the live manifest (PARAM_STORAGE_BOUNDARIES_DESIGN.md D12/
/// D4: `meta.params` is derived on save from the manifest, the sole live
/// authority — not a second thing calibration keeps in sync by hand). Every
/// OTHER field on the graph (nodes, wires, `preset_metadata.bindings`,
/// `skip_mode`, ...) serializes unchanged — this wrapper touches only the
/// `params` list's per-entry CONTENT, by id, never its shape: which entries
/// exist is still governed by expose/unexpose (`append_user_binding` /
/// `remove_user_binding_by_id`), not by this derivation.
pub(super) struct GraphWithDerivedParams<'a> {
    pub(super) graph: &'a crate::effect_graph_def::EffectGraphDef,
    pub(super) manifest: &'a crate::params::ParamManifest,
}

impl Serialize for GraphWithDerivedParams<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let Some(meta) = self.graph.preset_metadata.as_ref() else {
            // No metadata to derive onto — serialize the graph as-is.
            return self.graph.serialize(serializer);
        };
        if meta.params.iter().all(|p| self.manifest.get(&p.id).is_none()) {
            // Nothing on this graph resolves against the manifest (e.g. a
            // fused/frozen def with no matching live instance) — nothing to
            // derive; avoid the clone.
            return self.graph.serialize(serializer);
        }
        // Save-time-only cost (not per-frame) — the manifest double-build
        // this mirrors (D1) is priced the same way.
        let mut derived = self.graph.clone();
        if let Some(meta) = derived.preset_metadata.as_mut() {
            for spec in meta.params.iter_mut() {
                if let Some(p) = self.manifest.get(&spec.id) {
                    *spec = p.spec.clone();
                }
            }
        }
        derived.serialize(serializer)
    }
}

/// A minimal `ParamSpecDef` for a `user_added` binding with no matching
/// `meta.params` entry (pre-spec files): range 0..1, linear, integral-ness
/// inferred from the binding's convert.
fn spec_from_binding(
    b: &crate::effect_graph_def::BindingDef,
) -> crate::effect_graph_def::ParamSpecDef {
    let whole_numbers = matches!(
        b.convert,
        ParamConvert::IntRound | ParamConvert::EnumRound | ParamConvert::Trigger
    );
    crate::effect_graph_def::ParamSpecDef {
        id: b.id.clone(),
        name: b.label.clone(),
        min: 0.0,
        max: 1.0,
        default_value: b.default_value,
        whole_numbers,
        is_toggle: matches!(b.convert, ParamConvert::BoolThreshold),
        is_trigger: matches!(b.convert, ParamConvert::Trigger),
        value_labels: Vec::new(),
        format_string: None,
        osc_suffix: String::new(),
        curve: Default::default(),
        invert: false,
        // Pre-spec fallback: a `BindingDef` records no angle-ness, so the flag
        // starts false. A later expose/edit reseeds it with the real value.
        is_angle: false,
        is_trigger_gate: false,
        wraps: false,
        section: None,
    }
}

/// Placeholder descriptor for a wire entry whose preset template is
/// unresolvable at load (see `build_param_manifest`'s keep-don't-drop
/// branch): identity + state preserved, range 0..1 unless the entry carries
/// a calibration block (which `apply_to` overlays afterward).
fn placeholder_spec(
    id: &str,
    entry: &ParamEntryWire,
) -> crate::effect_graph_def::ParamSpecDef {
    crate::effect_graph_def::ParamSpecDef {
        id: id.to_string(),
        name: id.to_string(),
        min: 0.0,
        max: 1.0,
        default_value: entry.value,
        whole_numbers: false,
        is_toggle: false,
        is_trigger: false,
        value_labels: Vec::new(),
        format_string: None,
        osc_suffix: String::new(),
        curve: Default::default(),
        invert: false,
        is_angle: false,
        is_trigger_gate: false,
        wraps: false,
        section: None,
    }
}

/// Template + user-added descriptors a fresh manifest is seeded from at load,
/// in card order. Bundled descriptors: a graph-backed generator's own
/// `meta.params`, else the registry `param_defs`. User-added descriptors: the
/// per-instance graph's `user_added` bindings (spec from `meta.params`, else
/// synthesized). This load-time read of the graph reconstructs the manifest;
/// the manifest is the authority afterward.
fn gather_known_params(
    is_generator: bool,
    effect_type: &PresetTypeId,
    graph: &Option<EffectGraphDef>,
) -> Vec<(crate::effect_graph_def::ParamSpecDef, crate::params::ParamOrigin)> {
    use crate::params::ParamOrigin;
    let meta = graph.as_ref().and_then(|g| g.preset_metadata.as_ref());

    // Generator with a per-instance graph: its `meta.params` is the full
    // ordered descriptor authority; origin is driven by a matching `user_added`
    // binding.
    if is_generator
        && let Some(meta) = meta
        && !meta.params.is_empty()
    {
        return meta
            .params
            .iter()
            .map(|s| {
                let user = meta.bindings.iter().any(|b| b.user_added && b.id == s.id);
                let origin = if user {
                    ParamOrigin::UserAdded
                } else {
                    ParamOrigin::Bundled
                };
                (s.clone(), origin)
            })
            .collect();
    }

    // Effect (or graph-less generator): bundled from the registry, then the
    // user-added tail from the graph's `user_added` bindings.
    let mut out = Vec::new();
    if let Some(def) = crate::preset_definition_registry::try_get(effect_type) {
        for pd in def.param_defs.iter() {
            out.push((pd.spec.clone(), ParamOrigin::Bundled));
        }
    }
    if let Some(meta) = meta {
        for b in meta.bindings.iter().filter(|b| b.user_added) {
            let spec = meta
                .params
                .iter()
                .find(|p| p.id == b.id)
                .cloned()
                .unwrap_or_else(|| spec_from_binding(b));
            out.push((spec, ParamOrigin::UserAdded));
        }
    }
    out
}

/// Whether a descriptor authority resolves for this instance right now: an
/// inline generator graph's own `meta.params`, or a registry template.
/// Shared by [`build_param_manifest`] (decides informed-drop vs
/// keep-don't-drop) and [`PresetInstance::reconcile_manifest`] (decides
/// whether a reconcile pass definitively resolved the instance, so its
/// `pending_wire` stash can be cleared, or whether it should stay parked for
/// a later retry — BUG-036's class).
pub(super) fn template_known_for(
    is_generator: bool,
    effect_type: &PresetTypeId,
    graph: &Option<EffectGraphDef>,
) -> bool {
    (is_generator
        && graph
            .as_ref()
            .and_then(|g| g.preset_metadata.as_ref())
            .is_some_and(|m| !m.params.is_empty()))
        || crate::preset_definition_registry::try_get(effect_type).is_some()
}

/// Build a `PresetInstance`'s manifest from its V1.4 `params` wire map (§4 load
/// reconcile): seed known descriptors, overlay each file entry's state +
/// calibration by id (alias-aware), append self-describing inline-`spec`
/// entries that match nothing, and drop unknown entries with a warning
/// (today's unknown-id policy). Returns the manifest + the `base_tracked` bit.
pub(super) fn build_param_manifest(
    is_generator: bool,
    effect_type: &PresetTypeId,
    graph: &Option<EffectGraphDef>,
    wire: Option<std::collections::BTreeMap<String, ParamEntryWire>>,
) -> (crate::params::ParamManifest, bool) {
    use crate::params::{Param, ParamOrigin};
    let mut entries: Vec<Param> = gather_known_params(is_generator, effect_type, graph)
        .into_iter()
        .map(|(spec, origin)| match origin {
            ParamOrigin::Bundled => Param::bundled(spec),
            ParamOrigin::UserAdded => Param::user_added(spec),
        })
        .collect();

    // Alias map (old id → new id; `None` = deprecated, drop) from the graph's
    // per-preset aliases plus the registry's legacy renames.
    let mut alias: ahash::AHashMap<String, Option<String>> = ahash::AHashMap::new();
    if let Some(meta) = graph.as_ref().and_then(|g| g.preset_metadata.as_ref()) {
        for a in &meta.param_aliases {
            alias.insert(a.old.clone(), a.new.clone());
        }
    }
    if let Some(def) = crate::preset_definition_registry::try_get(effect_type) {
        for (old, new) in def.legacy_param_aliases.iter() {
            alias
                .entry((*old).to_string())
                .or_insert_with(|| new.map(str::to_string));
        }
    }

    // Whether a descriptor authority was actually available for this
    // instance: an inline generator graph's `meta.params`, or a registry
    // template. Only an *informed* drop is allowed — when the template is
    // resolvable and says the id is gone, that's a deliberate deprecation
    // (today's unknown-id policy). When NO template resolves (e.g. a
    // project-local import whose def isn't registered at deserialize time),
    // dropping is silent data loss (BUG-036), so the entry is kept on a
    // placeholder spec instead: state (value/base/exposed/calibration) is
    // everything the file stores for a bundled param, and the next load
    // with the template present reconciles it against the real descriptor.
    let template_known = template_known_for(is_generator, effect_type, graph);

    let mut base_tracked = false;
    if let Some(wire) = wire {
        for (raw_id, entry) in wire {
            let id = match alias.get(&raw_id) {
                Some(Some(new_id)) => new_id.clone(),
                Some(None) => continue, // deprecated, no replacement
                None => raw_id,
            };
            if let Some(p) = entries.iter_mut().find(|p| p.id() == id) {
                base_tracked |= entry.apply_to(p);
            } else if let Some(spec) = &entry.spec {
                let mut p = Param::user_added(spec.clone());
                base_tracked |= entry.apply_to(&mut p);
                entries.push(p);
            } else if !template_known {
                eprintln!(
                    "[manifold-core] keeping param {id:?} on {effect_type:?} load with a \
                     placeholder spec (preset template unresolved — project-local preset \
                     not registered yet?)"
                );
                // Bundled origin: the placeholder spec never serializes
                // (only state does), so the real descriptor wins on the
                // next resolvable load. Card position is tail-appended in
                // wire order — acceptable for this recovery path.
                let mut p = Param::bundled(placeholder_spec(&id, &entry));
                base_tracked |= entry.apply_to(&mut p);
                entries.push(p);
            } else {
                eprintln!(
                    "[manifold-core] dropping unknown param id {id:?} on {effect_type:?} load \
                     (no template descriptor, no inline spec)"
                );
            }
        }
    }
    (crate::params::ParamManifest::from_params(entries), base_tracked)
}

// ─── Custom Serialize / Deserialize for PresetInstance ───

impl Serialize for PresetInstance {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        // Generator-kind serializes the legacy `PresetInstance` shape
        // (generatorType + the generator-keyed param maps + envelopes +
        // genParamVersion) so existing generator fixtures stay byte-identical.
        if self.is_generator() {
            return self.serialize_as_generator(serializer);
        }

        // `params` always emits (D5: `base` rides inside each entry now,
        // gated by `base_tracked` — no more separate pre-modulation-base
        // field). Other optional fields use the same `skip_if_none` policy
        // as the previous derive(Serialize) impl.
        let mut field_count = 5; // id, effectType, enabled, collapsed, params
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
        if self.group_id.is_some() {
            field_count += 1;
        }
        if self.graph.is_some() {
            field_count += 1;
        }
        if self.legacy_param0.is_some() {
            field_count += 1;
        }
        if self.legacy_param1.is_some() {
            field_count += 1;
        }
        if self.legacy_param2.is_some() {
            field_count += 1;
        }
        if self.legacy_param3.is_some() {
            field_count += 1;
        }
        if self.relight {
            field_count += 1;
        }
        if !self.relight_params.is_default() {
            field_count += 1;
        }

        let mut s = serializer.serialize_struct("PresetInstance", field_count)?;
        s.serialize_field("id", &self.id)?;
        s.serialize_field("effectType", &self.effect_type)?;
        s.serialize_field("enabled", &self.enabled)?;
        s.serialize_field("collapsed", &self.collapsed)?;
        // The `params` map carries each manifest entry by its own id in card
        // order (bundled + user-added unified); the user-added bindings still
        // ride out inside the `graph` field.
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
        // Envelope-home unification: effect envelopes ride on the instance.
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
        if let Some(g) = &self.group_id {
            s.serialize_field("groupId", g)?;
        }
        // `graph` is skipped when None — same round-trip-invariance
        // policy. `None` means "use the catalog default for this
        // effect type"; only per-instance overrides emit. `params` on the
        // wrapper is derived from the live manifest (D12) — see
        // `GraphWithDerivedParams`.
        if let Some(graph) = &self.graph {
            s.serialize_field(
                "graph",
                &GraphWithDerivedParams { graph, manifest: &self.params },
            )?;
        }
        if let Some(v) = self.legacy_param0 {
            s.serialize_field("param0", &v)?;
        }
        if let Some(v) = self.legacy_param1 {
            s.serialize_field("param1", &v)?;
        }
        if let Some(v) = self.legacy_param2 {
            s.serialize_field("param2", &v)?;
        }
        if let Some(v) = self.legacy_param3 {
            s.serialize_field("param3", &v)?;
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

/// §9 U5 load migration: a legacy `audioTrigger` field (§8 D2's now-deleted
/// `AudioTriggerMod`) converts to a `ParameterAudioMod` on the instance's
/// trigger-gate param — the same param the `clip_trigger` toggle card lives
/// on (`spec.is_trigger_gate`). Runs from BOTH `PresetInstance` Deserialize
/// paths (effect `Raw` and generator `GeneratorInstanceRaw`), which is also
/// the only choke point either V1 JSON or V2 ZIP load ever passes through
/// (`manifold-io`'s loader deserializes the whole `Project` via one
/// `serde_json::from_str`, so there is nothing V2-specific to wire).
///
/// `enabled` and `mode` carry over exactly; `sensitivity` (an input-gain-
/// style fire-threshold knob) approximates onto `AudioModShape.sensitivity`
/// (the closest surviving "how hard is this to trigger" knob) — U5 is
/// explicit that exact-feel fidelity is NOT owed here, since the field
/// existed in roughly one project for one day. No trigger-gate param on this
/// instance (a hand-edited file, or one that predates the flag) drops the
/// config with a warning rather than guessing a target.
fn migrate_legacy_audio_trigger(
    legacy: crate::audio_trigger::LegacyAudioTriggerMod,
    params: &crate::params::ParamManifest,
    audio_mods: &mut Option<Vec<crate::audio_mod::ParameterAudioMod>>,
) {
    let Some(gate_id) = params
        .iter()
        .find(|p| p.spec.is_trigger_gate)
        .map(|p| p.spec.id.clone())
    else {
        log::warn!(
            "[Migration] legacy audioTrigger config found no trigger-gate param on this \
             instance; dropping it (the instance predates the trigger-gate flag or was \
             hand-edited)"
        );
        return;
    };

    let crate::audio_mod::AudioModSource { send_id, feature } = legacy.source;
    let mut m = crate::audio_mod::ParameterAudioMod::new(gate_id.into(), send_id, feature);
    m.enabled = legacy.enabled;
    m.trigger_mode = Some(legacy.mode);
    m.shape.sensitivity = legacy.sensitivity;
    audio_mods.get_or_insert_with(Vec::new).push(m);
}

impl<'de> Deserialize<'de> for PresetInstance {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(default = "generate_effect_id")]
            id: EffectId,
            #[serde(deserialize_with = "crate::preset_type_id::deserialize_effect_type")]
            effect_type: PresetTypeId,
            #[serde(default = "default_true")]
            enabled: bool,
            #[serde(default)]
            collapsed: bool,
            #[serde(default)]
            params: Option<std::collections::BTreeMap<String, ParamEntryWire>>,
            #[serde(default)]
            drivers: Option<Vec<ParameterDriver>>,
            #[serde(default)]
            envelopes: Option<Vec<ParamEnvelope>>,
            #[serde(default)]
            ableton_mappings: Option<Vec<crate::ableton_mapping::AbletonParamMapping>>,
            #[serde(default)]
            audio_mods: Option<Vec<crate::audio_mod::ParameterAudioMod>>,
            /// §9 U5: the deleted `AudioTriggerMod`'s wire shape, kept only so
            /// an old project's `audioTrigger` field migrates onto
            /// `audio_mods` below — see [`migrate_legacy_audio_trigger`].
            #[serde(default, rename = "audioTrigger")]
            legacy_audio_trigger: Option<crate::audio_trigger::LegacyAudioTriggerMod>,
            #[serde(default)]
            automation_lanes: Option<Vec<AutomationLane>>,
            #[serde(default)]
            group_id: Option<EffectGroupId>,
            #[serde(default)]
            graph: Option<EffectGraphDef>,
            #[serde(default, rename = "param0")]
            legacy_param0: Option<f32>,
            #[serde(default, rename = "param1")]
            legacy_param1: Option<f32>,
            #[serde(default, rename = "param2")]
            legacy_param2: Option<f32>,
            #[serde(default, rename = "param3")]
            legacy_param3: Option<f32>,
            #[serde(default)]
            relight: bool,
            #[serde(default)]
            relight_params: RelightParams,
        }

        let raw = Raw::deserialize(deserializer)?;
        // V1.4 §4 reconcile: seed the manifest from the effect's registry
        // template + the graph's `user_added` bindings, then overlay the
        // incoming `params` map (value/exposed/base/calibration by id, inline
        // spec for self-describing user params). Stash a copy of the wire
        // map first (PARAM_STORAGE_BOUNDARIES_DESIGN.md D1) — the loader's
        // `reconcile_param_manifests` re-runs this same build later, against
        // whatever registry state exists once the project's own embedded
        // presets have been installed.
        let pending_wire = raw.params.clone();
        let (params, base_tracked) =
            build_param_manifest(false, &raw.effect_type, &raw.graph, raw.params);

        let mut audio_mods = raw.audio_mods;
        if let Some(legacy) = raw.legacy_audio_trigger {
            migrate_legacy_audio_trigger(legacy, &params, &mut audio_mods);
        }

        Ok(PresetInstance {
            kind: crate::preset_def::PresetKind::Effect,
            id: raw.id,
            effect_type: raw.effect_type,
            enabled: raw.enabled,
            collapsed: raw.collapsed,
            params,
            base_tracked,
            pending_wire,
            drivers: raw.drivers,
            envelopes: raw.envelopes,
            ableton_mappings: raw.ableton_mappings,
            audio_mods,
            automation_lanes: raw.automation_lanes,
            group_id: raw.group_id,
            graph: raw.graph,
            graph_version: 0,
            graph_structure_version: 0,
            legacy_param0: raw.legacy_param0,
            legacy_param1: raw.legacy_param1,
            legacy_param2: raw.legacy_param2,
            legacy_param3: raw.legacy_param3,
            legacy_param_version: None,
            relight: raw.relight,
            relight_params: raw.relight_params,
        })
    }
}

/// Wire shape for a generator-kind instance — the legacy `PresetInstance`
/// JSON. Used by [`deserialize_generator_instance`] (and its Option wrapper) so
/// `Layer.gen_params` decodes into a `PresetInstance { kind: Generator }`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeneratorInstanceRaw {
    #[serde(
        default,
        deserialize_with = "crate::preset_type_id::deserialize_generator_type"
    )]
    generator_type: PresetTypeId,
    #[serde(default)]
    params: Option<std::collections::BTreeMap<String, ParamEntryWire>>,
    #[serde(default)]
    drivers: Option<Vec<ParameterDriver>>,
    #[serde(default)]
    envelopes: Option<Vec<ParamEnvelope>>,
    #[serde(default)]
    ableton_mappings: Option<Vec<crate::ableton_mapping::AbletonParamMapping>>,
    #[serde(default)]
    audio_mods: Option<Vec<crate::audio_mod::ParameterAudioMod>>,
    /// §9 U5: see `Raw::legacy_audio_trigger` on the effect-kind Deserialize
    /// impl above — same migration, generator wire shape.
    #[serde(default, rename = "audioTrigger")]
    legacy_audio_trigger: Option<crate::audio_trigger::LegacyAudioTriggerMod>,
    #[serde(default)]
    automation_lanes: Option<Vec<AutomationLane>>,
    /// The generator's per-instance graph override. Lives on the generator
    /// `PresetInstance` now (graph-home unification) exactly like an effect's
    /// `graph`; older projects carried it on the layer (`generatorGraph`) and
    /// the load migration relocates it here.
    #[serde(default)]
    graph: Option<EffectGraphDef>,
    #[serde(default, rename = "genParamVersion")]
    legacy_param_version: Option<i32>,
    #[serde(default)]
    relight: bool,
    #[serde(default)]
    relight_params: RelightParams,
}

impl GeneratorInstanceRaw {
    fn into_instance(self) -> PresetInstance {
        // V1.4 §4 reconcile: a graph-backed generator's own `meta.params` is
        // the descriptor authority (else the registry); overlay the incoming
        // `params` map by id. Stash a copy of the wire map first (D1) — see
        // the effect-kind `Deserialize` impl above for the same pattern.
        let pending_wire = self.params.clone();
        let (params, base_tracked) =
            build_param_manifest(true, &self.generator_type, &self.graph, self.params);
        let mut audio_mods = self.audio_mods;
        if let Some(legacy) = self.legacy_audio_trigger {
            migrate_legacy_audio_trigger(legacy, &params, &mut audio_mods);
        }
        PresetInstance {
            kind: crate::preset_def::PresetKind::Generator,
            id: generate_effect_id(),
            effect_type: self.generator_type,
            enabled: true,
            collapsed: false,
            params,
            base_tracked,
            pending_wire,
            drivers: self.drivers,
            envelopes: self.envelopes,
            ableton_mappings: self.ableton_mappings,
            audio_mods,
            automation_lanes: self.automation_lanes,
            group_id: None,
            graph: self.graph,
            graph_version: 0,
            graph_structure_version: 0,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
            legacy_param_version: self.legacy_param_version,
            relight: self.relight,
            relight_params: self.relight_params,
        }
    }
}

/// Decode a generator-kind `PresetInstance` from the legacy
/// `PresetInstance` JSON shape.
pub fn deserialize_generator_instance<'de, D>(deserializer: D) -> Result<PresetInstance, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(GeneratorInstanceRaw::deserialize(deserializer)?.into_instance())
}

/// `deserialize_with` for an `Option<PresetInstance>` field that holds a
/// generator (e.g. `Layer.gen_params`): decode the legacy generator JSON shape
/// into a `PresetInstance { kind: Generator }`.
pub fn deserialize_opt_generator_instance<'de, D>(
    deserializer: D,
) -> Result<Option<PresetInstance>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<GeneratorInstanceRaw>::deserialize(deserializer)?.map(|raw| raw.into_instance()))
}
