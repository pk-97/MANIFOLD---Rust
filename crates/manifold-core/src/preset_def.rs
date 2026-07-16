//! The unified preset definition — one type for effects and generators.
//!
//! Effects and generators do the same job: render through a node graph,
//! expose params, modulate, save/load, surface to the editor. They were
//! defined twice (legacy `EffectDef` / `GeneratorDef`). [`PresetDef`] is
//! the single shape both collapsed into, carrying the small set of real
//! differences behind a [`PresetKind`] discriminator.
//!
//! Step 7 of the unification (see `docs/PRESET_UNIFICATION_PLAN.md`):
//! both definition registries now STORE and RETURN `PresetDef`. The two
//! legacy structs and the `from_effect_def` / `from_generator_def`
//! bridges are gone — the registries build `PresetDef` directly. The
//! two registry modules and their `PresetTypeId` / `PresetTypeId`
//! keying stay separate; only the value type unified here.

use crate::effect_registration::{ParamAlias, ParamValueAlias};
use crate::effects::RegistryParamDef;
use crate::preset_definition_registry::StringParamDef;

/// Which kind of preset this is. The one word that carries every real
/// effect/generator difference — skip-mode semantics, wet/dry, OSC scheme,
/// line-based rendering — so the rest of the codebase stops forking on it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum PresetKind {
    Effect,
    Generator,
}

impl PresetKind {
    pub fn is_effect(self) -> bool {
        matches!(self, PresetKind::Effect)
    }
    pub fn is_generator(self) -> bool {
        matches!(self, PresetKind::Generator)
    }
}

/// The unified definition for a preset (effect or generator).
///
/// The union of the two legacy defs. Fields that exist on only one side
/// today carry a benign default for the other (an effect's
/// `string_param_defs` is empty, a generator's `legacy_value_aliases` is
/// empty, an effect's `is_line_based` is `false`) — those are the capability
/// gaps the unification closes, not real distinctions.
#[derive(Debug, Clone)]
pub struct PresetDef {
    pub kind: PresetKind,
    pub display_name: String,
    pub param_defs: Vec<RegistryParamDef>,
    /// Generator string params today; effects gain these as the
    /// string-binding capability gap closes. Empty when unused.
    pub string_param_defs: Vec<StringParamDef>,
    pub osc_prefix: Option<String>,
    /// Generators only render this way; `false` for effects.
    pub is_line_based: bool,
    pub legacy_param_aliases: &'static [ParamAlias],
    /// Effect slot-value migration table; effects only today, empty for
    /// generators until the capability gap closes.
    pub legacy_value_aliases:
        &'static [(&'static str, &'static [ParamValueAlias])],
}
