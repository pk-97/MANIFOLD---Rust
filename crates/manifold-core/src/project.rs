use crate::midi::MidiMappingConfig;
use crate::percussion::PercussionImportState;
use crate::recording::RecordingProvenance;
use crate::settings::ProjectSettings;
use crate::tempo::TempoMap;
use crate::timeline::Timeline;
use crate::types::ClipDurationMode;
use crate::units::Beats;
use crate::video::VideoLibrary;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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
        use crate::effect_type_id::EffectTypeId;
        let strip = |effects: &mut Vec<crate::effects::EffectInstance>| {
            effects.retain(|fx| *fx.effect_type() != EffectTypeId::UNKNOWN);
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

        // V2 user-exposed binding node-handle migration. Renames declared
        // via `EffectNodeAliasMetadata` propagate to saved bindings here.
        // No-op for fixtures and effects that ship without renames.
        self.resolve_user_param_binding_node_handles();

        // Normalize layer order into tree pre-order (group children contiguous
        // immediately after parent). Also reindexes.
        self.timeline.enforce_tree_order();
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
        use crate::effect_definition_registry;
        fn apply_to_effect(fx: &mut crate::effects::EffectInstance) {
            let Some(def) = effect_definition_registry::try_get(fx.effect_type()) else {
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
                    // Keep base value in sync so a later
                    // `reset_param_effectives` doesn't wipe the
                    // migration back to the pre-migration value.
                    if let Some(base) = fx.base_param_values.as_mut()
                        && let Some(b) = base.get_mut(slot_idx)
                    {
                        *b = to as f32;
                    }
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
        use crate::effect_definition_registry;
        use crate::effect_registration::resolve_param_alias;
        use crate::generator_definition_registry;

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
            effect_type: &crate::EffectTypeId,
            current_id: &str,
            legacy_index: Option<i32>,
        ) -> ResolveOutcome {
            let Some(def) = effect_definition_registry::try_get(effect_type) else {
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
            gen_type: &crate::GeneratorTypeId,
            current_id: &str,
            legacy_index: Option<i32>,
        ) -> ResolveOutcome {
            let Some(def) = generator_definition_registry::try_get(gen_type) else {
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
            effect_type: &crate::EffectTypeId,
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
            gen_type: &crate::GeneratorTypeId,
        ) {
            let outcome =
                resolve_for_generator(gen_type, &driver.param_id, driver.legacy_param_index);
            apply_outcome(
                outcome,
                &mut driver.param_id,
                &mut driver.legacy_param_index,
            );
        }

        fn resolve_envelope_id_for_effect(env: &mut crate::effects::ParamEnvelope) {
            let target = env.target_effect_type.clone();
            let outcome = resolve_for_effect(&target, &env.param_id, env.legacy_param_index);
            apply_outcome(outcome, &mut env.param_id, &mut env.legacy_param_index);
        }

        fn resolve_envelope_id_for_generator(
            env: &mut crate::effects::ParamEnvelope,
            gen_type: &crate::GeneratorTypeId,
        ) {
            let outcome = resolve_for_generator(gen_type, &env.param_id, env.legacy_param_index);
            apply_outcome(outcome, &mut env.param_id, &mut env.legacy_param_index);
        }

        fn resolve_ableton_id_for_effect(
            mapping: &mut crate::ableton_mapping::AbletonParamMapping,
            effect_type: &crate::EffectTypeId,
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
            gen_type: &crate::GeneratorTypeId,
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

        // Master effects.
        for fx in &mut self.settings.master_effects {
            let effect_type = fx.effect_type().clone();
            if let Some(drivers) = fx.drivers.as_mut() {
                for d in drivers {
                    resolve_driver_id_for_effect(d, &effect_type);
                }
            }
            if let Some(mappings) = fx.ableton_mappings.as_mut() {
                for m in mappings {
                    resolve_ableton_id_for_effect(m, &effect_type);
                }
            }
        }
        // Layer effects + layer envelopes + generator drivers/envelopes/mappings.
        for layer in &mut self.timeline.layers {
            if let Some(ref mut effects) = layer.effects {
                for fx in effects.iter_mut() {
                    let effect_type = fx.effect_type().clone();
                    if let Some(drivers) = fx.drivers.as_mut() {
                        for d in drivers {
                            resolve_driver_id_for_effect(d, &effect_type);
                        }
                    }
                    if let Some(mappings) = fx.ableton_mappings.as_mut() {
                        for m in mappings {
                            resolve_ableton_id_for_effect(m, &effect_type);
                        }
                    }
                }
            }
            // Layer-level envelopes target effects on this layer; each
            // envelope carries its own `target_effect_type`.
            if let Some(envelopes) = layer.envelopes.as_mut() {
                for env in envelopes {
                    resolve_envelope_id_for_effect(env);
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

    /// Walk every `EffectInstance.user_param_bindings` and resolve
    /// stale `node_handle` strings against the effect's
    /// `EffectDef::legacy_node_aliases` table.
    ///
    /// Direct analogue of `resolve_legacy_param_ids` but for inner-graph
    /// node handles. Same outcome semantics:
    ///
    /// - **Resolved (rename)** — handle aliases to a current handle.
    ///   Update `node_handle` in place.
    /// - **NoChange** — handle is current. No-op.
    /// - **Drop** — alias chain ends in `None`. The handle is left
    ///   untouched so the binding remains in the project file as an
    ///   orphan (visible in a future "broken bindings" UI per
    ///   `docs/EFFECT_RUNTIME_UNIFICATION.md` §10 question 7). The
    ///   renderer will fail handle resolution at apply time, log
    ///   once, and skip — the binding is inert until re-added.
    /// - **RegistryMissing** — registry has no def. Don't touch.
    ///   On a future load with the registry present, resolution
    ///   re-runs.
    fn resolve_user_param_binding_node_handles(&mut self) {
        use crate::effect_definition_registry;
        use crate::effect_registration::resolve_param_alias;

        fn resolve_one(
            effect_type: &crate::EffectTypeId,
            binding: &mut crate::effects::UserParamBinding,
        ) {
            let Some(def) = effect_definition_registry::try_get(effect_type) else {
                // RegistryMissing: leave handle untouched for next-load recovery.
                return;
            };
            if def.legacy_node_aliases.is_empty() {
                // NoChange across the board (no aliases declared).
                return;
            }
            match resolve_param_alias(def.legacy_node_aliases, &binding.node_handle) {
                // Already current — nothing to do.
                Some(resolved) if resolved == binding.node_handle => {}
                Some(resolved) => {
                    binding.node_handle = resolved.to_string();
                }
                // Drop: handle was retired. Leave as-is so the binding
                // surfaces as orphaned at apply time.
                None => {}
            }
        }

        for fx in &mut self.settings.master_effects {
            let effect_type = fx.effect_type().clone();
            for ub in &mut fx.user_param_bindings {
                resolve_one(&effect_type, ub);
            }
        }
        for layer in &mut self.timeline.layers {
            if let Some(ref mut effects) = layer.effects {
                for fx in effects.iter_mut() {
                    let effect_type = fx.effect_type().clone();
                    for ub in &mut fx.user_param_bindings {
                        resolve_one(&effect_type, ub);
                    }
                }
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
    ) -> Option<&crate::effects::EffectInstance> {
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

    /// Mutable variant of [`Self::find_effect_by_id`]. Used by
    /// graph-mutation commands to apply edits to the matching
    /// instance in place.
    pub fn find_effect_by_id_mut(
        &mut self,
        effect_id: &crate::id::EffectId,
    ) -> Option<&mut crate::effects::EffectInstance> {
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
            project_version: "1.3.0".to_string(),
            timeline: Timeline::default(),
            video_library: VideoLibrary::default(),
            midi_config: MidiMappingConfig::default(),
            settings: ProjectSettings::default(),
            tempo_map: TempoMap::default(),
            recording_provenance: RecordingProvenance::default(),
            percussion_import: None,
            last_saved_path: String::new(),
            saved_playhead_time: 0.0,
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
    "1.3.0".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EffectTypeId;
    use crate::effects::{EffectInstance, ParamSlot, ParameterDriver};
    use crate::types::{BeatDivision, DriverWaveform};

    /// Step 8 regression: a driver deserialized from the legacy
    /// `paramIndex` shape gets its `param_id` filled in by
    /// `resolve_legacy_param_ids` during `on_after_deserialize`.
    #[test]
    fn legacy_param_index_resolved_to_param_id_for_effect_drivers() {
        let mut p = Project::default();
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
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
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
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
    fn legacy_param_index_resolved_for_layer_envelopes() {
        use crate::effects::ParamEnvelope;
        use crate::layer::Layer;
        use crate::types::LayerType;

        let mut p = Project::default();
        let mut layer = Layer::new("test".to_string(), LayerType::Generator, 0);
        // Legacy-shaped envelope targeting Bloom paramIndex 0.
        layer.envelopes_mut().push(ParamEnvelope {
            target_effect_type: EffectTypeId::BLOOM,
            param_id: std::borrow::Cow::Borrowed(""),
            enabled: true,
            attack_beats: 0.1,
            decay_beats: 0.1,
            sustain_level: 0.5,
            release_beats: 0.1,
            target_normalized: 1.0,
            mode: crate::effects::EnvelopeMode::Adsr,
            random_jump: false,
            range_min: 0.0,
            range_max: 1.0,
            legacy_param_index: Some(0),
            current_level: 0.0,
            walk_value: -1.0,
            was_clip_active: false,
            last_elapsed: -1.0,
        });
        p.timeline.layers.push(layer);

        p.resolve_legacy_param_ids();

        let env = &p.timeline.layers[0].envelopes.as_ref().unwrap()[0];
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
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
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
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.param_values = vec![ParamSlot::exposed(0.5)];
        let mut driver_for_bloom = back.clone();
        // (In a real load, the driver lands inside an EffectInstance
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
        let unregistered = crate::EffectTypeId::from_string("not-a-real-effect-id".to_string());
        let mut fx = EffectInstance::new(unregistered);
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

    // ── User-exposed binding node-handle resolution (Phase 3) ─────

    fn build_user_binding(node_handle: &str) -> crate::effects::UserParamBinding {
        crate::effects::UserParamBinding {
            id: format!("user.{}.x.1", node_handle),
            label: "X".to_string(),
            node_handle: node_handle.to_string(),
            inner_param: "x".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            convert: crate::effects::ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
        }
    }

    #[test]
    fn user_binding_resolver_noop_when_no_aliases_declared() {
        // Production case: an effect ships without any node-handle
        // renames declared. The resolver must leave bindings untouched.
        let mut p = Project::default();
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.user_param_bindings
            .push(build_user_binding("uv_transform"));
        fx.user_param_bindings.push(build_user_binding("mix"));
        p.settings.master_effects.push(fx);

        p.resolve_user_param_binding_node_handles();

        let fx = &p.settings.master_effects[0];
        assert_eq!(fx.user_param_bindings[0].node_handle, "uv_transform");
        assert_eq!(fx.user_param_bindings[1].node_handle, "mix");
    }

    #[test]
    fn user_binding_resolver_preserves_handle_when_registry_missing() {
        // RegistryMissing case: effect type isn't registered (e.g.,
        // future-version fixture loaded on an older build). The
        // resolver must leave the handle so a future load with the
        // registry present can recover.
        let mut p = Project::default();
        let mut fx =
            EffectInstance::new(EffectTypeId::from_string("UnknownFutureEffect".to_string()));
        fx.user_param_bindings
            .push(build_user_binding("future_node"));
        p.settings.master_effects.push(fx);

        p.resolve_user_param_binding_node_handles();

        let fx = &p.settings.master_effects[0];
        assert_eq!(
            fx.user_param_bindings[0].node_handle, "future_node",
            "RegistryMissing must preserve node_handle for next-load recovery"
        );
    }

    #[test]
    fn user_binding_resolver_walks_layer_effects() {
        // Layer effects participate too — not just master.
        let mut p = Project::default();
        let mut layer =
            crate::layer::Layer::new("L".to_string(), crate::types::LayerType::Video, 0);
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.user_param_bindings
            .push(build_user_binding("uv_transform"));
        layer.effects = Some(vec![fx]);
        p.timeline.layers.push(layer);

        p.resolve_user_param_binding_node_handles();

        let fx = &p.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert_eq!(fx.user_param_bindings[0].node_handle, "uv_transform");
    }
}
