use serde::{Deserialize, Serialize};
use crate::types::GeneratorType;
use crate::effects::{ParameterDriver, ParamEnvelope};

/// Per-layer generator parameter state.
/// Port of Unity GeneratorParamState.cs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GeneratorParamState {
    #[serde(default)]
    pub generator_type: GeneratorType,
    #[serde(default)]
    pub param_values: Vec<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_param_values: Option<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drivers: Option<Vec<ParameterDriver>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelopes: Option<Vec<ParamEnvelope>>,

    // Legacy flat fields from V1.0.0 (before genParams nesting)
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genParamVersion")]
    pub legacy_param_version: Option<i32>,
}

impl GeneratorParamState {
    pub fn ensure_base_values(&mut self) {
        if self.base_param_values.is_none() ||
           self.base_param_values.as_ref().is_some_and(|b| b.len() != self.param_values.len())
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
        if let Some(def) = generator_definition_registry::try_get(self.generator_type) {
            if self.param_values.len() != def.param_count {
                self.init_defaults_for_type(self.generator_type);
            }
            if index < self.param_values.len() {
                self.param_values[index] = generator_definition_registry::clamp_param(
                    self.generator_type, index, value
                );
            }
        }
    }

    /// Read the user-set base value (before modulation).
    /// Unity GeneratorParamState.cs lines 64-69.
    pub fn get_param_base(&self, index: usize) -> f32 {
        if let Some(base) = &self.base_param_values {
            if index < base.len() {
                return base[index];
            }
        }
        self.get_param(index)
    }

    /// Set the user-intended base value.
    /// Unity GeneratorParamState.cs lines 75-88.
    pub fn set_param_base(&mut self, index: usize, value: f32) {
        use crate::generator_definition_registry;
        if let Some(def) = generator_definition_registry::try_get(self.generator_type) {
            if self.param_values.len() != def.param_count {
                self.init_defaults_for_type(self.generator_type);
            }
            self.ensure_base_values();
            if index < self.param_values.len() {
                let clamped = generator_definition_registry::clamp_param(
                    self.generator_type, index, value
                );
                if let Some(base) = &mut self.base_param_values {
                    if index < base.len() {
                        base[index] = clamped;
                    }
                }
                self.param_values[index] = clamped;
            }
        }
    }

    /// Find the driver for a given param index, or None.
    /// Unity GeneratorParamState.cs lines 34-40.
    pub fn find_driver(&self, param_index: i32) -> Option<&ParameterDriver> {
        self.drivers.as_ref()?.iter().find(|d| d.param_index == param_index)
    }

    /// Find the envelope for a given param index, or None.
    /// Unity GeneratorParamState.cs lines 121-127.
    pub fn find_envelope(&self, param_index: i32) -> Option<&ParamEnvelope> {
        self.envelopes.as_ref()?.iter().find(|e| e.param_index == param_index)
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
    pub fn change_type(&mut self, new_type: GeneratorType) {
        if new_type == GeneratorType::None {
            return;
        }
        self.generator_type = new_type;
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
    pub fn init_defaults_for_type(&mut self, gen_type: GeneratorType) {
        use crate::generator_definition_registry;
        if let Some(def) = generator_definition_registry::try_get(gen_type) {
            self.generator_type = gen_type;
            self.param_values = def.param_defs.iter().map(|pd| pd.default_value).collect();
            self.base_param_values = Some(self.param_values.clone());
        }
    }

    /// Legacy init_defaults (no parameter). Uses current generator_type.
    pub fn init_defaults(&mut self) {
        let gt = self.generator_type;
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
        self.drivers.as_ref().and_then(|d| {
            if d.is_empty() { None } else { Some(d.clone()) }
        })
    }

    /// Snapshot current envelopes (for undo). Returns deep copies.
    /// Unity GeneratorParamState.cs SnapshotEnvelopes lines 203-210.
    pub fn snapshot_envelopes(&self) -> Option<Vec<ParamEnvelope>> {
        self.envelopes.as_ref().and_then(|e| {
            if e.is_empty() { None } else { Some(e.clone()) }
        })
    }

    /// Restore from a snapshot (used by undo).
    /// Unity GeneratorParamState.cs Restore lines 168-183.
    pub fn restore(
        &mut self,
        gen_type: GeneratorType,
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
