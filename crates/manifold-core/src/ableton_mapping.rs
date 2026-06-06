//! Ableton Live OSC bridge data types.
//!
//! Stores the mapping between Ableton rack macro parameters and MANIFOLD
//! effect/generator parameters. Mappings are serialized in the project file
//! and validated at runtime via structural identity (device class names).

use crate::preset_type_id::PresetTypeId;
use crate::effects::ParamId;
use crate::id::LayerId;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

// ── Structural identity ───────────────────────────────────────────

/// Structural identity of an Ableton device.
/// Uses `device_class_name` which is Ableton-internal and immutable
/// (e.g. "InstrumentGroupDevice", "DrumGroupDevice").
/// Display names are never used for resolution — only for UI labels.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbletonDeviceIdentity {
    pub device_class_name: String,
}

// ── Macro address ─────────────────────────────────────────────────

/// Full address of a single Ableton rack macro parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbletonMacroAddress {
    // Runtime indices (mutable — updated on structural re-resolution)
    pub track_id: i32,
    pub device_id: i32,
    pub param_id: i32,
    // Structural identity (for validation, not name-based resolution)
    pub device_identity: AbletonDeviceIdentity,
    // Display names (UI only, refreshed on discovery)
    pub track_name: String,
    pub device_name: String,
    pub macro_name: String,
}

// ── Parameter mapping ─────────────────────────────────────────────

/// Runtime status of a mapping (not serialized).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AbletonMappingStatus {
    /// Disconnected or target not yet validated.
    #[default]
    Dormant,
    /// Connected and values flowing.
    Active,
    /// Connected but structural resolution is ambiguous — needs user action.
    Ambiguous,
}

/// Mapping from an Ableton rack macro to a MANIFOLD parameter.
///
/// Stored alongside `drivers` on `PresetInstance` and `GeneratorParamState`.
/// Replace mode: when active, the Ableton value overrides `base_param_values`.
///
/// Address shape: [`AbletonParamMapping::param_id`] is the canonical
/// MANIFOLD-side mapping key, mirroring [`crate::effects::ParameterDriver`].
/// Legacy V1.1 projects stored `paramIndex: usize` instead — the custom
/// [`Deserialize`] accepts either shape and parks legacy indices in
/// `legacy_param_index` for the post-load resolver.
///
/// Note: [`AbletonMacroAddress::param_id`] is a different concept — it's
/// the Ableton-side rack-macro parameter identifier (numeric, comes
/// from Ableton via OSC). The two `param_id` fields are nested at
/// different levels so the JSON shape is unambiguous.
///
/// Serialization (custom impl below): emits `paramId` when non-empty,
/// else `paramIndex` when `legacy_param_index` is `Some`. Mirrors the
/// recovery contract on [`crate::effects::ParameterDriver`].
#[derive(Debug, Clone)]
pub struct AbletonParamMapping {
    /// Stable MANIFOLD-side mapping key. Empty after legacy V1.1
    /// deserialization until the post-load resolver fills it in.
    pub param_id: ParamId,
    pub address: AbletonMacroAddress,
    /// Normalized 0–1 trim low (maps Ableton 0 to this point in param range).
    pub range_min: f32,
    /// Normalized 0–1 trim high (maps Ableton 1 to this point in param range).
    pub range_max: f32,
    /// When true, the Ableton value is inverted (1.0 - v) before trim range mapping.
    pub inverted: bool,
    /// Parked legacy `paramIndex: i32` from V1.1 deserialization or
    /// RegistryMissing fallback. See [`crate::effects::ParameterDriver::legacy_param_index`]
    /// for the recovery invariant — same contract here.
    pub legacy_param_index: Option<i32>,
    /// Last received value from Ableton (0–1, pre-range-mapping). Runtime only.
    pub last_value: f32,
    /// Runtime status (active/dormant/ambiguous). Not persisted.
    pub status: AbletonMappingStatus,
}

impl Serialize for AbletonParamMapping {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let emit_param_id = !self.param_id.is_empty();
        let emit_legacy_index = !emit_param_id && self.legacy_param_index.is_some();

        // 4 base fields (address, rangeMin, rangeMax, inverted) +
        // optional addressing field.
        let mut field_count = 4;
        if emit_param_id || emit_legacy_index {
            field_count += 1;
        }

        let mut s = serializer.serialize_struct("AbletonParamMapping", field_count)?;
        if emit_param_id {
            s.serialize_field("paramId", &self.param_id)?;
        } else if emit_legacy_index {
            s.serialize_field("paramIndex", &self.legacy_param_index.unwrap())?;
        }
        s.serialize_field("address", &self.address)?;
        s.serialize_field("rangeMin", &self.range_min)?;
        s.serialize_field("rangeMax", &self.range_max)?;
        s.serialize_field("inverted", &self.inverted)?;
        s.end()
    }
}

// Custom `Deserialize` accepting both V1.1 (`paramIndex: usize`) and
// V1.2+ (`paramId: "amount"`) shapes. See
// `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 step 10.
impl<'de> Deserialize<'de> for AbletonParamMapping {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(default)]
            param_id: Option<String>,
            #[serde(default)]
            param_index: Option<i32>,
            address: AbletonMacroAddress,
            #[serde(default)]
            range_min: f32,
            #[serde(default = "default_one")]
            range_max: f32,
            #[serde(default)]
            inverted: bool,
        }

        let raw = Raw::deserialize(deserializer)?;
        let (param_id, legacy_param_index) = match (raw.param_id, raw.param_index) {
            (Some(id), _) if !id.is_empty() => (Cow::Owned(id), None),
            (_, Some(idx)) => (Cow::Borrowed(""), Some(idx)),
            (_, None) => (Cow::Borrowed(""), None),
        };
        Ok(AbletonParamMapping {
            param_id,
            address: raw.address,
            range_min: raw.range_min,
            range_max: raw.range_max,
            inverted: raw.inverted,
            legacy_param_index,
            last_value: 0.0,
            status: AbletonMappingStatus::default(),
        })
    }
}

fn default_one() -> f32 {
    1.0
}

impl AbletonParamMapping {
    /// Map an incoming Ableton value (0–1) through the trim range to produce
    /// a normalized 0–1 output within `[range_min, range_max]`.
    pub fn map_value(&self, ableton_value: f32) -> f32 {
        let mut v = ableton_value.clamp(0.0, 1.0);
        if self.inverted {
            v = 1.0 - v;
        }
        self.range_min + (self.range_max - self.range_min) * v
    }

    /// Map an incoming Ableton value to the parameter's native range.
    pub fn map_to_param_range(&self, ableton_value: f32, param_min: f32, param_max: f32) -> f32 {
        let normalized = self.map_value(ableton_value);
        param_min + (param_max - param_min) * normalized
    }
}

// ── Default-name detection ────────────────────────────────────────

/// `true` if `name` is one of Ableton's default rack-macro labels
/// (`"Macro 1"`..=`"Macro 8"`). Used to forbid mapping against
/// unrenamed macros — see `crates/manifold-ui/src/panels/ableton_picker.rs`
/// for the reasoning. The check is intentionally strict: only the literal
/// patterns count as defaults; a user-typed `"Macro 1 (Filter)"` is fine.
pub fn is_default_macro_name(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("Macro ")
        && let Ok(n) = rest.parse::<u32>()
    {
        return (1..=8).contains(&n);
    }
    false
}

// ── Mapping target ────────────────────────────────────────────────

/// Which MANIFOLD parameter an Ableton mapping targets.
/// Mirrors `MacroMappingTarget` but scoped to Ableton bridge.
///
/// All variants address parameters by stable [`ParamId`] (since step 10).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum AbletonMappingTarget {
    MasterEffect {
        effect_type: PresetTypeId,
        param_id: ParamId,
    },
    LayerEffect {
        layer_id: LayerId,
        effect_type: PresetTypeId,
        param_id: ParamId,
    },
    GenParam {
        layer_id: LayerId,
        param_id: ParamId,
    },
    MacroSlot {
        slot_index: usize,
    },
}

impl AbletonMappingTarget {
    /// The stable [`ParamId`] this target addresses, for the three
    /// host-param variants. `None` for `MacroSlot`, which is addressed by
    /// slot index, not a param id, and stores a single mapping rather than
    /// a per-param vec.
    pub fn param_id(&self) -> Option<&ParamId> {
        match self {
            Self::MasterEffect { param_id, .. }
            | Self::LayerEffect { param_id, .. }
            | Self::GenParam { param_id, .. } => Some(param_id),
            Self::MacroSlot { .. } => None,
        }
    }
}

// ── Set context ───────────────────────────────────────────────────

/// Per-track structural signature: the ordered list of device class names.
/// Used for diffing on reconnect to detect structural changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbletonTrackSignature {
    pub device_classes: Vec<String>,
}

/// Identifies which Ableton set the project's mappings were created against.
/// Stored in `ProjectSettings` alongside the macro bank.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbletonSetContext {
    pub track_signatures: Vec<AbletonTrackSignature>,
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_value_full_range() {
        let mapping = AbletonParamMapping {
            param_id: Cow::Borrowed("amount"),
            address: test_address(),
            range_min: 0.0,
            range_max: 1.0,
            inverted: false,
            legacy_param_index: None,
            last_value: 0.0,
            status: AbletonMappingStatus::Active,
        };
        assert!((mapping.map_value(0.5) - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn map_value_sub_range() {
        let mapping = AbletonParamMapping {
            param_id: Cow::Borrowed("amount"),
            address: test_address(),
            range_min: 0.25,
            range_max: 0.75,
            inverted: false,
            legacy_param_index: None,
            last_value: 0.0,
            status: AbletonMappingStatus::Active,
        };
        // 0.0 → 0.25, 0.5 → 0.50, 1.0 → 0.75
        assert!((mapping.map_value(0.0) - 0.25).abs() < f32::EPSILON);
        assert!((mapping.map_value(0.5) - 0.50).abs() < f32::EPSILON);
        assert!((mapping.map_value(1.0) - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn map_to_param_range_scales() {
        let mapping = AbletonParamMapping {
            param_id: Cow::Borrowed("amount"),
            address: test_address(),
            range_min: 0.0,
            range_max: 1.0,
            inverted: false,
            legacy_param_index: None,
            last_value: 0.0,
            status: AbletonMappingStatus::Active,
        };
        // Param range 20–20000 (e.g. filter cutoff)
        let val = mapping.map_to_param_range(0.5, 20.0, 20000.0);
        assert!((val - 10010.0).abs() < 0.01);
    }

    #[test]
    fn map_value_inverted() {
        let mapping = AbletonParamMapping {
            param_id: Cow::Borrowed("amount"),
            address: test_address(),
            range_min: 0.25,
            range_max: 0.75,
            inverted: true,
            legacy_param_index: None,
            last_value: 0.0,
            status: AbletonMappingStatus::Active,
        };
        // Inverted: 0.0 → 1.0 → 0.75, 1.0 → 0.0 → 0.25
        assert!((mapping.map_value(0.0) - 0.75).abs() < f32::EPSILON);
        assert!((mapping.map_value(1.0) - 0.25).abs() < f32::EPSILON);
        assert!((mapping.map_value(0.5) - 0.50).abs() < f32::EPSILON);
    }

    #[test]
    fn map_value_clamps_input() {
        let mapping = AbletonParamMapping {
            param_id: Cow::Borrowed("amount"),
            address: test_address(),
            range_min: 0.0,
            range_max: 1.0,
            inverted: false,
            legacy_param_index: None,
            last_value: 0.0,
            status: AbletonMappingStatus::Active,
        };
        assert!(mapping.map_value(-0.5).abs() < f32::EPSILON);
        assert!((mapping.map_value(1.5) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn serde_roundtrip_mapping() {
        let mapping = AbletonParamMapping {
            param_id: Cow::Borrowed("threshold"),
            address: test_address(),
            range_min: 0.1,
            range_max: 0.9,
            inverted: true,
            legacy_param_index: None,
            last_value: 0.5,
            status: AbletonMappingStatus::Active,
        };
        let json = serde_json::to_string(&mapping).unwrap();
        let back: AbletonParamMapping = serde_json::from_str(&json).unwrap();
        assert_eq!(back.param_id, "threshold");
        assert!((back.range_min - 0.1).abs() < f32::EPSILON);
        assert!((back.range_max - 0.9).abs() < f32::EPSILON);
        assert!(back.inverted);
        // Runtime fields should be default after deser
        assert!(back.last_value.abs() < f32::EPSILON);
        assert_eq!(back.status, AbletonMappingStatus::Dormant);
        assert_eq!(back.legacy_param_index, None);
    }

    // ── Backward-compat Deserialize (step 10) ───────────────────

    #[test]
    fn deserialize_legacy_param_index() {
        let json = r#"{
            "paramIndex": 2,
            "address": {
                "trackId": 0,
                "deviceId": 0,
                "paramId": 8,
                "deviceIdentity": {"deviceClassName": "InstrumentGroupDevice"},
                "trackName": "Bass",
                "deviceName": "Rack",
                "macroName": "Filter"
            },
            "rangeMin": 0.0,
            "rangeMax": 1.0,
            "inverted": false
        }"#;
        let m: AbletonParamMapping = serde_json::from_str(json).unwrap();
        assert!(m.param_id.is_empty());
        assert_eq!(m.legacy_param_index, Some(2));
    }

    #[test]
    fn deserialize_canonical_param_id() {
        let json = r#"{
            "paramId": "amount",
            "address": {
                "trackId": 0,
                "deviceId": 0,
                "paramId": 8,
                "deviceIdentity": {"deviceClassName": "InstrumentGroupDevice"},
                "trackName": "Bass",
                "deviceName": "Rack",
                "macroName": "Filter"
            }
        }"#;
        let m: AbletonParamMapping = serde_json::from_str(json).unwrap();
        assert_eq!(m.param_id, "amount");
        assert_eq!(m.legacy_param_index, None);
    }

    #[test]
    fn deserialize_param_id_wins_when_both_present() {
        // The Ableton-side `address.paramId` (numeric) and the new
        // top-level `paramId` (string) live at different nesting
        // levels and don't collide.
        let json = r#"{
            "paramId": "threshold",
            "paramIndex": 99,
            "address": {
                "trackId": 0,
                "deviceId": 0,
                "paramId": 8,
                "deviceIdentity": {"deviceClassName": "InstrumentGroupDevice"},
                "trackName": "Bass",
                "deviceName": "Rack",
                "macroName": "Filter"
            }
        }"#;
        let m: AbletonParamMapping = serde_json::from_str(json).unwrap();
        assert_eq!(m.param_id, "threshold");
        assert_eq!(m.legacy_param_index, None);
        // Ableton-side address.paramId stays a separate concept.
        assert_eq!(m.address.param_id, 8);
    }

    #[test]
    fn serialize_writes_param_id_only() {
        let mapping = AbletonParamMapping {
            param_id: Cow::Borrowed("amount"),
            address: test_address(),
            range_min: 0.0,
            range_max: 1.0,
            inverted: false,
            legacy_param_index: None,
            last_value: 0.0,
            status: AbletonMappingStatus::Dormant,
        };
        let json = serde_json::to_string(&mapping).unwrap();
        assert!(json.contains("\"paramId\":\"amount\""));
        assert!(
            !json.contains("\"paramIndex\""),
            "Serialize must not write legacy paramIndex; got: {json}"
        );
    }

    #[test]
    fn serde_roundtrip_set_context() {
        let ctx = AbletonSetContext {
            track_signatures: vec![
                AbletonTrackSignature {
                    device_classes: vec![
                        "InstrumentGroupDevice".to_string(),
                        "AudioEffectGroupDevice".to_string(),
                    ],
                },
                AbletonTrackSignature {
                    device_classes: vec!["DrumGroupDevice".to_string()],
                },
            ],
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let back: AbletonSetContext = serde_json::from_str(&json).unwrap();
        assert_eq!(back.track_signatures.len(), 2);
        assert_eq!(back.track_signatures[0].device_classes.len(), 2);
    }

    #[test]
    fn device_identity_equality() {
        let a = AbletonDeviceIdentity {
            device_class_name: "InstrumentGroupDevice".to_string(),
        };
        let b = AbletonDeviceIdentity {
            device_class_name: "InstrumentGroupDevice".to_string(),
        };
        let c = AbletonDeviceIdentity {
            device_class_name: "DrumGroupDevice".to_string(),
        };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    fn test_address() -> AbletonMacroAddress {
        AbletonMacroAddress {
            track_id: 0,
            device_id: 0,
            param_id: 0,
            device_identity: AbletonDeviceIdentity {
                device_class_name: "InstrumentGroupDevice".to_string(),
            },
            track_name: "Bass".to_string(),
            device_name: "Instrument Rack".to_string(),
            macro_name: "Filter".to_string(),
        }
    }
}
