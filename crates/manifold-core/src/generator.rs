use crate::effects::{ParamDef, ParamEnvelope, ParamSource, ParameterDriver};
use crate::generator_type_id::GeneratorTypeId;
use crate::types::{BeatDivision, DriverWaveform};
use serde::{Deserialize, Serialize};

/// Per-layer generator parameter state.
/// Port of Unity GeneratorParamState.cs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GeneratorParamState {
    #[serde(default)]
    generator_type: GeneratorTypeId,
    #[serde(default)]
    pub param_values: Vec<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_param_values: Option<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drivers: Option<Vec<ParameterDriver>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelopes: Option<Vec<ParamEnvelope>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ableton_mappings: Option<Vec<crate::ableton_mapping::AbletonParamMapping>>,

    // Legacy flat fields from V1.0.0 (before genParams nesting)
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genParamVersion"
    )]
    pub legacy_param_version: Option<i32>,
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
    /// Unity GeneratorParamState.cs lines 51-61.
    pub fn set_param(&mut self, index: usize, value: f32) {
        use crate::generator_definition_registry;
        if let Some(def) = generator_definition_registry::try_get(&self.generator_type) {
            if self.param_values.len() != def.param_count {
                self.migrate_to_registry_length();
            }
            if index < self.param_values.len() {
                self.param_values[index] =
                    generator_definition_registry::clamp_param(&self.generator_type, index, value);
            }
        }
    }

    /// Migrate `param_values` (and `base_param_values`, if present) to match
    /// the current registry's parameter count for this generator type, while
    /// preserving every existing value. Missing tail entries are filled from
    /// the registry's default values; excess entries are truncated.
    ///
    /// This is what makes adding a new parameter to a generator non-destructive
    /// for projects saved before the parameter existed. Without this migration,
    /// the first slider interaction on an old clip would call
    /// `init_defaults_for_type` and wipe every saved value.
    pub fn migrate_to_registry_length(&mut self) {
        use crate::generator_definition_registry;
        let Some(def) = generator_definition_registry::try_get(&self.generator_type) else {
            return;
        };
        let target = def.param_count;
        if self.param_values.len() != target {
            let mut migrated = Vec::with_capacity(target);
            for i in 0..target {
                let v = self
                    .param_values
                    .get(i)
                    .copied()
                    .unwrap_or(def.param_defs[i].default_value);
                migrated.push(v);
            }
            self.param_values = migrated;
        }
        if let Some(base) = &self.base_param_values
            && base.len() != target
        {
            let old = base.clone();
            let mut migrated = Vec::with_capacity(target);
            for i in 0..target {
                let v = old.get(i).copied().unwrap_or(def.param_defs[i].default_value);
                migrated.push(v);
            }
            self.base_param_values = Some(migrated);
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
    /// Unity GeneratorParamState.cs lines 75-88.
    pub fn set_param_base(&mut self, index: usize, value: f32) {
        use crate::generator_definition_registry;
        if let Some(def) = generator_definition_registry::try_get(&self.generator_type) {
            if self.param_values.len() != def.param_count {
                self.migrate_to_registry_length();
            }
            self.ensure_base_values();
            if index < self.param_values.len() {
                let clamped =
                    generator_definition_registry::clamp_param(&self.generator_type, index, value);
                if let Some(base) = &mut self.base_param_values
                    && index < base.len()
                {
                    base[index] = clamped;
                }
                self.param_values[index] = clamped;
            }
        }
    }

    /// Find the driver for a given param index, or None.
    /// Unity GeneratorParamState.cs lines 34-40.
    pub fn find_driver(&self, param_index: i32) -> Option<&ParameterDriver> {
        self.drivers
            .as_ref()?
            .iter()
            .find(|d| d.param_index == param_index)
    }

    /// Find the envelope for a given param index, or None.
    /// Unity GeneratorParamState.cs lines 121-127.
    pub fn find_envelope(&self, param_index: i32) -> Option<&ParamEnvelope> {
        self.envelopes
            .as_ref()?
            .iter()
            .find(|e| e.param_index == param_index)
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
        if self.param_values.is_empty() {
            return;
        }
        self.ensure_base_values();
        let base = match &self.base_param_values {
            Some(b) => b,
            None => return,
        };

        if let Some(drivers) = &self.drivers {
            for driver in drivers {
                let idx = driver.param_index as usize;
                if driver.enabled && idx < self.param_values.len() && idx < base.len() {
                    self.param_values[idx] = base[idx];
                }
            }
        }

        if let Some(envelopes) = &self.envelopes {
            for env in envelopes {
                let idx = env.param_index as usize;
                if env.enabled && idx < self.param_values.len() && idx < base.len() {
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

    fn find_driver(&self, param_index: i32) -> Option<&ParameterDriver> {
        GeneratorParamState::find_driver(self, param_index)
    }

    fn get_drivers_list(&self) -> Option<&Vec<ParameterDriver>> {
        self.drivers.as_ref()
    }

    fn create_driver(&mut self, param_index: i32) -> &ParameterDriver {
        let driver = ParameterDriver::new(param_index, BeatDivision::Quarter, DriverWaveform::Sine);
        self.drivers_mut().push(driver);
        self.drivers.as_ref().unwrap().last().unwrap()
    }

    fn remove_driver(&mut self, param_index: i32) {
        if let Some(drivers) = &mut self.drivers {
            drivers.retain(|d| d.param_index != param_index);
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
                ParamSpec::continuous("Speed", 0.0, 5.0, 0.3, "F2", "speed"),
                ParamSpec::continuous("Cam Dist", 0.1, 50.0, 20.0, "F1", "camDist"),
                ParamSpec::continuous("Tilt", 0.0, 90.0, 15.0, "F0", "tilt"),
                ParamSpec::continuous("Rotate", -180.0, 180.0, 0.0, "F0", "rotate"),
                ParamSpec::whole("Steps", 50.0, 500.0, 150.0, "steps"),
                ParamSpec::continuous("Disk Inner", 2.0, 6.0, 3.0, "F1", "diskInner"),
                ParamSpec::continuous("Disk Outer", 5.0, 20.0, 10.0, "F1", "diskOuter"),
                ParamSpec::continuous("Disk Glow", 0.5, 5.0, 2.0, "F1", "diskGlow"),
                ParamSpec::continuous("Scale", 0.25, 3.0, 1.0, "F2", "scale"),
                ParamSpec::continuous("Stars", 0.0, 2.0, 0.5, "F2", "stars"),
                ParamSpec::continuous("Spin", -1.0, 1.0, 0.0, "F2", "spin"),
                ParamSpec::continuous("Particles", 0.0, 1.0, 0.0, "F2", "particles"),
                ParamSpec::continuous("Turbulence", 0.0, 5.0, 0.5, "F2", "turbulence"),
                ParamSpec::continuous("Cam Velocity", 0.0, 0.99, 0.0, "F2", "camVelocity"),
                ParamSpec::continuous("Freefall", 0.0, 1.0, 0.0, "F0", "freefall"),
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
}
