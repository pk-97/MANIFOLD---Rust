//! Distributed generator registration via `inventory`.
//!
//! Each generator submits a `GeneratorMetadata` from its implementation file.
//! The definition and type registries collect these at startup.

use crate::effects::ParamDef;
use crate::generator_definition_registry::{GeneratorDef, StringParamDef};
use crate::generator_type_id::GeneratorTypeId;
use crate::generator_type_registry::GeneratorTypeRegistration;

/// Static parameter specification — all fields are `'static` so the struct
/// can live in `inventory::submit!` blocks without allocation.
///
/// `id` is the **stable mapping key** referenced by OSC routing, Ableton
/// macro bindings, modulation drivers, envelopes, and project file
/// storage. Once shipped, `id` is forever — renaming an `id` is a
/// breaking change for every saved project. `name` is the editable
/// display label on the slider; rename it freely.
///
/// Convention: `id` is `snake_case`, derived from `name` in most cases
/// (e.g. `"Beat Division"` → `"beat_division"`). A few effects use
/// short hand-picked IDs (e.g. `"rot_xy"` rather than `"xy"`).
#[derive(Debug, Clone)]
pub struct ParamSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub default_value: f32,
    pub whole_numbers: bool,
    pub is_toggle: bool,
    pub value_labels: &'static [&'static str],
    pub format_string: Option<&'static str>,
    pub osc_suffix: &'static str,
}

impl ParamSpec {
    /// Continuous parameter with format string and OSC suffix.
    pub const fn continuous(
        id: &'static str,
        name: &'static str,
        min: f32,
        max: f32,
        default_value: f32,
        format_string: &'static str,
        osc_suffix: &'static str,
    ) -> Self {
        Self {
            id,
            name,
            min,
            max,
            default_value,
            whole_numbers: false,
            is_toggle: false,
            value_labels: &[],
            format_string: Some(format_string),
            osc_suffix,
        }
    }

    /// Toggle (boolean) parameter.
    pub const fn toggle(
        id: &'static str,
        name: &'static str,
        min: f32,
        max: f32,
        default_value: f32,
        osc_suffix: &'static str,
    ) -> Self {
        Self {
            id,
            name,
            min,
            max,
            default_value,
            whole_numbers: false,
            is_toggle: true,
            value_labels: &[],
            format_string: None,
            osc_suffix,
        }
    }

    /// Whole-number parameter (integer steps).
    pub const fn whole(
        id: &'static str,
        name: &'static str,
        min: f32,
        max: f32,
        default_value: f32,
        osc_suffix: &'static str,
    ) -> Self {
        Self {
            id,
            name,
            min,
            max,
            default_value,
            whole_numbers: true,
            is_toggle: false,
            value_labels: &[],
            format_string: None,
            osc_suffix,
        }
    }

    /// Whole-number parameter with named labels for each value.
    pub const fn whole_labels(
        id: &'static str,
        name: &'static str,
        min: f32,
        max: f32,
        default_value: f32,
        labels: &'static [&'static str],
        osc_suffix: &'static str,
    ) -> Self {
        Self {
            id,
            name,
            min,
            max,
            default_value,
            whole_numbers: true,
            is_toggle: false,
            value_labels: labels,
            format_string: None,
            osc_suffix,
        }
    }

    /// Convert to the existing `ParamDef` type (allocates Strings).
    pub fn to_param_def(&self) -> ParamDef {
        ParamDef {
            id: self.id.to_string(),
            name: self.name.to_string(),
            min: self.min,
            max: self.max,
            default_value: self.default_value,
            whole_numbers: self.whole_numbers,
            is_toggle: self.is_toggle,
            value_labels: if self.value_labels.is_empty() {
                None
            } else {
                Some(self.value_labels.iter().map(|s| s.to_string()).collect())
            },
            format_string: self.format_string.map(|s| s.to_string()),
            osc_suffix: if self.osc_suffix.is_empty() {
                None
            } else {
                Some(self.osc_suffix.to_string())
            },
        }
    }
}

/// Complete metadata for a generator, submitted via `inventory::submit!`.
pub struct GeneratorMetadata {
    pub id: GeneratorTypeId,
    pub display_name: &'static str,
    pub is_line_based: bool,
    pub available: bool,
    pub osc_prefix: &'static str,
    pub legacy_discriminant: Option<i32>,
    pub params: &'static [ParamSpec],
    /// String params: (name, key, default_value, use_dropdown).
    pub string_params: &'static [(&'static str, &'static str, &'static str, bool)],
}

inventory::collect!(GeneratorMetadata);

impl GeneratorMetadata {
    /// Convert to the existing `GeneratorDef` type.
    pub fn to_generator_def(&self) -> GeneratorDef {
        let param_defs: Vec<ParamDef> = self.params.iter().map(|p| p.to_param_def()).collect();
        let param_count = param_defs.len();
        let string_param_defs: Vec<StringParamDef> = self
            .string_params
            .iter()
            .map(|(name, key, default, dropdown)| StringParamDef {
                name,
                key,
                default_value: default,
                use_dropdown: *dropdown,
            })
            .collect();
        let id_to_index = self
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.id.is_empty())
            .map(|(i, p)| (p.id.to_string(), i))
            .collect();
        let param_ids: Vec<&'static str> = self.params.iter().map(|p| p.id).collect();
        GeneratorDef {
            display_name: self.display_name,
            is_line_based: self.is_line_based,
            param_count,
            param_defs,
            string_param_defs,
            osc_prefix: Some(self.osc_prefix),
            id_to_index,
            param_ids,
        }
    }

    /// Convert to the existing `GeneratorTypeRegistration` type.
    pub fn to_type_registration(&self) -> GeneratorTypeRegistration {
        GeneratorTypeRegistration {
            id: self.id.clone(),
            display_name: self.display_name,
            available: self.available,
        }
    }
}
