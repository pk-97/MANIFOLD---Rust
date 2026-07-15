use crate::PresetTypeId;
use crate::id::EffectId;
use crate::effect_graph_def::EffectGraphDef;
use crate::midi::MidiMappingConfig;
use crate::preset_def::PresetKind;
use crate::recording::RecordingProvenance;
use crate::session::SessionGrid;
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
    /// `Saved` = user pressed "Save to Project" / "Make Unique" / import
    /// (PRESET_LIBRARY_DESIGN D4/D9) — deliberate, resolves ON TOP of stock/
    /// user disk tiers. `Snapshot` = auto-captured at save for
    /// self-containment (D5) — pruned + refreshed every save, resolves
    /// BELOW disk (disk wins over a stale snapshot; the snapshot is the
    /// fallback when the library file is gone). Defaults to `Saved` so
    /// legacy files with no `origin` field keep today's on-top behavior.
    #[serde(default)]
    pub origin: EmbeddedOrigin,
}

/// See [`EmbeddedPreset::origin`].
#[derive(Default, Serialize, Deserialize, PartialEq, Eq, Clone, Copy, Debug)]
pub enum EmbeddedOrigin {
    #[default]
    Saved,
    Snapshot,
}

impl EmbeddedPreset {
    /// The preset's stable id (from its metadata), or `None` if unset.
    pub fn id(&self) -> Option<&crate::PresetTypeId> {
        self.def.preset_metadata.as_ref().map(|m| &m.id)
    }
}

/// The schema version this build writes and is the newest it can open. Bumped
/// by every migration step that changes on-disk field shape; the migrate chain's
/// final target and the forward-compat guard both read it. Single source of truth.
pub const CURRENT_PROJECT_VERSION: &str = "1.12.0";

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
    /// Audio input routing + named sends for audio modulation. Parallel to
    /// `midi_config`. Skipped on serialize when empty so projects that never
    /// configured audio round-trip byte-identically. See
    /// `docs/AUDIO_MODULATION_DESIGN.md`.
    #[serde(default, skip_serializing_if = "crate::audio_setup::AudioSetup::is_empty")]
    pub audio_setup: crate::audio_setup::AudioSetup,
    #[serde(default)]
    pub settings: ProjectSettings,
    #[serde(default)]
    pub tempo_map: TempoMap,
    #[serde(default)]
    pub recording_provenance: RecordingProvenance,
    #[serde(skip)]
    pub last_saved_path: String,
    #[serde(default)]
    pub saved_playhead_time: f32,

    /// What the last load silently repaired (unknown effects stripped,
    /// overlapping clips removed, orphaned references purged, missing media
    /// files). Transient runtime state, recomputed every load — never
    /// serialized, exactly the `clip.layer_id` pattern (`BUG-063`,
    /// `docs/PROJECT_FILE_INTEGRITY_DESIGN.md` §3.6).
    #[serde(skip)]
    pub load_report: LoadReport,

    /// Project-scoped presets ("forks") — self-contained preset defs that live
    /// in this project rather than the global catalog. Resolved by id via the
    /// catalog overlay when the project loads. Empty for projects that have
    /// never forked a preset; skipped on serialize when empty so existing
    /// fixtures round-trip byte-identically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedded_presets: Vec<EmbeddedPreset>,

    /// Session-mode grid: scenes (rows) x layer slots, launched live like
    /// Ableton session clips. Skipped on serialize when empty so projects
    /// that never touch session mode round-trip byte-identically. See
    /// `docs/SESSION_MODE_DESIGN.md`.
    #[serde(default, skip_serializing_if = "SessionGrid::is_empty")]
    pub session: SessionGrid,

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
    fn migrate_legacy_clip_triggers(&mut self) {
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
    fn clear_legacy_rate_on_flags(&mut self) {
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

    /// Whether any layer has an enabled [`crate::audio_trigger::LayerClipTrigger`]
    /// — the P2 replacement for `AudioSend::has_active_triggers()`, which now
    /// only ever reads drained (always-empty) legacy storage.
    pub fn has_active_clip_triggers(&self) -> bool {
        self.timeline
            .layers
            .iter()
            .any(|l| l.clip_triggers.iter().any(|c| c.enabled))
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

    /// Mint a human-readable, collision-free id for an explicit fork (Make
    /// Unique / Import — `ForkPresetCommand`) — a `base " {n}"` probe (e.g.
    /// `"Bloom 2"`) instead of `mint_embedded_preset_id`'s `base#{n}`. Starts
    /// at 2 so the first fork of a preset reads as its second instance, not
    /// literally "1" (D2: the design supersedes attempt #8's `#N` variant
    /// ids). The minted string is written to BOTH the embedded preset's id
    /// and its `display_name` — the id itself is now display-based, so the
    /// card can render it directly with no id-format parsing. Legacy `#N`
    /// ids already in a project keep resolving unchanged; this only changes
    /// what NEW forks mint.
    pub fn mint_forked_preset_id(&self, base: &str) -> PresetTypeId {
        let mut n = 2;
        loop {
            let candidate = format!("{base} {n}");
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

    /// Every `(id, kind)` referenced by a TRACKING instance (`graph: None`)
    /// anywhere in the project — master effects, every layer's effects,
    /// every clip's effects, and every layer's generator. A diverged
    /// instance (`graph: Some`) already carries its own private copy; its
    /// library id (still named by `effect_type`, D8) is not collected here
    /// because no self-containment snapshot is needed for it.
    ///
    /// Used at save time (PRESET_LIBRARY_DESIGN D5) to know which library
    /// ids need their current def cached into `embedded_presets` as
    /// `origin: Snapshot`. Renderer-free (reads only instance state), so it
    /// lives in core; the actual catalog lookup + upsert happens app-side
    /// (see `manifold-app::project_io::snapshot_and_prune_embedded_presets`),
    /// which has both this project AND the renderer's live catalog.
    pub fn tracking_preset_ids(&self) -> Vec<(PresetTypeId, PresetKind)> {
        fn collect(
            fx: &crate::effects::PresetInstance,
            kind: PresetKind,
            out: &mut Vec<(PresetTypeId, PresetKind)>,
        ) {
            if fx.graph.is_none() {
                out.push((fx.effect_type().clone(), kind));
            }
        }
        let mut out = Vec::new();
        for fx in &self.settings.master_effects {
            collect(fx, PresetKind::Effect, &mut out);
        }
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref() {
                for fx in effects {
                    collect(fx, PresetKind::Effect, &mut out);
                }
            }
            for clip in &layer.clips {
                for fx in &clip.effects {
                    collect(fx, PresetKind::Effect, &mut out);
                }
            }
            if let Some(gp) = layer.gen_params() {
                collect(gp, PresetKind::Generator, &mut out);
            }
        }
        out
    }

    /// Mutable-walk sibling of [`Self::tracking_preset_ids`]: visits every
    /// `PresetInstance` home in the project — master effects, every layer's
    /// effects, every clip's effects, every layer's generator — INCLUDING
    /// diverged instances (`graph: Some`), unlike the read-only walk above.
    /// A diverged instance still deserializes its own `params` wire map and
    /// still needs it reconciled, so this walker doesn't filter by
    /// `graph.is_none()` the way `tracking_preset_ids` does.
    fn for_each_preset_instance_mut(
        &mut self,
        mut f: impl FnMut(&mut crate::effects::PresetInstance),
    ) {
        for fx in &mut self.settings.master_effects {
            f(fx);
        }
        for layer in &mut self.timeline.layers {
            if let Some(effects) = layer.effects.as_mut() {
                for fx in effects {
                    f(fx);
                }
            }
            for clip in &mut layer.clips {
                for fx in &mut clip.effects {
                    f(fx);
                }
            }
            if let Some(gp) = layer.gen_params_mut() {
                f(gp);
            }
        }
    }

    /// Rebuild every instance's `ParamManifest` from its stashed wire entries
    /// against the CURRENT registry (PARAM_STORAGE_BOUNDARIES_DESIGN.md D1) —
    /// call after the project's embedded presets are installed. Idempotent:
    /// instances with no stash (freshly constructed, or already resolved by
    /// an earlier call) are untouched. Walks exactly the homes
    /// `tracking_preset_ids` walks (its mut sibling, above).
    ///
    /// Returns how many instances still have an unresolved preset template
    /// after this pass (`PresetInstance::template_unresolved`) — BUG-079:
    /// the loader folds this into `Project::load_report` so a missing
    /// preset def surfaces on-screen instead of only in an `eprintln`.
    pub fn reconcile_param_manifests(&mut self) -> usize {
        let mut unresolved = 0;
        self.for_each_preset_instance_mut(|fx| {
            fx.reconcile_manifest();
            if fx.template_unresolved() {
                unresolved += 1;
            }
        });
        unresolved
    }

    /// Remove every `Snapshot`-origin embedded preset whose id is not in
    /// `referenced` (D5) — the stale-snapshot prune that keeps the overlay
    /// from accumulating ids no tracking instance uses anymore (e.g. after
    /// an instance is retargeted or deleted). `Saved` entries are never
    /// touched here — they are a deliberate project-scoped fork, not
    /// save-time plumbing, and survive independent of what's referenced.
    pub fn prune_stale_snapshots(&mut self, referenced: &std::collections::HashSet<PresetTypeId>) {
        self.embedded_presets.retain(|p| {
            p.origin == EmbeddedOrigin::Saved || p.id().is_some_and(|id| referenced.contains(id))
        });
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
            origin: EmbeddedOrigin::Saved,
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

    /// The owning layer for an effect/clip-effect instance, or `None` for a
    /// master-chain effect (or an unknown id). Used where an `EffectId` needs
    /// its container — e.g. labelling an EffectId-addressed macro mapping.
    pub fn layer_id_for_effect(&self, effect_id: &crate::id::EffectId) -> Option<crate::id::LayerId> {
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref()
                && effects.iter().any(|fx| &fx.id == effect_id)
            {
                return Some(layer.layer_id.clone());
            }
            for clip in &layer.clips {
                if clip.effects.iter().any(|fx| &fx.id == effect_id) {
                    return Some(layer.layer_id.clone());
                }
            }
        }
        None
    }

    /// Whether any enabled audio modulation exists anywhere in the project —
    /// the gate the content thread uses to decide whether to run audio capture
    /// at all. Walks the same instance set the modulation pipeline evaluates:
    /// master effects, every layer's effects, and every layer's generator
    /// instance (NOT clip effects, which the pipeline neither resets nor
    /// modulates). Cheap — most instances carry no audio mods, short-circuiting
    /// on the `Option`.
    /// Send ids with at least one ENABLED audio mod reading `Pitch` or
    /// `Presence` — the D7 activation set (docs/AUDIO_OBJECT_TRACKING_DESIGN.md
    /// P4). The audio-mod runtime recomputes this only on a data-version
    /// change and switches each send analyzer's ridge tracker on/off with it,
    /// so projects that never bind pitch pay nothing (the tracker path is
    /// byte-identical when off — tested in manifold-audio).
    pub fn sends_with_pitch_mods(&self) -> ahash::AHashSet<crate::AudioSendId> {
        let mut out = ahash::AHashSet::new();
        let mut collect = |fx: &crate::effects::PresetInstance| {
            if let Some(mods) = fx.audio_mods.as_ref() {
                for m in mods.iter().filter(|m| m.enabled) {
                    if matches!(
                        m.source.feature.kind,
                        crate::audio_mod::AudioFeatureKind::Pitch | crate::audio_mod::AudioFeatureKind::Presence
                    ) {
                        out.insert(m.source.send_id.clone());
                    }
                }
            }
        };
        for fx in &self.settings.master_effects {
            collect(fx);
        }
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref() {
                for fx in effects {
                    collect(fx);
                }
            }
            if let Some(gp) = layer.gen_params() {
                collect(gp);
            }
        }
        out
    }

    pub fn has_active_audio_mods(&self) -> bool {
        fn inst_has(fx: &crate::effects::PresetInstance) -> bool {
            // §9 U4: a fire-mode (trigger-gate) mod is a normal `audio_mods`
            // entry now — no separate `audio_trigger` config to special-case,
            // so this plain check already covers it.
            fx.audio_mods
                .as_ref()
                .is_some_and(|v| v.iter().any(|a| a.enabled))
        }
        if self.settings.master_effects.iter().any(inst_has) {
            return true;
        }
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref()
                && effects.iter().any(inst_has)
            {
                return true;
            }
            if layer.gen_params().is_some_and(inst_has) {
                return true;
            }
        }
        false
    }

    /// Send ids the analysis runtime should actually spend cycles on: every
    /// send with at least one ENABLED audio mod reading it, plus every send
    /// with at least one enabled `LayerClipTrigger` sourcing it. Walks the
    /// same instance set [`Self::has_active_audio_mods`] does (master
    /// effects, layer effects, layer generator params — NOT clip effects,
    /// mirroring that function), plus every layer's `clip_triggers` (P2 —
    /// the §3.4 walker arm; a send-owned `AudioSend::triggers` is drained
    /// legacy storage now and is never read here again).
    /// `AudioModRuntime` recomputes this only on a data-version change and
    /// skips every send outside the set (unless it's the scope-tapped send) —
    /// see `docs/AUDIO_SENDS_UX_DESIGN.md` D4/§3.2,
    /// `docs/AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` §3.4.
    pub fn analysis_consumed_sends(&self) -> ahash::AHashSet<crate::AudioSendId> {
        // A plain fn (not a closure capturing `out`) so it can be called
        // alongside the direct `out.insert` the clip-trigger arm below needs —
        // a closure borrowing `out` mutably would keep it borrowed for the
        // whole loop and conflict with that direct access.
        fn collect(
            out: &mut ahash::AHashSet<crate::AudioSendId>,
            fx: &crate::effects::PresetInstance,
        ) {
            // §9 U4: a fire-mode mod is just an enabled `audio_mods` entry,
            // already covered below — no separate arm needed.
            if let Some(mods) = fx.audio_mods.as_ref() {
                for m in mods.iter().filter(|m| m.enabled) {
                    out.insert(m.source.send_id.clone());
                }
            }
        }
        let mut out = ahash::AHashSet::new();
        for fx in &self.settings.master_effects {
            collect(&mut out, fx);
        }
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref() {
                for fx in effects {
                    collect(&mut out, fx);
                }
            }
            if let Some(gp) = layer.gen_params() {
                collect(&mut out, gp);
            }
            for ct in layer.clip_triggers.iter().filter(|c| c.enabled) {
                out.insert(ct.source.send_id.clone());
            }
        }
        out
    }

    /// Number of parameters whose modulation references `send_id` (enabled or
    /// not), across master effects, layer effects, and generator params. Used to
    /// warn before deleting a send that sliders still depend on.
    pub fn audio_send_usage_count(&self, send_id: &crate::id::AudioSendId) -> usize {
        fn inst_count(fx: &crate::effects::PresetInstance, send_id: &crate::id::AudioSendId) -> usize {
            // §9 U4: a fire-mode mod is a normal `audio_mods` entry — already
            // counted below, no separate arm needed.
            fx.audio_mods
                .as_ref()
                .map(|v| v.iter().filter(|a| &a.source.send_id == send_id).count())
                .unwrap_or(0)
        }
        let mut count = self
            .settings
            .master_effects
            .iter()
            .map(|fx| inst_count(fx, send_id))
            .sum::<usize>();
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref() {
                count += effects.iter().map(|fx| inst_count(fx, send_id)).sum::<usize>();
            }
            if let Some(gp) = layer.gen_params() {
                count += inst_count(gp, send_id);
            }
        }
        count
    }

    /// Every ENABLED audio mod reading `send_id`, resolved to a legible
    /// `(owning layer, "LayerName \u{2022} EffectName \u{2022} ParamName")` pair — the
    /// Audio Setup panel's Consumers section (`docs/AUDIO_SENDS_UX_DESIGN.md`
    /// D1/D3). `layer_id` is `None` for a master-effects mod (nothing to jump
    /// to; the label reads "Master" instead). Walks the same instance set
    /// [`Self::audio_send_usage_count`] does.
    pub fn audio_mod_consumers(&self, send_id: &crate::id::AudioSendId) -> Vec<(Option<crate::id::LayerId>, String)> {
        fn collect(
            layer_id: Option<crate::id::LayerId>,
            layer_name: &str,
            fx: &crate::effects::PresetInstance,
            send_id: &crate::id::AudioSendId,
            out: &mut Vec<(Option<crate::id::LayerId>, String)>,
        ) {
            // §9 U4: a fire-mode mod is a normal `audio_mods` entry — already
            // listed below by its own param name (no more bespoke "Trigger"
            // label; the param the gate card lives on names itself).
            if let Some(mods) = fx.audio_mods.as_ref() {
                for m in mods.iter().filter(|m| m.enabled && &m.source.send_id == send_id) {
                    let effect_name = crate::preset_type_registry::display_name(fx.effect_type());
                    let param_name = fx
                        .params
                        .get(&m.param_id)
                        .map(|p| p.spec.name.clone())
                        .unwrap_or_else(|| m.param_id.to_string());
                    out.push((
                        layer_id.clone(),
                        format!("{layer_name} \u{2022} {effect_name} \u{2022} {param_name}"),
                    ));
                }
            }
        }
        let mut out = Vec::new();
        for fx in &self.settings.master_effects {
            collect(None, "Master", fx, send_id, &mut out);
        }
        for layer in &self.timeline.layers {
            if let Some(effects) = layer.effects.as_ref() {
                for fx in effects {
                    collect(Some(layer.layer_id.clone()), &layer.name, fx, send_id, &mut out);
                }
            }
            if let Some(gp) = layer.gen_params() {
                collect(Some(layer.layer_id.clone()), &layer.name, gp, send_id, &mut out);
            }
        }
        out
    }

    /// Consumers for the Audio Setup panel's Consumers section that are
    /// layer-owned `LayerClipTrigger` configs (P3, D2) rather than
    /// `PresetInstance` audio mods — the walk `audio_mod_consumers` above
    /// can't reach, since a clip trigger has no `param_id`/effect to name.
    /// Mirrors that method's shape: `(owning layer, display label)`, enabled
    /// configs sourcing `send_id` only. Label format is "Clip trigger •
    /// Layer • Band" (§7.2 item 7, P8, 2026-07-11 — matches the mod rows'
    /// "Layer • Effect • Param" bullet convention instead of the deleted
    /// Triggers matrix's arrow style, "Low → LayerName").
    pub fn clip_trigger_consumers(
        &self,
        send_id: &crate::id::AudioSendId,
    ) -> Vec<(Option<crate::id::LayerId>, String)> {
        let mut out = Vec::new();
        for layer in &self.timeline.layers {
            for cfg in &layer.clip_triggers {
                if !cfg.enabled || &cfg.source.send_id != send_id {
                    continue;
                }
                let feature = cfg.source.feature;
                let feature_label = match feature.kind {
                    crate::audio_mod::AudioFeatureKind::Transients => {
                        feature.band.label().to_string()
                    }
                    crate::audio_mod::AudioFeatureKind::Kick => "Kick".to_string(),
                    kind => format!("{} {}", kind.label(), feature.band.label()),
                };
                out.push((
                    Some(layer.layer_id.clone()),
                    format!("Clip trigger \u{2022} {} \u{2022} {feature_label}", layer.name),
                ));
            }
        }
        out
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

/// What the last `load_project` call silently repaired. Written by both
/// post-load call sites (`strip_unknown_effects` inside
/// `load_project_from_json_with`; `run_post_load_validation`'s overlap
/// repair, orphan purge, and missing-file detection) into
/// `Project::load_report` — the single owner of "what this load altered"
/// (`BUG-063`, `docs/PROJECT_FILE_INTEGRITY_DESIGN.md` §3.6).
#[derive(Debug, Clone, Default)]
pub struct LoadReport {
    pub unknown_effects_removed: usize,
    pub overlapping_clips_repaired: usize,
    pub orphaned_clips_purged: usize,
    pub orphaned_midi_purged: usize,
    pub missing_media_files: Vec<String>,
    /// BUG-079: preset instances whose def couldn't be resolved at load
    /// (deleted/unregistered/missing) — kept on a placeholder spec
    /// (keep-don't-drop) rather than dropped. Was console-only (`eprintln!`
    /// in `effects.rs` / `preset_runtime.rs`) before this field existed.
    pub unresolved_preset_templates: usize,
}

impl LoadReport {
    pub fn is_empty(&self) -> bool {
        self.unknown_effects_removed == 0
            && self.overlapping_clips_repaired == 0
            && self.orphaned_clips_purged == 0
            && self.orphaned_midi_purged == 0
            && self.missing_media_files.is_empty()
            && self.unresolved_preset_templates == 0
    }

    /// One human line per non-zero entry, e.g. "3 unknown effects removed".
    /// Singular/plural is correct for count == 1.
    pub fn human_lines(&self) -> Vec<String> {
        fn plural(n: usize, singular: &str, plural: &str) -> String {
            if n == 1 {
                singular.to_string()
            } else {
                plural.to_string()
            }
        }

        let mut lines = Vec::new();
        if self.unknown_effects_removed > 0 {
            lines.push(format!(
                "{} unknown {} removed",
                self.unknown_effects_removed,
                plural(self.unknown_effects_removed, "effect", "effects")
            ));
        }
        if self.overlapping_clips_repaired > 0 {
            lines.push(format!(
                "{} overlapping {} repaired",
                self.overlapping_clips_repaired,
                plural(self.overlapping_clips_repaired, "clip", "clips")
            ));
        }
        if self.orphaned_clips_purged > 0 {
            lines.push(format!(
                "{} orphaned {} purged",
                self.orphaned_clips_purged,
                plural(self.orphaned_clips_purged, "clip", "clips")
            ));
        }
        if self.orphaned_midi_purged > 0 {
            lines.push(format!(
                "{} orphaned MIDI {} purged",
                self.orphaned_midi_purged,
                plural(self.orphaned_midi_purged, "mapping", "mappings")
            ));
        }
        if !self.missing_media_files.is_empty() {
            lines.push(format!(
                "{} missing media {}",
                self.missing_media_files.len(),
                plural(self.missing_media_files.len(), "file", "files")
            ));
        }
        if self.unresolved_preset_templates > 0 {
            lines.push(format!(
                "{} unresolved preset {} kept with saved values (preset not registered)",
                self.unresolved_preset_templates,
                plural(self.unresolved_preset_templates, "reference", "references")
            ));
        }
        lines
    }
}

impl Default for Project {
    fn default() -> Self {
        Self {
            project_name: String::new(),
            project_version: CURRENT_PROJECT_VERSION.to_string(),
            timeline: Timeline::default(),
            video_library: VideoLibrary::default(),
            midi_config: MidiMappingConfig::default(),
            audio_setup: crate::audio_setup::AudioSetup::default(),
            settings: ProjectSettings::default(),
            tempo_map: TempoMap::default(),
            recording_provenance: RecordingProvenance::default(),
            last_saved_path: String::new(),
            saved_playhead_time: 0.0,
            load_report: LoadReport::default(),
            embedded_presets: Vec::new(),
            session: SessionGrid::default(),
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
    use crate::effects::{ParamId, ParameterDriver, PresetInstance};
    use crate::types::{BeatDivision, DriverWaveform};

    /// Build a bundled test [`crate::params::Param`] (mirrors
    /// `effects::tests::slot`, kept local since that helper is private to
    /// `effects.rs`'s own test module).
    fn slot(id: &str, value: f32, exposed: bool) -> crate::params::Param {
        let spec = crate::effect_graph_def::ParamSpecDef {
            id: id.to_string(),
            name: String::new(),
            min: 0.0,
            max: 1.0,
            default_value: value,
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
        };
        let mut p = crate::params::Param::bundled(spec);
        p.value = value;
        p.base = value;
        p.exposed = exposed;
        p
    }

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
    fn fork_preset_mints_id_and_retargets_instance() {
        let mut p = Project::default();
        let fx = PresetInstance::new(PresetTypeId::BLOOM);
        let fx_id = fx.id.clone();
        p.settings.master_effects.push(fx);

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
    fn mint_forked_preset_id_starts_at_2_and_probes_past_collisions() {
        let mut p = Project::default();
        // No embedded presets yet: first fork of "Bloom" reads as "Bloom 2",
        // not "Bloom 1" (D2 — a fork is presented as the preset's second
        // instance).
        assert_eq!(p.mint_forked_preset_id("Bloom").as_str(), "Bloom 2");

        p.embedded_presets.push(EmbeddedPreset {
            kind: PresetKind::Effect,
            def: graph_def_with_id("Bloom 2", "Bloom 2"),
            origin: EmbeddedOrigin::Saved,
        });
        // "Bloom 2" is taken — probes to "Bloom 3".
        assert_eq!(p.mint_forked_preset_id("Bloom").as_str(), "Bloom 3");
    }

    #[test]
    fn backfill_legacy_fork_display_names_derives_variant_label_from_id() {
        let mut p = Project::default();
        p.embedded_presets.push(EmbeddedPreset {
            kind: PresetKind::Effect,
            def: graph_def_with_id("Bloom#1", ""),
            origin: EmbeddedOrigin::Saved,
        });
        // A new-style fork already carries its own display_name — untouched.
        p.embedded_presets.push(EmbeddedPreset {
            kind: PresetKind::Effect,
            def: graph_def_with_id("Bloom 2", "Bloom 2"),
            origin: EmbeddedOrigin::Saved,
        });

        let n = p.backfill_legacy_fork_display_names();
        assert_eq!(n, 1);
        assert_eq!(
            p.embedded_preset(&PresetTypeId::from_string("Bloom#1".to_string()))
                .unwrap()
                .def
                .preset_metadata
                .as_ref()
                .unwrap()
                .display_name,
            "Bloom (variant)"
        );
        assert_eq!(
            p.embedded_preset(&PresetTypeId::from_string("Bloom 2".to_string()))
                .unwrap()
                .def
                .preset_metadata
                .as_ref()
                .unwrap()
                .display_name,
            "Bloom 2"
        );
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
            origin: EmbeddedOrigin::Saved,
        });

        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("embeddedPresets"), "non-empty must serialize: {json}");
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.embedded_presets.len(), 1);
        assert_eq!(back.embedded_presets[0].kind, PresetKind::Generator);
        assert_eq!(back.embedded_presets[0].origin, EmbeddedOrigin::Saved);
        assert_eq!(
            back.embedded_presets[0].id().map(|i| i.as_str()),
            Some("OilyFluid#layer2")
        );
    }

    /// D5: `origin` round-trips for BOTH variants, and a legacy embedded
    /// preset with no `origin` field on the wire (pre-P2 project files)
    /// loads as `Saved` — the deliberate, on-top-of-disk default that
    /// matches what those files' entries always meant before `Snapshot`
    /// existed.
    #[test]
    fn embedded_preset_origin_round_trips_both_variants_and_defaults_legacy_to_saved() {
        let mut p = Project::default();
        p.embedded_presets.push(EmbeddedPreset {
            kind: PresetKind::Effect,
            def: graph_def_with_id("Bloom 2", "Bloom 2"),
            origin: EmbeddedOrigin::Saved,
        });
        p.embedded_presets.push(EmbeddedPreset {
            kind: PresetKind::Effect,
            def: graph_def_with_id("Bloom", "Bloom"),
            origin: EmbeddedOrigin::Snapshot,
        });

        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"origin\":\"Saved\""), "Saved must serialize: {json}");
        assert!(json.contains("\"origin\":\"Snapshot\""), "Snapshot must serialize: {json}");

        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.embedded_preset(&PresetTypeId::from_string("Bloom 2".to_string()))
                .unwrap()
                .origin,
            EmbeddedOrigin::Saved
        );
        assert_eq!(
            back.embedded_preset(&PresetTypeId::from_string("Bloom".to_string()))
                .unwrap()
                .origin,
            EmbeddedOrigin::Snapshot
        );

        // Legacy shape: an embedded preset JSON object with no `origin` key
        // at all (as every pre-P2 project file has) must default to Saved.
        // `origin` is the last field in `EmbeddedPreset`, so it serializes
        // with a LEADING comma and no trailing one.
        let legacy_json = json.replacen(",\"origin\":\"Snapshot\"", "", 1);
        assert_eq!(
            legacy_json.matches("\"origin\"").count(),
            1,
            "sanity: exactly the Snapshot entry's origin key must be gone \
             (the untouched Saved entry still carries its own): {legacy_json}"
        );
        let legacy_back: Project = serde_json::from_str(&legacy_json).unwrap();
        assert_eq!(
            legacy_back
                .embedded_preset(&PresetTypeId::from_string("Bloom".to_string()))
                .unwrap()
                .origin,
            EmbeddedOrigin::Saved,
            "no `origin` field on the wire must default to Saved"
        );
    }

    /// Step 8 regression: a driver deserialized from the legacy
    /// `paramIndex` shape gets its `param_id` filled in by
    /// `resolve_legacy_param_ids` during `on_after_deserialize`.
    #[test]
    fn legacy_param_index_resolved_to_param_id_for_effect_drivers() {
        let mut p = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.5, true)]);
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
            free_period_beats: None,
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
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.5, true)]);
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
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.5, true)]);
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
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.5, true)]);
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
            free_period_beats: None,
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
            free_period_beats: None,
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
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.5, true)]);
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
        fx.params = crate::params::ParamManifest::from_params(vec![
            slot("p0", 0.5, true),
            slot("p1", 0.5, true),
            slot("p2", 0.5, true),
        ]);
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
            free_period_beats: None,
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

    // ─── analysis_consumed_sends (AUDIO_SENDS_UX_DESIGN D4) ───

    fn send_with_id(label: &str, id: &str) -> crate::audio_setup::AudioSend {
        let mut s = crate::audio_setup::AudioSend::new(label);
        s.id = crate::AudioSendId::new(id);
        s
    }

    fn amplitude_mod(send_id: crate::AudioSendId) -> crate::audio_mod::ParameterAudioMod {
        crate::audio_mod::ParameterAudioMod::new(
            ParamId::from("amount"),
            send_id,
            crate::audio_mod::AudioFeature::new(
                crate::audio_mod::AudioFeatureKind::Amplitude,
                crate::audio_mod::AudioBand::Full,
            ),
        )
    }

    #[test]
    fn analysis_consumed_sends_includes_only_the_send_with_an_enabled_mod() {
        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        let send_b = send_with_id("B", "send-b");
        p.audio_setup.sends.push(send_a.clone());
        p.audio_setup.sends.push(send_b.clone());

        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.audio_mods_mut().push(amplitude_mod(send_a.id.clone()));
        p.settings.master_effects.push(fx);

        let consumed = p.analysis_consumed_sends();
        assert_eq!(consumed.len(), 1);
        assert!(consumed.contains(&send_a.id));
        assert!(!consumed.contains(&send_b.id));
    }

    #[test]
    fn analysis_consumed_sends_is_empty_when_the_only_mod_is_disabled() {
        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        p.audio_setup.sends.push(send_a.clone());

        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        let m = fx.audio_mods_mut();
        m.push(amplitude_mod(send_a.id.clone()));
        m[0].enabled = false;
        p.settings.master_effects.push(fx);

        assert!(p.analysis_consumed_sends().is_empty());
    }

    /// A layer with one clip-trigger config sourcing `send_id`, `enabled` as
    /// given. Test helper for the P2 layer-owned trigger tests below.
    fn layer_with_clip_trigger(
        send_id: crate::AudioSendId,
        band: crate::audio_mod::AudioBand,
        enabled: bool,
    ) -> crate::layer::Layer {
        let mut layer = crate::layer::Layer::new("L".to_string(), crate::types::LayerType::Video, 0);
        let mut cfg = crate::audio_trigger::LayerClipTrigger::new(crate::audio_mod::AudioModSource {
            send_id,
            feature: crate::audio_mod::AudioFeature::new(
                crate::audio_mod::AudioFeatureKind::Transients,
                band,
            ),
        });
        cfg.enabled = enabled;
        layer.clip_triggers.push(cfg);
        layer
    }

    #[test]
    fn analysis_consumed_sends_includes_send_with_enabled_layer_clip_trigger_and_no_mod() {
        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        p.audio_setup.sends.push(send_a.clone());
        p.timeline.layers.push(layer_with_clip_trigger(
            send_a.id.clone(),
            crate::audio_mod::AudioBand::Low,
            true,
        ));

        let consumed = p.analysis_consumed_sends();
        assert_eq!(consumed.len(), 1);
        assert!(consumed.contains(&send_a.id));
    }

    #[test]
    fn analysis_consumed_sends_excludes_send_with_disabled_layer_clip_trigger() {
        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        p.audio_setup.sends.push(send_a.clone());
        // `enabled: false` — layer_with_clip_trigger's third arg.
        p.timeline.layers.push(layer_with_clip_trigger(
            send_a.id.clone(),
            crate::audio_mod::AudioBand::Low,
            false,
        ));

        assert!(p.analysis_consumed_sends().is_empty());
    }

    #[test]
    fn analysis_consumed_sends_ignores_drained_legacy_send_triggers() {
        // §3.4: `send.triggers` is deserialize-only legacy storage now —
        // even if something hand-populates it (bypassing the load
        // migration), `analysis_consumed_sends` must never read it again.
        let mut p = Project::default();
        let mut send_a = send_with_id("A", "send-a");
        let mut route = crate::audio_trigger::TriggerRoute::new(crate::audio_mod::AudioBand::Low);
        route.enabled = true;
        send_a.triggers.push(route);
        p.audio_setup.sends.push(send_a);

        assert!(p.analysis_consumed_sends().is_empty());
    }

    #[test]
    fn has_active_clip_triggers_true_only_when_some_layer_has_an_enabled_config() {
        let mut p = Project::default();
        assert!(!p.has_active_clip_triggers());

        p.timeline.layers.push(layer_with_clip_trigger(
            crate::AudioSendId::new("send-a"),
            crate::audio_mod::AudioBand::Low,
            false,
        ));
        assert!(!p.has_active_clip_triggers(), "disabled config doesn't count");

        p.timeline.layers[0].clip_triggers[0].enabled = true;
        assert!(p.has_active_clip_triggers());
    }

    // ─── migrate_legacy_clip_triggers (P2) ───

    #[test]
    fn migrate_legacy_clip_triggers_resolves_explicit_target_and_drains_send() {
        let mut p = Project::default();
        let send_a = send_with_id("Kick", "send-a");
        let send_id = send_a.id.clone();
        p.audio_setup.sends.push(send_a);

        let target =
            crate::layer::Layer::new("Strobe".to_string(), crate::types::LayerType::Video, 0);
        let target_id = target.layer_id.clone();
        p.timeline.layers.push(target);

        let mut route = crate::audio_trigger::TriggerRoute::new(crate::audio_mod::AudioBand::Low);
        route.enabled = true;
        route.sensitivity = 0.8;
        route.target_layer = Some(target_id.clone());
        p.audio_setup.sends[0].triggers.push(route);

        p.migrate_legacy_clip_triggers();

        assert!(p.audio_setup.sends[0].triggers.is_empty(), "legacy storage drained");
        let (_, layer) = p.timeline.find_layer_by_id(&target_id).unwrap();
        assert_eq!(layer.clip_triggers.len(), 1);
        let cfg = &layer.clip_triggers[0];
        assert!(cfg.enabled);
        assert_eq!(cfg.source.send_id, send_id);
        assert_eq!(cfg.shape.sensitivity, 0.8, "U5-verbatim sensitivity-to-Amount mapping");
    }

    #[test]
    fn migrate_legacy_clip_triggers_auto_routes_by_send_label_when_no_target_layer() {
        let mut p = Project::default();
        let send_a = send_with_id("Kick", "send-a"); // label "Kick"
        let send_id = send_a.id.clone();
        p.audio_setup.sends.push(send_a);

        // Name-matches the send label — the fire-time auto-route rule, run
        // once at load.
        let layer = crate::layer::Layer::new("Kick".to_string(), crate::types::LayerType::Video, 0);
        let target_id = layer.layer_id.clone();
        p.timeline.layers.push(layer);

        let mut route = crate::audio_trigger::TriggerRoute::new(crate::audio_mod::AudioBand::Full);
        route.enabled = true;
        // No target_layer set.
        p.audio_setup.sends[0].triggers.push(route);

        p.migrate_legacy_clip_triggers();

        let (_, layer) = p.timeline.find_layer_by_id(&target_id).unwrap();
        assert_eq!(layer.clip_triggers.len(), 1);
        assert_eq!(layer.clip_triggers[0].source.send_id, send_id);
    }

    #[test]
    fn migrate_legacy_clip_triggers_drops_unresolvable_route_and_still_drains() {
        let mut p = Project::default();
        // Label "Ghost" — no layer named "Ghost", no explicit target_layer.
        let send_a = send_with_id("Ghost", "send-a");
        p.audio_setup.sends.push(send_a);

        let mut route = crate::audio_trigger::TriggerRoute::new(crate::audio_mod::AudioBand::Full);
        route.enabled = true;
        p.audio_setup.sends[0].triggers.push(route);

        p.migrate_legacy_clip_triggers();

        assert!(p.audio_setup.sends[0].triggers.is_empty(), "drained even when unresolvable");
        assert!(p.timeline.layers.is_empty());
    }

    #[test]
    fn migrate_legacy_clip_triggers_is_idempotent_on_a_project_with_no_legacy_routes() {
        let mut p = Project::default();
        p.audio_setup.sends.push(send_with_id("A", "send-a"));
        p.timeline.layers.push(crate::layer::Layer::new(
            "L".to_string(),
            crate::types::LayerType::Video,
            0,
        ));
        p.migrate_legacy_clip_triggers();
        assert!(p.timeline.layers[0].clip_triggers.is_empty());
        assert!(p.audio_setup.sends[0].triggers.is_empty());
    }

    // ─── clear_legacy_rate_on_flags (§7.2 item 2) ───

    #[test]
    fn clear_legacy_rate_on_flags_clears_both_carriers_and_counts() {
        let mut p = Project::default();

        // A param mod (`PresetInstance.audio_mods`) with rate_of_change set.
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        let mut m = crate::audio_mod::ParameterAudioMod::new(
            "amount".into(),
            crate::AudioSendId::new("send-a"),
            crate::audio_mod::AudioFeature::new(
                crate::audio_mod::AudioFeatureKind::Amplitude,
                crate::audio_mod::AudioBand::Full,
            ),
        );
        m.shape.rate_of_change = true;
        fx.audio_mods = Some(vec![m]);
        p.settings.master_effects.push(fx);

        // A clip trigger (`Layer.clip_triggers`) with rate_of_change set.
        let mut layer = layer_with_clip_trigger(
            crate::AudioSendId::new("send-b"),
            crate::audio_mod::AudioBand::Low,
            true,
        );
        layer.clip_triggers[0].shape.rate_of_change = true;
        p.timeline.layers.push(layer);

        p.clear_legacy_rate_on_flags();

        assert!(
            !p.settings.master_effects[0].audio_mods.as_ref().unwrap()[0]
                .shape
                .rate_of_change,
            "param-mod carrier cleared"
        );
        assert!(
            !p.timeline.layers[0].clip_triggers[0].shape.rate_of_change,
            "clip-trigger carrier cleared"
        );
    }

    #[test]
    fn clear_legacy_rate_on_flags_is_idempotent_when_none_are_set() {
        let mut p = Project::default();
        p.timeline.layers.push(layer_with_clip_trigger(
            crate::AudioSendId::new("send-a"),
            crate::audio_mod::AudioBand::Low,
            true,
        ));
        // shape.rate_of_change defaults false — nothing to clear.
        p.clear_legacy_rate_on_flags();
        assert!(!p.timeline.layers[0].clip_triggers[0].shape.rate_of_change);
    }

    #[test]
    fn clear_legacy_rate_on_flags_stays_cleared_across_a_save_reload_round_trip() {
        // DESIGN_DOC_STANDARD §5's round-trip gate: create-path green is half
        // a gate for stateful features. Save → reload must not resurrect the
        // flag `on_after_deserialize` cleared on the previous load.
        let mut p = Project::default();
        p.timeline.layers.push(layer_with_clip_trigger(
            crate::AudioSendId::new("send-a"),
            crate::audio_mod::AudioBand::Low,
            true,
        ));
        p.timeline.layers[0].clip_triggers[0].shape.rate_of_change = true;
        p.clear_legacy_rate_on_flags();
        assert!(!p.timeline.layers[0].clip_triggers[0].shape.rate_of_change);

        let json = serde_json::to_string(&p).expect("serialize");
        let mut reloaded: Project = serde_json::from_str(&json).expect("deserialize");
        reloaded.on_after_deserialize();
        assert!(
            !reloaded.timeline.layers[0].clip_triggers[0].shape.rate_of_change,
            "round trip must not resurrect rate_of_change"
        );
    }

    /// A `clip_trigger`-shaped bundled param, `is_trigger_gate` set — the
    /// only thing project.rs's own `slot`-less test module needs to build a
    /// trigger-gate card by hand (mirrors `effects::tests::gate_slot`, kept
    /// local since that helper is private to `effects.rs`'s own test
    /// module).
    fn gate_param(id: &str) -> crate::params::Param {
        let mut p = slot(id, 0.0, true);
        p.spec.name = "Clip Trigger".to_string();
        p.spec.is_toggle = true;
        p.spec.is_trigger_gate = true;
        p
    }

    fn armed_trigger_gate_mod(send_id: crate::AudioSendId) -> crate::audio_mod::ParameterAudioMod {
        let mut m = crate::audio_mod::ParameterAudioMod::new(
            "clip_trigger".into(),
            send_id,
            crate::audio_mod::AudioFeature::new(
                crate::audio_mod::AudioFeatureKind::Transients,
                crate::audio_mod::AudioBand::Full,
            ),
        );
        m.trigger_mode = Some(crate::audio_trigger::TriggerFireMode::Transient);
        m
    }

    #[test]
    fn armed_trigger_gate_mod_turns_the_analysis_gate_on_and_claims_its_send() {
        // Regression (2026-07-07, class-collapsed 2026-07-07 per §9 U1/U4): a
        // project whose ONLY audio consumer is an armed fire-mode mod on a
        // trigger-gate param never started capture (has_active_audio_mods
        // false) and, even with capture running, its send was skipped by the
        // D4 gate (analysis_consumed_sends empty) — so armed audio triggers
        // silently never fired. §9 deletes the second per-instance config
        // type that caused it; this test is the proof the plain `audio_mods`
        // walk covers a fire-mode mod with zero special-case code.
        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        p.audio_setup.sends.push(send_a.clone());

        let mut layer = crate::layer::Layer::new("PLASMA".into(), crate::types::LayerType::Generator, 0);
        layer.layer_id = crate::LayerId::new("plasma-layer");
        let gp = layer.gen_params_or_init();
        gp.params.push(gate_param("clip_trigger"));
        gp.audio_mods_mut().push(armed_trigger_gate_mod(send_a.id.clone()));
        p.timeline.layers.push(layer);

        assert!(p.has_active_audio_mods(), "armed trigger-gate mod must start capture");
        let consumed = p.analysis_consumed_sends();
        assert!(consumed.contains(&send_a.id), "armed trigger's send must be analyzed");
        assert_eq!(p.audio_send_usage_count(&send_a.id), 1);
        let consumers = p.audio_mod_consumers(&send_a.id);
        assert_eq!(consumers.len(), 1);
        assert!(
            consumers[0].1.contains("Clip Trigger"),
            "consumers list names the param the gate card lives on, not a bespoke 'Trigger' label; got {}",
            consumers[0].1
        );
    }

    #[test]
    fn disarmed_trigger_gate_mod_does_not_gate_analysis_but_still_counts_as_send_usage() {
        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        p.audio_setup.sends.push(send_a.clone());

        let mut layer = crate::layer::Layer::new("PLASMA".into(), crate::types::LayerType::Generator, 0);
        layer.layer_id = crate::LayerId::new("plasma-layer");
        let gp = layer.gen_params_or_init();
        gp.params.push(gate_param("clip_trigger"));
        let mut m = armed_trigger_gate_mod(send_a.id.clone());
        m.enabled = false;
        gp.audio_mods_mut().push(m);
        p.timeline.layers.push(layer);

        assert!(!p.has_active_audio_mods(), "disarmed mod must not run capture");
        assert!(p.analysis_consumed_sends().is_empty());
        // Usage matches the plain audio-mod semantics: the mod still
        // references the send whether enabled or not, so deleting it should
        // warn.
        assert_eq!(p.audio_send_usage_count(&send_a.id), 1);
        assert!(p.audio_mod_consumers(&send_a.id).is_empty(), "consumers lists armed only");
    }

    // ─── audio_mod_consumers (AUDIO_SENDS_UX_DESIGN Phase 2, Consumers section) ───

    #[test]
    fn audio_mod_consumers_resolves_layer_effect_and_param_names() {
        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        p.audio_setup.sends.push(send_a.clone());

        let mut layer = crate::layer::Layer::new("BLOOM LAYER".into(), crate::types::LayerType::Video, 0);
        layer.layer_id = crate::LayerId::new("bloom-layer");
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.audio_mods_mut().push(amplitude_mod(send_a.id.clone()));
        layer.effects = Some(vec![fx]);
        p.timeline.layers.push(layer);

        let consumers = p.audio_mod_consumers(&send_a.id);
        assert_eq!(consumers.len(), 1);
        assert_eq!(consumers[0].0, Some(crate::LayerId::new("bloom-layer")));
        assert!(
            consumers[0].1.starts_with("BLOOM LAYER \u{2022} Bloom \u{2022} "),
            "label should read 'LayerName \u{2022} EffectName \u{2022} ParamName', got {}",
            consumers[0].1
        );
    }

    #[test]
    fn audio_mod_consumers_excludes_disabled_mods_and_other_sends() {
        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        let send_b = send_with_id("B", "send-b");
        p.audio_setup.sends.push(send_a.clone());
        p.audio_setup.sends.push(send_b.clone());

        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        let mods = fx.audio_mods_mut();
        mods.push(amplitude_mod(send_a.id.clone()));
        mods[0].enabled = false;
        mods.push(amplitude_mod(send_b.id.clone()));
        p.settings.master_effects.push(fx);

        assert!(p.audio_mod_consumers(&send_a.id).is_empty(), "disabled mod excluded");
        let b_consumers = p.audio_mod_consumers(&send_b.id);
        assert_eq!(b_consumers.len(), 1);
        assert_eq!(b_consumers[0].0, None, "master-effects mod has no owning layer");
        assert!(b_consumers[0].1.starts_with("Master \u{2022} "));
    }

    // ─── clip_trigger_consumers (P3, AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN D2) ───

    #[test]
    fn clip_trigger_consumers_resolves_layer_and_band_label() {
        use crate::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, AudioModSource};
        use crate::audio_trigger::LayerClipTrigger;

        let mut p = Project::default();
        let send_a = send_with_id("Kick", "send-a");
        p.audio_setup.sends.push(send_a.clone());

        let mut layer = crate::layer::Layer::new("STROBE".into(), crate::types::LayerType::Video, 0);
        layer.layer_id = crate::LayerId::new("strobe-layer");
        let mut cfg = LayerClipTrigger::new(AudioModSource {
            send_id: send_a.id.clone(),
            feature: AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Low),
        });
        cfg.enabled = true;
        layer.clip_triggers.push(cfg);
        p.timeline.layers.push(layer);

        let consumers = p.clip_trigger_consumers(&send_a.id);
        assert_eq!(consumers.len(), 1);
        assert_eq!(consumers[0].0, Some(crate::LayerId::new("strobe-layer")));
        assert_eq!(
            consumers[0].1,
            "Clip trigger \u{2022} STROBE \u{2022} Low",
            "Transients formats as the bare band label"
        );
    }

    #[test]
    fn clip_trigger_consumers_excludes_disabled_configs_and_other_sends() {
        use crate::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, AudioModSource};
        use crate::audio_trigger::LayerClipTrigger;

        let mut p = Project::default();
        let send_a = send_with_id("A", "send-a");
        let send_b = send_with_id("B", "send-b");
        p.audio_setup.sends.push(send_a.clone());
        p.audio_setup.sends.push(send_b.clone());

        let mut layer = crate::layer::Layer::new("L".into(), crate::types::LayerType::Video, 0);
        layer.layer_id = crate::LayerId::new("l1");
        // Disabled — excluded even though it sources send_a.
        let mut disabled = LayerClipTrigger::new(AudioModSource {
            send_id: send_a.id.clone(),
            feature: AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        });
        disabled.enabled = false;
        layer.clip_triggers.push(disabled);
        // Enabled but sources send_b — excluded from send_a's consumers.
        let mut other_send = LayerClipTrigger::new(AudioModSource {
            send_id: send_b.id.clone(),
            feature: AudioFeature::new(AudioFeatureKind::Centroid, AudioBand::Full),
        });
        other_send.enabled = true;
        layer.clip_triggers.push(other_send);
        p.timeline.layers.push(layer);

        assert!(p.clip_trigger_consumers(&send_a.id).is_empty(), "disabled config excluded");
        let b_consumers = p.clip_trigger_consumers(&send_b.id);
        assert_eq!(b_consumers.len(), 1);
        assert_eq!(
            b_consumers[0].1,
            "Clip trigger \u{2022} L \u{2022} Centroid Full",
            "non-Transients spells out the detector"
        );
    }
}
