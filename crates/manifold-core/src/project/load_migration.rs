//! Post-deserialize load migrations for Project (version upgrades, legacy field resolution).

use super::*;

impl Project {
    /// Remove effects with unrecognized types (e.g. removed Unity effects).
    /// Called before on_after_deserialize so they never enter the runtime.
    /// Returns the count removed (BUG-063 — feeds `Project::load_report`).
    pub fn strip_unknown_effects(&mut self) -> usize {
        use crate::preset_type_id::PresetTypeId;
        let mut removed = 0usize;
        let mut strip = |effects: &mut Vec<crate::effects::PresetInstance>| {
            let before = effects.len();
            effects.retain(|fx| *fx.effect_type() != PresetTypeId::UNKNOWN);
            removed += before - effects.len();
        };
        // Master effects
        strip(&mut self.settings.master_effects);
        // Layer effects
        for layer in &mut self.timeline.layers {
            if let Some(ref mut effects) = layer.effects {
                strip(effects);
            }
        }
        removed
    }

    /// Post-deserialization initialization. Rebuild caches and run migrations.
    pub fn on_after_deserialize(&mut self) {
        // Rebuild runtime caches
        self.video_library.rebuild_lookup();
        self.midi_config.rebuild_dictionary();
        self.timeline.rebuild_clip_lookup();
        self.session.rebuild_slot_lookup();

        // Fold a legacy pre-UID `deviceName` into a UID-less AudioDeviceRef so
        // older saves resolve their audio input by name. See
        // `docs/AUDIO_INFRASTRUCTURE.md` §5.
        self.audio_setup.migrate_legacy_device();

        // P2: drain each send's legacy `TriggerRoute`s (pre-unification
        // trigger matrix) into `LayerClipTrigger`s on the resolved target
        // layer. Cross-struct (send -> layer), so — unlike the U5
        // `LegacyAudioTriggerMod` migration, which runs inline inside a
        // single struct's `Deserialize` impl — this can't be a per-struct
        // drain. This Project-level pass, which runs after the whole
        // `Project` (sends AND layers) has deserialized, is the seam. See
        // `docs/AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` §3.2.
        self.migrate_legacy_clip_triggers();

        // §7.2 item 2 (2026-07-11): "Delta" (rate-of-change) left the drawer
        // UI everywhere — no button can toggle or clear it anymore. A saved
        // `rate_of_change: true` would carry invisible behavior the UI can't
        // show, so clear it on load across every carrier. See
        // `docs/AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` §7.2.
        self.clear_legacy_rate_on_flags();

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

        // (The former `align_all_effect_params` / `migrate_all_generator_params`
        // positional-resize passes are gone: the id-keyed manifest is seeded
        // whole on load and never has a length to reconcile — PARAM_STORAGE_DESIGN.md
        // D3.)

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

    /// P2 load migration: for each send, for each legacy `TriggerRoute`,
    /// resolve the target layer — `target_layer` id if set, else auto-route
    /// by send label (the same case-insensitive name match
    /// `manifold-playback::engine::resolve_trigger_layer` used at fire time,
    /// now run once at load) — and push a `LayerClipTrigger` onto that layer:
    /// `source` = (send id, Transients, route band); `shape.sensitivity` =
    /// the route's `sensitivity` verbatim (the exact U5 mapping — rough
    /// approximation, exact-feel fidelity NOT owed); `one_shot_beats` +
    /// `enabled` preserved. Then every send's `triggers` is drained to empty
    /// (never re-populated — the field is `skip_serializing` so it can't come
    /// back). Idempotent: a project with no legacy routes (already migrated,
    /// or never had any) is a no-op. Unresolvable routes (no such layer) are
    /// dropped — counted and named on stderr, never silent. See
    /// `docs/AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` §3.2.
    pub(super) fn migrate_legacy_clip_triggers(&mut self) {
        let mut dropped = 0usize;
        for send_idx in 0..self.audio_setup.sends.len() {
            let routes = std::mem::take(&mut self.audio_setup.sends[send_idx].triggers);
            if routes.is_empty() {
                continue;
            }
            let send_id = self.audio_setup.sends[send_idx].id.clone();
            let send_label = self.audio_setup.sends[send_idx].label.clone();
            for route in routes {
                let target_id = route.target_layer.clone().or_else(|| {
                    self.timeline
                        .layers
                        .iter()
                        .find(|l| l.name.eq_ignore_ascii_case(&send_label))
                        .map(|l| l.layer_id.clone())
                });
                let Some(target_id) = target_id else {
                    dropped += 1;
                    eprintln!(
                        "[Migration] legacy trigger route on send \"{send_label}\" \
                         (band {:?}) resolved to no layer — no target_layer set and no \
                         layer named \"{send_label}\"; dropping.",
                        route.source
                    );
                    continue;
                };
                let Some((_, layer)) = self.timeline.find_layer_by_id_mut(&target_id) else {
                    dropped += 1;
                    eprintln!(
                        "[Migration] legacy trigger route on send \"{send_label}\" \
                         (band {:?}) targeted a layer id that no longer exists; dropping.",
                        route.source
                    );
                    continue;
                };
                let shape = crate::audio_mod::AudioModShape {
                    sensitivity: route.sensitivity,
                    ..crate::audio_mod::AudioModShape::default()
                };
                layer.clip_triggers.push(crate::audio_trigger::LayerClipTrigger {
                    enabled: route.enabled,
                    source: crate::audio_mod::AudioModSource {
                        send_id: send_id.clone(),
                        feature: crate::audio_mod::AudioFeature::new(
                            crate::audio_mod::AudioFeatureKind::Transients,
                            route.source,
                        ),
                    },
                    shape,
                    one_shot_beats: route.one_shot_beats,
                });
            }
        }
        if dropped > 0 {
            log::warn!(
                "[Migration] dropped {dropped} unresolvable legacy trigger route(s) during \
                 clip-trigger migration (see stderr for per-route detail)"
            );
        }
    }

    /// §7.2 item 2 load migration (2026-07-11): "Delta" (rate-of-change)
    /// left the drawer UI everywhere — no button can toggle or clear
    /// `AudioModShape::rate_of_change` anymore, on either carrier. A `true`
    /// flag saved before this migration would carry invisible behavior no
    /// UI can show, so every carrier gets cleared on load: `ParameterAudioMod
    /// .shape` (every `PresetInstance.audio_mods` entry — master/layer/clip
    /// effects and the active layer's generator, via
    /// `for_each_preset_instance_mut`) and `LayerClipTrigger.shape` (every
    /// layer's `clip_triggers`). Counted + `eprintln!`'d, never silent — the
    /// same pattern `migrate_legacy_clip_triggers` uses for its drops. The
    /// runtime field and its `condition()` arm stay compiled for a possible
    /// future re-wire; this migration only clears what a project SAVED
    /// while the now-removed button could still set it. Idempotent: a
    /// project with no `rate_of_change: true` anywhere is a no-op.
    pub(super) fn clear_legacy_rate_on_flags(&mut self) {
        let mut cleared = 0usize;
        self.for_each_preset_instance_mut(|fx| {
            let Some(mods) = fx.audio_mods.as_mut() else { return };
            for m in mods.iter_mut() {
                if m.shape.rate_of_change {
                    m.shape.rate_of_change = false;
                    cleared += 1;
                }
            }
        });
        for layer in &mut self.timeline.layers {
            for cfg in layer.clip_triggers.iter_mut() {
                if cfg.shape.rate_of_change {
                    cfg.shape.rate_of_change = false;
                    cleared += 1;
                }
            }
        }
        if cleared > 0 {
            eprintln!(
                "[Migration] cleared rate_of_change on {cleared} audio-mod config(s) — \
                 \"Delta\" was removed from the drawer UI 2026-07-11 (§7.2 item 2)"
            );
        }
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
    pub(super) fn normalize_override_node_ids(&mut self) {
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
                // Resolve directly on the manifest by id — no registry index.
                let Some(p) = fx.params.get_mut(param_id) else {
                    continue;
                };
                let coerced = p.value.round() as i32;
                if let Some(&(_, to)) = value_aliases.iter().find(|(from, _)| *from == coerced) {
                    p.value = to as f32;
                    // Keep base in sync so a later `reset_param_effectives`
                    // doesn't wipe the migration back.
                    p.base = to as f32;
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
    pub(super) fn resolve_legacy_param_ids(&mut self) {
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
            param_defs: &'a [crate::effects::RegistryParamDef],
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
                if pd.spec.id.is_empty() {
                    return ResolveOutcome::Drop;
                }
                match resolve_param_alias(aliases, pd.spec.id.as_str()) {
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
        // A legacy (pre-EffectId) effect mapping carries a parked
        // `legacy_effect_addr` (layer/master + effect_type); we resolve it to a
        // concrete `EffectId` here by first-match (correct for pre-duplication
        // saves). It may also carry a `legacy_param_index` from the V1.1 shape,
        // resolved against the effect_type the address records. `GenParam`
        // requires the layer alive for generator-type lookup — a missing layer
        // is `RegistryMissing` so the index survives until it reappears.
        fn resolve_macro_mapping(
            mapping: &mut crate::macro_bank::MacroMapping,
            timeline: &crate::timeline::Timeline,
            master_effects: &[(PresetTypeId, EffectId)],
        ) {
            use crate::macro_bank::MacroMappingTarget;
            let legacy_idx = mapping.legacy_param_index;

            // Step 1: resolve a parked legacy effect address → EffectId.
            if let (MacroMappingTarget::Effect { effect_id, .. }, Some(addr)) =
                (&mut mapping.target, &mapping.legacy_effect_addr)
                && effect_id.is_empty()
            {
                let resolved = match &addr.layer_id {
                    None => master_effects
                        .iter()
                        .find(|(ty, _)| ty == &addr.effect_type)
                        .map(|(_, id)| id.clone()),
                    Some(lid) => timeline
                        .layers
                        .iter()
                        .find(|l| l.layer_id == *lid)
                        .and_then(|l| l.effects.as_ref())
                        .and_then(|fx| fx.iter().find(|f| f.effect_type() == &addr.effect_type))
                        .map(|f| f.id.clone()),
                };
                if let Some(id) = resolved {
                    *effect_id = id;
                }
            }

            // Step 2: resolve a parked legacy param index → param_id. A macro
            // mapping only ever has a legacy index in the legacy shape, so the
            // effect_type comes from the parked address.
            let outcome = match (&mapping.target, &mapping.legacy_effect_addr) {
                (MacroMappingTarget::Effect { param_id, .. }, Some(addr)) => {
                    resolve_for_effect(&addr.effect_type, param_id, legacy_idx)
                }
                (MacroMappingTarget::GenParam { layer_id, param_id }, _) => {
                    match timeline
                        .layers
                        .iter()
                        .find(|l| l.layer_id == *layer_id)
                        .and_then(|l| l.gen_params())
                    {
                        Some(gp) => {
                            resolve_for_generator(gp.generator_type(), param_id, legacy_idx)
                        }
                        None => ResolveOutcome::RegistryMissing,
                    }
                }
                // Opacity, or a native EffectId mapping (no legacy data).
                _ => ResolveOutcome::Drop,
            };

            match (&mut mapping.target, outcome) {
                (_, ResolveOutcome::NoChange | ResolveOutcome::Drop) => {
                    mapping.legacy_param_index = None;
                }
                (
                    MacroMappingTarget::Effect { param_id, .. }
                    | MacroMappingTarget::GenParam { param_id, .. },
                    ResolveOutcome::Update(id),
                ) => {
                    *param_id = std::borrow::Cow::Owned(id);
                    mapping.legacy_param_index = None;
                }
                (_, ResolveOutcome::Update(_)) => {
                    mapping.legacy_param_index = None;
                }
                (_, ResolveOutcome::RegistryMissing) => {
                    // Preserve legacy index for next-load recovery.
                }
            }

            // Once the EffectId is resolved, drop the parked address so it
            // isn't re-emitted as a legacy shape on the next save.
            if let MacroMappingTarget::Effect { effect_id, .. } = &mapping.target
                && !effect_id.is_empty()
            {
                mapping.legacy_effect_addr = None;
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

        // Macro mappings live on the bank. Resolving a legacy effect address
        // needs the master-effect chain (type → id) and `GenParam` needs the
        // generator type via the layer. Snapshot the master chain into an owned
        // (type, id) table first so its borrow ends before the mutable bank
        // borrow; `timeline` is a disjoint field of `Project`.
        let master_effects: Vec<(PresetTypeId, EffectId)> = self
            .settings
            .master_effects
            .iter()
            .map(|f| (f.effect_type().clone(), f.id.clone()))
            .collect();
        let timeline = &self.timeline;
        for slot in &mut self.settings.macro_bank.slots {
            for mapping in &mut slot.mappings {
                resolve_macro_mapping(mapping, timeline, &master_effects);
            }
        }
    }

    /// Load-time cosmetic pass (P1, D2): a legacy project-embedded fork
    /// minted before ids became display-based carries a `base#N` id whose
    /// `display_name` was never separately set. The card used to derive a
    /// "(variant)" label from the id at render time (`card_preset_name`'s
    /// `'#'` split, now deleted) — stamp an equivalent readable name once at
    /// load instead, so old projects still read cleanly. Never touches an
    /// entry that already has a `display_name` (new forks set it directly;
    /// this is purely for entries the previous, display-name-less minting
    /// path left behind). Returns the number of presets backfilled.
    pub fn backfill_legacy_fork_display_names(&mut self) -> usize {
        let mut n = 0;
        for ep in &mut self.embedded_presets {
            let Some(meta) = ep.def.preset_metadata.as_mut() else {
                continue;
            };
            if !meta.display_name.is_empty() {
                continue;
            }
            if let Some((base, _)) = meta.id.as_str().split_once('#') {
                meta.display_name = format!("{base} (variant)");
                n += 1;
            }
        }
        n
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
}
