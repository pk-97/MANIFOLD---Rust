//! Distributed effect registration via `inventory`.
//!
//! Each effect submits an `EffectMetadata` from its implementation file.
//! The definition and type registries collect these at startup.

use crate::effect_definition_registry::EffectDef;
use crate::effect_type_id::EffectTypeId;
use crate::effect_type_registry::EffectTypeRegistration;
use crate::effects::ParamDef;
use crate::generator_registration::ParamSpec;

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
/// Pure slice utility — no registry coupling. Both `EffectDef` and
/// `GeneratorDef` carry their own `&[ParamAlias]` slice; this function
/// walks either.
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
/// [`EffectDef::legacy_param_aliases`].
pub struct EffectAliasMetadata {
    pub id: EffectTypeId,
    pub aliases: &'static [ParamAlias],
}

inventory::collect!(EffectAliasMetadata);

/// Optional sidecar submission for effects whose **inner-graph node
/// handles** have been renamed or removed across schema versions.
/// Direct analogue of [`EffectAliasMetadata`]: parallel inventory,
/// parallel slice shape, parallel resolver via [`resolve_param_alias`].
///
/// User-exposed parameter bindings address inner nodes by stable
/// `node_handle` (set via `Graph::add_node_named` at the effect's
/// construction). When an effect refactor renames a node — say
/// `"feedback"` → `"feedback_a"` — the matching `EffectNodeAliasMetadata`
/// entry lets saved projects recover their bindings:
///
/// ```ignore
/// inventory::submit! {
///     EffectNodeAliasMetadata {
///         id: EffectTypeId::STYLIZED_FEEDBACK,
///         aliases: &[("feedback", Some("feedback_a"))],
///     }
/// }
/// ```
///
/// Discovered at registry-build time and merged into the matching
/// [`EffectDef::legacy_node_aliases`]. The resolver in
/// `Project::resolve_legacy_param_ids` walks every user binding's
/// `node_handle` through this table.
pub struct EffectNodeAliasMetadata {
    pub id: EffectTypeId,
    pub aliases: &'static [ParamAlias],
}

inventory::collect!(EffectNodeAliasMetadata);

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
        let param_ids: Vec<&'static str> = self.params.iter().map(|p| p.id).collect();
        EffectDef {
            display_name: self.display_name,
            param_count,
            param_defs,
            osc_prefix: Some(self.osc_prefix),
            id_to_index,
            param_ids,
            legacy_param_aliases: &[],
            legacy_node_aliases: &[],
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
