use crate::effect_graph_def::EffectGraphDef;
use crate::preset_type_id::PresetTypeId;
use crate::id::{EffectGroupId, EffectId, NodeId};
use crate::types::{BeatDivision, DriverWaveform};
use serde::{Deserialize, Serialize};

/// Stable string identifier for a host-visible parameter.
///
/// `Cow::Borrowed("amount")` for compile-time IDs (developer-defined
/// effects). `Cow::Owned(...)` for V2 user-exposed parameters allocated
/// at runtime. External mappings (OSC, Ableton, MIDI, modulation
/// drivers, envelopes) all key on this — never on positional indices.
///
/// See `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 for the full design.
///
/// Defined in `manifold-foundation` (the shared primitive vocabulary) and
/// re-exported here at its historical path so the UI can share the identical
/// type without depending on the engine. See `docs/UI_LAYERING_INVERSION.md`.
pub use manifold_foundation::ParamId;

mod automation;
mod bindings;
mod driver;
mod envelope;
mod group;
mod instance;
mod instance_serde;
mod param_defs;
mod relight;

pub use automation::{AutomationLane, AutomationPoint, RemovedAutomation, SegmentShape};
pub use bindings::{apply_card_reshape, binding_id_for_node_param_in, invert_card_reshape, ParamConvert, RemovedExposure, UserParamBinding};
pub use driver::{beat_division_helper, hash_to_float, hash_u32, ParameterDriver};
pub use envelope::{ParamEnvelope, DEFAULT_ENVELOPE_DECAY_BEATS};
pub use group::EffectGroup;
pub use instance::PresetInstance;
pub use instance_serde::{deserialize_generator_instance, deserialize_opt_generator_instance};
pub use param_defs::{RangeContract, RangeReason, RegistryParamDef};
pub use relight::{RelightField, RelightHeightFrom, RelightParams};



/// serde `skip_serializing_if` for [`crate::effect_graph_def::ParamSpecDef::curve`].
pub(crate) fn curve_is_linear(c: &crate::macro_bank::MacroCurve) -> bool {
    matches!(c, crate::macro_bank::MacroCurve::Linear)
}

/// serde `skip_serializing_if` for a defaulted `false` bool field.
pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}

// ─── Traits ───

/// Shared contract for entities that own a modular effects list.
/// Port of Unity IEffectContainer.cs.
/// Implemented by TimelineClip, Layer, and ProjectSettings.
pub trait EffectContainer {
    fn effects(&self) -> &[PresetInstance];
    fn effects_mut(&mut self) -> &mut Vec<PresetInstance>;
    fn effect_groups(&self) -> &[EffectGroup];
    fn effect_groups_mut(&mut self) -> &mut Vec<EffectGroup>;
    fn has_modular_effects(&self) -> bool;
    fn find_effect(&self, effect_type: &PresetTypeId) -> Option<&PresetInstance>;
    fn find_effect_group(&self, group_id: &str) -> Option<&EffectGroup>;
}

/// Abstracts a "thing with named params, drivers, and ranges."
/// Port of Unity IParamSource.cs.
/// Both PresetInstance and generator params implement this.
pub trait ParamSource {
    fn display_name(&self) -> &str;
    fn param_count(&self) -> usize;
    fn get_param_def(&self, id: &str) -> crate::effect_graph_def::ParamSpecDef;
    fn get_param(&self, id: &str) -> f32;
    fn set_param(&mut self, id: &str, value: f32);
    fn get_base_param(&self, id: &str) -> f32;
    fn set_base_param(&mut self, id: &str, value: f32);
    fn find_driver(&self, param_id: &str) -> Option<&ParameterDriver>;
    fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>>;
    fn create_driver(&mut self, param_id: ParamId) -> &ParameterDriver;
    fn remove_driver(&mut self, param_id: &str);
}







// ─── Default helpers ───

fn default_true() -> bool {
    true
}
fn default_one() -> f32 {
    1.0
}
fn generate_effect_id() -> EffectId {
    EffectId::new(crate::math::short_id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::Beats;

    #[test]
    fn card_reshape_identity_and_stages() {
        use crate::macro_bank::MacroCurve;
        // Identity: passes through untouched.
        assert!((apply_card_reshape(2.5, 0.0, 10.0, false, MacroCurve::Linear, 1.0, 0.0) - 2.5).abs() < 1e-4);
        // Invert: 25% of the range becomes 75%.
        assert!((apply_card_reshape(2.5, 0.0, 10.0, true, MacroCurve::Linear, 1.0, 0.0) - 7.5).abs() < 1e-4);
        // SCurve (Hermite 3t^2-2t^3): n=0.25 -> 0.15625 -> *10 = 1.5625.
        assert!((apply_card_reshape(2.5, 0.0, 10.0, false, MacroCurve::SCurve, 1.0, 0.0) - 1.5625).abs() < 1e-3);
        // Degenerate range: passthrough, no divide-by-zero.
        assert!((apply_card_reshape(42.0, 5.0, 5.0, false, MacroCurve::Exponential, 1.0, 0.0) - 42.0).abs() < 1e-6);
        // Folded affine (deg->rad): no invert/curve, so scale/offset apply to the
        // RAW value, unclamped — a past-max 400° must NOT pin to the slider max.
        let k = std::f32::consts::PI / 180.0;
        assert!((apply_card_reshape(85.0, 0.0, 360.0, false, MacroCurve::Linear, k, 0.0) - 85.0 * k).abs() < 1e-5);
        assert!((apply_card_reshape(400.0, 0.0, 360.0, false, MacroCurve::Linear, k, 0.0) - 400.0 * k).abs() < 1e-4);
    }

    /// `PARAM_TWO_WAY_BINDING_DESIGN.md` invariant: forward and inverse
    /// cannot drift. For a grid of (min, max, invert, curve, scale, offset) ×
    /// values: `apply(invert(x)) ≈ x` within 1e-4 across all four curves;
    /// `invert(apply(x)) ≈ x` for in-range x.
    #[test]
    fn card_reshape_roundtrips() {
        use crate::macro_bank::MacroCurve;
        let curves = [
            MacroCurve::Linear,
            MacroCurve::Exponential,
            MacroCurve::Logarithmic,
            MacroCurve::SCurve,
        ];
        let ranges: [(f32, f32); 3] = [(0.0, 1.0), (0.0, 10.0), (-5.0, 5.0)];
        let affines: [(f32, f32); 2] = [(1.0, 0.0), (2.0, 3.0)];
        for curve in curves {
            for invert in [false, true] {
                for (min, max) in ranges {
                    for (scale, offset) in affines {
                        let mut x = min;
                        let step = (max - min) / 10.0;
                        while x <= max {
                            let target = apply_card_reshape(x, min, max, invert, curve, scale, offset);
                            let back = invert_card_reshape(target, min, max, invert, curve, scale, offset)
                                .expect("non-degenerate scale");
                            assert!(
                                (back - x).abs() < 1e-3,
                                "{curve:?} invert={invert} range=({min},{max}) affine=({scale},{offset}): \
                                 invert_card_reshape(apply_card_reshape({x})) = {back}, expected ~{x}"
                            );
                            x += step;
                        }
                    }
                }
            }
        }
        // Degenerate affine: no inverse representable.
        assert!(invert_card_reshape(1.0, 0.0, 1.0, false, MacroCurve::Linear, 0.0, 0.0).is_none());
    }

    #[test]
    fn duplicated_assigns_fresh_id_and_drops_hardware_bindings() {
        // BUG-001/004: a duplicated/pasted effect must be an INDEPENDENT copy —
        // a fresh EffectId (not a shared reference) and no carried-over hardware
        // bindings (Ableton mappings / audio mods). Per-instance modulation
        // (drivers) is kept; group_id is left for the caller to decide.
        let mut src = PresetInstance::new(PresetTypeId::new("Blur"));
        src.ableton_mappings = Some(Vec::new());
        src.audio_mods = Some(Vec::new());
        src.group_id = Some(EffectGroupId::new("grp"));
        src.create_driver("amount".into());
        assert!(src.has_drivers());

        let copy = src.duplicated();

        assert_ne!(copy.id, src.id, "copy must get a fresh EffectId");
        assert!(
            copy.ableton_mappings.is_none(),
            "Ableton mappings must not ride along on a copy"
        );
        assert!(
            copy.audio_mods.is_none(),
            "audio mods must not ride along on a copy"
        );
        assert!(copy.has_drivers(), "per-instance drivers are kept");
        assert_eq!(
            copy.group_id, src.group_id,
            "duplicated() leaves group_id for the caller to remap/clear"
        );
    }

    #[test]
    fn test_driver_sine() {
        let val =
            ParameterDriver::evaluate(Beats(0.0), BeatDivision::Quarter, DriverWaveform::Sine, 0.0);
        assert!((val - 0.5).abs() < 0.01);

        let val = ParameterDriver::evaluate(
            Beats(0.25),
            BeatDivision::Quarter,
            DriverWaveform::Sine,
            0.0,
        );
        assert!((val - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_driver_square() {
        let val = ParameterDriver::evaluate(
            Beats(0.1),
            BeatDivision::Quarter,
            DriverWaveform::Square,
            0.0,
        );
        assert_eq!(val, 1.0);

        let val = ParameterDriver::evaluate(
            Beats(0.6),
            BeatDivision::Quarter,
            DriverWaveform::Square,
            0.0,
        );
        assert_eq!(val, 0.0);
    }

    #[test]
    fn test_driver_random_hash_matches_unity() {
        let val = ParameterDriver::evaluate(
            Beats(1.0),
            BeatDivision::Quarter,
            DriverWaveform::Random,
            0.0,
        );
        assert!((0.0..=1.0).contains(&val));
        // Same cycle should give same value
        let val2 = ParameterDriver::evaluate(
            Beats(1.5),
            BeatDivision::Quarter,
            DriverWaveform::Random,
            0.0,
        );
        assert_eq!(val, val2);
    }

    // ── ParameterDriver backward-compat Deserialize (step 8) ──────

    #[test]
    fn driver_deserialize_legacy_param_index() {
        // V1.1.0 shape: { paramIndex: 1, ... }. The custom Deserialize
        // parks the index in `legacy_param_index` and leaves
        // `param_id` empty. The post-load resolver fills `param_id`
        // later — this test only covers the Deserialize step.
        let json = r#"{
            "paramIndex": 2,
            "beatDivision": 4,
            "waveform": 0,
            "enabled": true,
            "phase": 0.0,
            "baseValue": 0.0,
            "trimMin": 0.0,
            "trimMax": 1.0,
            "reversed": false
        }"#;
        let d: ParameterDriver = serde_json::from_str(json).unwrap();
        assert!(
            d.param_id.is_empty(),
            "legacy shape must leave param_id empty until post-load resolution"
        );
        assert_eq!(d.legacy_param_index, Some(2));
        assert_eq!(d.beat_division, BeatDivision::Half);
    }

    #[test]
    fn driver_deserialize_canonical_param_id() {
        // V1.2+ shape: { paramId: "amount", ... }. No post-load
        // resolution needed — `param_id` is already set, and
        // `legacy_param_index` stays None.
        let json = r#"{
            "paramId": "amount",
            "beatDivision": 5,
            "waveform": 1,
            "enabled": true,
            "phase": 0.5,
            "baseValue": 0.0,
            "trimMin": 0.1,
            "trimMax": 0.9,
            "reversed": false
        }"#;
        let d: ParameterDriver = serde_json::from_str(json).unwrap();
        assert_eq!(d.param_id, "amount");
        assert_eq!(d.legacy_param_index, None);
        assert_eq!(d.beat_division, BeatDivision::Whole);
        assert!((d.phase - 0.5).abs() < 1e-6);
    }

    #[test]
    fn driver_deserialize_param_id_wins_when_both_present() {
        // If both fields are sent (forward-migration test fixtures or
        // a transitional save shape), `param_id` is canonical and
        // `param_index` is ignored. No legacy resolution scheduled.
        let json = r#"{
            "paramId": "threshold",
            "paramIndex": 99,
            "beatDivision": 3,
            "waveform": 0
        }"#;
        let d: ParameterDriver = serde_json::from_str(json).unwrap();
        assert_eq!(d.param_id, "threshold");
        assert_eq!(d.legacy_param_index, None);
    }

    #[test]
    fn driver_deserialize_missing_both_loads_as_unresolvable() {
        // No paramId, no paramIndex — load doesn't error; the driver
        // stays present but inert. Better than refusing the entire
        // project. Real recovery path is the post-load resolver, but
        // there's nothing for it to do here.
        let json = r#"{
            "beatDivision": 4
        }"#;
        let d: ParameterDriver = serde_json::from_str(json).unwrap();
        assert_eq!(d.param_id, "");
        assert_eq!(d.legacy_param_index, None);
    }

    #[test]
    fn driver_serialize_writes_param_id_only() {
        // After step 8, saved files always carry the new shape. The
        // legacy `paramIndex` field is never written (skipped via
        // custom Deserialize / derived Serialize on the canonical
        // field set).
        let driver = ParameterDriver::new("amount", BeatDivision::Half, DriverWaveform::Triangle);
        let json = serde_json::to_string(&driver).unwrap();
        assert!(json.contains("\"paramId\":\"amount\""));
        assert!(
            !json.contains("paramIndex"),
            "Serialize must not write legacy paramIndex field; got: {json}"
        );
        assert!(
            !json.contains("legacyParamIndex"),
            "Serialize must not leak the runtime-only legacy_param_index field; got: {json}"
        );
    }

    #[test]
    fn driver_round_trips_through_canonical_shape() {
        let driver =
            ParameterDriver::new("threshold", BeatDivision::FourWhole, DriverWaveform::Square);
        let json = serde_json::to_string(&driver).unwrap();
        let back: ParameterDriver = serde_json::from_str(&json).unwrap();
        assert_eq!(back.param_id, driver.param_id);
        assert_eq!(back.beat_division, driver.beat_division);
        assert_eq!(back.waveform, driver.waveform);
        assert_eq!(back.legacy_param_index, None);
    }

    #[test]
    fn driver_sync_mode_omits_free_period_field() {
        // Sync mode (the default) must not write freePeriodBeats — pre-free-mode
        // projects round-trip byte-identically and stay tiny.
        let driver = ParameterDriver::new("amount", BeatDivision::Quarter, DriverWaveform::Sine);
        assert_eq!(driver.free_period_beats, None);
        let json = serde_json::to_string(&driver).unwrap();
        assert!(
            !json.contains("freePeriodBeats"),
            "sync-mode driver must not emit freePeriodBeats; got: {json}"
        );
    }

    #[test]
    fn driver_free_period_round_trips() {
        let mut driver =
            ParameterDriver::new("amount", BeatDivision::Quarter, DriverWaveform::Sine);
        driver.free_period_beats = Some(3.0);
        let json = serde_json::to_string(&driver).unwrap();
        assert!(json.contains("\"freePeriodBeats\":3"), "got: {json}");
        let back: ParameterDriver = serde_json::from_str(&json).unwrap();
        assert_eq!(back.free_period_beats, Some(3.0));
    }

    #[test]
    fn driver_legacy_json_loads_as_sync_mode() {
        // A project saved before free mode existed has no freePeriodBeats key.
        let json = r#"{
            "paramId": "amount",
            "beatDivision": 3,
            "waveform": 0,
            "enabled": true,
            "phase": 0.0,
            "baseValue": 0.0,
            "trimMin": 0.0,
            "trimMax": 1.0,
            "reversed": false
        }"#;
        let d: ParameterDriver = serde_json::from_str(json).unwrap();
        assert_eq!(d.free_period_beats, None, "legacy driver must default to sync mode");
        assert_eq!(d.period_beats(), BeatDivision::Quarter.beats());
    }

    #[test]
    fn free_period_overrides_division_for_evaluation() {
        // period_beats() prefers the free period; evaluate_with_period cycles on it.
        let mut d = ParameterDriver::new("amount", BeatDivision::Quarter, DriverWaveform::Sawtooth);
        d.free_period_beats = Some(3.0);
        assert_eq!(d.period_beats(), 3.0);
        // Sawtooth = phase; at beat 0 phase 0, at beat 1.5 phase 0.5 over a 3-beat period.
        let v0 = ParameterDriver::evaluate_with_period(
            Beats(0.0),
            d.period_beats(),
            d.waveform,
            d.phase,
        );
        let v_half = ParameterDriver::evaluate_with_period(
            Beats(1.5),
            d.period_beats(),
            d.waveform,
            d.phase,
        );
        assert!(v0.abs() < 1e-6, "phase 0 at beat 0");
        assert!((v_half - 0.5).abs() < 1e-6, "half phase at beat 1.5 over 3-beat period");
    }

    // ── ParamEnvelope backward-compat Deserialize (step 9) ──────

    #[test]
    fn envelope_deserialize_legacy_param_index() {
        // V1.1 shape: { targetEffectType, targetParamIndex: 1, ... }. The
        // leftover targetEffectType is ignored (the v1.5→v1.6 migration
        // consumes it to place the envelope on the right instance).
        let json = r#"{
            "targetEffectType": "Bloom",
            "targetParamIndex": 0,
            "enabled": true,
            "attackBeats": 0.25,
            "decayBeats": 0.25,
            "sustainLevel": 0.5,
            "releaseBeats": 0.25,
            "targetNormalized": 1.0
        }"#;
        let e: ParamEnvelope = serde_json::from_str(json).unwrap();
        assert!(e.param_id.is_empty());
        assert_eq!(e.legacy_param_index, Some(0));
    }

    #[test]
    fn envelope_deserialize_canonical_param_id() {
        // Legacy ADSR keys (attackBeats etc.) are ignored post-simplification —
        // the envelope loads as a plain decay envelope keeping only its depth.
        let json = r#"{
            "paramId": "amount",
            "enabled": true,
            "attackBeats": 0.5,
            "targetNormalized": 0.7
        }"#;
        let e: ParamEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(e.param_id, "amount");
        assert_eq!(e.legacy_param_index, None);
        assert!((e.target_normalized - 0.7).abs() < 1e-6);
    }

    #[test]
    fn envelope_deserialize_param_id_wins_when_both_present() {
        let json = r#"{
            "paramId": "threshold",
            "targetParamIndex": 99,
            "enabled": true
        }"#;
        let e: ParamEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(e.param_id, "threshold");
        assert_eq!(e.legacy_param_index, None);
    }

    #[test]
    fn envelope_serialize_writes_param_id_only() {
        let env = ParamEnvelope::new("amount");
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains("\"paramId\":\"amount\""));
        assert!(
            !json.contains("targetParamIndex"),
            "Serialize must not write legacy targetParamIndex; got: {json}"
        );
        assert!(!json.contains("legacyParamIndex"));
        assert!(
            !json.contains("targetEffectType"),
            "Serialize must not write targetEffectType post-unification; got: {json}"
        );
    }

    #[test]
    fn envelope_round_trips_through_canonical_shape() {
        let env = ParamEnvelope::new("amount");
        let json = serde_json::to_string(&env).unwrap();
        let back: ParamEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back.param_id, env.param_id);
        assert_eq!(back.legacy_param_index, None);
    }

    // ── PresetInstance `params` wire format (V1.4, PARAM_STORAGE_DESIGN.md §4) ──
    //
    // The typed (de)serialize understands ONLY the id-keyed `params` map —
    // the four historical `paramValues` shapes (positional/keyed × bare-f32/
    // {value,exposed}) are deleted, not reimplemented here (D4); their
    // conversion tests now live in `manifold-io`'s
    // `migrations::param_storage_v14`, which runs before typed deserialize
    // ever sees the JSON. These tests cover what's left on this side: the
    // V1.4 shape itself, `base` folding, and unregistered-type degradation.
    //
    // "TestCreateDefaultUntouched" (registered below, single param
    // "amount", default 0.42) and "TestTwoParamRoundTrip" (registered
    // below, "alpha"/"beta") stand in for a real bundled effect — Rust
    // module items are visible regardless of declaration order.

    #[test]
    fn effect_instance_deserialize_v14_params_map() {
        let json = r#"{
            "id": "abc12345",
            "effectType": "TestCreateDefaultUntouched",
            "enabled": true,
            "collapsed": false,
            "params": { "amount": { "value": 0.75, "exposed": true, "base": 0.5 } }
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        assert_eq!(fx.params.len(), 1);
        let amount = fx.params.get("amount").unwrap();
        assert!((amount.value - 0.75).abs() < f32::EPSILON);
        assert!(amount.exposed);
        // `base` present on the entry → base_tracked, folded into the entry.
        assert!(fx.base_tracked);
        assert!((amount.base - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn effect_instance_deserialize_v14_params_without_base_leaves_base_untracked() {
        let json = r#"{
            "effectType": "TestCreateDefaultUntouched",
            "enabled": true,
            "collapsed": false,
            "params": { "amount": { "value": 0.75 } }
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        assert!(!fx.base_tracked);
        // exposed defaults to true when the key is absent from the entry.
        assert!(fx.params.get("amount").unwrap().exposed);
    }

    #[test]
    fn effect_instance_deserialize_params_without_registry_keeps_state() {
        // No registry def for this type → the template is UNRESOLVABLE,
        // which is not the same as "this id was deprecated by its template".
        // Dropping here was the BUG-036 class (a project-local preset's
        // template registers after layer deserialize under the wrong load
        // order); the entry is kept on a placeholder spec instead, so no
        // param state is ever lost to a missing template.
        let json = r#"{
            "effectType": "TotallyUnregisteredEffectType",
            "enabled": true,
            "collapsed": false,
            "params": { "amount": { "value": 0.7 } }
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        assert_eq!(fx.params.len(), 1, "entry kept despite missing template");
        let p = fx.params.get("amount").unwrap();
        assert!((p.value - 0.7).abs() < f32::EPSILON);
        // Placeholder spec carries identity; a later load with the template
        // present reconciles the real descriptor (only state serializes for
        // a bundled-origin param).
        assert_eq!(p.spec.id, "amount");
    }

    #[test]
    fn effect_instance_serialize_omits_params_without_registry() {
        // No registry def and no user-added tail → `params` has nothing to
        // key its entries by, so it serializes empty. This is the honest
        // consequence of deleting the positional fallback (D4): an
        // unregistered type's values are not addressable, so they are not
        // written, rather than dumped into an array nothing can read back
        // by id. In production this path is unreachable (every shipping
        // effect is registered).
        let fx = PresetInstance {
            kind: crate::preset_def::PresetKind::Effect,
            id: EffectId::new("abc12345"),
            effect_type: PresetTypeId::from_string("TotallyUnregisteredEffectType".to_string()),
            enabled: true,
            collapsed: false,
            // Post-manifest (D4): there is no "unaddressable positional values"
            // failure mode — every `Param` is self-describing by id. An
            // unregistered type seeds an EMPTY manifest (no template), so
            // `params` serializes empty; the instance is never lost.
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
        let json = serde_json::to_string(&fx).unwrap();
        assert!(
            json.contains("\"params\":{}"),
            "unregistered type must serialize an empty params map, not lose the instance; got: {json}"
        );
    }

    #[test]
    fn effect_instance_serialize_round_trips_hidden_and_visible_params() {
        let fx = PresetInstance {
            kind: crate::preset_def::PresetKind::Effect,
            id: EffectId::new("abc12345"),
            effect_type: PresetTypeId::from_string("TestTwoParamRoundTrip".to_string()),
            enabled: true,
            collapsed: false,
            params: crate::params::ParamManifest::from_params(vec![
                slot("alpha", 0.1, true),
                slot("beta", 0.2, false),
            ]),
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
        let json = serde_json::to_string(&fx).unwrap();
        assert!(json.contains("\"alpha\":{\"value\":0.1,\"exposed\":true}"));
        assert!(json.contains("\"beta\":{\"value\":0.2,\"exposed\":false}"));
        let back: PresetInstance = serde_json::from_str(&json).unwrap();
        assert_eq!(back.params.len(), 2);
        let a = back.params.get("alpha").unwrap();
        assert_eq!(a.value, 0.1);
        assert!(a.exposed);
        let b = back.params.get("beta").unwrap();
        assert_eq!(b.value, 0.2);
        assert!(!b.exposed);
    }

    /// `docs/DEPTH_RELIGHT_DESIGN.md` P5: a pre-P5 project file — no
    /// `relight`/`relightParams` keys at all — must load with the toggle off
    /// and every knob at its proven-recipe default (D2's "every existing
    /// project loads unchanged" contract), and a freshly-constructed instance
    /// must serialize with NEITHER key present (byte-identical old projects).
    #[test]
    fn relight_defaults_false_and_omits_from_wire_when_untouched() {
        let fx = PresetInstance::new(PresetTypeId::from_string("Mirror".to_string()));
        assert!(!fx.relight, "relight must default to false");
        assert_eq!(
            fx.relight_params,
            RelightParams::default(),
            "relight_params must default to the D3 proven recipe"
        );
        let json = serde_json::to_string(&fx).unwrap();
        assert!(!json.contains("\"relight\""), "untouched instance must not emit `relight`: {json}");
        assert!(
            !json.contains("relightParams"),
            "untouched instance must not emit `relightParams`: {json}"
        );

        // A pre-P5 project's raw JSON (no relight keys at all) still loads —
        // the field-less shape is exactly what an old saved project looks
        // like on disk.
        let legacy_json = r#"{"id":"abc12345","effectType":"Mirror","enabled":true,"collapsed":false,"params":{}}"#;
        let back: PresetInstance = serde_json::from_str(legacy_json).unwrap();
        assert!(!back.relight);
        assert_eq!(back.relight_params, RelightParams::default());

        // Toggling on + editing a knob DOES round-trip.
        let mut on = fx;
        on.relight = true;
        on.relight_params.relief = 0.8;
        let json_on = serde_json::to_string(&on).unwrap();
        assert!(json_on.contains("\"relight\":true"));
        assert!(json_on.contains("relightParams"));
        let back_on: PresetInstance = serde_json::from_str(&json_on).unwrap();
        assert!(back_on.relight);
        assert_eq!(back_on.relight_params.relief, 0.8);
    }

    #[test]
    fn effect_instance_legacy_param0_through_param3_round_trip() {
        // V1.0 had flat param0..param3 fields alongside the param wire.
        // The custom Deserialize must continue to capture them so the
        // existing align_to_definition migration sees both shapes.
        let json = r#"{
            "effectType": "TestCreateDefaultUntouched",
            "enabled": true,
            "collapsed": false,
            "params": {},
            "param0": 0.5,
            "param1": 1.0
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        assert_eq!(fx.legacy_param0, Some(0.5));
        assert_eq!(fx.legacy_param1, Some(1.0));
        assert_eq!(fx.legacy_param2, None);
        assert_eq!(fx.legacy_param3, None);
        // Round-trip preserves them.
        let json = serde_json::to_string(&fx).unwrap();
        assert!(json.contains("\"param0\":0.5"));
        assert!(json.contains("\"param1\":1.0"));
    }

    #[test]
    fn effect_instance_skip_serializing_optional_none() {
        let fx = PresetInstance::new(PresetTypeId::from_string("TestEffect".to_string()));
        let json = serde_json::to_string(&fx).unwrap();
        // `params` always emits (even empty); `base` never appears on any
        // entry for a fresh, untouched instance.
        assert!(json.contains("\"params\":{}"));
        assert!(!json.contains("\"base\":"));
        assert!(!json.contains("\"drivers\""));
        assert!(!json.contains("\"abletonMappings\""));
        assert!(!json.contains("\"groupId\""));
        assert!(!json.contains("\"param0\""));
        // After the binding-storage unification there is no separate
        // `userParamBindings` field at all — user bindings live in the
        // graph. A fresh effect has no graph, so nothing extra emits and
        // existing fixtures round-trip byte-identically.
        assert!(!json.contains("\"userParamBindings\""));
    }

    // ── Map deserialize alias-aware path (step 15) ────────────────

    #[test]
    fn params_map_deserialize_drops_unknown_id() {
        // Without any alias entries, an unknown id is silently dropped.
        // This is the orphan policy — same as drivers/envelopes/Ableton.
        let json = r#"{
            "id": "abc12345",
            "effectType": "TestCreateDefaultUntouched",
            "enabled": true,
            "collapsed": false,
            "params": { "amount": { "value": 0.7 }, "old_phantom_param": { "value": 0.5 } }
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        // amount resolves via the registry; old_phantom_param has nowhere
        // to go (not static, not a user-added tail id) and is dropped.
        assert_eq!(fx.params.len(), 1);
        assert!((fx.params.get("amount").unwrap().value - 0.7).abs() < f32::EPSILON);
    }

    inventory::submit! {
        crate::effect_registration::EffectMetadata {
            id: PresetTypeId::new("TestTwoParamRoundTrip"),
            display_name: "Test Two Param Round Trip",
            category: "Test",
            available: true,
            osc_prefix: "testTwoParamRoundTrip",
            legacy_discriminant: None,
            params: &[
                crate::generator_registration::ParamSpec::continuous(
                    "alpha", "Alpha", 0.0, 1.0, 0.0, "F2", "",
                ),
                crate::generator_registration::ParamSpec::continuous(
                    "beta", "Beta", 0.0, 1.0, 0.0, "F2", "",
                ),
            ],
        }
    }

    // ── User-exposed parameter bindings (Phase 3 step 20) ─────────

    fn sample_user_binding(id: &str, node: &str, inner: &str) -> UserParamBinding {
        UserParamBinding {
            id: id.to_string(),
            label: inner.to_string(),
            node_id: NodeId::new(node),
            legacy_node_handle: None,
            inner_param: inner.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.25,
            convert: ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
            value_labels: Vec::new(),
            section: None,
        }
    }

    /// Build a bundled test [`Param`] (value == base == `value`) with the given
    /// id, exposure, and a 0..1 range. Replaces the old positional `ParamSlot`.
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

    /// Build a manifest from positional `(value, exposed)` pairs, assigning
    /// synthetic ids `p0`, `p1`, … in card order — the value-only analogue of
    /// the old `param_values: vec![ParamSlot::exposed(..)]`.
    fn manifest(slots: &[(f32, bool)]) -> crate::params::ParamManifest {
        crate::params::ParamManifest::from_params(
            slots
                .iter()
                .enumerate()
                .map(|(i, &(v, e))| slot(&format!("p{i}"), v, e))
                .collect(),
        )
    }

    #[test]
    fn user_param_binding_serde_round_trip() {
        // A standalone UserParamBinding round-trips through JSON
        // without losing any field. Wire shape uses camelCase keys.
        let ub = sample_user_binding("user.uv_transform.translate.1", "uv_transform", "translate");
        let json = serde_json::to_string(&ub).unwrap();
        assert!(json.contains("\"id\":\"user.uv_transform.translate.1\""));
        assert!(json.contains("\"nodeId\":\"uv_transform\""));
        // The runtime addressing key is `nodeId`; the legacy `nodeHandle`
        // key only ever appears when reading a pre-node-id file and is
        // skip-serialized once cleared.
        assert!(!json.contains("nodeHandle"));
        assert!(json.contains("\"innerParam\":\"translate\""));
        assert!(json.contains("\"defaultValue\":0.25"));
        assert!(json.contains("\"convert\":{\"type\":\"Float\"}"));
        let back: UserParamBinding = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ub);
    }

    #[test]
    fn user_param_binding_convert_default_is_float() {
        // Missing `convert` field defaults to Float — older serialized
        // bindings (if we ever ship without it) load cleanly.
        let json = r#"{
            "id": "user.x.y.1", "label": "Y",
            "nodeHandle": "x", "innerParam": "y",
            "min": 0.0, "max": 1.0, "defaultValue": 0.5
        }"#;
        let ub: UserParamBinding = serde_json::from_str(json).unwrap();
        assert_eq!(ub.convert, ParamConvert::Float);
        // Pre-node-id `nodeHandle` is captured by the load shim (node_id
        // stays empty until the renderer-layer migration resolves it).
        assert_eq!(ub.legacy_node_handle.as_deref(), Some("x"));
        assert!(ub.node_id.is_empty());
    }

    #[test]
    fn effect_instance_round_trip_with_user_bindings_against_bloom() {
        // Bloom is registered in this crate's tests with one param
        // `amount`. Add two user bindings and verify the whole
        // PresetInstance round-trips through JSON, including the
        // user-binding tail values landing in the right param_values
        // slots regardless of JSON key ordering.
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.7, true)]); // static prefix
        fx.append_user_binding(sample_user_binding(
            "user.uv_transform.translate.1",
            "uv_transform",
            "translate",
        ));
        fx.append_user_binding(sample_user_binding("user.mix.amount.1", "mix", "amount"));
        // After append, the manifest should carry [amount=0.7, translate=0.25, mix.amount=0.25].
        assert_eq!(fx.params.len(), 3);
        assert_eq!(fx.params.get("amount").unwrap().value, 0.7);
        assert_eq!(fx.params.get("user.uv_transform.translate.1").unwrap().value, 0.25);
        assert_eq!(fx.params.get("user.mix.amount.1").unwrap().value, 0.25);
        // Tweak the user-tail values to verify they round-trip.
        fx.params.get_mut("user.uv_transform.translate.1").unwrap().value = 0.42;
        fx.params.get_mut("user.mix.amount.1").unwrap().value = 0.91;

        let json = serde_json::to_string(&fx).unwrap();
        // User bindings now ride out inside the per-instance `graph`
        // (preset_metadata.bindings, userAdded), not a separate array.
        assert!(json.contains("\"graph\""));
        assert!(json.contains("\"userAdded\":true"));
        // V1.3 wire emits {value, exposed} objects per entry; the
        // param_values tail is keyed by the user-binding id.
        assert!(json.contains("\"amount\":{\"value\":0.7,\"exposed\":true}"));
        assert!(json.contains("\"user.uv_transform.translate.1\":{\"value\":0.42"));
        assert!(json.contains("\"user.mix.amount.1\":{\"value\":0.91"));

        let back: PresetInstance = serde_json::from_str(&json).unwrap();
        let back_bindings = back.user_param_bindings();
        assert_eq!(back_bindings.len(), 2);
        assert_eq!(back_bindings[0].id, "user.uv_transform.translate.1");
        assert_eq!(back_bindings[1].id, "user.mix.amount.1");
        assert_eq!(back.params.len(), 3);
        assert_eq!(back.params.get("amount").unwrap().value, 0.7);
        assert_eq!(back.params.get("user.uv_transform.translate.1").unwrap().value, 0.42);
        assert_eq!(back.params.get("user.mix.amount.1").unwrap().value, 0.91);
    }

    #[test]
    fn user_exposed_angle_param_carries_is_angle_through_manifest_and_synth() {
        // Regression guard for the P5 inspector fix: before `is_angle` had a
        // home on the spec, exposing an angle inner param dropped the flag at
        // persistence and `synth_user_binding` rebuilt it as `false`, so the
        // card never showed degrees. Now the flag is seeded onto the manifest
        // spec at expose, survives a JSON round-trip, and synth reads it back.
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.7, true)]);

        let mut angle = sample_user_binding("user.rotate.angle.1", "rotate", "angle");
        angle.is_angle = true;
        fx.append_user_binding(angle);
        let plain = sample_user_binding("user.mix.amount.1", "mix", "amount"); // is_angle: false
        fx.append_user_binding(plain);

        // Seed: the flag reached the live manifest spec (single home).
        assert!(fx.params.get("user.rotate.angle.1").unwrap().spec.is_angle);
        assert!(!fx.params.get("user.mix.amount.1").unwrap().spec.is_angle);

        // Read-back: synth (the card/renderer view) reflects the spec, not a
        // hardcoded false.
        let synth = fx.user_param_bindings();
        let a = synth.iter().find(|b| b.id == "user.rotate.angle.1").unwrap();
        let p = synth.iter().find(|b| b.id == "user.mix.amount.1").unwrap();
        assert!(a.is_angle, "angle user param must synth is_angle=true");
        assert!(!p.is_angle, "plain user param must stay is_angle=false");

        // Persistence: `is_angle: true` is emitted (skip_serializing_if only
        // skips false), so the flag survives save/load; false stays off disk.
        let json = serde_json::to_string(&fx).unwrap();
        assert!(json.contains("\"isAngle\":true"), "true angle flag must serialize");
        let back: PresetInstance = serde_json::from_str(&json).unwrap();
        assert!(back.params.get("user.rotate.angle.1").unwrap().spec.is_angle);
        assert!(!back.params.get("user.mix.amount.1").unwrap().spec.is_angle);
        assert!(
            back.user_param_bindings()
                .iter()
                .find(|b| b.id == "user.rotate.angle.1")
                .unwrap()
                .is_angle
        );
    }

    /// Regression for PARAM_STORAGE_BOUNDARIES_DESIGN.md P2 (D12): `graph
    /// .preset_metadata.params` is derived from the live manifest ONLY at
    /// serialize time — `EditParamMappingCommand` no longer dual-writes it,
    /// so the sole way a calibrated range can reach the wire is
    /// `GraphWithDerivedParams`. This builds an instance whose graph carries
    /// a STALE (template) `amount` spec that nothing in this test ever
    /// touches again, calibrates ONLY the manifest (mirroring what the
    /// command does post-P2), and proves the serialized `graph.presetMetadata
    /// .params` entry reflects the calibration, not the stale shadow — with
    /// a byte-comparison against the manifest's own spec.
    #[test]
    fn calibrated_param_derives_meta_params_on_save_not_the_stale_shadow() {
        use crate::effect_graph_def::{
            BindingDef, BindingTarget, EffectGraphDef, ParamSpecDef, PresetMetadata,
        };

        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.7, true)]);
        // Calibrate the manifest — the live authority (PARAM_STORAGE_DESIGN
        // D6) — diverging it from the template range the graph below still
        // carries untouched.
        {
            let p = fx.params.get_mut("amount").unwrap();
            p.spec.min = 10.0;
            p.spec.max = 20.0;
            p.spec.name = "Recalibrated Amount".to_string();
            p.calibrated = true;
        }
        // The graph's own shadow copy — STALE template range (0..1, "Amount").
        // Nothing after this construction ever writes to it directly; only
        // the derive-on-save wrapper may change what actually serializes.
        fx.graph = Some(EffectGraphDef {
            version: crate::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: PresetTypeId::BLOOM,
                display_name: String::new(),
                category: String::new(),
                osc_prefix: String::new(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: vec![ParamSpecDef {
                    id: "amount".to_string(),
                    name: "Amount".to_string(),
                    min: 0.0,
                    max: 1.0,
                    default_value: 0.7,
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
                }],
                bindings: vec![BindingDef {
                    id: "amount".to_string(),
                    label: "Amount".to_string(),
                    default_value: 0.7,
                    target: BindingTarget::Node {
                        node_id: NodeId::new("grade"),
                        param: "amount".to_string(),
                    },
                    convert: ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                }],
                skip_mode: Default::default(),
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
            }),
            nodes: Vec::new(),
            wires: Vec::new(),
        });

        let json = serde_json::to_string(&fx).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let on_wire = &parsed["graph"]["presetMetadata"]["params"][0];
        assert_eq!(
            on_wire["min"], 10.0,
            "serialized graph must carry the CALIBRATED min, not the stale template 0.0",
        );
        assert_eq!(
            on_wire["max"], 20.0,
            "serialized graph must carry the CALIBRATED max, not the stale template 1.0",
        );
        assert_eq!(on_wire["name"], "Recalibrated Amount");

        // Byte-comparison guard: the derived wire entry is JSON-identical to
        // the live manifest spec, serialized independently. Round-trip both
        // sides through JSON TEXT (not `to_value` directly) so `serde_json`'s
        // float-formatting path matches on both sides of the comparison
        // (`to_value` on an f32-sourced f64 keeps its imprecise binary
        // widening, e.g. `0.7_f32` -> `0.699999988079071`, while the text
        // path prints/reparses the shortest round-tripping form, `0.7`).
        let manifest_spec_json: serde_json::Value = serde_json::from_str(
            &serde_json::to_string(&fx.params.get("amount").unwrap().spec).unwrap(),
        )
        .unwrap();
        assert_eq!(
            on_wire, &manifest_spec_json,
            "the derived meta.params entry must be byte-identical to the manifest's own spec",
        );

        // Round trip: reload and confirm the manifest — the card's
        // authority — carries the calibrated range through, not just the
        // one-shot JSON snapshot above.
        let back: PresetInstance = serde_json::from_str(&json).unwrap();
        assert_eq!(back.params.get("amount").unwrap().spec.min, 10.0);
        assert_eq!(back.params.get("amount").unwrap().spec.max, 20.0);
    }

    #[test]
    fn append_user_binding_grows_param_values_with_default() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.7, true)]);
        fx.ensure_base_values();

        fx.append_user_binding(sample_user_binding("user.a.b.1", "a", "b"));
        assert_eq!(fx.params.len(), 2);
        assert_eq!(fx.params.get("amount").unwrap().value, 0.7);
        assert_eq!(fx.params.get("user.a.b.1").unwrap().value, 0.25);
        // base rides each slot now (fork #16).
        assert!(fx.base_tracked);
        assert_eq!(fx.params.get("amount").unwrap().base, 0.7);
        assert_eq!(fx.params.get("user.a.b.1").unwrap().base, 0.25);
        // The binding now lives in the graph (the single storage list).
        assert_eq!(fx.user_param_count(), 1);
    }

    #[test]
    fn remove_user_binding_drops_corresponding_value_slot() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.7, true)]);
        fx.append_user_binding(sample_user_binding("user.a.b.1", "a", "b"));
        fx.append_user_binding(sample_user_binding("user.c.d.1", "c", "d"));
        // A real slider edit sets base + value together (fork #16); set both so
        // the surviving slot is coherent after compaction.
        fx.set_base_param("user.a.b.1", 0.3);
        fx.set_base_param("user.c.d.1", 0.6);

        let removed = fx.remove_user_binding_by_id("user.a.b.1");
        assert!(removed.is_some());
        assert_eq!(fx.user_param_count(), 1);
        // Static prefix preserved + user tail compacted around the gap.
        // "amount" was seeded directly (never a `set_base_param` hand) so it
        // stays untouched; "user.c.d.1"'s value came from
        // `set_base_param("user.c.d.1", 0.6)` above, so it carries
        // `touched: true` — the funnel every hand (including this test's own
        // setup) writes through.
        assert_eq!(fx.params.len(), 2);
        let amount = fx.params.get("amount").unwrap();
        assert_eq!(amount.value, 0.7);
        assert!(!amount.touched);
        let cd = fx.params.get("user.c.d.1").unwrap();
        assert_eq!(cd.value, 0.6);
        assert_eq!(cd.base, 0.6);
        assert!(cd.exposed);
        assert!(cd.touched);
    }

    #[test]
    fn remove_user_binding_unknown_id_returns_none() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = crate::params::ParamManifest::from_params(vec![slot("amount", 0.7, true)]);
        let removed = fx.remove_user_binding_by_id("user.nope.1");
        assert!(removed.is_none());
        assert_eq!(fx.params.len(), 1);
        assert_eq!(fx.params.get("amount").unwrap().value, 0.7);
    }


    #[test]
    fn user_binding_index_lookup_by_id() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.append_user_binding(sample_user_binding("user.a.b.1", "a", "b"));
        fx.append_user_binding(sample_user_binding("user.c.d.1", "c", "d"));
        assert_eq!(fx.user_binding_index("user.a.b.1"), Some(0));
        assert_eq!(fx.user_binding_index("user.c.d.1"), Some(1));
        assert_eq!(fx.user_binding_index("user.nope.1"), None);
    }

    #[test]
    fn snapshot_values_into_def_bakes_current_base_as_default() {
        // Make Unique / Export must freeze the card's current values into the
        // def as its new defaults, so the preset reproduces the look later.
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.append_user_binding(sample_user_binding("user.a.b.1", "a", "b"));
        assert!(fx.set_base_param_by_id("user.a.b.1", 0.83));

        let mut def = fx.graph.clone().expect("graph carries metadata");
        fx.snapshot_values_into_def(&mut def);

        let meta = def.preset_metadata.as_ref().unwrap();
        let p = meta.params.iter().find(|p| p.id == "user.a.b.1").unwrap();
        assert_eq!(
            p.default_value, 0.83,
            "current base value becomes the def's param default"
        );
        let b = meta.bindings.iter().find(|b| b.id == "user.a.b.1").unwrap();
        assert_eq!(b.default_value, 0.83, "the binding default tracks it too");
    }

    #[test]
    fn reseed_param_values_from_def_replaces_values_with_def_defaults() {
        // Import retargets to a def with a different param structure; the old
        // positional values can't carry over, so reseed rebuilds them from the
        // def's defaults (declaration order, all exposed).
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.params = manifest(&[(0.1, true), (0.2, true)]);

        let mut donor = PresetInstance::new(PresetTypeId::BLOOM);
        donor.append_user_binding(sample_user_binding("user.x.y.1", "x", "y"));
        assert!(donor.set_base_param_by_id("user.x.y.1", 0.55));
        let mut def = donor.graph.clone().expect("graph carries metadata");
        donor.snapshot_values_into_def(&mut def);

        fx.reseed_param_values_from_def(&def);
        assert_eq!(
            fx.params.len(),
            1,
            "reseed rebuilds the manifest from the def's (snapshotted) defaults",
        );
        assert_eq!(fx.params.get("user.x.y.1").unwrap().value, 0.55);
    }

    #[test]
    fn remove_exposures_for_node_prunes_then_restores_round_trip() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        // Two exposed user params on different nodes; we delete node "blur".
        fx.append_user_binding(sample_user_binding("user.blur.radius.1", "blur", "radius"));
        fx.append_user_binding(sample_user_binding("user.other.x.1", "other", "x"));
        assert!(fx.set_base_param_by_id("user.blur.radius.1", 0.66));
        // Automation on the blur param — must be pruned with it, restored on undo.
        fx.create_driver(ParamId::from("user.blur.radius.1"));
        fx.envelopes = Some(vec![ParamEnvelope::new("user.blur.radius.1")]);

        // Snapshot entry content (not the whole manifest — `topology` bumps on
        // every push/remove/insert_at, so it legitimately differs after a
        // remove+restore round trip even though every param's own state is
        // back to identical).
        let pre_entries: Vec<crate::params::Param> = fx.params.iter().cloned().collect();

        let removed = fx.remove_exposures_for_node(&NodeId::new("blur"));
        assert_eq!(removed.len(), 1, "one slider was bound to the deleted node");

        // Slider, slot, driver, envelope all gone; the unrelated slider survives.
        assert!(!fx.params.contains("user.blur.radius.1"));
        assert!(fx.find_driver("user.blur.radius.1").is_none());
        assert!(
            fx.envelopes.is_none(),
            "pruning the last envelope collapses the list to None"
        );
        assert!(fx.params.contains("user.other.x.1"));

        // Undo restores values, metadata, and automation.
        fx.restore_exposures(removed);
        let post_entries: Vec<crate::params::Param> = fx.params.iter().cloned().collect();
        assert_eq!(
            post_entries, pre_entries,
            "value slots restored at their original positions"
        );
        assert!(
            fx.params.contains("user.blur.radius.1"),
            "binding + param spec restored"
        );
        assert!(fx.find_driver("user.blur.radius.1").is_some(), "driver restored");
        assert!(
            fx.find_envelope("user.blur.radius.1").is_some(),
            "envelope restored"
        );
    }

    #[test]
    fn prune_orphaned_automation_drops_unresolvable_then_restores() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.append_user_binding(sample_user_binding("user.a.b.1", "a", "b")); // resolves
        fx.create_driver(ParamId::from("user.a.b.1")); // live
        fx.create_driver(ParamId::from("user.gone.x.1")); // orphan — never bound
        fx.envelopes = Some(vec![ParamEnvelope::new("user.gone.x.1")]); // orphan
        fx.automation_lanes = Some(vec![AutomationLane {
            param_id: ParamId::from("user.gone.x.1"),
            enabled: true,
            points: vec![AutomationPoint {
                beat: Beats(0.0),
                value: 0.5,
                shape: SegmentShape::Linear,
            }],
        }]); // orphan — same unresolvable id as the driver/envelope above

        let removed = fx.prune_orphaned_automation();
        assert!(fx.find_driver("user.a.b.1").is_some(), "live driver kept");
        assert!(fx.find_driver("user.gone.x.1").is_none(), "orphan driver pruned");
        assert!(
            fx.envelopes.is_none(),
            "sole orphan envelope pruned, list collapses to None"
        );
        assert!(
            fx.automation_lanes.is_none(),
            "sole orphan automation lane pruned, list collapses to None"
        );

        fx.restore_automation(removed);
        assert!(
            fx.find_driver("user.gone.x.1").is_some(),
            "orphan driver restored on undo"
        );
        assert!(
            fx.find_envelope("user.gone.x.1").is_some(),
            "orphan envelope restored on undo"
        );
        assert_eq!(
            fx.automation_lanes.as_ref().map(|v| v.len()),
            Some(1),
            "orphan automation lane restored on undo"
        );
    }

    #[test]
    fn remove_exposures_for_node_is_noop_when_nothing_bound() {
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.append_user_binding(sample_user_binding("user.blur.radius.1", "blur", "radius"));
        let before = fx.params.clone();
        let removed = fx.remove_exposures_for_node(&NodeId::new("nonexistent"));
        assert!(removed.is_empty(), "no binding targets that node");
        assert_eq!(fx.params, before, "nothing changed");
    }

    #[test]
    fn get_param_def_synthesizes_user_binding_def() {
        // ParamSource::get_param_def must return a ParamSpecDef shaped from
        // the user binding for indices past the static count, so UI code
        // (slider rendering, OSC formatting) gets correct min/max/label.
        let mut fx = PresetInstance::new(PresetTypeId::BLOOM);
        fx.append_user_binding(UserParamBinding {
            id: "user.uv.translate.1".to_string(),
            label: "Translate".to_string(),
            node_id: NodeId::new("uv_transform"),
            legacy_node_handle: None,
            inner_param: "translate".to_string(),
            min: -2.0,
            max: 2.0,
            default_value: 0.0,
            convert: ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
            value_labels: Vec::new(),
            section: None,
        });
        let pd = ParamSource::get_param_def(&fx, "user.uv.translate.1");
        assert_eq!(pd.id, "user.uv.translate.1");
        assert_eq!(pd.name, "Translate");
        assert!((pd.min + 2.0).abs() < f32::EPSILON);
        assert!((pd.max - 2.0).abs() < f32::EPSILON);
        assert!(!pd.whole_numbers);
        assert!(!pd.is_toggle);
    }

    #[test]
    fn deserialize_keyed_param_values_routes_user_ids_to_tail() {
        // The key insight: `params` comes in as a Map. The custom
        // Deserialize must consult the graph's `user_added` bindings (the
        // single storage list after the unification) to route user ids to
        // the right tail slots — regardless of JSON key order in the Map.
        let json = r#"{
            "id": "abc12345",
            "effectType": "Bloom",
            "enabled": true,
            "collapsed": false,
            "params": {
                "amount": { "value": 0.7 },
                "user.foo.bar.1": { "value": 0.3 },
                "user.baz.qux.1": { "value": 0.9 }
            },
            "graph": {
                "version": 0,
                "nodes": [],
                "wires": [],
                "presetMetadata": {
                    "id": "",
                    "displayName": "",
                    "category": "",
                    "oscPrefix": "",
                    "params": [
                        { "id": "user.foo.bar.1", "name": "Foo", "min": 0.0, "max": 1.0, "defaultValue": 0.5 },
                        { "id": "user.baz.qux.1", "name": "Baz", "min": 0.0, "max": 1.0, "defaultValue": 0.5 }
                    ],
                    "bindings": [
                        { "id": "user.foo.bar.1", "label": "Foo", "defaultValue": 0.5, "userAdded": true, "target": { "kind": "node", "nodeId": "foo", "param": "bar" } },
                        { "id": "user.baz.qux.1", "label": "Baz", "defaultValue": 0.5, "userAdded": true, "target": { "kind": "node", "nodeId": "baz", "param": "qux" } }
                    ]
                }
            }
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        assert_eq!(fx.user_param_count(), 2);
        assert_eq!(fx.params.len(), 3);
        assert!((fx.params.get("amount").unwrap().value - 0.7).abs() < f32::EPSILON);
        assert!((fx.params.get("user.foo.bar.1").unwrap().value - 0.3).abs() < f32::EPSILON);
        assert!((fx.params.get("user.baz.qux.1").unwrap().value - 0.9).abs() < f32::EPSILON);
    }

    // ─── Per-instance graph override (Phase 1) ──────────────────

    #[test]
    fn new_effect_instance_has_no_graph_override() {
        let fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        assert!(fx.graph.is_none());
        assert_eq!(fx.graph_version, 0);
    }

    #[test]
    fn graph_field_skipped_when_none() {
        // Existing fixtures (Liveschool, Burn, WAYPOINTS) must
        // continue to round-trip byte-identically — the new field
        // must not appear in their JSON unless explicitly set.
        let fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        let json = serde_json::to_string(&fx).unwrap();
        assert!(
            !json.contains("\"graph\""),
            "graph field must be skipped when None — got: {json}"
        );
    }

    #[test]
    fn graph_field_round_trips_when_present() {
        use crate::effect_graph_def::{
            EFFECT_GRAPH_VERSION, EffectGraphDef, EffectGraphNode, EffectGraphWire,
            SerializedParamValue,
        };

        let mut params = std::collections::BTreeMap::new();
        params.insert("mode".to_string(), SerializedParamValue::Enum { value: 7 });

        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![
                EffectGraphNode {
                    id: 0,
                    node_id: crate::NodeId::default(),
                    type_id: "system.source".to_string(),
                    handle: Some("source".to_string()),
                    params: Default::default(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: std::collections::BTreeMap::new(),
                    output_canvas_scales: std::collections::BTreeMap::new(),
                    group: None,
                },
                EffectGraphNode {
                    id: 1,
                    node_id: crate::NodeId::default(),
                    type_id: "node.transform".to_string(),
                    handle: Some("uv_transform".to_string()),
                    params,
                    exposed_params: Default::default(),
                    editor_pos: Some((100.0, 200.0)),
                    wgsl_source: None,
                    title: None,
                    output_formats: std::collections::BTreeMap::new(),
                    output_canvas_scales: std::collections::BTreeMap::new(),
                    group: None,
                },
            ],
            wires: vec![EffectGraphWire {
                from_node: 0,
                from_port: "out".to_string(),
                to_node: 1,
                to_port: "source".to_string(),
            }],
        };

        let mut fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        fx.graph = Some(def.clone());

        let json = serde_json::to_string(&fx).unwrap();
        assert!(json.contains("\"graph\""));

        let back: PresetInstance = serde_json::from_str(&json).unwrap();
        assert_eq!(back.graph, Some(def));
        // `graph_version` is not serialized — it resets on load.
        assert_eq!(back.graph_version, 0);
    }

    #[test]
    fn legacy_fixture_without_graph_field_still_loads() {
        // Pre-Phase-1 fixtures have no `graph` field at all. Loading
        // them must succeed with `graph: None`.
        let json = r#"{
            "id": "abc12345",
            "effectType": "Mirror",
            "enabled": true,
            "collapsed": false,
            "params": { "amount": { "value": 1.0, "exposed": true } }
        }"#;
        let fx: PresetInstance = serde_json::from_str(json).unwrap();
        assert!(fx.graph.is_none());
    }

    // ─── Automation lane curve evaluation ───

    fn pt(beat: f64, value: f32, shape: SegmentShape) -> AutomationPoint {
        AutomationPoint {
            beat: Beats(beat),
            value,
            shape,
        }
    }

    #[test]
    fn automation_lane_empty_returns_zero() {
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: Vec::new(),
        };
        assert_eq!(lane.value_at(Beats(4.0)), 0.0);
    }

    #[test]
    fn automation_lane_single_point_holds_everywhere() {
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![pt(4.0, 0.7, SegmentShape::Linear)],
        };
        assert_eq!(lane.value_at(Beats(-10.0)), 0.7);
        assert_eq!(lane.value_at(Beats(4.0)), 0.7);
        assert_eq!(lane.value_at(Beats(100.0)), 0.7);
    }

    #[test]
    fn automation_lane_before_first_point_holds_first_value() {
        // Ableton behavior: no backward extrapolation.
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(4.0, 0.2, SegmentShape::Linear),
                pt(8.0, 0.8, SegmentShape::Linear),
            ],
        };
        assert_eq!(lane.value_at(Beats(0.0)), 0.2);
        assert_eq!(lane.value_at(Beats(4.0)), 0.2);
    }

    #[test]
    fn automation_lane_after_last_point_holds_last_value() {
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(4.0, 0.2, SegmentShape::Linear),
                pt(8.0, 0.8, SegmentShape::Linear),
            ],
        };
        assert_eq!(lane.value_at(Beats(8.0)), 0.8);
        assert_eq!(lane.value_at(Beats(1000.0)), 0.8);
    }

    #[test]
    fn automation_lane_linear_segment_interpolates() {
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Linear),
                pt(4.0, 1.0, SegmentShape::Linear),
            ],
        };
        assert!((lane.value_at(Beats(2.0)) - 0.5).abs() < 1e-6);
        assert!((lane.value_at(Beats(1.0)) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn automation_lane_hold_segment_steps() {
        // `Hold` on the earlier point: the segment holds that point's value
        // for its whole span, then jumps at the next point — required for
        // enum/int-backed params.
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Hold),
                pt(4.0, 1.0, SegmentShape::Hold),
                pt(8.0, 2.0, SegmentShape::Linear),
            ],
        };
        assert_eq!(lane.value_at(Beats(0.0)), 0.0);
        assert_eq!(lane.value_at(Beats(3.9)), 0.0, "holds through the segment");
        assert_eq!(lane.value_at(Beats(4.0)), 1.0, "steps exactly at the next point");
        assert_eq!(lane.value_at(Beats(7.9)), 1.0);
    }

    #[test]
    fn automation_lane_curved_segment_bends_but_keeps_endpoints() {
        let convex = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Curved(1.0)),
                pt(4.0, 1.0, SegmentShape::Linear),
            ],
        };
        let concave = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Curved(-1.0)),
                pt(4.0, 1.0, SegmentShape::Linear),
            ],
        };
        // Endpoints exact regardless of bend.
        assert_eq!(convex.value_at(Beats(0.0)), 0.0);
        assert_eq!(convex.value_at(Beats(4.0)), 1.0);
        // Midpoint: positive bend (convex) sits BELOW the linear midpoint
        // (slow start); negative bend (concave) sits ABOVE it (fast start).
        let mid_linear = 0.5;
        let mid_convex = convex.value_at(Beats(2.0));
        let mid_concave = concave.value_at(Beats(2.0));
        assert!(mid_convex < mid_linear, "convex bend lags at the midpoint");
        assert!(mid_concave > mid_linear, "concave bend leads at the midpoint");
    }

    #[test]
    fn automation_lane_bend_out_of_range_is_clamped() {
        // `Curved` bends are only meaningful in -1..1; anything past that
        // clamps rather than producing a wild exponent.
        let over = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Curved(5.0)),
                pt(4.0, 1.0, SegmentShape::Linear),
            ],
        };
        let clamped = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Curved(1.0)),
                pt(4.0, 1.0, SegmentShape::Linear),
            ],
        };
        assert!((over.value_at(Beats(2.0)) - clamped.value_at(Beats(2.0))).abs() < 1e-6);
    }

    #[test]
    fn automation_lane_three_points_binary_search_finds_middle_segment() {
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Linear),
                pt(4.0, 1.0, SegmentShape::Linear),
                pt(8.0, 0.0, SegmentShape::Linear),
            ],
        };
        assert!((lane.value_at(Beats(6.0)) - 0.5).abs() < 1e-6);
    }

    // ─── PresetInstance.automation_lanes serde (skip-when-empty) ───

    #[test]
    fn preset_instance_without_automation_lanes_serializes_byte_identical() {
        // No lanes → no `automationLanes` key at all, and round-tripping a
        // fixture that never had lanes must not introduce one. Same
        // skip-when-empty convention as `drivers`/`envelopes`/`audioMods`.
        let fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        assert!(fx.automation_lanes.is_none());
        let json = serde_json::to_string(&fx).unwrap();
        assert!(
            !json.contains("automationLanes"),
            "no automation_lanes → no key on the wire; got: {json}"
        );
        let back: PresetInstance = serde_json::from_str(&json).unwrap();
        assert!(back.automation_lanes.is_none());
    }

    #[test]
    fn preset_instance_automation_lanes_roundtrip_when_present() {
        let mut fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        fx.automation_lanes = Some(vec![AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![pt(0.0, 0.25, SegmentShape::Curved(0.5))],
        }]);
        let json = serde_json::to_string(&fx).unwrap();
        assert!(json.contains("automationLanes"));
        let back: PresetInstance = serde_json::from_str(&json).unwrap();
        let lanes = back.automation_lanes.expect("lanes round-trip");
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].param_id, ParamId::from("amount"));
        assert!(lanes[0].enabled);
        assert_eq!(lanes[0].points.len(), 1);
        assert_eq!(lanes[0].points[0].value, 0.25);
        assert_eq!(lanes[0].points[0].shape, SegmentShape::Curved(0.5));
    }

    // ─── touched flag: the automation self-trigger footgun ───

    #[test]
    fn set_base_param_marks_touched() {
        // The single funnel every live hand writes through — the automation
        // evaluator's touch-detection relies on this.
        let mut fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        fx.params = manifest(&[(0.0, true)]);
        fx.set_base_param("p0", 0.5);
        assert!(fx.params.get("p0").unwrap().touched, "set_base_param marks touched");
    }

    #[test]
    fn write_base_param_does_not_mark_touched() {
        // System-level seeding (registry defaults) must not look like a hand
        // touch — see `preset_definition_registry::create_default`.
        let mut fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        fx.params = manifest(&[(0.0, true)]);
        fx.write_base_param("p0", 0.5);
        assert!(
            !fx.params.get("p0").unwrap().touched,
            "write_base_param must not set touched"
        );
        assert_eq!(fx.params.get("p0").unwrap().base, 0.5);
        assert_eq!(fx.params.get("p0").unwrap().value, 0.5);
    }

    #[test]
    fn set_base_param_from_automation_does_not_mark_touched() {
        // The automation evaluator's own write path — using the public
        // set_base_param here would self-latch the very next frame.
        let mut fx = PresetInstance::new(PresetTypeId::new("Mirror"));
        fx.params = manifest(&[(0.0, true)]);
        fx.set_base_param_from_automation("p0", 0.5);
        assert!(
            !fx.params.get("p0").unwrap().touched,
            "set_base_param_from_automation must not set touched"
        );
        assert_eq!(fx.params.get("p0").unwrap().base, 0.5);
        assert_eq!(fx.params.get("p0").unwrap().value, 0.5);
    }

    // Registered via `inventory::submit!` at module scope (mirrors
    // `manifold-playback`'s `modulation::tests` fixture pattern) — the
    // registry is normally populated by manifold-renderer's effect
    // implementations, which manifold-core's own test binary doesn't link.
    inventory::submit! {
        crate::effect_registration::EffectMetadata {
            id: PresetTypeId::new("TestCreateDefaultUntouched"),
            display_name: "Test Create Default Untouched",
            category: "Test",
            available: true,
            osc_prefix: "testCreateDefaultUntouched",
            legacy_discriminant: None,
            params: &[crate::generator_registration::ParamSpec::continuous(
                "amount", "Amount", 0.0, 1.0, 0.42, "F2", "",
            )],
        }
    }

    #[test]
    fn create_default_does_not_mark_params_touched() {
        // The exact bug this phase's call-site audit found: `create_default`
        // used to seed via the public `set_base_param`, which would have
        // marked every freshly-created effect's params `touched` before any
        // lane or hand ever existed — pre-latching any lane authored on them
        // later.
        let inst = crate::preset_definition_registry::create_default(&PresetTypeId::new(
            "TestCreateDefaultUntouched",
        ));
        assert!(
            !inst.params.get("amount").unwrap().touched,
            "create_default must not mark freshly-seeded params touched"
        );
        assert_eq!(inst.params.get("amount").unwrap().base, 0.42);
    }

    // `bundled_slider_delete_does_not_misroute_survivor_drivers` (and its
    // `TestBundledSliderMisroute` fixture registration) was DELETED
    // (PARAM_STORAGE_DESIGN.md D3): it existed to prove a fix for a bug
    // that only the OLD dual-resolution scheme could have — a live
    // per-instance `meta.params` position (`param_id_to_value_index`)
    // disagreeing with a frozen-registry position (`resolve_param_in`)
    // after a bundled slider was deleted mid-array. Both mechanisms are
    // gone; every param is now addressed by stable id everywhere (card
    // display, pruning, and runtime modulation resolution alike), so
    // there is no positional index to disagree in the first place.

    // ── §9 U1: unified trigger-gate mods ─────────────────────────────────

    /// A bundled `is_trigger_gate` param — mirrors [`slot`] but flips the
    /// gate flag, the same way a `clip_trigger` toggle card ships on the 11
    /// trigger-responsive generator presets.
    fn gate_slot(id: &str) -> crate::params::Param {
        let mut p = slot(id, 0.0, true);
        p.spec.is_toggle = true;
        p.spec.is_trigger_gate = true;
        p
    }

    #[test]
    fn clip_edge_enabled_matrix() {
        use crate::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod};
        use crate::audio_trigger::TriggerFireMode;
        use crate::id::AudioSendId;

        let mut inst = PresetInstance::new(PresetTypeId::new("TestGate"));
        inst.params.push(gate_slot("clip_trigger"));

        // No mod at all → clip edge unconditionally on (pre-§8 behavior).
        assert!(inst.clip_edge_enabled());

        let mut m = ParameterAudioMod::new(
            "clip_trigger".into(),
            AudioSendId::new("send-1"),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        m.trigger_mode = Some(TriggerFireMode::Transient);
        m.enabled = false;
        inst.audio_mods_mut().push(m);

        // Disabled mod → disabled-means-absent, clip edge stays on.
        assert!(inst.clip_edge_enabled(), "disabled mod must be inert");

        inst.audio_mods.as_mut().unwrap()[0].enabled = true;
        assert!(!inst.clip_edge_enabled(), "armed Transient mode gates the clip edge");

        inst.audio_mods.as_mut().unwrap()[0].trigger_mode = Some(TriggerFireMode::ClipEdge);
        assert!(inst.clip_edge_enabled());

        inst.audio_mods.as_mut().unwrap()[0].trigger_mode = Some(TriggerFireMode::Both);
        assert!(inst.clip_edge_enabled());
    }

    #[test]
    fn legacy_audio_trigger_migrates_onto_a_parameter_audio_mod_on_the_gate_param() {
        // The exact `audioTrigger` shape a project saved during the one day
        // §8's `AudioTriggerMod` shipped (see
        // `docs/LIVE_AUDIO_TRIGGERS_DESIGN.md` §9 U5). A generator-kind
        // instance's `graph.presetMetadata.params` is the only route to an
        // `is_trigger_gate` param outside the JSON preset path (the
        // compile-time `ParamSpec` inventory format has no field for it —
        // see `generator_registration::ParamSpec::to_param_def`), so the
        // fixture carries its own minimal per-instance graph.
        let json = r#"{
            "generatorType": "TestGenTrig",
            "graph": {
                "version": 2,
                "presetMetadata": {
                    "id": "TestGenTrig",
                    "displayName": "Test Gen Trig",
                    "category": "Test",
                    "oscPrefix": "testGenTrig",
                    "params": [
                        {
                            "id": "clip_trigger",
                            "name": "Clip Trigger",
                            "min": 0.0,
                            "max": 1.0,
                            "defaultValue": 0.0,
                            "isToggle": true,
                            "isTriggerGate": true
                        }
                    ],
                    "bindings": []
                },
                "nodes": [],
                "wires": []
            },
            "audioTrigger": {
                "enabled": false,
                "source": {
                    "sendId": "e14b42f8",
                    "feature": { "kind": "transients", "band": "full" }
                },
                "sensitivity": 1.0,
                "mode": "transient"
            }
        }"#;

        let mut de = serde_json::Deserializer::from_str(json);
        let inst = deserialize_generator_instance(&mut de).unwrap();

        assert_eq!(inst.kind, crate::preset_def::PresetKind::Generator);
        let mods = inst
            .audio_mods
            .as_ref()
            .expect("legacy audioTrigger must migrate onto audio_mods");
        assert_eq!(mods.len(), 1);
        let m = &mods[0];
        assert_eq!(m.param_id.as_ref(), "clip_trigger", "targets the gate param");
        assert!(!m.enabled, "legacy enabled=false carries over");
        assert_eq!(m.source.send_id, crate::id::AudioSendId::new("e14b42f8"));
        assert_eq!(
            m.source.feature,
            crate::audio_mod::AudioFeature::new(
                crate::audio_mod::AudioFeatureKind::Transients,
                crate::audio_mod::AudioBand::Full
            )
        );
        assert_eq!(
            m.trigger_mode,
            Some(crate::audio_trigger::TriggerFireMode::Transient)
        );
        assert_eq!(m.shape.sensitivity, 1.0, "sensitivity approximates onto Amount (U5)");
    }

    #[test]
    fn legacy_audio_trigger_with_no_gate_param_is_dropped_not_guessed() {
        // No `isTriggerGate` param anywhere on the instance → the migration
        // has no target to attach to and must drop the config rather than
        // guess one (a hand-edited file, or an instance saved before the
        // flag existed).
        let json = r#"{
            "generatorType": "TestGenTrigNoGate",
            "audioTrigger": {
                "enabled": true,
                "source": {
                    "sendId": "send-1",
                    "feature": { "kind": "transients", "band": "full" }
                },
                "sensitivity": 0.5,
                "mode": "both"
            }
        }"#;

        let mut de = serde_json::Deserializer::from_str(json);
        let inst = deserialize_generator_instance(&mut de).unwrap();
        assert!(inst.audio_mods.is_none(), "no gate param means nothing to migrate onto");
    }
}
