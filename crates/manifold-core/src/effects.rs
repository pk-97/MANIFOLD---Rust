use serde::{Deserialize, Serialize};
use crate::types::{BeatDivision, DriverWaveform, EffectType};

// ─── Param Definition ───

/// Metadata for a single parameter slot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamDef {
    pub name: String,
    pub min: f32,
    pub max: f32,
    #[serde(default)]
    pub default_value: f32,
    #[serde(default)]
    pub whole_numbers: bool,
    #[serde(default)]
    pub is_toggle: bool,
    #[serde(default)]
    pub value_labels: Option<Vec<String>>,
    #[serde(default)]
    pub format_string: Option<String>,
    #[serde(default)]
    pub osc_suffix: Option<String>,
}

// ─── Effect Instance ───

/// A single effect applied to a clip, layer, or master chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectInstance {
    pub effect_type: EffectType,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default)]
    pub param_values: Vec<f32>,
    #[serde(default)]
    pub base_param_values: Option<Vec<f32>>,
    #[serde(default)]
    pub drivers: Option<Vec<ParameterDriver>>,
    #[serde(default)]
    pub group_id: Option<String>,

    // Legacy flat param fields (V1.0.0 format)
    #[serde(default, rename = "param0")]
    pub legacy_param0: Option<f32>,
    #[serde(default, rename = "param1")]
    pub legacy_param1: Option<f32>,
    #[serde(default, rename = "param2")]
    pub legacy_param2: Option<f32>,
    #[serde(default, rename = "param3")]
    pub legacy_param3: Option<f32>,
}

impl EffectInstance {
    pub fn clone_deep(&self) -> Self {
        self.clone()
    }

    /// Reset effective param values from base values.
    pub fn reset_param_effectives(&mut self) {
        if let Some(base) = &self.base_param_values {
            for (i, &val) in base.iter().enumerate() {
                if i < self.param_values.len() {
                    self.param_values[i] = val;
                }
            }
        }
    }

    /// Ensure base_param_values exists (cloned from param_values on first access).
    pub fn ensure_base_values(&mut self) {
        if self.base_param_values.is_none() {
            self.base_param_values = Some(self.param_values.clone());
        }
    }

    /// Set a base param value at index, ensuring capacity.
    pub fn set_base_param(&mut self, index: usize, value: f32) {
        self.ensure_base_values();
        if let Some(base) = &mut self.base_param_values {
            while base.len() <= index {
                base.push(0.0);
            }
            base[index] = value;
        }
        while self.param_values.len() <= index {
            self.param_values.push(0.0);
        }
        self.param_values[index] = value;
    }

    /// Get the drivers list, creating it if None.
    pub fn drivers_mut(&mut self) -> &mut Vec<ParameterDriver> {
        if self.drivers.is_none() {
            self.drivers = Some(Vec::new());
        }
        self.drivers.as_mut().unwrap()
    }
}

// ─── Effect Group ───

/// A rack group containing multiple effects with shared bypass and wet/dry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectGroup {
    pub id: String,
    #[serde(default = "default_group_name")]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default = "default_one")]
    pub wet_dry: f32,
    #[serde(default)]
    pub parent_group_id: Option<String>,
}

impl EffectGroup {
    pub fn new(name: String) -> Self {
        Self {
            id: crate::short_id(),
            name,
            enabled: true,
            collapsed: false,
            wet_dry: 1.0,
            parent_group_id: None,
        }
    }

    pub fn clone_with_new_id(&self) -> Self {
        let mut cloned = self.clone();
        cloned.id = crate::short_id();
        cloned
    }
}

// ─── Parameter Driver (LFO) ───

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterDriver {
    pub param_index: i32,
    #[serde(default)]
    pub beat_division: BeatDivision,
    #[serde(default)]
    pub waveform: DriverWaveform,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub phase: f32,
    #[serde(default)]
    pub base_value: f32,
    #[serde(default)]
    pub trim_min: f32,
    #[serde(default = "default_one")]
    pub trim_max: f32,
    #[serde(default)]
    pub reversed: bool,
}

impl ParameterDriver {
    /// Evaluate driver at given beat position → [0, 1].
    pub fn evaluate(current_beat: f32, division: BeatDivision, waveform: DriverWaveform, phase_offset: f32) -> f32 {
        let period = division.beats();
        if period <= 0.0 {
            return 0.5;
        }
        let phase = ((current_beat / period) + phase_offset).fract();
        let phase = if phase < 0.0 { phase + 1.0 } else { phase };

        match waveform {
            DriverWaveform::Sine => (phase * std::f32::consts::TAU).sin() * 0.5 + 0.5,
            DriverWaveform::Triangle => {
                if phase < 0.5 { phase * 2.0 } else { 2.0 - phase * 2.0 }
            }
            DriverWaveform::Sawtooth => phase,
            DriverWaveform::Square => if phase < 0.5 { 1.0 } else { 0.0 },
            DriverWaveform::Random => {
                // Deterministic per-period hash
                let seed = (current_beat / period).floor() as u32;
                let hash = seed.wrapping_mul(2654435761);
                (hash as f32) / (u32::MAX as f32)
            }
        }
    }
}

// ─── Param Envelope (ADSR modulation) ───

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamEnvelope {
    #[serde(default)]
    pub target_effect_type: EffectType,
    /// Unity V2 serializes this as "targetParamIndex" via [JsonProperty].
    #[serde(default, alias = "targetParamIndex")]
    pub param_index: i32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub attack_beats: f32,
    #[serde(default)]
    pub decay_beats: f32,
    #[serde(default)]
    pub sustain_level: f32,
    #[serde(default)]
    pub release_beats: f32,
    #[serde(default = "default_one")]
    pub target_normalized: f32,
}

// ─── Default helpers ───

fn default_true() -> bool { true }
fn default_one() -> f32 { 1.0 }
fn default_quarter() -> f32 { 0.25 }
fn default_group_name() -> String { "Group".to_string() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_driver_sine() {
        let val = ParameterDriver::evaluate(0.0, BeatDivision::Quarter, DriverWaveform::Sine, 0.0);
        assert!((val - 0.5).abs() < 0.01);

        let val = ParameterDriver::evaluate(0.25, BeatDivision::Quarter, DriverWaveform::Sine, 0.0);
        assert!((val - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_driver_square() {
        let val = ParameterDriver::evaluate(0.1, BeatDivision::Quarter, DriverWaveform::Square, 0.0);
        assert_eq!(val, 1.0);

        let val = ParameterDriver::evaluate(0.6, BeatDivision::Quarter, DriverWaveform::Square, 0.0);
        assert_eq!(val, 0.0);
    }
}
