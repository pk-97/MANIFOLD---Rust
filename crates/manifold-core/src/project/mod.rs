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
mod validate;
pub use validate::{LoadReport, PurgeResult};
mod queries;
mod presets;
mod load_migration;

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
    pub fn layer_count(&self) -> usize {
        self.timeline.layers.len()
    }

    pub fn total_clip_count(&self) -> usize {
        self.timeline.total_clip_count()
    }

    /// Sync BPM from tempo map beat 0, clamped to 20-300.
    /// Port of C# ProjectSerializer.cs lines 39-43.
    pub fn sync_bpm_from_tempo_map(&mut self) {
        self.settings.bpm = self
            .tempo_map
            .get_bpm_at_beat(Beats::ZERO, self.settings.bpm);
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
