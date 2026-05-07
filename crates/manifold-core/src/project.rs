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

    /// Resolve every driver / envelope / Ableton-mapping that came in via
    /// the legacy `paramIndex: i32` shape. The custom `Deserialize` for
    /// each of those types parks the legacy index in
    /// `legacy_param_index`; here we walk every site, look up the
    /// effect/generator's registry definition, and assign
    /// `param_id = def.param_defs[idx].id`.
    ///
    /// After this pass, every driver/envelope/mapping in memory has a
    /// non-empty `param_id` (assuming the index was in range and the
    /// effect type is registered). Stragglers — drivers whose param
    /// disappeared because the effect's param list shrunk — are left
    /// with empty `param_id` and will be ignored at runtime.
    fn resolve_legacy_param_ids(&mut self) {
        use crate::effect_definition_registry;
        use crate::generator_definition_registry;

        fn resolve_driver_id_for_effect(
            driver: &mut crate::effects::ParameterDriver,
            effect_type: &crate::EffectTypeId,
        ) {
            if !driver.param_id.is_empty() {
                driver.legacy_param_index = None;
                return;
            }
            let Some(idx) = driver.legacy_param_index else {
                return;
            };
            let Some(def) = effect_definition_registry::try_get(effect_type) else {
                return;
            };
            if let Some(pd) = def.param_defs.get(idx as usize)
                && !pd.id.is_empty()
            {
                driver.param_id = std::borrow::Cow::Owned(pd.id.clone());
            }
            driver.legacy_param_index = None;
        }

        fn resolve_driver_id_for_generator(
            driver: &mut crate::effects::ParameterDriver,
            gen_type: &crate::GeneratorTypeId,
        ) {
            if !driver.param_id.is_empty() {
                driver.legacy_param_index = None;
                return;
            }
            let Some(idx) = driver.legacy_param_index else {
                return;
            };
            let Some(def) = generator_definition_registry::try_get(gen_type) else {
                return;
            };
            if let Some(pd) = def.param_defs.get(idx as usize)
                && !pd.id.is_empty()
            {
                driver.param_id = std::borrow::Cow::Owned(pd.id.clone());
            }
            driver.legacy_param_index = None;
        }

        fn resolve_envelope_id_for_effect(env: &mut crate::effects::ParamEnvelope) {
            // Layer envelopes carry their own `target_effect_type` —
            // resolution doesn't depend on which layer they live on.
            if !env.param_id.is_empty() {
                env.legacy_param_index = None;
                return;
            }
            let Some(idx) = env.legacy_param_index else {
                return;
            };
            let Some(def) = effect_definition_registry::try_get(&env.target_effect_type) else {
                return;
            };
            if let Some(pd) = def.param_defs.get(idx as usize)
                && !pd.id.is_empty()
            {
                env.param_id = std::borrow::Cow::Owned(pd.id.clone());
            }
            env.legacy_param_index = None;
        }

        fn resolve_envelope_id_for_generator(
            env: &mut crate::effects::ParamEnvelope,
            gen_type: &crate::GeneratorTypeId,
        ) {
            if !env.param_id.is_empty() {
                env.legacy_param_index = None;
                return;
            }
            let Some(idx) = env.legacy_param_index else {
                return;
            };
            let Some(def) = generator_definition_registry::try_get(gen_type) else {
                return;
            };
            if let Some(pd) = def.param_defs.get(idx as usize)
                && !pd.id.is_empty()
            {
                env.param_id = std::borrow::Cow::Owned(pd.id.clone());
            }
            env.legacy_param_index = None;
        }

        fn resolve_ableton_id_for_effect(
            mapping: &mut crate::ableton_mapping::AbletonParamMapping,
            effect_type: &crate::EffectTypeId,
        ) {
            if !mapping.param_id.is_empty() {
                mapping.legacy_param_index = None;
                return;
            }
            let Some(idx) = mapping.legacy_param_index else {
                return;
            };
            let Some(def) = effect_definition_registry::try_get(effect_type) else {
                return;
            };
            if let Some(pd) = def.param_defs.get(idx as usize)
                && !pd.id.is_empty()
            {
                mapping.param_id = std::borrow::Cow::Owned(pd.id.clone());
            }
            mapping.legacy_param_index = None;
        }

        fn resolve_ableton_id_for_generator(
            mapping: &mut crate::ableton_mapping::AbletonParamMapping,
            gen_type: &crate::GeneratorTypeId,
        ) {
            if !mapping.param_id.is_empty() {
                mapping.legacy_param_index = None;
                return;
            }
            let Some(idx) = mapping.legacy_param_index else {
                return;
            };
            let Some(def) = generator_definition_registry::try_get(gen_type) else {
                return;
            };
            if let Some(pd) = def.param_defs.get(idx as usize)
                && !pd.id.is_empty()
            {
                mapping.param_id = std::borrow::Cow::Owned(pd.id.clone());
            }
            mapping.legacy_param_index = None;
        }

        // Macro mappings are stored on `settings.macro_bank.slots[*].mappings`.
        // Each `MacroMapping` carries a `legacy_param_index` parked from the
        // V1.1 shape; the variant tells us whether to look up via the effect
        // or generator registry. `GenParam` requires the layer to be alive
        // because the generator type isn't recorded on the target itself.
        fn resolve_macro_mapping(
            mapping: &mut crate::macro_bank::MacroMapping,
            timeline: &crate::timeline::Timeline,
        ) {
            use crate::macro_bank::MacroMappingTarget;
            let Some(idx) = mapping.legacy_param_index else {
                return;
            };
            match &mut mapping.target {
                MacroMappingTarget::MasterOpacity | MacroMappingTarget::LayerOpacity { .. } => {
                    // No param to resolve.
                }
                MacroMappingTarget::MasterEffect {
                    effect_type,
                    param_id,
                } => {
                    if param_id.is_empty()
                        && let Some(def) = effect_definition_registry::try_get(effect_type)
                        && let Some(pd) = def.param_defs.get(idx as usize)
                        && !pd.id.is_empty()
                    {
                        *param_id = std::borrow::Cow::Owned(pd.id.clone());
                    }
                }
                MacroMappingTarget::LayerEffect {
                    effect_type,
                    param_id,
                    ..
                } => {
                    if param_id.is_empty()
                        && let Some(def) = effect_definition_registry::try_get(effect_type)
                        && let Some(pd) = def.param_defs.get(idx as usize)
                        && !pd.id.is_empty()
                    {
                        *param_id = std::borrow::Cow::Owned(pd.id.clone());
                    }
                }
                MacroMappingTarget::GenParam { layer_id, param_id } => {
                    if param_id.is_empty()
                        && let Some(layer) = timeline
                            .layers
                            .iter()
                            .find(|l| l.layer_id == *layer_id)
                        && let Some(gp) = layer.gen_params()
                        && let Some(def) =
                            generator_definition_registry::try_get(gp.generator_type())
                        && let Some(pd) = def.param_defs.get(idx as usize)
                        && !pd.id.is_empty()
                    {
                        *param_id = std::borrow::Cow::Owned(pd.id.clone());
                    }
                }
            }
            mapping.legacy_param_index = None;
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

    pub fn layer_count(&self) -> usize {
        self.timeline.layers.len()
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
            project_version: "1.2.0".to_string(),
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
    "1.2.0".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EffectTypeId;
    use crate::effects::{EffectInstance, ParameterDriver};
    use crate::types::{BeatDivision, DriverWaveform};

    /// Step 8 regression: a driver deserialized from the legacy
    /// `paramIndex` shape gets its `param_id` filled in by
    /// `resolve_legacy_param_ids` during `on_after_deserialize`.
    #[test]
    fn legacy_param_index_resolved_to_param_id_for_effect_drivers() {
        let mut p = Project::default();
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.param_values = vec![0.5];
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
        assert_eq!(d.param_id, "amount", "Bloom paramIndex 0 should resolve to 'amount'");
        assert_eq!(d.legacy_param_index, None, "legacy index must be cleared");
    }

    #[test]
    fn legacy_resolution_idempotent_when_param_id_already_set() {
        let mut p = Project::default();
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.param_values = vec![0.5];
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
    fn legacy_resolution_leaves_unresolvable_drivers_empty() {
        // If the legacy index is out of range (effect's param list shrunk
        // since save), the driver gets `param_id = ""` and is ignored
        // at runtime. Better than panicking on a stale project.
        let mut p = Project::default();
        let mut fx = EffectInstance::new(EffectTypeId::BLOOM);
        fx.param_values = vec![0.5];
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
            "legacy index always cleared, even when unresolvable"
        );
    }
}
