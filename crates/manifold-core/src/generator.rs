use serde::{Deserialize, Serialize};
use crate::types::GeneratorType;
use crate::effects::{ParameterDriver, ParamEnvelope};

/// Per-layer generator parameter state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GeneratorParamState {
    #[serde(default)]
    pub generator_type: GeneratorType,
    #[serde(default)]
    pub param_values: Vec<f32>,
    #[serde(default)]
    pub base_param_values: Option<Vec<f32>>,
    #[serde(default)]
    pub drivers: Option<Vec<ParameterDriver>>,
    #[serde(default)]
    pub envelopes: Option<Vec<ParamEnvelope>>,

    // Legacy flat fields from V1.0.0 (before genParams nesting)
    #[serde(default, rename = "genParamVersion")]
    pub legacy_param_version: Option<i32>,
}

impl GeneratorParamState {
    pub fn ensure_base_values(&mut self) {
        if self.base_param_values.is_none() && !self.param_values.is_empty() {
            self.base_param_values = Some(self.param_values.clone());
        }
    }

    /// Reset effective param values to base — ONLY for params with active drivers or envelopes.
    /// Port of C# GeneratorParamState.ResetEffectives().
    /// Unity resets selectively to preserve user-adjusted params that have no modulation.
    pub fn reset_effectives(&mut self) {
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

    pub fn change_type(&mut self, new_type: GeneratorType) {
        self.generator_type = new_type;
        self.init_defaults();
        self.drivers = None;
        self.envelopes = None;
    }

    /// Fill param_values and base_param_values from the definition defaults.
    pub fn init_defaults(&mut self) {
        let defs = self.generator_type.param_defs();
        self.param_values = defs.iter().map(|d| d.3).collect();
        self.base_param_values = Some(self.param_values.clone());
    }
}
