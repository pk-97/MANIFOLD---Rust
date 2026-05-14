//! Macro bank: 8 macro sliders that fan out to multiple project parameters.
//!
//! Each macro receives a single 0–1 value (via OSC `/macro/1`–`/macro/8` or UI)
//! and distributes it to N target parameters through configurable mappings with
//! optional response curves. Targets are identified by the same addressing scheme
//! as `OscParamTarget` so the fan-out reuses the existing parameter write path.

use crate::effect_type_id::EffectTypeId;
use crate::effects::ParamId;
use crate::id::LayerId;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Number of macro slots in the bank.
pub const MACRO_COUNT: usize = 8;

// ── Response curve ─────────────────────────────────────────────────

/// Response curve applied when mapping a macro value to a target parameter.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MacroCurve {
    #[default]
    Linear,
    Exponential,
    Logarithmic,
    SCurve,
}

impl MacroCurve {
    /// Map a normalized 0–1 input through this curve, returning 0–1.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::Exponential => t * t,
            Self::Logarithmic => t.sqrt(),
            Self::SCurve => {
                // Hermite S-curve: 3t² - 2t³
                t * t * (3.0 - 2.0 * t)
            }
        }
    }
}

// ── Mapping target ─────────────────────────────────────────────────

/// What a macro mapping points to. Mirrors OscParamTarget but is serializable.
///
/// Parameter-bearing variants (`MasterEffect`, `LayerEffect`, `GenParam`)
/// address by stable [`ParamId`] (since step 11). Custom [`Deserialize`]
/// accepts both V1.1 (`paramIndex: usize`) and V1.2+ (`paramId: "amount"`)
/// shapes; legacy indices are parked on [`MacroMapping::legacy_param_index`]
/// for the post-load resolver.
///
/// Serialize: this enum has no recovery state of its own — its
/// [`MacroMapping`] wrapper carries `legacy_param_index`. To preserve
/// recovery information across save→load when the registry is missing,
/// the wrapper's custom [`Serialize`] re-renders the variant via
/// internal helpers (it can't delegate to `derive(Serialize)` here
/// because the "emit param_id XOR param_index" choice depends on the
/// wrapper's state, not the variant's).
#[derive(Debug, Clone)]
pub enum MacroMappingTarget {
    MasterOpacity,
    MasterEffect {
        effect_type: EffectTypeId,
        param_id: ParamId,
    },
    LayerOpacity {
        layer_id: LayerId,
    },
    LayerEffect {
        layer_id: LayerId,
        effect_type: EffectTypeId,
        param_id: ParamId,
    },
    GenParam {
        layer_id: LayerId,
        param_id: ParamId,
    },
}

// ── Mapping ────────────────────────────────────────────────────────

/// A single mapping from a macro slot to a project parameter.
#[derive(Debug, Clone)]
pub struct MacroMapping {
    pub target: MacroMappingTarget,
    pub range_min: f32,
    pub range_max: f32,
    pub curve: MacroCurve,
    /// Parked legacy `param_index` from V1.1 deserialization or
    /// RegistryMissing fallback. See
    /// [`crate::effects::ParameterDriver::legacy_param_index`] for the
    /// recovery invariant — same contract here, but the param_id lives
    /// in the target variant rather than on the wrapper.
    pub legacy_param_index: Option<i32>,
}

// Custom Serialize: the wrapper's `legacy_param_index` plus the variant's
// `param_id` together determine which addressing shape to emit. We
// can't delegate to `derive(Serialize)` on `MacroMappingTarget` because
// the choice depends on the wrapper's state.
//
// Wire shape preserved exactly — variant tag is camelCase, target field
// names are snake_case (matches existing fixtures).
impl Serialize for MacroMapping {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("MacroMapping", 4)?;
        s.serialize_field(
            "target",
            &MacroMappingTargetSer {
                target: &self.target,
                legacy_param_index: self.legacy_param_index,
            },
        )?;
        s.serialize_field("rangeMin", &self.range_min)?;
        s.serialize_field("rangeMax", &self.range_max)?;
        s.serialize_field("curve", &self.curve)?;
        s.end()
    }
}

/// Serialize-side wrapper for `MacroMappingTarget` that carries the
/// outer mapping's `legacy_param_index`. Used to re-emit `param_index`
/// when the variant's `param_id` is empty AND the index is parked.
struct MacroMappingTargetSer<'a> {
    target: &'a MacroMappingTarget,
    legacy_param_index: Option<i32>,
}

impl Serialize for MacroMappingTargetSer<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;

        let leg = self.legacy_param_index;
        match self.target {
            MacroMappingTarget::MasterOpacity => {
                let mut m = serializer.serialize_map(Some(1))?;
                m.serialize_entry("type", "masterOpacity")?;
                m.end()
            }
            MacroMappingTarget::LayerOpacity { layer_id } => {
                let mut m = serializer.serialize_map(Some(2))?;
                m.serialize_entry("type", "layerOpacity")?;
                m.serialize_entry("layer_id", layer_id)?;
                m.end()
            }
            MacroMappingTarget::MasterEffect {
                effect_type,
                param_id,
            } => {
                let (emit_id, emit_idx) = decide_emit(param_id, leg);
                let mut count = 2;
                if emit_id || emit_idx {
                    count += 1;
                }
                let mut m = serializer.serialize_map(Some(count))?;
                m.serialize_entry("type", "masterEffect")?;
                m.serialize_entry("effect_type", effect_type)?;
                if emit_id {
                    m.serialize_entry("param_id", param_id)?;
                } else if emit_idx {
                    m.serialize_entry("param_index", &leg.unwrap())?;
                }
                m.end()
            }
            MacroMappingTarget::LayerEffect {
                layer_id,
                effect_type,
                param_id,
            } => {
                let (emit_id, emit_idx) = decide_emit(param_id, leg);
                let mut count = 3;
                if emit_id || emit_idx {
                    count += 1;
                }
                let mut m = serializer.serialize_map(Some(count))?;
                m.serialize_entry("type", "layerEffect")?;
                m.serialize_entry("layer_id", layer_id)?;
                m.serialize_entry("effect_type", effect_type)?;
                if emit_id {
                    m.serialize_entry("param_id", param_id)?;
                } else if emit_idx {
                    m.serialize_entry("param_index", &leg.unwrap())?;
                }
                m.end()
            }
            MacroMappingTarget::GenParam { layer_id, param_id } => {
                let (emit_id, emit_idx) = decide_emit(param_id, leg);
                let mut count = 2;
                if emit_id || emit_idx {
                    count += 1;
                }
                let mut m = serializer.serialize_map(Some(count))?;
                m.serialize_entry("type", "genParam")?;
                m.serialize_entry("layer_id", layer_id)?;
                if emit_id {
                    m.serialize_entry("param_id", param_id)?;
                } else if emit_idx {
                    m.serialize_entry("param_index", &leg.unwrap())?;
                }
                m.end()
            }
        }
    }
}

/// Returns `(emit_param_id, emit_param_index)`. Exactly one is `true`
/// when there's recoverable addressing data; both are `false` when the
/// mapping is permanently orphaned (param_id empty AND no legacy idx).
fn decide_emit(param_id: &ParamId, legacy_index: Option<i32>) -> (bool, bool) {
    let emit_id = !param_id.is_empty();
    let emit_idx = !emit_id && legacy_index.is_some();
    (emit_id, emit_idx)
}

// Custom `Deserialize` accepting both V1.1 (`paramIndex: usize` inside
// the target variant) and V1.2+ (`paramId: "amount"` inside the target
// variant) shapes. Legacy indices are parked on the wrapper's
// `legacy_param_index` field for the post-load resolver to translate.
//
// See `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 step 11.
impl<'de> Deserialize<'de> for MacroMapping {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Raw target shape: accepts either `paramIndex` or `paramId`.
        // Variants without parameters (`masterOpacity`, `layerOpacity`)
        // pass through unchanged.
        //
        // Field names on the wire are snake_case (matching legacy V1.1
        // projects); `rename_all = "camelCase"` here only affects the
        // variant tag (`masterEffect`, `layerEffect`, …). Adding
        // `paramId` as the canonical key keeps the rest of the variant
        // shape stable.
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", tag = "type")]
        enum RawTarget {
            MasterOpacity,
            MasterEffect {
                effect_type: EffectTypeId,
                #[serde(default)]
                param_id: Option<String>,
                #[serde(default)]
                param_index: Option<i32>,
            },
            LayerOpacity {
                layer_id: LayerId,
            },
            LayerEffect {
                layer_id: LayerId,
                effect_type: EffectTypeId,
                #[serde(default)]
                param_id: Option<String>,
                #[serde(default)]
                param_index: Option<i32>,
            },
            GenParam {
                layer_id: LayerId,
                #[serde(default)]
                param_id: Option<String>,
                #[serde(default)]
                param_index: Option<i32>,
            },
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            target: RawTarget,
            #[serde(default)]
            range_min: f32,
            #[serde(default = "default_one")]
            range_max: f32,
            #[serde(default)]
            curve: MacroCurve,
        }

        fn split_id(param_id: Option<String>, param_index: Option<i32>) -> (ParamId, Option<i32>) {
            match (param_id, param_index) {
                (Some(id), _) if !id.is_empty() => (Cow::Owned(id), None),
                (_, Some(idx)) => (Cow::Borrowed(""), Some(idx)),
                (_, None) => (Cow::Borrowed(""), None),
            }
        }

        let raw = Raw::deserialize(deserializer)?;
        let (target, legacy_param_index) = match raw.target {
            RawTarget::MasterOpacity => (MacroMappingTarget::MasterOpacity, None),
            RawTarget::MasterEffect {
                effect_type,
                param_id,
                param_index,
            } => {
                let (param_id, legacy) = split_id(param_id, param_index);
                (
                    MacroMappingTarget::MasterEffect {
                        effect_type,
                        param_id,
                    },
                    legacy,
                )
            }
            RawTarget::LayerOpacity { layer_id } => {
                (MacroMappingTarget::LayerOpacity { layer_id }, None)
            }
            RawTarget::LayerEffect {
                layer_id,
                effect_type,
                param_id,
                param_index,
            } => {
                let (param_id, legacy) = split_id(param_id, param_index);
                (
                    MacroMappingTarget::LayerEffect {
                        layer_id,
                        effect_type,
                        param_id,
                    },
                    legacy,
                )
            }
            RawTarget::GenParam {
                layer_id,
                param_id,
                param_index,
            } => {
                let (param_id, legacy) = split_id(param_id, param_index);
                (MacroMappingTarget::GenParam { layer_id, param_id }, legacy)
            }
        };

        Ok(MacroMapping {
            target,
            range_min: raw.range_min,
            range_max: raw.range_max,
            curve: raw.curve,
            legacy_param_index,
        })
    }
}

fn default_one() -> f32 {
    1.0
}

// ── Slot ───────────────────────────────────────────────────────────

/// One of 8 macro slots. Holds the current value and its mappings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroSlot {
    #[serde(default)]
    pub value: f32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mappings: Vec<MacroMapping>,
    /// Ableton Live macro mapped to this slot (drives `value` when active).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ableton_mapping: Option<crate::ableton_mapping::AbletonParamMapping>,
}

impl Default for MacroSlot {
    fn default() -> Self {
        Self {
            value: 0.0,
            label: String::new(),
            mappings: Vec::new(),
            ableton_mapping: None,
        }
    }
}

// ── Bank ───────────────────────────────────────────────────────────

/// Bank of 8 macros. Always exactly 8 slots.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroBank {
    #[serde(default = "default_slots")]
    pub slots: Vec<MacroSlot>,
}

fn default_slots() -> Vec<MacroSlot> {
    (0..MACRO_COUNT).map(|_| MacroSlot::default()).collect()
}

impl Default for MacroBank {
    fn default() -> Self {
        Self {
            slots: default_slots(),
        }
    }
}

impl MacroBank {
    /// Ensure exactly MACRO_COUNT slots (handles old files with fewer/more).
    pub fn normalize(&mut self) {
        self.slots.resize_with(MACRO_COUNT, MacroSlot::default);
    }

    /// Apply a macro value change: update the slot and fan out to all mapped
    /// targets. Called from both OSC `apply()` and UI dispatch.
    pub fn apply_macro(project: &mut crate::project::Project, index: usize, value: f32) {
        if index >= MACRO_COUNT {
            return;
        }

        let value = value.clamp(0.0, 1.0);
        project.settings.macro_bank.slots[index].value = value;

        // Collect mappings to avoid borrow conflict (slot borrows project.settings)
        let mappings: Vec<_> = project.settings.macro_bank.slots[index].mappings.clone();

        for mapping in &mappings {
            let curved = mapping.curve.apply(value);
            let mapped = mapping.range_min + (mapping.range_max - mapping.range_min) * curved;

            match &mapping.target {
                MacroMappingTarget::MasterOpacity => {
                    project.settings.set_master_opacity(mapped);
                }
                MacroMappingTarget::MasterEffect {
                    effect_type,
                    param_id,
                } => {
                    let Some(idx) = crate::effect_definition_registry::param_id_to_index(
                        effect_type,
                        param_id.as_ref(),
                    ) else {
                        continue;
                    };
                    if let Some(fx) = project
                        .settings
                        .master_effects
                        .iter_mut()
                        .find(|f| f.effect_type() == effect_type)
                    {
                        fx.set_base_param(idx, mapped);
                    }
                }
                MacroMappingTarget::LayerOpacity { layer_id } => {
                    if let Some((_, layer)) =
                        project.timeline.find_layer_by_id_mut(layer_id.as_str())
                    {
                        layer.opacity = mapped.clamp(0.0, 1.0);
                    }
                }
                MacroMappingTarget::LayerEffect {
                    layer_id,
                    effect_type,
                    param_id,
                } => {
                    let Some(idx) = crate::effect_definition_registry::param_id_to_index(
                        effect_type,
                        param_id.as_ref(),
                    ) else {
                        continue;
                    };
                    if let Some((_, layer)) =
                        project.timeline.find_layer_by_id_mut(layer_id.as_str())
                        && let Some(effects) = &mut layer.effects
                        && let Some(fx) =
                            effects.iter_mut().find(|f| f.effect_type() == effect_type)
                    {
                        fx.set_base_param(idx, mapped);
                    }
                }
                MacroMappingTarget::GenParam { layer_id, param_id } => {
                    if let Some((_, layer)) =
                        project.timeline.find_layer_by_id_mut(layer_id.as_str())
                        && let Some(gp) = layer.gen_params_mut()
                    {
                        let gen_type = gp.generator_type().clone();
                        let Some(idx) = crate::generator_definition_registry::param_id_to_index(
                            &gen_type,
                            param_id.as_ref(),
                        ) else {
                            continue;
                        };
                        gp.set_param_base(idx, mapped);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bank_has_8_slots() {
        let bank = MacroBank::default();
        assert_eq!(bank.slots.len(), MACRO_COUNT);
    }

    #[test]
    fn curve_linear() {
        assert!((MacroCurve::Linear.apply(0.5) - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn curve_exponential() {
        assert!((MacroCurve::Exponential.apply(0.5) - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn curve_logarithmic() {
        let val = MacroCurve::Logarithmic.apply(0.25);
        assert!((val - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn curve_scurve_endpoints() {
        assert!((MacroCurve::SCurve.apply(0.0)).abs() < f32::EPSILON);
        assert!((MacroCurve::SCurve.apply(1.0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn curve_clamps_input() {
        assert!((MacroCurve::Linear.apply(-0.5)).abs() < f32::EPSILON);
        assert!((MacroCurve::Linear.apply(1.5) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn serde_roundtrip() {
        let bank = MacroBank::default();
        let json = serde_json::to_string(&bank).unwrap();
        let back: MacroBank = serde_json::from_str(&json).unwrap();
        assert_eq!(back.slots.len(), MACRO_COUNT);
    }

    #[test]
    fn normalize_handles_short_vec() {
        let mut bank = MacroBank {
            slots: vec![MacroSlot::default(); 3],
        };
        bank.normalize();
        assert_eq!(bank.slots.len(), MACRO_COUNT);
    }

    // ── Backward-compat Deserialize (step 11) ───────────────────

    // Field names within target variants are snake_case on the wire
    // (matches existing V1.1 project files). The variant tag is the
    // only camelCase identifier — see fixture `Liveschool Live Show V6
    // LEDS.manifold` for an authoritative shape sample.

    #[test]
    fn deserialize_legacy_master_effect_param_index() {
        let json = r#"{
            "target": {
                "type": "masterEffect",
                "effect_type": "Bloom",
                "param_index": 2
            },
            "rangeMin": 0.0,
            "rangeMax": 1.0
        }"#;
        let m: MacroMapping = serde_json::from_str(json).unwrap();
        match &m.target {
            MacroMappingTarget::MasterEffect { param_id, .. } => {
                assert!(param_id.is_empty());
            }
            _ => panic!("wrong variant"),
        }
        assert_eq!(m.legacy_param_index, Some(2));
    }

    #[test]
    fn deserialize_canonical_layer_effect_param_id() {
        let json = r#"{
            "target": {
                "type": "layerEffect",
                "layer_id": "layer-1",
                "effect_type": "Mirror",
                "param_id": "amount"
            }
        }"#;
        let m: MacroMapping = serde_json::from_str(json).unwrap();
        match &m.target {
            MacroMappingTarget::LayerEffect { param_id, .. } => {
                assert_eq!(param_id, "amount");
            }
            _ => panic!("wrong variant"),
        }
        assert_eq!(m.legacy_param_index, None);
    }

    #[test]
    fn deserialize_legacy_gen_param() {
        let json = r#"{
            "target": {
                "type": "genParam",
                "layer_id": "layer-7",
                "param_index": 4
            }
        }"#;
        let m: MacroMapping = serde_json::from_str(json).unwrap();
        match &m.target {
            MacroMappingTarget::GenParam { layer_id, param_id } => {
                assert_eq!(layer_id.as_str(), "layer-7");
                assert!(param_id.is_empty());
            }
            _ => panic!("wrong variant"),
        }
        assert_eq!(m.legacy_param_index, Some(4));
    }

    #[test]
    fn deserialize_param_id_wins_over_param_index() {
        let json = r#"{
            "target": {
                "type": "masterEffect",
                "effect_type": "Bloom",
                "param_id": "threshold",
                "param_index": 99
            }
        }"#;
        let m: MacroMapping = serde_json::from_str(json).unwrap();
        match &m.target {
            MacroMappingTarget::MasterEffect { param_id, .. } => {
                assert_eq!(param_id, "threshold");
            }
            _ => panic!("wrong variant"),
        }
        assert_eq!(m.legacy_param_index, None);
    }

    #[test]
    fn deserialize_master_opacity_passes_through() {
        let json = r#"{
            "target": { "type": "masterOpacity" }
        }"#;
        let m: MacroMapping = serde_json::from_str(json).unwrap();
        assert!(matches!(m.target, MacroMappingTarget::MasterOpacity));
        assert_eq!(m.legacy_param_index, None);
    }

    #[test]
    fn deserialize_layer_opacity_passes_through() {
        let json = r#"{
            "target": { "type": "layerOpacity", "layer_id": "layer-3" }
        }"#;
        let m: MacroMapping = serde_json::from_str(json).unwrap();
        match &m.target {
            MacroMappingTarget::LayerOpacity { layer_id } => {
                assert_eq!(layer_id.as_str(), "layer-3");
            }
            _ => panic!("wrong variant"),
        }
        assert_eq!(m.legacy_param_index, None);
    }

    #[test]
    fn serialize_emits_param_id_not_param_index() {
        let mapping = MacroMapping {
            target: MacroMappingTarget::MasterEffect {
                effect_type: EffectTypeId::from_string("Bloom".to_string()),
                param_id: Cow::Borrowed("amount"),
            },
            range_min: 0.0,
            range_max: 1.0,
            curve: MacroCurve::Linear,
            legacy_param_index: None,
        };
        let json = serde_json::to_string(&mapping).unwrap();
        assert!(
            json.contains("\"param_id\":\"amount\""),
            "Serialize must emit param_id; got: {json}"
        );
        assert!(
            !json.contains("\"param_index\""),
            "Serialize must not write legacy param_index; got: {json}"
        );
        assert!(
            !json.contains("\"legacy_param_index\"") && !json.contains("\"legacyParamIndex\""),
            "Serialize must not write internal legacy_param_index; got: {json}"
        );
    }
}
