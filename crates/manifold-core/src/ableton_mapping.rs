//! Ableton Live OSC bridge data types.
//!
//! Stores the mapping between Ableton rack macro parameters and MANIFOLD
//! effect/generator parameters. Mappings are serialized in the project file
//! and validated at runtime via structural identity (device class names).

use crate::effect_type_id::EffectTypeId;
use crate::id::LayerId;
use serde::{Deserialize, Serialize};

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
/// Stored alongside `drivers` on `EffectInstance` and `GeneratorParamState`.
/// Replace mode: when active, the Ableton value overrides `base_param_values`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbletonParamMapping {
    pub param_index: usize,
    pub address: AbletonMacroAddress,
    /// Normalized 0–1 trim low (maps Ableton 0 to this point in param range).
    #[serde(default)]
    pub range_min: f32,
    /// Normalized 0–1 trim high (maps Ableton 1 to this point in param range).
    #[serde(default = "default_one")]
    pub range_max: f32,
    /// When true, the Ableton value is inverted (1.0 - v) before trim range mapping.
    #[serde(default)]
    pub inverted: bool,
    /// Last received value from Ableton (0–1, pre-range-mapping). Runtime only.
    #[serde(skip)]
    pub last_value: f32,
    /// Runtime status (active/dormant/ambiguous). Not persisted.
    #[serde(skip)]
    pub status: AbletonMappingStatus,
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

// ── Mapping target ────────────────────────────────────────────────

/// Which MANIFOLD parameter an Ableton mapping targets.
/// Mirrors `MacroMappingTarget` but scoped to Ableton bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum AbletonMappingTarget {
    MasterEffect {
        effect_type: EffectTypeId,
        param_index: usize,
    },
    LayerEffect {
        layer_id: LayerId,
        effect_type: EffectTypeId,
        param_index: usize,
    },
    GenParam {
        layer_id: LayerId,
        param_index: usize,
    },
    MacroSlot {
        slot_index: usize,
    },
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
    pub set_file_path: String,
    pub track_signatures: Vec<AbletonTrackSignature>,
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_value_full_range() {
        let mapping = AbletonParamMapping {
            param_index: 0,
            address: test_address(),
            range_min: 0.0,
            range_max: 1.0,
            inverted: false,
            last_value: 0.0,
            status: AbletonMappingStatus::Active,
        };
        assert!((mapping.map_value(0.5) - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn map_value_sub_range() {
        let mapping = AbletonParamMapping {
            param_index: 0,
            address: test_address(),
            range_min: 0.25,
            range_max: 0.75,
            inverted: false,
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
            param_index: 0,
            address: test_address(),
            range_min: 0.0,
            range_max: 1.0,
            inverted: false,
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
            param_index: 0,
            address: test_address(),
            range_min: 0.25,
            range_max: 0.75,
            inverted: true,
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
            param_index: 0,
            address: test_address(),
            range_min: 0.0,
            range_max: 1.0,
            inverted: false,
            last_value: 0.0,
            status: AbletonMappingStatus::Active,
        };
        assert!(mapping.map_value(-0.5).abs() < f32::EPSILON);
        assert!((mapping.map_value(1.5) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn serde_roundtrip_mapping() {
        let mapping = AbletonParamMapping {
            param_index: 2,
            address: test_address(),
            range_min: 0.1,
            range_max: 0.9,
            inverted: true,
            last_value: 0.5,
            status: AbletonMappingStatus::Active,
        };
        let json = serde_json::to_string(&mapping).unwrap();
        let back: AbletonParamMapping = serde_json::from_str(&json).unwrap();
        assert_eq!(back.param_index, 2);
        assert!((back.range_min - 0.1).abs() < f32::EPSILON);
        assert!((back.range_max - 0.9).abs() < f32::EPSILON);
        assert!(back.inverted);
        // Runtime fields should be default after deser
        assert!(back.last_value.abs() < f32::EPSILON);
        assert_eq!(back.status, AbletonMappingStatus::Dormant);
    }

    #[test]
    fn serde_roundtrip_set_context() {
        let ctx = AbletonSetContext {
            set_file_path: "/path/to/set.als".to_string(),
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
