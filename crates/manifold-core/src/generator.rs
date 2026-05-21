use crate::effects::{ParamDef, ParamEnvelope, ParamId, ParamSource, ParameterDriver};
use crate::generator_type_id::GeneratorTypeId;
use crate::types::{BeatDivision, DriverWaveform};
use serde::{Deserialize, Serialize};

/// Per-layer generator parameter state.
/// Port of Unity GeneratorParamState.cs.
///
/// Serialization (custom impls below):
///
/// - `paramValues` / `baseParamValues` accept both V1.x positional
///   `Array<f32>` and V1.2+ id-keyed `Object` shapes. On save, the
///   id-keyed Map form is emitted when the generator's registry def
///   is available; otherwise the legacy Array form is emitted.
///
/// In-memory storage stays positional (`Vec<f32>`). See
/// `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 step 13.
#[derive(Debug, Clone, Default)]
pub struct GeneratorParamState {
    generator_type: GeneratorTypeId,
    pub param_values: Vec<f32>,
    pub base_param_values: Option<Vec<f32>>,
    pub drivers: Option<Vec<ParameterDriver>>,
    pub envelopes: Option<Vec<ParamEnvelope>>,
    pub ableton_mappings: Option<Vec<crate::ableton_mapping::AbletonParamMapping>>,

    /// Legacy flat field from V1.0.0 (before genParams nesting).
    pub legacy_param_version: Option<i32>,
}

// ─── Custom Serialize / Deserialize for GeneratorParamState ───

impl Serialize for GeneratorParamState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        // Field count: generatorType + paramValues, plus optional fields.
        let mut field_count = 2;
        if self.base_param_values.is_some() {
            field_count += 1;
        }
        if self.drivers.is_some() {
            field_count += 1;
        }
        if self.envelopes.is_some() {
            field_count += 1;
        }
        if self.ableton_mappings.is_some() {
            field_count += 1;
        }
        if self.legacy_param_version.is_some() {
            field_count += 1;
        }

        let mut s = serializer.serialize_struct("GeneratorParamState", field_count)?;
        s.serialize_field("generatorType", &self.generator_type)?;
        s.serialize_field(
            "paramValues",
            &GenParamValuesSer {
                values: &self.param_values,
                gen_type: &self.generator_type,
            },
        )?;
        if let Some(base) = &self.base_param_values {
            s.serialize_field(
                "baseParamValues",
                &GenParamValuesSer {
                    values: base,
                    gen_type: &self.generator_type,
                },
            )?;
        }
        if let Some(d) = &self.drivers {
            s.serialize_field("drivers", d)?;
        }
        if let Some(e) = &self.envelopes {
            s.serialize_field("envelopes", e)?;
        }
        if let Some(m) = &self.ableton_mappings {
            s.serialize_field("abletonMappings", m)?;
        }
        if let Some(v) = self.legacy_param_version {
            s.serialize_field("genParamVersion", &v)?;
        }
        s.end()
    }
}

/// Serialize-side wrapper for generator `paramValues` / `baseParamValues`.
struct GenParamValuesSer<'a> {
    values: &'a [f32],
    gen_type: &'a GeneratorTypeId,
}

impl Serialize for GenParamValuesSer<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        crate::effects::serialize_param_values_for_generator(self.values, self.gen_type, serializer)
    }
}

impl<'de> Deserialize<'de> for GeneratorParamState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(default)]
            generator_type: GeneratorTypeId,
            #[serde(default)]
            param_values: Option<crate::effects::FloatValuesWire>,
            #[serde(default)]
            base_param_values: Option<crate::effects::FloatValuesWire>,
            #[serde(default)]
            drivers: Option<Vec<ParameterDriver>>,
            #[serde(default)]
            envelopes: Option<Vec<ParamEnvelope>>,
            #[serde(default)]
            ableton_mappings: Option<Vec<crate::ableton_mapping::AbletonParamMapping>>,
            #[serde(default, rename = "genParamVersion")]
            legacy_param_version: Option<i32>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let param_values = raw
            .param_values
            .map(|w| w.into_positional_for_generator(&raw.generator_type))
            .unwrap_or_default();
        let base_param_values = raw
            .base_param_values
            .map(|w| w.into_positional_for_generator(&raw.generator_type));

        Ok(GeneratorParamState {
            generator_type: raw.generator_type,
            param_values,
            base_param_values,
            drivers: raw.drivers,
            envelopes: raw.envelopes,
            ableton_mappings: raw.ableton_mappings,
            legacy_param_version: raw.legacy_param_version,
        })
    }
}

impl GeneratorParamState {
    /// Create a new GeneratorParamState with the given type, fully initialized
    /// from the generator definition registry.
    pub fn new(gen_type: GeneratorTypeId) -> Self {
        let mut state = Self::default();
        state.change_type(gen_type);
        state
    }

    /// The generator type for this param state.
    #[inline]
    pub fn generator_type(&self) -> &GeneratorTypeId {
        &self.generator_type
    }

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

    /// Indexed read for effective (modulated) param value.
    /// Unity GeneratorParamState.cs lines 43-48.
    pub fn get_param(&self, index: usize) -> f32 {
        self.param_values.get(index).copied().unwrap_or(0.0)
    }

    /// Write to effective (modulated) param value.
    ///
    /// Auto-extends `param_values` to cover `index`, then writes. The
    /// effect-side mirror (`EffectInstance::set_param`) uses the same
    /// pattern — neither path gates on a registry def existing for the
    /// host type, because JSON-only generators (Wireframe, etc.) have
    /// no static-registry entry but still need slider writes to land.
    /// Clamping against the registry's declared range still happens
    /// when a def exists; absence is treated as "pass through unchanged."
    pub fn set_param(&mut self, index: usize, value: f32) {
        use crate::generator_definition_registry;
        while self.param_values.len() <= index {
            self.param_values.push(0.0);
        }
        self.param_values[index] =
            generator_definition_registry::clamp_param(&self.generator_type, index, value);
    }

    /// Ensure `param_values` (and `base_param_values`, if present) is
    /// AT LEAST as long as the registry's parameter count for this
    /// generator type, padding the tail with registry default values.
    ///
    /// **Extend-only.** When `param_values.len() > def.param_count`, the
    /// tail is left intact — the surplus belongs to user-added bindings
    /// stored in the layer's per-instance `generator_graph.preset_metadata`
    /// (approach A in `docs/EFFECT_GENERATOR_CARD_UNIFICATION.md`).
    /// Truncating here would wipe those user-added slots on the first
    /// slider interaction.
    ///
    /// Without this migration, the first slider interaction on a project
    /// saved before a new bundled param was added would call
    /// `init_defaults_for_type` and wipe every saved value.
    pub fn migrate_to_registry_length(&mut self) {
        use crate::generator_definition_registry;
        let Some(def) = generator_definition_registry::try_get(&self.generator_type) else {
            return;
        };
        let min_target = def.param_count;
        if self.param_values.len() < min_target {
            self.param_values.reserve(min_target - self.param_values.len());
            for i in self.param_values.len()..min_target {
                self.param_values.push(def.param_defs[i].default_value);
            }
        }
        if let Some(base) = &mut self.base_param_values
            && base.len() < min_target
        {
            base.reserve(min_target - base.len());
            for i in base.len()..min_target {
                base.push(def.param_defs[i].default_value);
            }
        }
    }

    /// Read the user-set base value (before modulation).
    /// Unity GeneratorParamState.cs lines 64-69.
    pub fn get_param_base(&self, index: usize) -> f32 {
        if let Some(base) = &self.base_param_values
            && index < base.len()
        {
            return base[index];
        }
        self.get_param(index)
    }

    /// Set the user-intended base value.
    ///
    /// Auto-extends both `param_values` and `base_param_values` to
    /// cover `index`, then writes. Matches the effect-side
    /// `EffectInstance::set_base_param` shape: registry presence is
    /// only consulted for clamping, never as a gate on the write
    /// itself. Without this, JSON-only generators (Wireframe, etc.)
    /// silently dropped every slider drag — they have no Rust-side
    /// `inventory::submit!` entry and `generator_definition_registry::try_get`
    /// returns `None`. User-added bindings (whose slot indices sit
    /// past the registry's declared `param_count`) ride the same path.
    ///
    /// When the registry IS present and `param_values` is shorter than
    /// `def.param_count` (project saved before a bundled param was
    /// added), `migrate_to_registry_length` runs first to pad the tail
    /// with registry defaults — same migrate-on-touch behaviour the
    /// pre-unification code relied on.
    pub fn set_param_base(&mut self, index: usize, value: f32) {
        use crate::generator_definition_registry;
        if let Some(def) = generator_definition_registry::try_get(&self.generator_type)
            && self.param_values.len() < def.param_count
        {
            self.migrate_to_registry_length();
        }
        self.ensure_base_values();
        while self.param_values.len() <= index {
            self.param_values.push(0.0);
        }
        if let Some(base) = &mut self.base_param_values {
            while base.len() <= index {
                base.push(0.0);
            }
        }
        let clamped =
            generator_definition_registry::clamp_param(&self.generator_type, index, value);
        if let Some(base) = &mut self.base_param_values {
            base[index] = clamped;
        }
        self.param_values[index] = clamped;
    }

    /// Find the driver for a given param id, or None.
    pub fn find_driver(&self, param_id: &str) -> Option<&ParameterDriver> {
        self.drivers
            .as_ref()?
            .iter()
            .find(|d| d.param_id == param_id)
    }

    /// Find the envelope for a given param id, or None.
    pub fn find_envelope(&self, param_id: &str) -> Option<&ParamEnvelope> {
        self.envelopes
            .as_ref()?
            .iter()
            .find(|e| e.param_id == param_id)
    }

    /// True if this state has envelopes (no-alloc check).
    /// Unity GeneratorParamState.cs line 130.
    pub fn has_envelopes(&self) -> bool {
        self.envelopes.as_ref().is_some_and(|e| !e.is_empty())
    }

    /// Drivers list, auto-created on first access.
    /// Unity GeneratorParamState.cs lines 24-31.
    pub fn drivers_mut(&mut self) -> &mut Vec<ParameterDriver> {
        if self.drivers.is_none() {
            self.drivers = Some(Vec::new());
        }
        self.drivers.as_mut().unwrap()
    }

    /// Envelopes list, auto-created on first access.
    /// Unity GeneratorParamState.cs lines 133-140.
    pub fn envelopes_mut(&mut self) -> &mut Vec<ParamEnvelope> {
        if self.envelopes.is_none() {
            self.envelopes = Some(Vec::new());
        }
        self.envelopes.as_mut().unwrap()
    }

    /// Reset effective param values to base — ONLY for params with active drivers or envelopes.
    /// Port of C# GeneratorParamState.ResetEffectives().
    pub fn reset_effectives(&mut self) {
        use crate::generator_definition_registry;

        if self.param_values.is_empty() {
            return;
        }
        self.ensure_base_values();
        let base = match &self.base_param_values {
            Some(b) => b,
            None => return,
        };
        let id_to_index =
            generator_definition_registry::try_get(&self.generator_type).map(|d| &d.id_to_index);

        if let Some(drivers) = &self.drivers {
            for driver in drivers {
                if !driver.enabled {
                    continue;
                }
                let Some(&idx) = id_to_index.and_then(|m| m.get(driver.param_id.as_ref())) else {
                    continue;
                };
                if idx < self.param_values.len() && idx < base.len() {
                    self.param_values[idx] = base[idx];
                }
            }
        }

        if let Some(envelopes) = &self.envelopes {
            for env in envelopes {
                if !env.enabled {
                    continue;
                }
                let Some(&idx) = id_to_index.and_then(|m| m.get(env.param_id.as_ref())) else {
                    continue;
                };
                if idx < self.param_values.len() && idx < base.len() {
                    self.param_values[idx] = base[idx];
                }
            }
        }
    }

    /// Change generator type. Unity GeneratorParamState.cs ChangeType.
    pub fn change_type(&mut self, new_type: GeneratorTypeId) {
        if new_type == GeneratorTypeId::NONE {
            return;
        }
        self.generator_type = new_type.clone();
        self.init_defaults_for_type(new_type);
        if let Some(drivers) = &mut self.drivers {
            drivers.clear();
        }
        if let Some(envelopes) = &mut self.envelopes {
            envelopes.clear();
        }
    }

    /// Initialize both base and effective arrays from registry defaults.
    /// Unity GeneratorParamState.cs InitDefaults(GeneratorType genType) lines 143-155.
    /// Takes a type parameter and sets self.generator_type = genType.
    pub fn init_defaults_for_type(&mut self, gen_type: GeneratorTypeId) {
        use crate::generator_definition_registry;
        if let Some(def) = generator_definition_registry::try_get(&gen_type) {
            self.generator_type = gen_type;
            self.param_values = def.param_defs.iter().map(|pd| pd.default_value).collect();
            self.base_param_values = Some(self.param_values.clone());
        }
    }

    /// Legacy init_defaults (no parameter). Uses current generator_type.
    pub fn init_defaults(&mut self) {
        let gt = self.generator_type.clone();
        self.init_defaults_for_type(gt);
    }

    /// Snapshot current base param values (for undo). Returns a clone.
    /// Unity GeneratorParamState.cs SnapshotParams lines 186-190.
    pub fn snapshot_params(&self) -> Vec<f32> {
        if let Some(base) = &self.base_param_values {
            base.clone()
        } else if !self.param_values.is_empty() {
            self.param_values.clone()
        } else {
            Vec::new()
        }
    }

    /// Snapshot current drivers (for undo). Returns deep copies.
    /// Unity GeneratorParamState.cs SnapshotDrivers lines 193-200.
    pub fn snapshot_drivers(&self) -> Option<Vec<ParameterDriver>> {
        self.drivers
            .as_ref()
            .and_then(|d| if d.is_empty() { None } else { Some(d.clone()) })
    }

    /// Snapshot current envelopes (for undo). Returns deep copies.
    /// Unity GeneratorParamState.cs SnapshotEnvelopes lines 203-210.
    pub fn snapshot_envelopes(&self) -> Option<Vec<ParamEnvelope>> {
        self.envelopes
            .as_ref()
            .and_then(|e| if e.is_empty() { None } else { Some(e.clone()) })
    }

    /// Restore from a snapshot (used by undo).
    /// Unity GeneratorParamState.cs Restore lines 168-183.
    pub fn restore(
        &mut self,
        gen_type: GeneratorTypeId,
        params: Vec<f32>,
        drivers: Option<Vec<ParameterDriver>>,
        envelopes: Option<Vec<ParamEnvelope>>,
    ) {
        self.generator_type = gen_type;
        self.param_values = params.clone();
        self.base_param_values = Some(params);
        if let Some(d) = &mut self.drivers {
            d.clear();
        }
        if let Some(snapshot_drivers) = drivers {
            self.drivers_mut().extend(snapshot_drivers);
        }
        if let Some(e) = &mut self.envelopes {
            e.clear();
        }
        if let Some(snapshot_envelopes) = envelopes {
            self.envelopes_mut().extend(snapshot_envelopes);
        }
    }
}

/// Unified parameter interface — mirrors `impl ParamSource for EffectInstance`.
impl ParamSource for GeneratorParamState {
    fn display_name(&self) -> &str {
        use crate::generator_definition_registry;
        generator_definition_registry::try_get(&self.generator_type)
            .map(|d| d.display_name)
            .unwrap_or("Generator")
    }

    fn param_count(&self) -> usize {
        self.param_values.len()
    }

    fn get_param_def(&self, index: usize) -> ParamDef {
        use crate::generator_definition_registry;
        match generator_definition_registry::try_get(&self.generator_type) {
            Some(def) if index < def.param_count => def.param_defs[index].clone(),
            _ => ParamDef::default(),
        }
    }

    fn get_param(&self, index: usize) -> f32 {
        GeneratorParamState::get_param(self, index)
    }

    fn set_param(&mut self, index: usize, value: f32) {
        GeneratorParamState::set_param(self, index, value);
    }

    fn get_base_param(&self, index: usize) -> f32 {
        GeneratorParamState::get_param_base(self, index)
    }

    fn set_base_param(&mut self, index: usize, value: f32) {
        GeneratorParamState::set_param_base(self, index, value);
    }

    fn find_driver(&self, param_id: &str) -> Option<&ParameterDriver> {
        GeneratorParamState::find_driver(self, param_id)
    }

    fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>> {
        self.drivers.as_ref()
    }

    fn create_driver(&mut self, param_id: ParamId) -> &ParameterDriver {
        let driver = ParameterDriver::new(param_id, BeatDivision::Quarter, DriverWaveform::Sine);
        self.drivers_mut().push(driver);
        self.drivers.as_ref().unwrap().last().unwrap()
    }

    fn remove_driver(&mut self, param_id: &str) {
        if let Some(drivers) = &mut self.drivers {
            drivers.retain(|d| d.param_id != param_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator_definition_registry;
    use crate::generator_registration::{GeneratorMetadata, ParamSpec};

    // Test-only inventory submission — BLACK_HOLE isn't linked from manifold-renderer in unit tests.
    inventory::submit! {
        GeneratorMetadata {
            id: GeneratorTypeId::BLACK_HOLE,
            display_name: "Black Hole",
            is_line_based: false,
            available: true,
            osc_prefix: "blackHole",
            legacy_discriminant: Some(21),
            params: &[
                ParamSpec::continuous("speed", "Speed", 0.0, 5.0, 0.3, "F2", "speed"),
                ParamSpec::continuous("cam_dist", "Cam Dist", 0.1, 50.0, 20.0, "F1", "camDist"),
                ParamSpec::continuous("tilt", "Tilt", 0.0, 90.0, 15.0, "F0", "tilt"),
                ParamSpec::continuous("rotate", "Rotate", -180.0, 180.0, 0.0, "F0", "rotate"),
                ParamSpec::whole("steps", "Steps", 50.0, 500.0, 150.0, "steps"),
                ParamSpec::continuous("disk_inner", "Disk Inner", 2.0, 6.0, 3.0, "F1", "diskInner"),
                ParamSpec::continuous("disk_outer", "Disk Outer", 5.0, 20.0, 10.0, "F1", "diskOuter"),
                ParamSpec::continuous("disk_glow", "Disk Glow", 0.5, 5.0, 2.0, "F1", "diskGlow"),
                ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
                ParamSpec::continuous("stars", "Stars", 0.0, 2.0, 0.5, "F2", "stars"),
                ParamSpec::continuous("spin", "Spin", -1.0, 1.0, 0.0, "F2", "spin"),
                ParamSpec::continuous("particles", "Particles", 0.0, 1.0, 0.0, "F2", "particles"),
                ParamSpec::continuous("turbulence", "Turbulence", 0.0, 5.0, 0.5, "F2", "turbulence"),
                ParamSpec::continuous("cam_velocity", "Cam Velocity", 0.0, 0.99, 0.0, "F2", "camVelocity"),
                ParamSpec::continuous("freefall", "Freefall", 0.0, 1.0, 0.0, "F0", "freefall"),
            ],
            string_params: &[],
        }
    }

    #[test]
    fn migrate_pads_short_param_arrays_with_defaults_preserving_existing() {
        let gt = GeneratorTypeId::BLACK_HOLE;
        let target_count = generator_definition_registry::try_get(&gt)
            .expect("BLACK_HOLE registered")
            .param_count;
        assert!(
            target_count >= 4,
            "test assumes BLACK_HOLE has at least 4 params"
        );

        // Simulate a project saved when BLACK_HOLE had only 3 params.
        let mut state = GeneratorParamState {
            param_values: vec![1.5, 2.5, 3.5],
            base_param_values: Some(vec![1.5, 2.5, 3.5]),
            ..Default::default()
        };
        state.generator_type = gt.clone();

        state.migrate_to_registry_length();

        assert_eq!(state.param_values.len(), target_count);
        assert_eq!(state.param_values[0], 1.5);
        assert_eq!(state.param_values[1], 2.5);
        assert_eq!(state.param_values[2], 3.5);

        // The new tail entries should match the registry defaults exactly.
        let def = generator_definition_registry::try_get(&gt).unwrap();
        for i in 3..target_count {
            assert_eq!(
                state.param_values[i], def.param_defs[i].default_value,
                "tail index {i} should be registry default"
            );
        }

        let base = state.base_param_values.as_ref().unwrap();
        assert_eq!(base.len(), target_count);
        assert_eq!(base[0], 1.5);
        assert_eq!(base[1], 2.5);
        assert_eq!(base[2], 3.5);
    }

    #[test]
    fn set_param_after_registry_growth_does_not_wipe_existing_values() {
        // Regression test for the bug where set_param's length-mismatch branch
        // called init_defaults_for_type, wiping every saved value.
        let gt = GeneratorTypeId::BLACK_HOLE;
        let target_count = generator_definition_registry::try_get(&gt)
            .expect("BLACK_HOLE registered")
            .param_count;

        // Use values inside each param's clamp range:
        //   Speed 0..5, Cam Dist 0.1..50, Tilt 0..90.
        let mut state = GeneratorParamState {
            param_values: vec![2.5, 8.0, 9.0],
            base_param_values: Some(vec![2.5, 8.0, 9.0]),
            ..Default::default()
        };
        state.generator_type = gt;

        // Touch the first slider — this previously wiped everything.
        state.set_param_base(0, 2.5);

        assert_eq!(state.param_values.len(), target_count);
        assert_eq!(state.param_values[0], 2.5);
        assert_eq!(state.param_values[1], 8.0);
        assert_eq!(state.param_values[2], 9.0);
    }

    #[test]
    fn set_param_base_writes_through_for_json_only_generator_with_no_registry_entry() {
        // Regression test for the bug Peter spotted while smoke-testing:
        // JSON-only generators (Wireframe, TrivialPassthrough, etc.)
        // have no `inventory::submit!` entry, so `try_get` returns
        // `None`. The previous `set_param_base` body was entirely
        // gated on registry presence — every slider drag was silently
        // dropped on these generators, including bundled params.
        //
        // After the fix, registry absence stops gating the write.
        // Clamping is the only thing that depends on a def existing.
        let unknown_type = GeneratorTypeId::from_string("DoesNotExist".to_string());
        assert!(
            generator_definition_registry::try_get(&unknown_type).is_none(),
            "fixture relies on this type NOT being in the registry"
        );

        let mut state = GeneratorParamState {
            generator_type: unknown_type,
            param_values: vec![0.0, 1.0], // two bundled slots
            base_param_values: Some(vec![0.0, 1.0]),
            ..Default::default()
        };

        state.set_param_base(1, 0.75);

        assert_eq!(state.param_values[1], 0.75, "write landed on bundled slot");
        assert_eq!(
            state.base_param_values.as_ref().unwrap()[1],
            0.75,
            "write landed on base slot too"
        );

        // User-added tail slot — slot index past the original length.
        state.set_param_base(2, 0.42);
        assert_eq!(state.param_values.len(), 3, "param_values auto-extended");
        assert_eq!(state.param_values[2], 0.42, "tail write landed");
        assert_eq!(
            state.base_param_values.as_ref().unwrap()[2],
            0.42,
            "tail base write landed too"
        );
    }
}
