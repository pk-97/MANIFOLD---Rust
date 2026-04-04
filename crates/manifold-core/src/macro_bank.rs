//! Macro bank: 8 macro sliders that fan out to multiple project parameters.
//!
//! Each macro receives a single 0–1 value (via OSC `/macro/1`–`/macro/8` or UI)
//! and distributes it to N target parameters through configurable mappings with
//! optional response curves. Targets are identified by the same addressing scheme
//! as `OscParamTarget` so the fan-out reuses the existing parameter write path.

use crate::effect_type_id::EffectTypeId;
use crate::id::LayerId;
use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum MacroMappingTarget {
    MasterOpacity,
    MasterEffect {
        effect_type: EffectTypeId,
        param_index: usize,
    },
    LayerOpacity {
        layer_id: LayerId,
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
}

// ── Mapping ────────────────────────────────────────────────────────

/// A single mapping from a macro slot to a project parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroMapping {
    pub target: MacroMappingTarget,
    #[serde(default)]
    pub range_min: f32,
    #[serde(default = "default_one")]
    pub range_max: f32,
    #[serde(default)]
    pub curve: MacroCurve,
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
                    param_index,
                } => {
                    if let Some(fx) = project
                        .settings
                        .master_effects
                        .iter_mut()
                        .find(|f| f.effect_type() == effect_type)
                    {
                        fx.set_base_param(*param_index, mapped);
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
                    param_index,
                } => {
                    if let Some((_, layer)) =
                        project.timeline.find_layer_by_id_mut(layer_id.as_str())
                        && let Some(effects) = &mut layer.effects
                        && let Some(fx) =
                            effects.iter_mut().find(|f| f.effect_type() == effect_type)
                    {
                        fx.set_base_param(*param_index, mapped);
                    }
                }
                MacroMappingTarget::GenParam {
                    layer_id,
                    param_index,
                } => {
                    if let Some((_, layer)) =
                        project.timeline.find_layer_by_id_mut(layer_id.as_str())
                        && let Some(gp) = layer.gen_params_mut()
                    {
                        gp.set_param_base(*param_index, mapped);
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
}
