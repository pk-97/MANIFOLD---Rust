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

    pub fn reset_effectives(&mut self) {
        if let Some(base) = &self.base_param_values {
            for (i, &val) in base.iter().enumerate() {
                if i < self.param_values.len() {
                    self.param_values[i] = val;
                }
            }
        }
    }

    pub fn change_type(&mut self, new_type: GeneratorType) {
        self.generator_type = new_type;
        self.param_values.clear();
        self.base_param_values = None;
        self.drivers = None;
        self.envelopes = None;
    }
}
