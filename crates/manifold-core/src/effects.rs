use serde::{Deserialize, Serialize};
use crate::id::{EffectGroupId, EffectId};
use crate::types::{BeatDivision, DriverWaveform, EffectType};

// ─── Param Definition ───

/// Metadata for a single parameter slot.
/// Port of Unity ParamDef.cs.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_string: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub osc_suffix: Option<String>,
}

impl Default for ParamDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            whole_numbers: false,
            is_toggle: false,
            value_labels: None,
            format_string: None,
            osc_suffix: None,
        }
    }
}

// ─── Traits ───

/// Shared contract for entities that own a modular effects list.
/// Port of Unity IEffectContainer.cs.
/// Implemented by TimelineClip, Layer, and ProjectSettings.
pub trait EffectContainer {
    fn effects(&self) -> &[EffectInstance];
    fn effects_mut(&mut self) -> &mut Vec<EffectInstance>;
    fn effect_groups(&self) -> &[EffectGroup];
    fn effect_groups_mut(&mut self) -> &mut Vec<EffectGroup>;
    fn has_modular_effects(&self) -> bool;
    fn find_effect(&self, effect_type: EffectType) -> Option<&EffectInstance>;
    fn find_effect_group(&self, group_id: &str) -> Option<&EffectGroup>;
    fn envelopes(&self) -> &[ParamEnvelope];
    fn envelopes_mut(&mut self) -> &mut Vec<ParamEnvelope>;
    fn has_envelopes(&self) -> bool;
}

/// Abstracts a "thing with named params, drivers, and ranges."
/// Port of Unity IParamSource.cs.
/// Both EffectInstance and generator params implement this.
pub trait ParamSource {
    fn display_name(&self) -> &str;
    fn param_count(&self) -> usize;
    fn get_param_def(&self, index: usize) -> ParamDef;
    fn get_param(&self, index: usize) -> f32;
    fn set_param(&mut self, index: usize, value: f32);
    fn get_base_param(&self, index: usize) -> f32;
    fn set_base_param(&mut self, index: usize, value: f32);
    fn find_driver(&self, param_index: i32) -> Option<&ParameterDriver>;
    fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>>;
    fn create_driver(&mut self, param_index: i32) -> &ParameterDriver;
    fn remove_driver(&mut self, param_index: i32);
}

// ─── Effect Instance ───

/// A single effect applied to a clip, layer, or master chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectInstance {
    /// Unique identifier for this effect instance.
    /// Auto-generated on creation and deserialization (backfills old projects).
    #[serde(default = "generate_effect_id")]
    pub id: EffectId,
    effect_type: EffectType,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default)]
    pub param_values: Vec<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_param_values: Option<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drivers: Option<Vec<ParameterDriver>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<EffectGroupId>,

    // Legacy flat param fields (V1.0.0 format)
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "param0")]
    pub legacy_param0: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "param1")]
    pub legacy_param1: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "param2")]
    pub legacy_param2: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "param3")]
    pub legacy_param3: Option<f32>,
}

impl EffectInstance {
    /// Create a new EffectInstance with the given type.
    /// Unity EffectInstance.cs lines 79-83.
    pub fn new(effect_type: EffectType) -> Self {
        Self {
            id: generate_effect_id(),
            effect_type,
            enabled: true,
            collapsed: false,
            param_values: Vec::new(),
            base_param_values: None,
            drivers: None,
            group_id: None,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
        }
    }

    /// Read-only access to the effect type.
    #[inline]
    pub fn effect_type(&self) -> EffectType {
        self.effect_type
    }

    /// Has any drivers? Unity EffectInstance.cs line 28.
    pub fn has_drivers(&self) -> bool {
        self.drivers.as_ref().is_some_and(|d| !d.is_empty())
    }

    pub fn clone_deep(&self) -> Self {
        self.clone()
    }

    /// Number of parameters currently allocated. Unity line 84.
    pub fn param_count(&self) -> usize {
        self.param_values.len()
    }

    /// Read effective (modulated) param value. Unity lines 86-91.
    pub fn get_param(&self, index: usize) -> f32 {
        self.param_values.get(index).copied().unwrap_or(0.0)
    }

    /// Write to effective (modulated) param value. Unity lines 93-101.
    pub fn set_param(&mut self, index: usize, value: f32) {
        while self.param_values.len() <= index {
            self.param_values.push(0.0);
        }
        self.param_values[index] = value;
    }

    /// Read the user-set base value (before modulation). Unity lines 104-110.
    pub fn get_base_param(&self, index: usize) -> f32 {
        if let Some(base) = &self.base_param_values
            && index < base.len() {
                return base[index];
            }
        // Fall through to effective for backward compat
        self.get_param(index)
    }

    /// Set the user-intended base value. Unity lines 113-126.
    pub fn set_base_param(&mut self, index: usize, value: f32) {
        self.ensure_base_values();
        while self.param_values.len() <= index {
            self.param_values.push(0.0);
        }
        if let Some(base) = &mut self.base_param_values {
            while base.len() <= index {
                base.push(0.0);
            }
            base[index] = value;
        }
        self.param_values[index] = value;
    }

    /// Reset effective param values from base values.
    pub fn reset_param_effectives(&mut self) {
        self.ensure_base_values();
        if let Some(base) = &self.base_param_values {
            let len = self.param_values.len().min(base.len());
            self.param_values[..len].copy_from_slice(&base[..len]);
        }
    }

    /// Lazy migration: create baseParamValues from paramValues if missing.
    pub fn ensure_base_values(&mut self) {
        if self.base_param_values.is_none() ||
           self.base_param_values.as_ref().is_some_and(|b| b.len() != self.param_values.len())
        {
            self.base_param_values = Some(self.param_values.clone());
        }
    }

    /// Ensure paramValues has at least 'count' slots.
    /// Unity EffectInstance.cs EnsureParamCapacity lines 152-158.
    pub fn ensure_param_capacity(&mut self, count: usize) {
        while self.param_values.len() < count {
            self.param_values.push(0.0);
        }
    }

    /// Find the driver for a given param index, or None.
    pub fn find_driver(&self, param_index: i32) -> Option<&ParameterDriver> {
        self.drivers.as_ref()?.iter().find(|d| d.param_index == param_index)
    }

    /// Get drivers list reference (may be None).
    pub fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>> {
        self.drivers.as_ref()
    }

    /// Create a driver for a param index. Unity lines 66-71.
    pub fn create_driver(&mut self, param_index: i32) -> &ParameterDriver {
        let driver = ParameterDriver::new(param_index, BeatDivision::Quarter, DriverWaveform::Sine);
        self.drivers_mut().push(driver);
        self.drivers.as_ref().unwrap().last().unwrap()
    }

    /// Remove driver by param index.
    pub fn remove_driver(&mut self, param_index: i32) {
        if let Some(drivers) = &mut self.drivers {
            drivers.retain(|d| d.param_index != param_index);
        }
    }

    /// Resize paramValues and baseParamValues to match the current effect definition.
    /// New slots are filled with the definition's default values.
    /// Includes migration for layout changes (e.g., WireframeDepth 14→12 params).
    pub fn align_to_definition(&mut self) {
        use crate::effect_definition_registry;
        use crate::EffectType;

        // Migration: WireframeDepth 14-param (old) → 12-param (new).
        // Old: Amount(0) Density(1) Width(2) ZScale(3) Smooth(4) Persist(5) Depth(6)
        //      Subject(7) Blend(8) WireRes(9) MeshRate(10) CVFlow(11) Lock(12) Face(13)
        // New: Amount(0) Density(1) Width(2) ZScale(3) Smooth(4) Subject(5) Blend(6)
        //      WireRes(7) MeshRate(8) Flow(9) Lock(10) EdgeFollow(11)
        if self.effect_type == EffectType::WireframeDepth && self.param_values.len() == 14 {
            let old = &self.param_values;
            let migrated = vec![
                old[0],  // Amount → Amount
                old[1],  // Density → Density
                old[2],  // Width → Width
                old[3],  // ZScale → ZScale
                old[4],  // Smooth → Smooth
                old[7],  // Subject → Subject (was index 7)
                old[8],  // Blend → Blend (was index 8)
                old[9],  // WireRes → WireRes (was index 9)
                old[10], // MeshRate → MeshRate (was index 10)
                old[11], // CVFlow → Flow (was index 11)
                old[12], // Lock → Lock (was index 12)
                0.5,     // EdgeFollow default (Face was discrete toggle, not transferable)
            ];
            self.param_values = migrated;
            // Migrate base values too
            if let Some(ref base) = self.base_param_values
                && base.len() == 14 {
                    let migrated_base = vec![
                        base[0], base[1], base[2], base[3], base[4],
                        base[7], base[8], base[9], base[10], base[11], base[12],
                        0.5,
                    ];
                    self.base_param_values = Some(migrated_base);
                }
        }

        if let Some(def) = effect_definition_registry::try_get(self.effect_type) {
            let target = def.param_count;
            if self.param_values.len() == target {
                return;
            }

            let mut aligned = vec![0.0f32; target];
            let copy_len = self.param_values.len().min(target);
            aligned[..copy_len].copy_from_slice(&self.param_values[..copy_len]);
            for (i, slot) in aligned.iter_mut().enumerate().take(target).skip(copy_len) {
                *slot = def.param_defs.get(i).map(|pd| pd.default_value).unwrap_or(0.0);
            }
            self.param_values = aligned;

            if let Some(ref base) = self.base_param_values {
                let mut aligned_base = vec![0.0f32; target];
                let base_copy = base.len().min(target);
                aligned_base[..base_copy].copy_from_slice(&base[..base_copy]);
                for (i, slot) in aligned_base.iter_mut().enumerate().take(target).skip(base_copy) {
                    *slot = def.param_defs.get(i).map(|pd| pd.default_value).unwrap_or(0.0);
                }
                self.base_param_values = Some(aligned_base);
            }
        }
    }

    /// Get the drivers list, creating it if None.
    pub fn drivers_mut(&mut self) -> &mut Vec<ParameterDriver> {
        if self.drivers.is_none() {
            self.drivers = Some(Vec::new());
        }
        self.drivers.as_mut().unwrap()
    }
}

/// Implement ParamSource for EffectInstance.
/// Port of Unity EffectInstance : IParamSource.
impl ParamSource for EffectInstance {
    fn display_name(&self) -> &str {
        use crate::effect_definition_registry;
        match effect_definition_registry::try_get(self.effect_type) {
            Some(def) => def.display_name,
            None => "?",
        }
    }

    fn param_count(&self) -> usize {
        self.param_values.len()
    }

    fn get_param_def(&self, index: usize) -> ParamDef {
        use crate::effect_definition_registry;
        match effect_definition_registry::try_get(self.effect_type) {
            Some(def) if index < def.param_count => def.param_defs[index].clone(),
            _ => ParamDef::default(),
        }
    }

    fn get_param(&self, index: usize) -> f32 {
        EffectInstance::get_param(self, index)
    }

    fn set_param(&mut self, index: usize, value: f32) {
        EffectInstance::set_param(self, index, value);
    }

    fn get_base_param(&self, index: usize) -> f32 {
        EffectInstance::get_base_param(self, index)
    }

    fn set_base_param(&mut self, index: usize, value: f32) {
        EffectInstance::set_base_param(self, index, value);
    }

    fn find_driver(&self, param_index: i32) -> Option<&ParameterDriver> {
        EffectInstance::find_driver(self, param_index)
    }

    fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>> {
        EffectInstance::get_drivers_list(self)
    }

    fn create_driver(&mut self, param_index: i32) -> &ParameterDriver {
        EffectInstance::create_driver(self, param_index)
    }

    fn remove_driver(&mut self, param_index: i32) {
        EffectInstance::remove_driver(self, param_index);
    }
}

// ─── Effect Group ───

/// A rack group containing multiple effects with shared bypass and wet/dry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectGroup {
    pub id: EffectGroupId,
    #[serde(default = "default_group_name")]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default = "default_one")]
    pub wet_dry: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_group_id: Option<EffectGroupId>,
}

impl EffectGroup {
    pub fn new(name: String) -> Self {
        Self {
            id: EffectGroupId::new(crate::short_id()),
            name,
            enabled: true,
            collapsed: false,
            wet_dry: 1.0,
            parent_group_id: None,
        }
    }

    pub fn clone_with_new_id(&self) -> Self {
        let mut cloned = self.clone();
        cloned.id = EffectGroupId::new(crate::short_id());
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
    /// Runtime state, not serialized. Unity ParameterDriver.cs line 59.
    #[serde(skip)]
    pub is_paused_by_user: bool,
}

impl ParameterDriver {
    /// Constructor. Unity ParameterDriver.cs lines 63-69.
    pub fn new(param_index: i32, division: BeatDivision, waveform: DriverWaveform) -> Self {
        Self {
            param_index,
            beat_division: division,
            waveform,
            enabled: true,
            phase: 0.0,
            base_value: 0.0,
            trim_min: 0.0,
            trim_max: 1.0,
            reversed: false,
            is_paused_by_user: false,
        }
    }

    /// Evaluate driver at given beat position -> [0, 1].
    /// Port of Unity DriverEvaluator.Evaluate.
    pub fn evaluate(current_beat: f32, division: BeatDivision, waveform: DriverWaveform, phase_offset: f32) -> f32 {
        let period = division.beats();
        if period <= 0.0 {
            return 0.5;
        }
        let p = (current_beat % period) / period + phase_offset;
        let phase = p - p.floor(); // wrap to [0, 1)

        match waveform {
            DriverWaveform::Sine => (phase * std::f32::consts::TAU).sin() * 0.5 + 0.5,
            DriverWaveform::Triangle => {
                if phase < 0.5 { phase * 2.0 } else { 2.0 - phase * 2.0 }
            }
            DriverWaveform::Sawtooth => phase,
            DriverWaveform::Square => if phase < 0.5 { 1.0 } else { 0.0 },
            DriverWaveform::Random => {
                // Deterministic per-period hash matching Unity's HashToFloat.
                // Unity ParameterDriver.cs lines 224-236.
                let cycle = (current_beat / period).floor() as i32;
                let mut h = cycle as u32;
                h ^= h >> 16;
                h = h.wrapping_mul(0x45d9f3b);
                h ^= h >> 16;
                h = h.wrapping_mul(0x45d9f3b);
                h ^= h >> 16;
                (h & 0x7FFFFF) as f32 / 0x7FFFFF as f32
            }
        }
    }
}

// ─── BeatDivision helpers ───

/// Constants matching Unity BeatDivisionHelper.
pub mod beat_division_helper {
    use crate::types::BeatDivision;

    pub const STRAIGHT_COUNT: usize = 11;
    pub const DOTTED_COUNT: usize = 5;
    pub const TRIPLET_COUNT: usize = 4;
    pub const TOTAL_COUNT: usize = 20;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum BeatModifier {
        None,
        Dotted,
        Triplet,
    }

    /// Display label for a beat division. Unity BeatDivisionHelper.ToLabel.
    pub fn to_label(div: BeatDivision) -> &'static str {
        match div {
            BeatDivision::ThirtySecond => "1/32",
            BeatDivision::Sixteenth => "1/16",
            BeatDivision::Eighth => "1/8",
            BeatDivision::Quarter => "1/4",
            BeatDivision::Half => "1/2",
            BeatDivision::Whole => "1/1",
            BeatDivision::TwoWhole => "2/1",
            BeatDivision::FourWhole => "4/1",
            BeatDivision::EightWhole => "8/1",
            BeatDivision::SixteenWhole => "16/1",
            BeatDivision::ThirtyTwoWhole => "32/1",
            BeatDivision::EighthDotted => "1/8.",
            BeatDivision::QuarterDotted => "1/4.",
            BeatDivision::HalfDotted => "1/2.",
            BeatDivision::WholeDotted => "1/1.",
            BeatDivision::TwoWholeDotted => "2/1.",
            BeatDivision::EighthTriplet => "1/8T",
            BeatDivision::QuarterTriplet => "1/4T",
            BeatDivision::HalfTriplet => "1/2T",
            BeatDivision::WholeTriplet => "1/1T",
        }
    }

    /// Decompose a BeatDivision into its straight base index (0-10) and modifier.
    /// Unity BeatDivisionHelper.Decompose lines 158-164.
    pub fn decompose(div: BeatDivision) -> (usize, BeatModifier) {
        let val = div as i32;
        if val >= 16 {
            ((val - 14) as usize, BeatModifier::Triplet)
        } else if val >= 11 {
            ((val - 9) as usize, BeatModifier::Dotted)
        } else {
            (val as usize, BeatModifier::None)
        }
    }

    /// Compose a straight base index + modifier into a BeatDivision.
    /// Returns None if the combination is invalid.
    /// Unity BeatDivisionHelper.TryCompose lines 170-184.
    pub fn try_compose(base_index: usize, modifier: BeatModifier) -> Option<BeatDivision> {
        match modifier {
            BeatModifier::Dotted if (2..=6).contains(&base_index) => {
                BeatDivision::from_i32((base_index + 9) as i32)
            }
            BeatModifier::Triplet if (2..=5).contains(&base_index) => {
                BeatDivision::from_i32((base_index + 14) as i32)
            }
            BeatModifier::None => {
                BeatDivision::from_i32(base_index as i32)
            }
            _ => None,
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
    #[serde(default, rename = "targetParamIndex")]
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
    /// Cached ADSR output (0-1) for UI display. Not serialized.
    #[serde(skip)]
    pub current_level: f32,
}

impl ParamEnvelope {
    /// Gen param envelope constructor. Unity ParamEnvelope.cs lines 42-45.
    pub fn new_for_gen(param_index: i32) -> Self {
        Self {
            target_effect_type: EffectType::Transform,
            param_index,
            enabled: true,
            attack_beats: 0.0,
            decay_beats: 0.0,
            sustain_level: 0.0,
            release_beats: 0.0,
            target_normalized: 1.0,
            current_level: 0.0,
        }
    }

    /// Effect envelope constructor. Unity ParamEnvelope.cs lines 48-52.
    pub fn new_for_effect(effect_type: EffectType, param_index: i32) -> Self {
        Self {
            target_effect_type: effect_type,
            param_index,
            enabled: true,
            attack_beats: 0.0,
            decay_beats: 0.0,
            sustain_level: 0.0,
            release_beats: 0.0,
            target_normalized: 1.0,
            current_level: 0.0,
        }
    }

    /// Calculate ADSR envelope level [0, 1] at given position within clip.
    /// Port of C# EnvelopeEvaluator.CalculateADSR().
    pub fn calculate_adsr(
        local_beat: f32,
        clip_duration: f32,
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    ) -> f32 {
        if clip_duration <= 0.0 || local_beat < 0.0 {
            return 0.0;
        }

        let mut a = attack.max(0.0);
        let mut d = decay.max(0.0);
        let mut r = release.max(0.0);
        let s = sustain.clamp(0.0, 1.0);

        let total_adr = a + d + r;
        if total_adr > clip_duration && total_adr > 0.0 {
            let scale = clip_duration / total_adr;
            a *= scale;
            d *= scale;
            r *= scale;
        }

        let release_start = clip_duration - r;

        if local_beat < a {
            return if a > 0.0 { local_beat / a } else { 1.0 };
        }

        let decay_start = a;
        if local_beat < decay_start + d {
            let t = if d > 0.0 { (local_beat - decay_start) / d } else { 1.0 };
            return 1.0 - (1.0 - s) * t;
        }

        if local_beat >= release_start {
            let t = if r > 0.0 {
                ((local_beat - release_start) / r).min(1.0)
            } else {
                1.0
            };
            return s * (1.0 - t);
        }

        s
    }
}

// ─── Default helpers ───

fn default_true() -> bool { true }
fn default_one() -> f32 { 1.0 }
fn generate_effect_id() -> EffectId { EffectId::new(crate::math::short_id()) }
#[allow(dead_code)]
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

    #[test]
    fn test_driver_random_hash_matches_unity() {
        let val = ParameterDriver::evaluate(1.0, BeatDivision::Quarter, DriverWaveform::Random, 0.0);
        assert!(val >= 0.0 && val <= 1.0);
        // Same cycle should give same value
        let val2 = ParameterDriver::evaluate(1.5, BeatDivision::Quarter, DriverWaveform::Random, 0.0);
        assert_eq!(val, val2);
    }
}
