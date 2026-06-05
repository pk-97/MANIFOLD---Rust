//! Distributed effect registration via `inventory`.
//!
//! Each effect submits an `EffectMetadata` from its implementation file.
//! The definition and type registries collect these at startup.

use crate::effect_type_id::EffectTypeId;
use crate::effect_type_registry::EffectTypeRegistration;
use crate::effects::ParamDef;
use crate::generator_registration::ParamSpec;
use crate::preset_def::{PresetDef, PresetKind};

/// Declarative migration entry: an old `param_id` and its current
/// replacement (`Some(new_id)`) or `None` if the param was dropped.
///
/// Lives on `EffectMetadata` (and `GeneratorMetadata`) so a schema
/// change is one literal addition next to the effect's `params`
/// slice, instead of a hardcoded match arm in
/// `align_to_definition`. See `docs/EFFECT_RUNTIME_UNIFICATION.md` §7
/// step 15.
///
/// Applied by:
/// - The post-load `Project::resolve_legacy_param_ids` pass when
///   resolving driver/envelope/Ableton/macro mapping ids.
/// - `ParamValuesWire::into_positional` when deserializing V1.2+
///   id-keyed `paramValues` maps.
///
/// Renames and drops compose: an alias table can chain
/// `("old_a", Some("old_b")) → ("old_b", Some("current"))` so each
/// schema bump only needs to add the latest hop.
pub type ParamAlias = (&'static str, Option<&'static str>);

/// Resolve a (possibly stale) `param_id` through an effect or generator's
/// alias table.
///
/// - `Some(id)` if `id` is current (not in the alias table) or aliases to a
///   current id (chained through multiple hops).
/// - `None` if the alias chain ends at `None` (param was dropped) or a
///   cycle is detected.
///
/// Pure slice utility — no registry coupling. Every `PresetDef` (effect
/// or generator) carries its own `&[ParamAlias]` slice; this function
/// walks any of them.
///
/// Termination: bounded chain walk (`aliases.len() + 1` hops). Aliases
/// are static-author data; cycles indicate a developer mistake at
/// declaration time and should fail gracefully rather than infinite-loop.
pub fn resolve_param_alias<'a>(aliases: &'a [ParamAlias], id: &'a str) -> Option<&'a str> {
    let mut current = id;
    let mut hops = 0;
    while hops <= aliases.len() {
        match aliases.iter().find(|(old, _)| *old == current) {
            Some((_, Some(new))) => {
                current = new;
                hops += 1;
            }
            Some((_, None)) => return None,
            None => return Some(current),
        }
    }
    None
}

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

/// Optional sidecar submission for effects whose param list has been
/// renamed or trimmed across schema versions. Submitted via a
/// separate `inventory::submit!` block so effects with no aliases —
/// the common case — don't need to spell out an empty slice.
///
/// Discovered at registry-build time and merged into the matching
/// [`crate::preset_def::PresetDef::legacy_param_aliases`].
pub struct EffectAliasMetadata {
    pub id: EffectTypeId,
    pub aliases: &'static [ParamAlias],
}

inventory::collect!(EffectAliasMetadata);

/// One value-space migration entry for a single param: legacy slot
/// value `from` is rewritten to `to` at project load time. Used when
/// dropping a `ParamConvert::EnumRemap` curation — old projects have
/// outer-indexed values that no longer correspond to inner enum
/// indices, and they need a one-time translation on load.
///
/// Stored as `i32` because the values being migrated are always enum
/// indices in practice. `f32` slot values are coerced to `i32` for
/// the comparison; if a slot's rounded value matches `from` exactly,
/// the slot is rewritten to `to as f32`. Other values pass through
/// untouched.
pub type ParamValueAlias = (i32, i32);

/// Optional sidecar submission for effects whose **slot values** —
/// not ids, not node handles — need translation when loading
/// pre-migration project files. Companion to
/// [`EffectAliasMetadata`] (id renames). Each entry is
/// `(param_id, &[(legacy_value, current_value)])`.
///
/// Canonical use case: Mirror's `mode` param. The legacy outer slider
/// indexed `{Horiz: 0, Vert: 1, Both: 2}` and converted to inner
/// `Transform.mode` via a `ParamConvert::EnumRemap([6, 7, 8])`. After
/// we drop the curation and expose `Transform.mode`'s full 9-option
/// enum directly, the outer index *is* the inner index — but
/// projects saved at `mode = 1` still mean "Vert" semantically, so
/// the load path migrates them to `mode = 7` (FoldY). Submission:
///
/// ```ignore
/// inventory::submit! {
///     EffectValueAliasMetadata {
///         id: EffectTypeId::MIRROR,
///         aliases: &[
///             ("mode", &[(0, 6), (1, 7), (2, 8)]),
///         ],
///     }
/// }
/// ```
///
/// Discovered at registry-build time and merged into
/// [`crate::preset_def::PresetDef::legacy_value_aliases`].
/// `Project::migrate_legacy_param_values` walks each effect
/// instance's `param_values` and applies the table.
///
/// Idempotent: once a value has been migrated, the next load sees
/// the post-migration value, which doesn't match any `from` entry
/// (because `from` values are by definition pre-migration). Multiple
/// passes are safe.
pub struct EffectValueAliasMetadata {
    pub id: EffectTypeId,
    pub aliases: &'static [(&'static str, &'static [ParamValueAlias])],
}

inventory::collect!(EffectValueAliasMetadata);

impl EffectMetadata {
    /// Convert to the unified `PresetDef` (kind = `Effect`).
    pub fn to_effect_def(&self) -> PresetDef {
        let param_defs: Vec<ParamDef> = self.params.iter().map(|p| p.to_param_def()).collect();
        let param_count = param_defs.len();
        let id_to_index = self
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.id.is_empty())
            .map(|(i, p)| (p.id.to_string(), i))
            .collect();
        let param_ids: Vec<String> = self.params.iter().map(|p| p.id.to_string()).collect();
        PresetDef {
            kind: PresetKind::Effect,
            display_name: self.display_name.to_string(),
            param_count,
            param_defs,
            string_param_defs: Vec::new(),
            osc_prefix: Some(self.osc_prefix.to_string()),
            is_line_based: false,
            id_to_index,
            param_ids,
            legacy_param_aliases: &[],
            legacy_value_aliases: &[],
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
