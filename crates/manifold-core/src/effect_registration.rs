//! Distributed effect registration via `inventory`.
//!
//! Each effect submits an `EffectMetadata` from its implementation file.
//! The definition and type registries collect these at startup.

use crate::effect_definition_registry::EffectDef;
use crate::effect_type_id::EffectTypeId;
use crate::effect_type_registry::EffectTypeRegistration;
use crate::effects::ParamDef;
use crate::generator_registration::ParamSpec;

/// Complete metadata for an effect, submitted via `inventory::submit!`.
pub struct EffectMetadata {
    pub id: EffectTypeId,
    pub display_name: &'static str,
    pub category: &'static str,
    pub available: bool,
    pub osc_prefix: &'static str,
    pub legacy_discriminant: Option<i32>,
    pub params: &'static [ParamSpec],
}

inventory::collect!(EffectMetadata);

impl EffectMetadata {
    /// Convert to the existing `EffectDef` type.
    pub fn to_effect_def(&self) -> EffectDef {
        let param_defs: Vec<ParamDef> = self.params.iter().map(|p| p.to_param_def()).collect();
        let param_count = param_defs.len();
        let id_to_index = self
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.id.is_empty())
            .map(|(i, p)| (p.id.to_string(), i))
            .collect();
        EffectDef {
            display_name: self.display_name,
            param_count,
            param_defs,
            osc_prefix: Some(self.osc_prefix),
            id_to_index,
        }
    }

    /// Convert to the existing `EffectTypeRegistration` type.
    pub fn to_type_registration(&self) -> EffectTypeRegistration {
        EffectTypeRegistration {
            id: self.id.clone(),
            display_name: self.display_name,
            category: self.category,
            available: self.available,
        }
    }
}
