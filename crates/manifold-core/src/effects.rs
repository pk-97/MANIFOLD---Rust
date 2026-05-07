use crate::effect_type_id::EffectTypeId;
use crate::id::{EffectGroupId, EffectId};
use crate::types::{BeatDivision, DriverWaveform};
use crate::units::Beats;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Stable string identifier for a host-visible parameter.
///
/// `Cow::Borrowed("amount")` for compile-time IDs (developer-defined
/// effects). `Cow::Owned(...)` for V2 user-exposed parameters allocated
/// at runtime. External mappings (OSC, Ableton, MIDI, modulation
/// drivers, envelopes) all key on this — never on positional indices.
///
/// See `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 for the full design.
pub type ParamId = Cow<'static, str>;

// ─── Param Definition ───

/// Metadata for a single parameter slot.
/// Port of Unity ParamDef.cs.
///
/// `id` is the **stable mapping key** referenced by every external
/// addressing site (OSC, Ableton, modulation drivers, project file
/// storage). Once shipped, `id` is forever — renaming an `id` is a
/// breaking change for every saved project.
///
/// `name` is the display label on the slider. Free to edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamDef {
    /// Stable mapping key. `snake_case` convention. Empty for legacy
    /// `ParamDef` instances loaded from V1.0.0 project files; the
    /// post-load alignment pass fills it in from the registry.
    #[serde(default)]
    pub id: String,
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
            id: String::new(),
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
    fn find_effect(&self, effect_type: &EffectTypeId) -> Option<&EffectInstance>;
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
    fn find_driver(&self, param_id: &str) -> Option<&ParameterDriver>;
    fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>>;
    fn create_driver(&mut self, param_id: ParamId) -> &ParameterDriver;
    fn remove_driver(&mut self, param_id: &str);
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
    effect_type: EffectTypeId,
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
    pub ableton_mappings: Option<Vec<crate::ableton_mapping::AbletonParamMapping>>,
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
    pub fn new(effect_type: EffectTypeId) -> Self {
        Self {
            id: generate_effect_id(),
            effect_type,
            enabled: true,
            collapsed: false,
            param_values: Vec::new(),
            base_param_values: None,
            drivers: None,
            ableton_mappings: None,
            group_id: None,
            legacy_param0: None,
            legacy_param1: None,
            legacy_param2: None,
            legacy_param3: None,
        }
    }

    /// Read-only access to the effect type.
    #[inline]
    pub fn effect_type(&self) -> &EffectTypeId {
        &self.effect_type
    }

    /// Has any drivers? Unity EffectInstance.cs line 28.
    pub fn has_drivers(&self) -> bool {
        self.drivers.as_ref().is_some_and(|d| !d.is_empty())
    }

    pub fn clone_deep(&self) -> Self {
        self.clone()
    }

    /// Assign a fresh EffectId (used when deep-cloning a layer or effect chain).
    pub fn regenerate_id(&mut self) {
        self.id = EffectId::new(crate::math::short_id());
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
            && index < base.len()
        {
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
        if self.base_param_values.is_none()
            || self
                .base_param_values
                .as_ref()
                .is_some_and(|b| b.len() != self.param_values.len())
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

    /// Find the driver for a given param id, or None.
    pub fn find_driver(&self, param_id: &str) -> Option<&ParameterDriver> {
        self.drivers
            .as_ref()?
            .iter()
            .find(|d| d.param_id == param_id)
    }

    /// Get drivers list reference (may be None).
    pub fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>> {
        self.drivers.as_ref()
    }

    /// Create a driver for a param id.
    pub fn create_driver(&mut self, param_id: ParamId) -> &ParameterDriver {
        let driver = ParameterDriver::new(param_id, BeatDivision::Quarter, DriverWaveform::Sine);
        self.drivers_mut().push(driver);
        self.drivers.as_ref().unwrap().last().unwrap()
    }

    /// Remove driver by param id.
    pub fn remove_driver(&mut self, param_id: &str) {
        if let Some(drivers) = &mut self.drivers {
            drivers.retain(|d| d.param_id != param_id);
        }
    }

    /// Resize paramValues and baseParamValues to match the current effect definition.
    /// New slots are filled with the definition's default values.
    /// Includes migration for layout changes (e.g., WireframeDepth 14→12 params).
    pub fn align_to_definition(&mut self) {
        use crate::effect_definition_registry;

        // Migration: WireframeDepth 14-param (old) → 12-param (new).
        // Old: Amount(0) Density(1) Width(2) ZScale(3) Smooth(4) Persist(5) Depth(6)
        //      Subject(7) Blend(8) WireRes(9) MeshRate(10) CVFlow(11) Lock(12) Face(13)
        // New: Amount(0) Density(1) Width(2) ZScale(3) Smooth(4) Subject(5) Blend(6)
        //      WireRes(7) MeshRate(8) Flow(9) Lock(10) EdgeFollow(11)
        if self.effect_type == EffectTypeId::WIREFRAME_DEPTH && self.param_values.len() == 14 {
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
                && base.len() == 14
            {
                let migrated_base = vec![
                    base[0], base[1], base[2], base[3], base[4], base[7], base[8], base[9],
                    base[10], base[11], base[12], 0.5,
                ];
                self.base_param_values = Some(migrated_base);
            }
        }

        if let Some(def) = effect_definition_registry::try_get(&self.effect_type) {
            let target = def.param_count;
            if self.param_values.len() == target {
                return;
            }

            let mut aligned = vec![0.0f32; target];
            let copy_len = self.param_values.len().min(target);
            aligned[..copy_len].copy_from_slice(&self.param_values[..copy_len]);
            for (i, slot) in aligned.iter_mut().enumerate().take(target).skip(copy_len) {
                *slot = def
                    .param_defs
                    .get(i)
                    .map(|pd| pd.default_value)
                    .unwrap_or(0.0);
            }
            self.param_values = aligned;

            if let Some(ref base) = self.base_param_values {
                let mut aligned_base = vec![0.0f32; target];
                let base_copy = base.len().min(target);
                aligned_base[..base_copy].copy_from_slice(&base[..base_copy]);
                for (i, slot) in aligned_base
                    .iter_mut()
                    .enumerate()
                    .take(target)
                    .skip(base_copy)
                {
                    *slot = def
                        .param_defs
                        .get(i)
                        .map(|pd| pd.default_value)
                        .unwrap_or(0.0);
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
        match effect_definition_registry::try_get(&self.effect_type) {
            Some(def) => def.display_name,
            None => "?",
        }
    }

    fn param_count(&self) -> usize {
        self.param_values.len()
    }

    fn get_param_def(&self, index: usize) -> ParamDef {
        use crate::effect_definition_registry;
        match effect_definition_registry::try_get(&self.effect_type) {
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

    fn find_driver(&self, param_id: &str) -> Option<&ParameterDriver> {
        EffectInstance::find_driver(self, param_id)
    }

    fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>> {
        EffectInstance::get_drivers_list(self)
    }

    fn create_driver(&mut self, param_id: ParamId) -> &ParameterDriver {
        EffectInstance::create_driver(self, param_id)
    }

    fn remove_driver(&mut self, param_id: &str) {
        EffectInstance::remove_driver(self, param_id);
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

/// LFO modulating a single effect or generator parameter.
///
/// Address shape: `param_id` is the canonical mapping key referenced by
/// project file storage and (by extension) any external client that
/// reads/writes saved JSON. Legacy V1 projects stored `paramIndex: i32`
/// instead — the custom [`Deserialize`] accepts either shape, parking
/// the legacy index in [`ParameterDriver::legacy_param_index`] for the
/// post-load resolver to translate via the registry.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterDriver {
    /// Stable mapping key. After post-load resolution, every driver in
    /// memory has a non-empty `param_id`. During the brief window
    /// between `Deserialize` and the post-load pass, a legacy V1
    /// driver may have `param_id = ""` and `legacy_param_index = Some`.
    pub param_id: ParamId,
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
    /// Set during `Deserialize` from the legacy `paramIndex` field.
    /// The post-load resolver (`crate::param_id_resolve`) walks every
    /// driver, looks up the effect/generator type's registry def, and
    /// assigns `param_id = def.param_defs[idx].id`. Once resolved this
    /// field is cleared. Never serialized.
    #[serde(skip)]
    pub legacy_param_index: Option<i32>,
    /// Runtime state, not serialized. Unity ParameterDriver.cs line 59.
    #[serde(skip)]
    pub is_paused_by_user: bool,
}

impl ParameterDriver {
    /// Constructor.
    pub fn new(
        param_id: impl Into<ParamId>,
        division: BeatDivision,
        waveform: DriverWaveform,
    ) -> Self {
        Self {
            param_id: param_id.into(),
            beat_division: division,
            waveform,
            enabled: true,
            phase: 0.0,
            base_value: 0.0,
            trim_min: 0.0,
            trim_max: 1.0,
            reversed: false,
            legacy_param_index: None,
            is_paused_by_user: false,
        }
    }

    /// Evaluate driver at given beat position -> [0, 1].
    /// Port of Unity DriverEvaluator.Evaluate.
    pub fn evaluate(
        current_beat: Beats,
        division: BeatDivision,
        waveform: DriverWaveform,
        phase_offset: f32,
    ) -> f32 {
        let period = division.beats();
        if period <= 0.0 {
            return 0.5;
        }
        let beat = current_beat.as_f32();
        let p = (beat % period) / period + phase_offset;
        let phase = p - p.floor(); // wrap to [0, 1)

        match waveform {
            DriverWaveform::Sine => (phase * std::f32::consts::TAU).sin() * 0.5 + 0.5,
            DriverWaveform::Triangle => {
                if phase < 0.5 {
                    phase * 2.0
                } else {
                    2.0 - phase * 2.0
                }
            }
            DriverWaveform::Sawtooth => phase,
            DriverWaveform::Square => {
                if phase < 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            DriverWaveform::Random => {
                // Deterministic per-period hash matching Unity's HashToFloat.
                // Unity ParameterDriver.cs lines 224-236.
                let cycle = (beat / period).floor() as i32;
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

// Custom `Deserialize` accepting both V1.1 (`paramIndex: i32`) and V1.2+
// (`paramId: "amount"`) project file shapes. The runtime always reads
// `param_id`; legacy projects park the index in `legacy_param_index`
// for the post-load resolver to translate. See
// `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 step 8.
impl<'de> Deserialize<'de> for ParameterDriver {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Mirror struct with both shapes accepted. `param_id` and
        // `param_index` are both optional — the driver must carry one
        // or the other. If both are present, `param_id` wins (forward
        // migration takes precedence over legacy index).
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(default)]
            param_id: Option<String>,
            #[serde(default)]
            param_index: Option<i32>,
            #[serde(default)]
            beat_division: BeatDivision,
            #[serde(default)]
            waveform: DriverWaveform,
            #[serde(default = "default_true")]
            enabled: bool,
            #[serde(default)]
            phase: f32,
            #[serde(default)]
            base_value: f32,
            #[serde(default)]
            trim_min: f32,
            #[serde(default = "default_one")]
            trim_max: f32,
            #[serde(default)]
            reversed: bool,
        }

        let raw = Raw::deserialize(deserializer)?;
        let (param_id, legacy_param_index) = match (raw.param_id, raw.param_index) {
            // Canonical V1.2+ shape — param_id present and non-empty.
            (Some(id), _) if !id.is_empty() => (Cow::Owned(id), None),
            // Legacy V1.1 shape — only paramIndex present. Park for
            // post-load resolution.
            (_, Some(idx)) => (Cow::Borrowed(""), Some(idx)),
            // Round-tripped shape from a project saved before the
            // post-load resolver could fill in `param_id` (e.g. test
            // harness without effect registry, or a future case where
            // the effect type was unregistered at save time). Treat
            // as "unresolvable" rather than erroring — driver stays
            // present but inert until the registry has the metadata
            // again. Better than refusing to load the project at all.
            (_, None) => (Cow::Borrowed(""), None),
        };
        Ok(ParameterDriver {
            param_id,
            beat_division: raw.beat_division,
            waveform: raw.waveform,
            enabled: raw.enabled,
            phase: raw.phase,
            base_value: raw.base_value,
            trim_min: raw.trim_min,
            trim_max: raw.trim_max,
            reversed: raw.reversed,
            legacy_param_index,
            is_paused_by_user: false,
        })
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
            BeatModifier::None => BeatDivision::from_i32(base_index as i32),
            _ => None,
        }
    }
}

// ─── Param Envelope (ADSR modulation) ───

/// Envelope evaluation mode.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EnvelopeMode {
    /// Classic ADSR envelope shape driven by clip timing.
    #[default]
    Adsr,
    /// Random value on each clip rising edge (walk or jump).
    Random,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamEnvelope {
    #[serde(default)]
    pub target_effect_type: EffectTypeId,
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
    /// Envelope evaluation mode (ADSR or Random).
    #[serde(default)]
    pub mode: EnvelopeMode,
    /// When mode=Random: true = jump to fully random value, false = walk by step.
    #[serde(default)]
    pub random_jump: bool,
    /// Random mode range minimum (normalized 0-1). Walk/jump stays within this range.
    #[serde(default)]
    pub range_min: f32,
    /// Random mode range maximum (normalized 0-1). Walk/jump stays within this range.
    #[serde(default = "default_one")]
    pub range_max: f32,
    /// Cached ADSR output (0-1) for UI display. Not serialized.
    #[serde(skip)]
    pub current_level: f32,
    /// Current random walk position (normalized 0-1). Runtime only.
    #[serde(skip)]
    pub walk_value: f32,
    /// Rising edge detection: was a clip active on the previous frame?
    #[serde(skip)]
    pub was_clip_active: bool,
    /// Previous frame's elapsed beats within the active clip. Used by Random
    /// mode to detect clip restarts and loop points (elapsed decreases).
    #[serde(skip)]
    pub last_elapsed: f32,
}

impl ParamEnvelope {
    /// Gen param envelope constructor. Unity ParamEnvelope.cs lines 42-45.
    pub fn new_for_gen(param_index: i32) -> Self {
        Self {
            target_effect_type: EffectTypeId::TRANSFORM,
            param_index,
            enabled: true,
            attack_beats: 0.0,
            decay_beats: 0.0,
            sustain_level: 0.0,
            release_beats: 0.0,
            target_normalized: 1.0,
            mode: EnvelopeMode::Adsr,
            random_jump: false,
            range_min: 0.0,
            range_max: 1.0,
            current_level: 0.0,
            walk_value: -1.0,
            was_clip_active: false,
            last_elapsed: -1.0,
        }
    }

    /// Effect envelope constructor. Unity ParamEnvelope.cs lines 48-52.
    pub fn new_for_effect(effect_type: EffectTypeId, param_index: i32) -> Self {
        Self {
            target_effect_type: effect_type,
            param_index,
            enabled: true,
            attack_beats: 0.0,
            decay_beats: 0.0,
            sustain_level: 0.0,
            release_beats: 0.0,
            target_normalized: 1.0,
            mode: EnvelopeMode::Adsr,
            random_jump: false,
            range_min: 0.0,
            range_max: 1.0,
            current_level: 0.0,
            walk_value: -1.0,
            was_clip_active: false,
            last_elapsed: -1.0,
        }
    }

    /// Calculate ADSR envelope level [0, 1] at given position within clip.
    /// Port of C# EnvelopeEvaluator.CalculateADSR().
    pub fn calculate_adsr(
        local_beat: Beats,
        clip_duration: Beats,
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    ) -> f32 {
        if clip_duration <= Beats::ZERO || local_beat < Beats::ZERO {
            return 0.0;
        }

        let local_beat = local_beat.as_f32();
        let clip_duration = clip_duration.as_f32();

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
            let t = if d > 0.0 {
                (local_beat - decay_start) / d
            } else {
                1.0
            };
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

fn default_true() -> bool {
    true
}
fn default_one() -> f32 {
    1.0
}
fn generate_effect_id() -> EffectId {
    EffectId::new(crate::math::short_id())
}
fn default_group_name() -> String {
    "Group".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(val >= 0.0 && val <= 1.0);
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
        let driver =
            ParameterDriver::new("amount", BeatDivision::Half, DriverWaveform::Triangle);
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
}
