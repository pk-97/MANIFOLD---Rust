use crate::PresetTypeId;
use crate::effect_graph_def::EffectGraphDef;
use crate::midi::MidiMappingConfig;
use crate::percussion::PercussionImportState;
use crate::preset_def::PresetKind;
use crate::recording::RecordingProvenance;
use crate::settings::ProjectSettings;
use crate::tempo::TempoMap;
use crate::timeline::Timeline;
use crate::types::ClipDurationMode;
use crate::units::Beats;
use crate::video::VideoLibrary;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// A project-scoped preset (a "fork"): a complete, self-contained preset
/// (graph + exposed params + ranges, carried in [`EffectGraphDef`]) that lives
/// inside the project file rather than the global catalog. Created when the
/// user diverges a shared preset (Phase 4 fork ergonomics) and resolvable in
/// the same id namespace as stock/user presets via the catalog overlay. The
/// preset's id and display name live in `def.preset_metadata`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedPreset {
    /// Effect vs generator — directory-derived for disk presets, explicit here.
    pub kind: PresetKind,
    /// The complete preset definition (graph + `preset_metadata`).
    pub def: EffectGraphDef,
}

impl EmbeddedPreset {
    /// The preset's stable id (from its metadata), or `None` if unset.
    pub fn id(&self) -> Option<&crate::PresetTypeId> {
        self.def.preset_metadata.as_ref().map(|m| &m.id)
    }
}

/// Root project aggregate. Contains all project data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    #[serde(default)]
    pub project_name: String,
    #[serde(default = "default_version")]
    pub project_version: String,
    #[serde(default)]
    pub timeline: Timeline,
    #[serde(default)]
    pub video_library: VideoLibrary,
    #[serde(default, rename = "midiConfig")]
    pub midi_config: MidiMappingConfig,
    #[serde(default)]
    pub settings: ProjectSettings,
    #[serde(default)]
    pub tempo_map: TempoMap,
    #[serde(default)]
    pub recording_provenance: RecordingProvenance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percussion_import: Option<PercussionImportState>,
    #[serde(skip)]
    pub last_saved_path: String,
    #[serde(default)]
    pub saved_playhead_time: f32,

    /// Project-scoped presets ("forks") — self-contained preset defs that live
    /// in this project rather than the global catalog. Resolved by id via the
    /// catalog overlay when the project loads. Empty for projects that have
    /// never forked a preset; skipped on serialize when empty so existing
    /// fixtures round-trip byte-identically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedded_presets: Vec<EmbeddedPreset>,

    // ── Legacy top-level fields from V1.0.0 (before percussionImport nesting) ──
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "importedPercussionAudioPath"
    )]
    pub legacy_perc_audio_path: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "importedPercussionAudioStartBeat"
    )]
    pub legacy_perc_audio_start_beat: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "importedPercussionClipPlacements"
    )]
    pub legacy_perc_clip_placements: Option<serde_json::Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "percussionEnergyEnvelope"
    )]
    pub legacy_perc_energy_envelope: Option<Vec<f32>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "importedStemPaths"
    )]
    pub legacy_imported_stem_paths: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "importedPercussionAudioHash"
    )]
    pub legacy_perc_audio_hash: Option<String>,
}

impl Project {
    /// Remove effects with unrecognized types (e.g. removed Unity effects).
    /// Called before on_after_deserialize so they never enter the runtime.
    pub fn strip_unknown_effects(&mut self) {
        use crate::preset_type_id::PresetTypeId;
        let strip = |effects: &mut Vec<crate::effects::PresetInstance>| {
            effects.retain(|fx| *fx.effect_type() != PresetTypeId::UNKNOWN);
        };
        // Master effects
        strip(&mut self.settings.master_effects);
        // Layer effects
        for layer in &mut self.timeline.layers {
            if let Some(ref mut effects) = layer.effects {
                strip(effects);
            }
        }
    }

    /// Post-deserialization initialization. Rebuild caches and run migrations.
    pub fn on_after_deserialize(&mut self) {
        // Rebuild runtime caches
        self.video_library.rebuild_lookup();
        self.midi_config.rebuild_dictionary();
        self.timeline.rebuild_clip_lookup();

        // Validate tempo map data
        self.tempo_map.ensure_valid();
        self.tempo_map
            .ensure_default_at_beat_zero(self.settings.bpm, crate::TempoPointSource::Manual);

        // Sync BPM from tempo map at beat 0
        self.settings.bpm = self
            .tempo_map
            .get_bpm_at_beat(Beats::ZERO, self.settings.bpm);

        // Clamp saved playhead
        self.saved_playhead_time = self.saved_playhead_time.max(0.0);

        // Align all effect params to current definitions
        self.align_all_effect_params();

        // Migrate generator param arrays to current registry length, preserving
        // every existing value. Without this, the first slider interaction on a
        // clip whose generator gained a parameter since save time would wipe
        // every value to defaults.
        self.migrate_all_generator_params();

        // V1.1 → V1.2 migration: every driver/envelope/Ableton mapping
        // deserialized from a legacy `paramIndex: i32` shape needs its
        // `param_id` filled in from the registry. See
        // `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 step 8.
        self.resolve_legacy_param_ids();

        // Slot-value migration. Walks the per-effect
        // `legacy_value_aliases` table and translates pre-migration
        // enum / numeric values (e.g., Mirror.mode 0/1/2 → 6/7/8 after
        // the TouchDesigner unification dropped the `EnumRemap`
        // curation). Idempotent — post-migration values aren't in the
        // alias table, so re-running is a no-op.
        self.migrate_legacy_param_values();

        // Node-id normalization for pre-node-id graph overrides. A node's
        // stable id defaults to its handle; old override defs
        // (PresetInstance.graph, Layer.generator_graph) saved their nodes
        // without ids, so stamp empty ones = handle here. This makes the
        // handle-targeted bindings those same files carry resolve (the
        // serde layer upgrades `handleNode` targets to `node` with
        // node_id == handle to match). Idempotent; only ever fills empties,
        // which post-cutover documents don't have.
        self.normalize_override_node_ids();

        // The remaining backfill — per-instance `UserParamBinding`s whose
        // target lives in a `graph: None` instance — runs at the renderer
        // layer (`migrate_user_param_bindings_to_node_id`), because
        // resolving those handles needs the canonical bundled-preset
        // graph, which is renderer-side. Override-hosted user bindings
        // resolve against the nodes normalized just above.

        // Normalize layer order into tree pre-order (group children contiguous
        // immediately after parent). Also reindexes.
        self.timeline.enforce_tree_order();
    }

    /// Stamp `node_id == handle` on every graph-override node whose id is
    /// empty (a pre-node-id document). Walks `PresetInstance.graph` on
    /// master / layer / clip effects and each layer's `generator_graph`,
    /// recursing into group bodies.
    ///
    /// A node's stable id defaults to its handle — the same convention the
    /// bundled-preset stamp uses — so a handle-targeted binding (which the
    /// serde layer reads as `node_id == handle`) lands on the right node.
    /// Idempotent: non-empty ids and handle-less boundary nodes are left
    /// untouched, so post-cutover documents pass through unchanged.
    fn normalize_override_node_ids(&mut self) {
        use crate::effect_graph_def::{EffectGraphDef, EffectGraphNode};

        fn stamp_nodes(nodes: &mut [EffectGraphNode]) {
            for n in nodes.iter_mut() {
                if n.node_id.is_empty()
                    && let Some(handle) = n.handle.clone()
                {
                    n.node_id = crate::NodeId::new(handle);
                }
                if let Some(group) = n.group.as_mut() {
                    stamp_nodes(&mut group.nodes);
                }
            }
        }
        fn stamp(def: &mut EffectGraphDef) {
            stamp_nodes(&mut def.nodes);
        }

        for fx in &mut self.settings.master_effects {
            if let Some(def) = fx.graph.as_mut() {
                stamp(def);
            }
        }
        for layer in &mut self.timeline.layers {
            if let Some(gen_graph) = layer.gen_params_mut().and_then(|gp| gp.graph.as_mut()) {
                stamp(gen_graph);
            }
            if let Some(effects) = layer.effects.as_mut() {
                for fx in effects.iter_mut() {
                    if let Some(def) = fx.graph.as_mut() {
                        stamp(def);
                    }
                }
            }
            for clip in &mut layer.clips {
                for fx in &mut clip.effects {
                    if let Some(def) = fx.graph.as_mut() {
                        stamp(def);
                    }
                }
            }
        }
    }

    /// Resize all effect param arrays to match their definitions.
    fn align_all_effect_params(&mut self) {
        // Master effects
        for fx in &mut self.settings.master_effects {
            fx.align_to_definition();
        }
        // Layer effects
        for layer in &mut self.timeline.layers {
            if let Some(ref mut effects) = layer.effects {
                for fx in effects.iter_mut() {
                    fx.align_to_definition();
                }
            }
        }
    }

    /// Apply per-effect `legacy_value_aliases` to every effect
    /// instance's `param_values`. Translates pre-migration enum /
    /// numeric values to current ones when loading old projects — the
    /// canonical case is Mirror's `mode` after the TouchDesigner
    /// unification dropped its `ParamConvert::EnumRemap` curation
    /// (legacy `{0,1,2}` → `{6,7,8}`).
    ///
    /// Comparison is integer-coerced: `slot.value.round() as i32`
    /// against each alias's `from`. A match rewrites the slot to
    /// `to as f32`. Non-matching values pass through. Slots referencing
    /// a param id not in the alias table are untouched.
    ///
    /// Walks master + every layer's effects.
    fn migrate_legacy_param_values(&mut self) {
        fn apply_to_effect(fx: &mut crate::effects::PresetInstance) {
            let Some(def) = crate::preset_definition_registry::try_get(fx.effect_type()) else {
                return;
            };
            if def.legacy_value_aliases.is_empty() {
                return;
            }
            for (param_id, value_aliases) in def.legacy_value_aliases {
                let Some(&slot_idx) = def.id_to_index.get(*param_id) else {
                    // Param renamed out from under the alias table —
                    // nothing to migrate. The id-alias resolver should
                    // have caught the rename in a prior pass.
                    continue;
                };
                let Some(slot) = fx.param_values.get_mut(slot_idx) else {
                    continue;
                };
                let coerced = slot.value.round() as i32;
                if let Some(&(_, to)) = value_aliases.iter().find(|(from, _)| *from == coerced) {
                    slot.value = to as f32;
                    // Keep base in sync so a later `reset_param_effectives`
                    // doesn't wipe the migration back. base rides the slot now
                    // (fork #16), so this is the same slot we just wrote.
                    slot.base = to as f32;
                }
            }
        }
        for fx in &mut self.settings.master_effects {
            apply_to_effect(fx);
        }
        for layer in &mut self.timeline.layers {
            if let Some(ref mut effects) = layer.effects {
                for fx in effects.iter_mut() {
                    apply_to_effect(fx);
                }
            }
        }
    }

    /// Migrate every layer's generator param arrays to the current registry
    /// length, preserving existing values and filling new tail entries from
    /// the registry's defaults.
    fn migrate_all_generator_params(&mut self) {
        for layer in &mut self.timeline.layers {
            if let Some(gp) = layer.gen_params_mut() {
                gp.migrate_to_registry_length();
            }
        }
    }

    /// Resolve every driver / envelope / Ableton-mapping / macro-mapping
    /// addressing site that came in via the legacy `paramIndex: i32`
    /// shape. The custom `Deserialize` for each of those types parks the
    /// legacy index in `legacy_param_index`; here we walk every site,
    /// look up the effect/generator's registry definition, and assign
    /// `param_id` from `def.param_defs[idx].id`, walking the
    /// `legacy_param_aliases` table on the way.
    ///
    /// Outcomes (see [`ResolveOutcome`] inside the body):
    ///
    /// - **Resolved** — registry knows the type, addressing translates
    ///   to a current id. Update `param_id`, clear legacy index.
    /// - **NoChange** — registry knows the type, current id is already
    ///   stable. Clear legacy index (nothing to do).
    /// - **Drop** — registry knows the type, but the legacy index is
    ///   out of range, points at an unnamed slot, or aliases through
    ///   to `None` (param dropped). The mapping is permanently
    ///   orphaned. Clear legacy index — there's no recovery possible.
    /// - **RegistryMissing** — registry has no def for this effect or
    ///   generator type. **Preserve** the legacy index so a future load
    ///   on a build that does have the registry can recover. Without
    ///   this preservation, loading on (e.g.) the `manifold-io` test
    ///   harness which doesn't link `manifold-renderer` would silently
    ///   strip every driver's addressing data on the first save.
    fn resolve_legacy_param_ids(&mut self) {
        use crate::effect_registration::resolve_param_alias;

        /// Outcome of a single addressing-site resolution attempt.
        ///
        /// Each variant maps to a distinct policy at the call site:
        /// clear or preserve `legacy_param_index`, with or without
        /// writing a new `param_id`. Centralized so every addressing-
        /// site applies the same policy — the resolver's contract
        /// lives here.
        enum ResolveOutcome {
            /// Param id is current — nothing to write. Clear legacy idx.
            NoChange,
            /// New param id resolved (rename or legacy-index translation).
            /// Write it; clear legacy idx.
            Update(String),
            /// Registry knows the type but the addressing is dead.
            /// Clear legacy idx — no recovery possible.
            Drop,
            /// Registry has no def for this effect/generator type. Don't
            /// touch anything, including the legacy idx, so the
            /// addressing can recover on a future load with a populated
            /// registry.
            RegistryMissing,
        }

        // Apply an outcome to a `(param_id, legacy_param_index)` pair.
        fn apply_outcome(
            outcome: ResolveOutcome,
            param_id: &mut crate::effects::ParamId,
            legacy_param_index: &mut Option<i32>,
        ) {
            match outcome {
                ResolveOutcome::NoChange | ResolveOutcome::Drop => {
                    *legacy_param_index = None;
                }
                ResolveOutcome::Update(id) => {
                    *param_id = std::borrow::Cow::Owned(id);
                    *legacy_param_index = None;
                }
                ResolveOutcome::RegistryMissing => {
                    // Preserve both fields for next-load recovery.
                }
            }
        }

        // Common resolution body, parameterized by which registry
        // supplied the def. Effect and generator defs share enough
        // shape (`legacy_param_aliases` + `param_defs[].id`) that we
        // pass them through opaque accessors rather than a trait.
        fn resolve_against<'a>(
            current_id: &str,
            legacy_index: Option<i32>,
            aliases: &'a [crate::effect_registration::ParamAlias],
            param_defs: &'a [crate::effects::ParamDef],
        ) -> ResolveOutcome {
            if !current_id.is_empty() {
                // V1.2+ id-keyed reference: walk the alias chain so
                // schema-bumped renames catch up.
                if aliases.is_empty() {
                    return ResolveOutcome::NoChange;
                }
                match resolve_param_alias(aliases, current_id) {
                    Some(resolved) if resolved == current_id => ResolveOutcome::NoChange,
                    Some(resolved) => ResolveOutcome::Update(resolved.to_string()),
                    None => ResolveOutcome::Drop,
                }
            } else if let Some(idx) = legacy_index {
                // Legacy positional reference: translate via param_defs,
                // then walk the alias chain.
                let Some(pd) = param_defs.get(idx as usize) else {
                    return ResolveOutcome::Drop;
                };
                if pd.id.is_empty() {
                    return ResolveOutcome::Drop;
                }
                match resolve_param_alias(aliases, pd.id.as_str()) {
                    Some(resolved) => ResolveOutcome::Update(resolved.to_string()),
                    None => ResolveOutcome::Drop,
                }
            } else {
                // No id, no legacy index — nothing addressable. Registry
                // is present (caller checked), so clear for consistency.
                ResolveOutcome::Drop
            }
        }

        fn resolve_for_effect(
            effect_type: &crate::PresetTypeId,
            current_id: &str,
            legacy_index: Option<i32>,
        ) -> ResolveOutcome {
            let Some(def) = crate::preset_definition_registry::try_get(effect_type) else {
                return ResolveOutcome::RegistryMissing;
            };
            resolve_against(
                current_id,
                legacy_index,
                def.legacy_param_aliases,
                &def.param_defs,
            )
        }

        fn resolve_for_generator(
            gen_type: &crate::PresetTypeId,
            current_id: &str,
            legacy_index: Option<i32>,
        ) -> ResolveOutcome {
            let Some(def) = crate::preset_definition_registry::try_get(gen_type) else {
                return ResolveOutcome::RegistryMissing;
            };
            resolve_against(
                current_id,
                legacy_index,
                def.legacy_param_aliases,
                &def.param_defs,
            )
        }

        fn resolve_driver_id_for_effect(
            driver: &mut crate::effects::ParameterDriver,
            effect_type: &crate::PresetTypeId,
        ) {
            let outcome =
                resolve_for_effect(effect_type, &driver.param_id, driver.legacy_param_index);
            apply_outcome(
                outcome,
                &mut driver.param_id,
                &mut driver.legacy_param_index,
            );
        }

        fn resolve_driver_id_for_generator(
            driver: &mut crate::effects::ParameterDriver,
            gen_type: &crate::PresetTypeId,
        ) {
            let outcome =
                resolve_for_generator(gen_type, &driver.param_id, driver.legacy_param_index);
            apply_outcome(
                outcome,
                &mut driver.param_id,
                &mut driver.legacy_param_index,
            );
        }

        fn resolve_envelope_id_for_effect(
            env: &mut crate::effects::ParamEnvelope,
            effect_type: &crate::PresetTypeId,
        ) {
            let outcome = resolve_for_effect(effect_type, &env.param_id, env.legacy_param_index);
            apply_outcome(outcome, &mut env.param_id, &mut env.legacy_param_index);
        }

        fn resolve_envelope_id_for_generator(
            env: &mut crate::effects::ParamEnvelope,
            gen_type: &crate::PresetTypeId,
        ) {
            let outcome = resolve_for_generator(gen_type, &env.param_id, env.legacy_param_index);
            apply_outcome(outcome, &mut env.param_id, &mut env.legacy_param_index);
        }

        fn resolve_ableton_id_for_effect(
            mapping: &mut crate::ableton_mapping::AbletonParamMapping,
            effect_type: &crate::PresetTypeId,
        ) {
            let outcome =
                resolve_for_effect(effect_type, &mapping.param_id, mapping.legacy_param_index);
            apply_outcome(
                outcome,
                &mut mapping.param_id,
                &mut mapping.legacy_param_index,
            );
        }

        fn resolve_ableton_id_for_generator(
            mapping: &mut crate::ableton_mapping::AbletonParamMapping,
            gen_type: &crate::PresetTypeId,
        ) {
            let outcome =
                resolve_for_generator(gen_type, &mapping.param_id, mapping.legacy_param_index);
            apply_outcome(
                outcome,
                &mut mapping.param_id,
                &mut mapping.legacy_param_index,
            );
        }

        // Macro mappings are stored on `settings.macro_bank.slots[*].mappings`.
        // Each `MacroMapping` carries a `legacy_param_index` parked from the
        // V1.1 shape; the variant tells us whether to look up via the effect
        // or generator registry. `GenParam` requires the layer to be alive
        // because the generator type isn't recorded on the target itself —
        // a missing layer is treated as `RegistryMissing` so the index
        // survives until the layer reappears.
        fn resolve_macro_mapping(
            mapping: &mut crate::macro_bank::MacroMapping,
            timeline: &crate::timeline::Timeline,
        ) {
            use crate::macro_bank::MacroMappingTarget;
            let legacy_idx = mapping.legacy_param_index;
            let outcome = match &mapping.target {
                MacroMappingTarget::MasterOpacity | MacroMappingTarget::LayerOpacity { .. } => {
                    // No param-bearing variant — drop legacy idx, no id work.
                    ResolveOutcome::Drop
                }
                MacroMappingTarget::MasterEffect {
                    effect_type,
                    param_id,
                } => resolve_for_effect(effect_type, param_id, legacy_idx),
                MacroMappingTarget::LayerEffect {
                    effect_type,
                    param_id,
                    ..
                } => resolve_for_effect(effect_type, param_id, legacy_idx),
                MacroMappingTarget::GenParam { layer_id, param_id } => {
                    match timeline
                        .layers
                        .iter()
                        .find(|l| l.layer_id == *layer_id)
                        .and_then(|l| l.gen_params())
                    {
                        Some(gp) => {
                            resolve_for_generator(gp.generator_type(), param_id, legacy_idx)
                        }
                        // Layer or its gen_params missing — same recovery
                        // semantics as registry-missing on effect/generator.
                        None => ResolveOutcome::RegistryMissing,
                    }
                }
            };

            // Apply the outcome to the variant's `param_id` (where it
            // exists) plus the wrapper's `legacy_param_index`.
            match (&mut mapping.target, outcome) {
                (_, ResolveOutcome::NoChange | ResolveOutcome::Drop) => {
                    mapping.legacy_param_index = None;
                }
                (
                    MacroMappingTarget::MasterEffect { param_id, .. }
                    | MacroMappingTarget::LayerEffect { param_id, .. }
                    | MacroMappingTarget::GenParam { param_id, .. },
                    ResolveOutcome::Update(id),
                ) => {
                    *param_id = std::borrow::Cow::Owned(id);
                    mapping.legacy_param_index = None;
                }
                (_, ResolveOutcome::Update(_)) => {
                    // Update outcome on a no-param variant can't happen
                    // given the resolve match above, but be safe.
                    mapping.legacy_param_index = None;
                }
                (_, ResolveOutcome::RegistryMissing) => {
                    // Preserve legacy index for next-load recovery.
                }
            }
        }

        // Master effects. Envelope-home unification: an effect's
        // drivers/envelopes/ableton mappings all ride on the instance now,
        // each resolved against the instance's own type.
        for fx in &mut self.settings.master_effects {
            let effect_type = fx.effect_type().clone();
            if let Some(drivers) = fx.drivers.as_mut() {
                for d in drivers {
                    resolve_driver_id_for_effect(d, &effect_type);
                }
            }
            if let Some(envelopes) = fx.envelopes.as_mut() {
                for env in envelopes {
                    resolve_envelope_id_for_effect(env, &effect_type);
                }
            }
            if let Some(mappings) = fx.ableton_mappings.as_mut() {
                for m in mappings {
                    resolve_ableton_id_for_effect(m, &effect_type);
                }
            }
        }
        // Layer effects + generator drivers/envelopes/mappings.
        for layer in &mut self.timeline.layers {
            if let Some(ref mut effects) = layer.effects {
                for fx in effects.iter_mut() {
                    let effect_type = fx.effect_type().clone();
                    if let Some(drivers) = fx.drivers.as_mut() {
                        for d in drivers {
                            resolve_driver_id_for_effect(d, &effect_type);
                        }
                    }
                    if let Some(envelopes) = fx.envelopes.as_mut() {
                        for env in envelopes {
                            resolve_envelope_id_for_effect(env, &effect_type);
                        }
                    }
                    if let Some(mappings) = fx.ableton_mappings.as_mut() {
                        for m in mappings {
                            resolve_ableton_id_for_effect(m, &effect_type);
                        }
                    }
                }
            }
            if let Some(gp) = layer.gen_params_mut() {
                let gen_type = gp.generator_type().clone();
                if let Some(drivers) = gp.drivers.as_mut() {
                    for d in drivers {
                        resolve_driver_id_for_generator(d, &gen_type);
                    }
                }
                if let Some(envelopes) = gp.envelopes.as_mut() {
                    for env in envelopes {
                        resolve_envelope_id_for_generator(env, &gen_type);
                    }
                }
                if let Some(mappings) = gp.ableton_mappings.as_mut() {
                    for m in mappings {
                        resolve_ableton_id_for_generator(m, &gen_type);
                    }
                }
            }
        }

        // Macro mappings live on the bank, but `GenParam` resolution
        // needs to look up the generator type via the layer. `timeline`
        // and `settings` are disjoint fields of `Project` so the split
        // borrow is fine.
        let timeline = &self.timeline;
        for slot in &mut self.settings.macro_bank.slots {
            for mapping in &mut slot.mappings {
                resolve_macro_mapping(mapping, timeline);
            }
        }
    }

    pub fn layer_count(&self) -> usize {
        self.timeline.layers.len()
    }

    /// Walk every effect list in the project (master, every layer's
    /// effects, every clip's effects) for an instance whose stable id
    /// matches `effect_id`. Returns the first match or `None`. Linear
    /// in total effect count; used by editor-canvas snapshotting and
    /// graph-mutation commands — not on the per-frame hot path.
    pub fn find_effect_by_id(
        &self,
        effect_id: &crate::id::EffectId,
    ) -> Option<&crate::effects::PresetInstance> {
        for fx in &self.settings.master_effects {
            if &fx.id == effect_id {
                return Some(fx);
            }
        }
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref() {
                for fx in effects {
                    if &fx.id == effect_id {
                        return Some(fx);
                    }
                }
            }
            for clip in &layer.clips {
                for fx in &clip.effects {
                    if &fx.id == effect_id {
                        return Some(fx);
                    }
                }
            }
        }
        None
    }

    /// Count instances (effects + generators) that use a given preset id.
    /// The fork-if-shared rule (Phase 4/5): a preset-level edit edits in place
    /// when the count is 1 (sole user) and forks a project variant when > 1.
    pub fn count_preset_uses(&self, id: &PresetTypeId) -> usize {
        let mut n = 0;
        for fx in &self.settings.master_effects {
            if fx.effect_type() == id {
                n += 1;
            }
        }
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref() {
                n += effects.iter().filter(|fx| fx.effect_type() == id).count();
            }
            for clip in &layer.clips {
                n += clip.effects.iter().filter(|fx| fx.effect_type() == id).count();
            }
            if let Some(gp) = layer.gen_params()
                && gp.generator_type() == id
            {
                n += 1;
            }
        }
        n
    }

    /// The project-embedded preset with this id, if any.
    pub fn embedded_preset(&self, id: &PresetTypeId) -> Option<&EmbeddedPreset> {
        self.embedded_presets.iter().find(|p| p.id() == Some(id))
    }

    /// Mint an embedded-preset id derived from `base` (a `base#N` suffix probe)
    /// that collides with no existing embedded preset.
    pub fn mint_embedded_preset_id(&self, base: &str) -> PresetTypeId {
        let mut n = 1;
        loop {
            let candidate = format!("{base}#{n}");
            let taken = self
                .embedded_presets
                .iter()
                .any(|p| p.id().map(|i| i.as_str()) == Some(candidate.as_str()));
            if !taken {
                return PresetTypeId::from_string(candidate);
            }
            n += 1;
        }
    }

    /// The current preset id of the instance addressed by `target`, if found.
    pub fn instance_preset_id(&self, target: &crate::GraphTarget) -> Option<PresetTypeId> {
        match target {
            crate::GraphTarget::Effect(effect_id) => {
                self.find_effect_by_id(effect_id).map(|fx| fx.effect_type().clone())
            }
            crate::GraphTarget::Generator(layer_id) => self
                .timeline
                .layers
                .iter()
                .find(|l| &l.layer_id == layer_id)
                .and_then(|l| l.gen_params())
                .map(|gp| gp.generator_type().clone()),
        }
    }

    /// The [`PresetInstance`](crate::effects::PresetInstance) a
    /// [`GraphTarget`] resolves to — const twin of
    /// [`Self::with_preset_graph_mut`]. Effect by stable
    /// [`EffectId`](crate::id::EffectId); generator by its host layer's
    /// `gen_params`. `None` if the target doesn't resolve. The single const
    /// locate behind read-side per-target accessors (e.g. resolving a preset's
    /// graph def for fork / export).
    pub fn preset_instance(
        &self,
        target: &crate::GraphTarget,
    ) -> Option<&crate::effects::PresetInstance> {
        match target {
            crate::GraphTarget::Effect(eid) => self.find_effect_by_id(eid),
            crate::GraphTarget::Generator(lid) => self
                .timeline
                .layers
                .iter()
                .find(|l| &l.layer_id == lid)
                .and_then(|l| l.gen_params()),
        }
    }

    /// Mutable variant of [`Self::preset_instance`]. Resolves the effect or
    /// generator instance behind a [`GraphTarget`] for in-place edits (e.g.
    /// re-seeding `param_values` after a fork/import retarget).
    pub fn preset_instance_mut(
        &mut self,
        target: &crate::GraphTarget,
    ) -> Option<&mut crate::effects::PresetInstance> {
        match target {
            crate::GraphTarget::Effect(eid) => self.find_effect_by_id_mut(eid),
            crate::GraphTarget::Generator(lid) => self
                .timeline
                .layers
                .iter_mut()
                .find(|l| &l.layer_id == lid)
                .and_then(|l| l.gen_params_mut()),
        }
    }

    /// Insert (or replace by id) a project-embedded preset.
    pub fn upsert_embedded_preset(&mut self, preset: EmbeddedPreset) {
        let id = preset.id().cloned();
        if let Some(id) = id {
            self.embedded_presets.retain(|p| p.id() != Some(&id));
        }
        self.embedded_presets.push(preset);
    }

    /// Remove a project-embedded preset by id. Returns it if present.
    pub fn remove_embedded_preset(&mut self, id: &PresetTypeId) -> Option<EmbeddedPreset> {
        let pos = self.embedded_presets.iter().position(|p| p.id() == Some(id))?;
        Some(self.embedded_presets.remove(pos))
    }

    /// Retarget the instance addressed by `target` at a different preset id,
    /// keeping its param values. Returns `false` if the target wasn't found.
    pub fn set_instance_preset_id(
        &mut self,
        target: &crate::GraphTarget,
        id: PresetTypeId,
    ) -> bool {
        match target {
            crate::GraphTarget::Effect(effect_id) => {
                if let Some(fx) = self.find_effect_by_id_mut(effect_id) {
                    fx.set_preset_id(id);
                    return true;
                }
                false
            }
            crate::GraphTarget::Generator(layer_id) => {
                for layer in &mut self.timeline.layers {
                    if &layer.layer_id == layer_id {
                        if let Some(gp) = layer.gen_params_mut() {
                            gp.set_preset_id(id);
                            return true;
                        }
                        return false;
                    }
                }
                false
            }
        }
    }

    /// Fork: register `source_def` as a new project-embedded preset (id minted
    /// uniquely from its current id) and retarget the instance at `target` to
    /// it. Returns the new preset id, or `None` if the target wasn't found.
    /// The instance keeps its param values — a fork is a copy of the same
    /// preset under a new id, so the values stay valid.
    pub fn fork_preset(
        &mut self,
        target: &crate::GraphTarget,
        kind: PresetKind,
        mut source_def: EffectGraphDef,
    ) -> Option<PresetTypeId> {
        let base = source_def
            .preset_metadata
            .as_ref()
            .map(|m| m.id.as_str().to_string())
            .unwrap_or_else(|| "preset".to_string());
        let new_id = self.mint_embedded_preset_id(&base);
        if let Some(m) = source_def.preset_metadata.as_mut() {
            m.id = new_id.clone();
        }
        if !self.set_instance_preset_id(target, new_id.clone()) {
            return None;
        }
        self.embedded_presets.push(EmbeddedPreset {
            kind,
            def: source_def,
        });
        Some(new_id)
    }

    /// Mutable variant of [`Self::find_effect_by_id`]. Used by
    /// graph-mutation commands to apply edits to the matching
    /// instance in place.
    pub fn find_effect_by_id_mut(
        &mut self,
        effect_id: &crate::id::EffectId,
    ) -> Option<&mut crate::effects::PresetInstance> {
        for fx in &mut self.settings.master_effects {
            if &fx.id == effect_id {
                return Some(fx);
            }
        }
        for layer in &mut self.timeline.layers {
            if let Some(effects) = layer.effects.as_mut() {
                for fx in effects {
                    if &fx.id == effect_id {
                        return Some(fx);
                    }
                }
            }
            for clip in &mut layer.clips {
                for fx in &mut clip.effects {
                    if &fx.id == effect_id {
                        return Some(fx);
                    }
                }
            }
        }
        None
    }

    /// Run `f` against the [`crate::effects::PresetInstance`] that a
    /// [`crate::graph_target::GraphTarget`] resolves to, returning its
    /// result (`None` if the target doesn't resolve). The one entry point
    /// editing commands use to operate on an effect instance or a layer's
    /// generator without forking — both are a `PresetInstance` now that the
    /// generator's graph lives on `gen_params` (graph-home unification), so
    /// there is no `GraphHost`/`GeneratorHost` abstraction. A generator target
    /// initializes the layer's `gen_params` if absent (graph editing must work
    /// before param state exists), inheriting the layer's generator type.
    pub fn with_preset_graph_mut<R>(
        &mut self,
        target: &crate::graph_target::GraphTarget,
        f: impl FnOnce(&mut crate::effects::PresetInstance) -> R,
    ) -> Option<R> {
        match target {
            crate::graph_target::GraphTarget::Effect(eid) => {
                let fx = self.find_effect_by_id_mut(eid)?;
                Some(f(fx))
            }
            crate::graph_target::GraphTarget::Generator(lid) => {
                let (_, layer) = self.timeline.find_layer_by_id_mut(lid.as_str())?;
                Some(f(layer.gen_params_or_init()))
            }
        }
    }

    /// The `&mut PresetInstance` an Ableton mapping target addresses —
    /// located the way the Ableton bridge addresses hosts: by `effect_type`
    /// within master / a layer, or a layer's generator. `None` for
    /// `MacroSlot` (a macro slot is not a preset instance) or an unresolved
    /// host. This is the single master/layer/generator locate-fork: every
    /// per-target Ableton accessor (the mappings vec, live value writes)
    /// routes through here so the dispatch is written exactly once.
    pub fn find_preset_instance_mut(
        &mut self,
        target: &crate::ableton_mapping::AbletonMappingTarget,
    ) -> Option<&mut crate::effects::PresetInstance> {
        use crate::ableton_mapping::AbletonMappingTarget as T;
        match target {
            T::MasterEffect { effect_type, .. } => self
                .settings
                .master_effects
                .iter_mut()
                .find(|f| f.effect_type() == effect_type),
            T::LayerEffect {
                layer_id,
                effect_type,
                ..
            } => self
                .timeline
                .find_layer_by_id_mut(layer_id.as_str())
                .and_then(|(_, layer)| layer.effects.as_mut())
                .and_then(|effects| effects.iter_mut().find(|f| f.effect_type() == effect_type)),
            T::GenParam { layer_id, .. } => self
                .timeline
                .find_layer_by_id_mut(layer_id.as_str())
                .and_then(|(_, layer)| layer.gen_params_mut()),
            T::MacroSlot { .. } => None,
        }
    }

    /// Const twin of [`Self::find_preset_instance_mut`].
    pub fn find_preset_instance(
        &self,
        target: &crate::ableton_mapping::AbletonMappingTarget,
    ) -> Option<&crate::effects::PresetInstance> {
        use crate::ableton_mapping::AbletonMappingTarget as T;
        match target {
            T::MasterEffect { effect_type, .. } => self
                .settings
                .master_effects
                .iter()
                .find(|f| f.effect_type() == effect_type),
            T::LayerEffect {
                layer_id,
                effect_type,
                ..
            } => self
                .timeline
                .find_layer_by_id(layer_id.as_str())
                .and_then(|(_, layer)| layer.effects.as_ref())
                .and_then(|effects| effects.iter().find(|f| f.effect_type() == effect_type)),
            T::GenParam { layer_id, .. } => self
                .timeline
                .find_layer_by_id(layer_id.as_str())
                .and_then(|(_, layer)| layer.gen_params()),
            T::MacroSlot { .. } => None,
        }
    }

    /// The `&mut Option<Vec<AbletonParamMapping>>` an Ableton mapping
    /// target's per-param mappings live in. Thin projection of
    /// [`Self::find_preset_instance_mut`]; `None` for `MacroSlot` (single
    /// mapping, not a per-param vec — its call sites keep their own arm) or
    /// an unresolved host.
    pub fn ableton_param_mappings_mut(
        &mut self,
        target: &crate::ableton_mapping::AbletonMappingTarget,
    ) -> Option<&mut Option<Vec<crate::ableton_mapping::AbletonParamMapping>>> {
        self.find_preset_instance_mut(target)
            .map(|fx| &mut fx.ableton_mappings)
    }

    /// Const twin of [`Self::ableton_param_mappings_mut`].
    pub fn ableton_param_mappings(
        &self,
        target: &crate::ableton_mapping::AbletonMappingTarget,
    ) -> Option<&Option<Vec<crate::ableton_mapping::AbletonParamMapping>>> {
        self.find_preset_instance(target)
            .map(|fx| &fx.ableton_mappings)
    }

    /// Port of Unity Project.ImportedPercussionClipPlacements property.
    /// Returns a mutable reference to the clip placements slice inside percussion_import.
    /// Initializes percussion_import if absent (matches Unity's lazy-init pattern).
    pub fn imported_percussion_clip_placements_mut(
        &mut self,
    ) -> &mut Vec<crate::percussion::ImportedPercussionClipPlacement> {
        if self.percussion_import.is_none() {
            self.percussion_import = Some(crate::percussion::PercussionImportState::default());
        }
        &mut self.percussion_import.as_mut().unwrap().clip_placements
    }

    /// Port of Unity Project.ImportedPercussionClipPlacements (read-only path).
    pub fn imported_percussion_clip_placements(
        &self,
    ) -> Option<&Vec<crate::percussion::ImportedPercussionClipPlacement>> {
        self.percussion_import.as_ref().map(|s| &s.clip_placements)
    }

    /// Port of Unity Project.ImportedPercussionAudioStartBeat getter.
    pub fn imported_percussion_audio_start_beat(&self) -> f32 {
        self.percussion_import
            .as_ref()
            .map_or(0.0, |s| s.audio_start_beat.as_f32())
    }

    /// Port of Unity Project.ImportedPercussionAudioStartBeat setter (Mathf.Max(0f, value)).
    pub fn set_imported_percussion_audio_start_beat(&mut self, value: f32) {
        if self.percussion_import.is_none() {
            self.percussion_import = Some(crate::percussion::PercussionImportState::default());
        }
        self.percussion_import.as_mut().unwrap().audio_start_beat = Beats::from_f32(value.max(0.0));
    }

    pub fn total_clip_count(&self) -> usize {
        self.timeline.total_clip_count()
    }

    /// Migrate old projects: force all layers to NoteOff duration mode.
    /// Port of C# ProjectSerializer.cs lines 45-50.
    pub fn migrate_duration_modes(&mut self) {
        for layer in &mut self.timeline.layers {
            if layer.duration_mode != Some(ClipDurationMode::NoteOff) {
                layer.duration_mode = Some(ClipDurationMode::NoteOff);
            }
        }
    }

    /// Sync BPM from tempo map beat 0, clamped to 20-300.
    /// Port of C# ProjectSerializer.cs lines 39-43.
    pub fn sync_bpm_from_tempo_map(&mut self) {
        self.settings.bpm = self
            .tempo_map
            .get_bpm_at_beat(Beats::ZERO, self.settings.bpm);
    }

    /// Validate project structure. Returns list of error strings.
    /// Port of C# Project.Validate (lines 245-286).
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        // Validate timeline clip references
        for layer in &self.timeline.layers {
            for clip in &layer.clips {
                if layer.layer_type == crate::types::LayerType::Generator
                    || clip.video_clip_id.is_empty()
                {
                    continue;
                }
                if !self.video_library.has_clip(&clip.video_clip_id) {
                    errors.push(format!(
                        "Timeline clip {} references missing video {}",
                        clip.id, clip.video_clip_id
                    ));
                }
            }
        }

        errors
    }

    /// Purge orphaned references: timeline clips pointing at missing library entries,
    /// stale MIDI mappings. Port of C# Project.PurgeOrphanedReferences (lines 305-358).
    pub fn purge_orphaned_references(&mut self) -> PurgeResult {
        let mut result = PurgeResult::default();

        // Build set of all valid video clip IDs in the library
        let valid_ids: HashSet<String> = self
            .video_library
            .clips
            .iter()
            .map(|c| c.id.clone())
            .collect();

        // Stage 1: Remove timeline clips referencing missing library entries
        for layer in &mut self.timeline.layers {
            let before = layer.clips.len();
            let is_gen_layer = layer.layer_type == crate::types::LayerType::Generator;
            layer.clips.retain(|clip| {
                // Keep generator layer clips — they have no video reference
                if is_gen_layer {
                    return true;
                }
                if clip.video_clip_id.is_empty() {
                    return true;
                }
                valid_ids.contains(&clip.video_clip_id)
            });
            result.timeline_clips_removed += before - layer.clips.len();
        }

        // Stage 2: Purge stale clip IDs from MIDI mappings
        result.midi_mappings_removed = self.midi_config.purge_orphaned_clip_ids(&valid_ids);

        // Stage 3: Rebuild clip lookup cache if anything changed
        if result.total_removed() > 0 {
            self.timeline.rebuild_clip_lookup();
        }

        result
    }
}

/// Result of purge_orphaned_references().
/// Port of C# Project.PurgeResult.
#[derive(Debug, Clone, Default)]
pub struct PurgeResult {
    pub timeline_clips_removed: usize,
    pub midi_mappings_removed: usize,
}

impl PurgeResult {
    pub fn total_removed(&self) -> usize {
        self.timeline_clips_removed + self.midi_mappings_removed
    }
}

impl Default for Project {
    fn default() -> Self {
        Self {
            project_name: String::new(),
            project_version: "1.7.0".to_string(),
            timeline: Timeline::default(),
            video_library: VideoLibrary::default(),
            midi_config: MidiMappingConfig::default(),
            settings: ProjectSettings::default(),
            tempo_map: TempoMap::default(),
            recording_provenance: RecordingProvenance::default(),
            percussion_import: None,
            last_saved_path: String::new(),
            saved_playhead_time: 0.0,
            embedded_presets: Vec::new(),
            legacy_perc_audio_path: None,
            legacy_perc_audio_start_beat: None,
            legacy_perc_clip_placements: None,
            legacy_perc_energy_envelope: None,
            legacy_imported_stem_paths: None,
            legacy_perc_audio_hash: None,
        }
    }
}

fn default_version() -> String {
    "1.4.0".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PresetTypeId;
    use crate::effects::{PresetInstance, ParamSlot, ParameterDriver};
    use crate::types::{BeatDivision, DriverWaveform};

    fn graph_def_with_id(id: &str, name: &str) -> crate::effect_graph_def::EffectGraphDef {
        crate::effect_graph_def::EffectGraphDef {
            version: crate::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: Some(name.to_string()),
            description: None,
            preset_metadata: Some(crate::effect_graph_def::PresetMetadata {
                id: PresetTypeId::from_string(id.to_string()),
                display_name: name.to_string(),
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
            }),
            nodes: Vec::new(),
            wires: Vec::new(),
        }
    }

    #[test]
    fn fork_preset_mints_id_retargets_instance_and_updates_use_count() {
        let mut p = Project::default();
        let fx = PresetInstance::new(PresetTypeId::BLOOM);
        let fx_id = fx.id.clone();
        p.settings.master_effects.push(fx);

        assert_eq!(p.count_preset_uses(&PresetTypeId::BLOOM), 1);

        let target = crate::GraphTarget::Effect(fx_id.clone());
        let new_id = p
            .fork_preset(&target, PresetKind::Effect, graph_def_with_id("Bloom", "Bloom"))
            .expect("fork retargets an existing instance");

        // Minted a distinct project-scoped id.
        assert_eq!(new_id.as_str(), "Bloom#1");
        // The instance now points at the fork; the embedded preset exists.
        assert_eq!(p.find_effect_by_id(&fx_id).unwrap().effect_type(), &new_id);
        assert!(p.embedded_preset(&new_id).is_some());
        assert_eq!(p.embedded_preset(&new_id).unwrap().def.preset_metadata.as_ref().unwrap().id, new_id);
        // Use counts moved from the stock id to the fork.
        assert_eq!(p.count_preset_uses(&PresetTypeId::BLOOM), 0);
        assert_eq!(p.count_preset_uses(&new_id), 1);

        // A second fork of the same base mints a fresh id.
        let fx2 = PresetInstance::new(PresetTypeId::BLOOM);
        let fx2_id = fx2.id.clone();
        p.settings.master_effects.push(fx2);
        let new_id2 = p
            .fork_preset(
                &crate::GraphTarget::Effect(fx2_id),
                PresetKind::Effect,
                graph_def_with_id("Bloom", "Bloom"),
            )
            .unwrap();
        assert_eq!(new_id2.as_str(), "Bloom#2");
        assert_eq!(p.embedded_presets.len(), 2);
    }

    #[test]
    fn embedded_presets_round_trip_and_skip_when_empty() {
        // Empty by default → no `embeddedPresets` field on the wire (existing
        // fixtures stay byte-identical).
        let p = Project::default();
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("embeddedPresets"), "empty must be skipped: {json}");

        // A forked preset round-trips inside the project JSON.
        let mut p = Project::default();
        let mut def = crate::effect_graph_def::EffectGraphDef {
            version: crate::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: Some("Oily Fluid (Layer 2 variant)".to_string()),
            description: None,
            preset_metadata: None,
            nodes: Vec::new(),
            wires: Vec::new(),
        };
        def.preset_metadata = Some(crate::effect_graph_def::PresetMetadata {
            id: PresetTypeId::from_string("OilyFluid#layer2".to_string()),
            display_name: "Oily Fluid (Layer 2 variant)".to_string(),
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
        p.embedded_presets.push(EmbeddedPreset {
            kind: PresetKind::Generator,
            def,
        });

        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("embeddedPresets"), "non-empty must serialize: {json}");
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.embedded_presets.len(), 1);
        assert_eq!(back.embedded_presets[0].kind, PresetKind::Generator);
        assert_eq!(
            back.embedded_presets[0].id().map(|i| i.as_str()),
            Some("OilyFluid#layer2")
        );
    }

    /// Step 8 regression: a driver deserialized from the legacy
    /// `paramIndex` shape gets its `param_id` filled in by
    /// `resolve_legacy_param_ids` during `on_after_deserialize`.
    #[test]
    fn legacy_param_index_resolved_to_param_id_for_effect_drivers() {
        let mut p = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.param_values = vec![ParamSlot::exposed(0.5)];
        // Construct a driver as if it came from legacy JSON: empty
        // param_id, legacy_param_index = Some(0).
        fx.drivers = Some(vec![ParameterDriver {
            param_id: std::borrow::Cow::Borrowed(""),
            beat_division: BeatDivision::Quarter,
            waveform: DriverWaveform::Sine,
            enabled: true,
            phase: 0.0,
            base_value: 0.0,
            trim_min: 0.0,
            trim_max: 1.0,
            reversed: false,
            legacy_param_index: Some(0),
            is_paused_by_user: false,
        }]);
        p.settings.master_effects.push(fx);

        p.resolve_legacy_param_ids();

        let d = &p.settings.master_effects[0].drivers.as_ref().unwrap()[0];
        assert_eq!(
            d.param_id, "amount",
            "Bloom paramIndex 0 should resolve to 'amount'"
        );
        assert_eq!(d.legacy_param_index, None, "legacy index must be cleared");
    }

    #[test]
    fn legacy_resolution_idempotent_when_param_id_already_set() {
        let mut p = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.param_values = vec![ParamSlot::exposed(0.5)];
        fx.drivers = Some(vec![ParameterDriver::new(
            "amount",
            BeatDivision::Quarter,
            DriverWaveform::Sine,
        )]);
        p.settings.master_effects.push(fx);

        p.resolve_legacy_param_ids();
        p.resolve_legacy_param_ids(); // idempotent

        let d = &p.settings.master_effects[0].drivers.as_ref().unwrap()[0];
        assert_eq!(d.param_id, "amount");
        assert_eq!(d.legacy_param_index, None);
    }

    #[test]
    fn legacy_param_index_resolved_for_effect_envelopes() {
        use crate::effects::{ParamEnvelope, PresetInstance};
        use crate::layer::Layer;
        use crate::types::LayerType;

        // Envelope-home unification: an effect envelope rides on the effect
        // instance and resolves its legacy param index against the instance's
        // own type (no `target_effect_type` on the envelope anymore).
        let mut p = Project::default();
        let mut layer = Layer::new("test".to_string(), LayerType::Video, 0);
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.param_values = vec![ParamSlot::exposed(0.5)];
        fx.envelopes = Some(vec![ParamEnvelope {
            param_id: std::borrow::Cow::Borrowed(""),
            enabled: true,
            target_normalized: 1.0,
            decay_beats: 1.0,
            legacy_param_index: Some(0),
            current_level: 0.0,
            was_clip_active: false,
        }]);
        layer.effects = Some(vec![fx]);
        p.timeline.layers.push(layer);

        p.resolve_legacy_param_ids();

        let env =
            &p.timeline.layers[0].effects.as_ref().unwrap()[0].envelopes.as_ref().unwrap()[0];
        assert_eq!(env.param_id, "amount");
        assert_eq!(env.legacy_param_index, None);
    }

    #[test]
    fn legacy_resolution_drops_orphans_when_effect_known() {
        // If the registry knows the effect (Bloom) but the legacy index
        // is out of range (param list shrunk since save), the entry is
        // permanently orphaned: clear legacy index, leave param_id
        // empty. Same fail-soft policy as alias-drop. Driver is then
        // ignored at runtime (`param_id_to_index` returns None on "").
        let mut p = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.param_values = vec![ParamSlot::exposed(0.5)];
        fx.drivers = Some(vec![ParameterDriver {
            param_id: std::borrow::Cow::Borrowed(""),
            beat_division: BeatDivision::Quarter,
            waveform: DriverWaveform::Sine,
            enabled: true,
            phase: 0.0,
            base_value: 0.0,
            trim_min: 0.0,
            trim_max: 1.0,
            reversed: false,
            legacy_param_index: Some(99),
            is_paused_by_user: false,
        }]);
        p.settings.master_effects.push(fx);

        p.resolve_legacy_param_ids();

        let d = &p.settings.master_effects[0].drivers.as_ref().unwrap()[0];
        assert_eq!(d.param_id, "", "out-of-range index leaves param_id empty");
        assert_eq!(
            d.legacy_param_index, None,
            "registry-known + out-of-range = Drop; legacy idx cleared"
        );
    }

    #[test]
    fn registry_missing_recovery_round_trip() {
        // End-to-end: a driver that loaded against an unregistered
        // effect (RegistryMissing) keeps its `legacy_param_index`
        // parked. The custom `Serialize` impl re-emits it as
        // `paramIndex`, so a save→reload cycle on a build without the
        // registry preserves recovery information verbatim. On a
        // future load against a populated registry, the resolver fills
        // in `param_id` cleanly.
        use crate::effects::{ParamId, ParameterDriver};

        // Step 1: simulate a load where the registry was missing for
        // this effect type. The driver is in the parked state.
        let driver = ParameterDriver {
            param_id: ParamId::Borrowed(""),
            beat_division: BeatDivision::Half,
            waveform: DriverWaveform::Sine,
            enabled: true,
            phase: 0.25,
            base_value: 0.0,
            trim_min: 0.0,
            trim_max: 1.0,
            reversed: false,
            legacy_param_index: Some(2),
            is_paused_by_user: false,
        };

        // Step 2: serialize. Custom Serialize re-emits the parked
        // index as `paramIndex` since param_id is empty.
        let json = serde_json::to_string(&driver).expect("serialize");
        assert!(
            json.contains("\"paramIndex\":2"),
            "registry-missing driver must re-emit legacy paramIndex on save; got: {json}"
        );
        assert!(
            !json.contains("\"paramId\""),
            "must NOT emit paramId when it's empty; got: {json}"
        );

        // Step 3: reload. Deserialize parks the index again.
        let back: ParameterDriver = serde_json::from_str(&json).expect("deserialize");
        assert!(
            back.param_id.is_empty(),
            "param_id remains empty until resolver"
        );
        assert_eq!(
            back.legacy_param_index,
            Some(2),
            "index re-parked from wire"
        );

        // Step 4: now imagine the registry just came online (Bloom is
        // registered in this test crate; pretend the driver was for it).
        let mut p = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.param_values = vec![ParamSlot::exposed(0.5)];
        let mut driver_for_bloom = back.clone();
        // (In a real load, the driver lands inside an PresetInstance
        // from the deserialize tree; here we simulate by re-attaching.)
        driver_for_bloom.legacy_param_index = Some(0); // Bloom only has 1 param; fake idx 0
        fx.drivers = Some(vec![driver_for_bloom]);
        p.settings.master_effects.push(fx);

        // Step 5: resolver runs against the populated registry. Recovery
        // completes: param_id resolves, legacy index clears.
        p.resolve_legacy_param_ids();
        let d = &p.settings.master_effects[0].drivers.as_ref().unwrap()[0];
        assert_eq!(
            d.param_id, "amount",
            "registry came online; resolver fills param_id"
        );
        assert_eq!(
            d.legacy_param_index, None,
            "legacy index cleared on successful resolve"
        );
    }

    #[test]
    fn legacy_resolution_preserves_legacy_idx_when_registry_missing() {
        // The cross-cutting recovery path: if the registry doesn't have a
        // def for this effect type at load time (e.g., a tooling crate
        // that didn't link manifold-renderer), the resolver must NOT
        // clear `legacy_param_index`. Otherwise the next save→reload on
        // a properly-registered build would silently lose the addressing
        // forever. The custom Serialize for `ParameterDriver` re-emits
        // `paramIndex` when `param_id` is empty, completing the recovery
        // loop end-to-end.
        let mut p = Project::default();
        // Synthetic effect type with no registry def in this test build.
        let unregistered = crate::PresetTypeId::from_string("not-a-real-effect-id".to_string());
        let mut fx = PresetInstance::new(unregistered);
        fx.param_values = vec![
            ParamSlot::exposed(0.5),
            ParamSlot::exposed(0.5),
            ParamSlot::exposed(0.5),
        ];
        fx.drivers = Some(vec![ParameterDriver {
            param_id: std::borrow::Cow::Borrowed(""),
            beat_division: BeatDivision::Quarter,
            waveform: DriverWaveform::Sine,
            enabled: true,
            phase: 0.0,
            base_value: 0.0,
            trim_min: 0.0,
            trim_max: 1.0,
            reversed: false,
            legacy_param_index: Some(2),
            is_paused_by_user: false,
        }]);
        p.settings.master_effects.push(fx);

        p.resolve_legacy_param_ids();

        let d = &p.settings.master_effects[0].drivers.as_ref().unwrap()[0];
        assert_eq!(d.param_id, "", "no registry def -> param_id stays empty");
        assert_eq!(
            d.legacy_param_index,
            Some(2),
            "RegistryMissing must preserve legacy index for next-load recovery"
        );
    }

    // ── Node-id normalization for pre-node-id graph overrides ─────

    #[test]
    fn normalize_override_node_ids_stamps_empty_ids_with_handle() {
        use crate::effect_graph_def::{EFFECT_GRAPH_VERSION, EffectGraphDef, EffectGraphNode};
        use std::collections::{BTreeMap, BTreeSet};

        let make_node = |id: u32, handle: Option<&str>| EffectGraphNode {
            id,
            node_id: crate::NodeId::default(), // pre-node-id document
            type_id: "node.blur".to_string(),
            handle: handle.map(|h| h.to_string()),
            params: BTreeMap::new(),
            exposed_params: BTreeSet::new(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        };
        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            // One handled node + one anonymous boundary node.
            nodes: vec![make_node(0, Some("softblur")), make_node(1, None)],
            wires: vec![],
        };
        let mut fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        fx.graph = Some(def);
        let mut p = Project::default();
        p.settings.master_effects.push(fx);

        p.normalize_override_node_ids();

        let nodes = &p.settings.master_effects[0].graph.as_ref().unwrap().nodes;
        assert_eq!(nodes[0].node_id, "softblur", "handled node id defaults to handle");
        assert!(
            nodes[1].node_id.is_empty(),
            "anonymous node left empty — never a binding target"
        );

        // Idempotent: a node that already has an explicit id is untouched.
        let mut fx2 = PresetInstance::new(PresetTypeId::new("Mirror"));
        let mut explicit = make_node(0, Some("softblur"));
        explicit.node_id = crate::NodeId::new("explicit");
        fx2.graph = Some(EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![explicit],
            wires: vec![],
        });
        let mut p2 = Project::default();
        p2.settings.master_effects.push(fx2);
        p2.normalize_override_node_ids();
        assert_eq!(
            p2.settings.master_effects[0].graph.as_ref().unwrap().nodes[0].node_id,
            "explicit",
            "explicit id preserved"
        );
    }

}
