//! Distributed generator registration via `inventory`.
//!
//! Each generator submits a `GeneratorMetadata` from its implementation file.
//! The definition and type registries collect these at startup.

use crate::effects::RegistryParamDef;
use crate::preset_type_id::PresetTypeId;
use crate::preset_type_registry::PresetTypeRegistration;
use crate::preset_def::{PresetDef, PresetKind};

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
    pub is_trigger: bool,
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
            is_trigger: false,
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
            is_trigger: false,
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
            is_trigger: false,
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
            is_trigger: false,
            value_labels: labels,
            format_string: None,
            osc_suffix,
        }
    }

    /// Momentary "fire once" button parameter. Storage is a monotonic
    /// `u32` counter held as `f32`; each outer-card click increments by
    /// one. Consuming primitives detect rising edges (cold-start absorbs
    /// the initial value — see `node.trigger_gate`).
    pub const fn trigger(
        id: &'static str,
        name: &'static str,
        osc_suffix: &'static str,
    ) -> Self {
        Self {
            id,
            name,
            min: 0.0,
            max: f32::MAX,
            default_value: 0.0,
            whole_numbers: true,
            is_toggle: false,
            is_trigger: true,
            value_labels: &[],
            format_string: None,
            osc_suffix,
        }
    }

    /// Convert to the unified [`RegistryParamDef`] (allocates Strings).
    pub fn to_param_def(&self) -> RegistryParamDef {
        RegistryParamDef {
            spec: crate::effect_graph_def::ParamSpecDef {
                id: self.id.to_string(),
                name: self.name.to_string(),
                min: self.min,
                max: self.max,
                default_value: self.default_value,
                whole_numbers: self.whole_numbers,
                is_toggle: self.is_toggle,
                is_trigger: self.is_trigger,
                value_labels: self.value_labels.iter().map(|s| s.to_string()).collect(),
                format_string: self.format_string.map(|s| s.to_string()),
                osc_suffix: self.osc_suffix.to_string(),
                // Inventory-submitted generator params ship identity slider
                // response; preset-authored curve/invert live in the disk JSON.
                curve: crate::macro_bank::MacroCurve::Linear,
                invert: false,
                // §8 D6: this compile-time inventory struct pre-dates the
                // trigger-gate flag and carries no field for it — every
                // trigger-gate card ships via the JSON preset path
                // (`ParamSpecDef`/`preset_metadata_to_def`), not this one.
                is_trigger_gate: false,
                // Same story for D5 sections and the is_angle/wraps mirrors: an
                // inventory-submitted (hand-written Rust) generator has none —
                // only JSON-authored/glTF-imported presets do, via
                // `preset_metadata_to_def`.
                is_angle: false,
                wraps: false,
                section: None,
                card_visible: true,
            },
            // Same story for range contracts: this compile-time inventory
            // struct describes an outer card param, which never carries a
            // contract (PARAM_RANGE_CONTRACT_DESIGN.md D3/D4).
            contract: None,
        }
    }
}

/// Complete metadata for a generator, submitted via `inventory::submit!`.
pub struct GeneratorMetadata {
    pub id: PresetTypeId,
    pub display_name: &'static str,
    pub is_line_based: bool,
    pub available: bool,
    pub osc_prefix: &'static str,
    pub legacy_discriminant: Option<i32>,
    pub params: &'static [ParamSpec],
}

inventory::collect!(GeneratorMetadata);

/// Sidecar alias submission for generators. Mirrors
/// [`crate::effect_registration::EffectAliasMetadata`].
pub struct GeneratorAliasMetadata {
    pub id: PresetTypeId,
    pub aliases: &'static [crate::effect_registration::ParamAlias],
}

inventory::collect!(GeneratorAliasMetadata);

impl GeneratorMetadata {
    /// Convert to the unified `PresetDef` (kind = `Generator`).
    pub fn to_generator_def(&self) -> PresetDef {
        let param_defs: Vec<RegistryParamDef> = self.params.iter().map(|p| p.to_param_def()).collect();
        PresetDef {
            kind: PresetKind::Generator,
            display_name: self.display_name.to_string(),
            is_line_based: self.is_line_based,
            param_defs,
            // Outer-card string params are owned by the disk JSON preset
            // (`stringParams`), which overrides this inventory def in the
            // registry. The inventory path carries none.
            string_param_defs: Vec::new(),
            osc_prefix: Some(self.osc_prefix.to_string()),
            legacy_param_aliases: &[],
            legacy_value_aliases: &[],
        }
    }

    /// Convert to the unified picker registration (kind = Generator).
    pub fn to_type_registration(&self) -> PresetTypeRegistration {
        PresetTypeRegistration {
            id: self.id.clone(),
            display_name: self.display_name,
            category: None,
            kind: crate::preset_def::PresetKind::Generator,
            available: self.available,
        }
    }
}
