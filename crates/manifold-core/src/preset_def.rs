//! The unified preset definition — one type for effects and generators.
//!
//! Effects and generators do the same job: render through a node graph,
//! expose params, modulate, save/load, surface to the editor. They have
//! been defined twice ([`EffectDef`](crate::effect_definition_registry::EffectDef)
//! and [`GeneratorDef`](crate::generator_definition_registry::GeneratorDef)).
//! [`PresetDef`] is the single shape both collapse into, carrying the small
//! set of real differences behind a [`PresetKind`] discriminator.
//!
//! Step 2 of the unification (see `docs/PRESET_UNIFICATION_PLAN.md`):
//! this introduces the target type only. No consumer is migrated yet — the
//! two registries still produce `EffectDef` / `GeneratorDef`. The
//! [`PresetDef::from_effect_def`] / [`PresetDef::from_generator_def`]
//! bridges let later steps fold consumers onto `PresetDef` one at a time.

use ahash::AHashMap;

use crate::effect_registration::{ParamAlias, ParamValueAlias};
use crate::effects::ParamDef;
use crate::generator_definition_registry::StringParamDef;

/// Which kind of preset this is. The one word that carries every real
/// effect/generator difference — skip-mode semantics, wet/dry, OSC scheme,
/// line-based rendering — so the rest of the codebase stops forking on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    pub param_count: usize,
    pub param_defs: Vec<ParamDef>,
    /// Generator string params today; effects gain these as the
    /// string-binding capability gap closes. Empty when unused.
    pub string_param_defs: Vec<StringParamDef>,
    pub osc_prefix: Option<String>,
    /// Generators only render this way; `false` for effects.
    pub is_line_based: bool,
    pub id_to_index: AHashMap<String, usize>,
    pub param_ids: Vec<String>,
    pub legacy_param_aliases: &'static [ParamAlias],
    /// Effect slot-value migration table; effects only today, empty for
    /// generators until the capability gap closes.
    pub legacy_value_aliases:
        &'static [(&'static str, &'static [ParamValueAlias])],
}

impl PresetDef {
    /// Bridge from the legacy effect def. Clones the owned fields; used while
    /// consumers migrate onto `PresetDef` (steps 6–7). The `'static` alias
    /// tables are shared by reference.
    pub fn from_effect_def(d: &crate::effect_definition_registry::EffectDef) -> Self {
        PresetDef {
            kind: PresetKind::Effect,
            display_name: d.display_name.to_string(),
            param_count: d.param_count,
            param_defs: d.param_defs.clone(),
            string_param_defs: Vec::new(),
            osc_prefix: d.osc_prefix.map(str::to_string),
            is_line_based: false,
            id_to_index: d.id_to_index.clone(),
            param_ids: d.param_ids.iter().map(|s| s.to_string()).collect(),
            legacy_param_aliases: d.legacy_param_aliases,
            legacy_value_aliases: d.legacy_value_aliases,
        }
    }

    /// Bridge from the legacy generator def. Generators carry no slot-value
    /// alias table yet, so `legacy_value_aliases` is empty.
    pub fn from_generator_def(
        d: &crate::generator_definition_registry::GeneratorDef,
    ) -> Self {
        PresetDef {
            kind: PresetKind::Generator,
            display_name: d.display_name.to_string(),
            param_count: d.param_count,
            param_defs: d.param_defs.clone(),
            string_param_defs: d.string_param_defs.clone(),
            osc_prefix: d.osc_prefix.map(str::to_string),
            is_line_based: d.is_line_based,
            id_to_index: d.id_to_index.clone(),
            param_ids: d.param_ids.iter().map(|s| s.to_string()).collect(),
            legacy_param_aliases: d.legacy_param_aliases,
            legacy_value_aliases: &[],
        }
    }

    /// Resolve a param id to its storage index, honoring the legacy alias
    /// table — the same contract both legacy defs expose.
    pub fn index_for_param(&self, id: &str) -> Option<usize> {
        if let Some(&i) = self.id_to_index.get(id) {
            return Some(i);
        }
        let resolved =
            crate::effect_registration::resolve_param_alias(self.legacy_param_aliases, id)?;
        self.id_to_index.get(resolved).copied()
    }
}
