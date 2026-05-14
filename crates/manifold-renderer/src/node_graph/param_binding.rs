//! Host-visible parameter bindings — the surface between an effect's
//! UI sliders / OSC paths / Ableton macros and the inner graph nodes
//! that actually consume the values.
//!
//! See `docs/EFFECT_RUNTIME_UNIFICATION.md` §7 for the full design.
//!
//! ## Three layers of identity
//!
//! 1. [`ParamId`] — stable string forever once shipped. External mappings
//!    (OSC, Ableton, MIDI, modulation drivers) key on this. Renaming the
//!    label or reorganizing the underlying graph never invalidates a
//!    `ParamId`.
//! 2. `spec.name` — display label on the slider. Free to edit.
//! 3. `target` — current routing path to a graph node parameter. May
//!    change as the effect's internals are decomposed or refactored.
//!
//! ## Why `Cow<'static, str>`
//!
//! Static strings (V1: developer-defined effects compiled in) and owned
//! strings (V2: user-exposed parameters generated at runtime) flow
//! through the same code paths. `Cow::Borrowed` for compile-time IDs,
//! `Cow::Owned` for user-generated. Same trick `EffectTypeId` uses.
//!
//! ## What this module is *not*
//!
//! No callers yet. Step 5 of Phase 2 in the migration plan: define the
//! types and conversion semantics so the rest of Phase 2 has a stable
//! target shape to build toward. Effects, drivers, Ableton mappings,
//! and project-file serialization migrations come in subsequent steps.

use std::borrow::Cow;

use manifold_core::effects::ParamSlot;
use manifold_core::generator_registration::ParamSpec;

use crate::node_graph::composites::CompositeHandle;
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::validation::GraphError;

// ParamId now lives in `manifold_core::effects` so the data-model layer
// (ParameterDriver, ParamEnvelope, AbletonParamMapping) can use the
// same type. Re-exported here for renderer-internal call sites.
pub use manifold_core::effects::ParamId;

/// Maps a single host-visible parameter to a graph-side route.
///
/// Lives at the boundary between the host's `Vec<f32>` parameter
/// storage and the graph's typed `ParamValue` parameter map. Each
/// binding consumes one f32 from the host's `param_values` and routes
/// the converted value to its `target`.
#[derive(Debug, Clone)]
pub struct ParamBinding {
    /// Stable identity. Forever rule: never rename, never reuse.
    pub id: ParamId,
    /// UI metadata — slider label, range, default, format string,
    /// enum labels. `spec.name` is the editable display string;
    /// `id` is the stable mapping key.
    pub spec: ParamSpec,
    /// Where this parameter's value flows in the graph.
    pub target: ParamTarget,
    /// Conversion from f32 (UI/storage form) to the typed
    /// [`ParamValue`] the graph node expects.
    pub convert: ParamConvert,
}

/// Routing destination for a [`ParamBinding`].
#[derive(Debug, Clone)]
pub enum ParamTarget {
    /// Routed through a [`CompositeHandle`]'s exposed-param map.
    /// Used by composite-shaped effects (Mirror, SoftFocus, Bloom)
    /// where one outer name resolves to one or more inner-node
    /// parameters via the handle.
    Composite { outer_name: Cow<'static, str> },
    /// Direct route to a single node parameter. Used by
    /// single-primitive effects (StylizedFeedback wraps just
    /// `Feedback`) or any case that doesn't need composite-level
    /// indirection. `node` is captured at effect construction; `param`
    /// is the inner node's compile-time parameter name.
    Node {
        node: NodeInstanceId,
        param: &'static str,
    },
    /// Escape hatch for routing that's neither composite nor a single
    /// node. Function pointer (no captures); for closures, build a
    /// tiny helper struct that implements [`PartialEq`] and route via
    /// `Composite`. Rare in practice.
    Custom(fn(&mut Graph, f32)),
}

/// Conversion from f32 (UI / storage form) to typed [`ParamValue`].
///
/// Each variant handles one common shape. Adding a new conversion is
/// one variant + one `match` arm in [`ParamConvert::convert`]. Vec2 /
/// Color / multi-input gathers are deferred — they'd require a
/// different signature (read multiple slots from the values array).
/// The five variants here cover every existing effect.
#[derive(Debug, Clone)]
pub enum ParamConvert {
    /// 1:1 — `value` becomes `ParamValue::Float(value)`. The default
    /// for any continuous parameter.
    Float,
    /// `value.round() as i32` → `ParamValue::Int`.
    IntRound,
    /// `value > 0.5` → `ParamValue::Bool`.
    BoolThreshold,
    /// `value.round().max(0.0) as u32` → `ParamValue::Enum`. The
    /// straightforward "host enum index = graph enum value" case.
    EnumRound,
    /// Enum remap: index the legacy enum into a static table to get
    /// the new enum value. Used by Mirror, where the legacy mode
    /// (0=Horiz / 1=Vert / 2=Both) maps to the new Transform mode
    /// (6=FoldX / 7=FoldY / 8=FoldBoth) — the host slider keeps its
    /// 0/1/2 surface, the graph node receives 6/7/8.
    ///
    /// Out-of-range inputs clamp to the table's last entry rather
    /// than panicking — the host might emit values briefly outside
    /// the declared range during a drag.
    EnumRemap(Cow<'static, [u32]>),
}

impl ParamConvert {
    /// Convert one f32 to a `ParamValue` per this variant's rules.
    pub fn convert(&self, value: f32) -> ParamValue {
        match self {
            Self::Float => ParamValue::Float(value),
            Self::IntRound => ParamValue::Int(value.round() as i32),
            Self::BoolThreshold => ParamValue::Bool(value > 0.5),
            Self::EnumRound => {
                let v = value.round().max(0.0) as u32;
                ParamValue::Enum(v)
            }
            Self::EnumRemap(table) => {
                if table.is_empty() {
                    return ParamValue::Enum(0);
                }
                let idx = value.round().max(0.0) as usize;
                let mapped = table.get(idx).copied().unwrap_or_else(|| {
                    // Out-of-range: clamp to the last entry. unwrap_or
                    // is safe — `table.is_empty()` was checked above.
                    table[table.len() - 1]
                });
                ParamValue::Enum(mapped)
            }
        }
    }
}

impl ParamBinding {
    /// Apply this binding's value to the graph.
    ///
    /// `handle` is required iff `target` is [`ParamTarget::Composite`].
    /// Passing `None` for a `Composite` target panics — the caller is
    /// expected to know whether their effect uses composite routing.
    pub fn apply(
        &self,
        graph: &mut Graph,
        handle: Option<&CompositeHandle>,
        value: f32,
    ) -> Result<(), GraphError> {
        let pv = self.convert.convert(value);
        match &self.target {
            ParamTarget::Composite { outer_name } => handle
                .expect("ParamTarget::Composite requires a CompositeHandle")
                .set_param(graph, outer_name, pv),
            ParamTarget::Node { node, param } => graph.set_param(*node, param, pv),
            ParamTarget::Custom(f) => {
                f(graph, value);
                Ok(())
            }
        }
    }
}

/// Per-instance user-exposed binding in its renderer-runtime form.
///
/// Sibling to [`ParamBinding`]: same role at apply time, different
/// identity story. Static bindings carry compile-time
/// [`ParamSpec`]s (everything `&'static str`); user bindings come
/// from the project file with owned [`String`]s in their
/// [`manifold_core::effects::UserParamBinding`] form, and we
/// hydrate them into this runtime shape once whenever
/// `EffectInstance.user_param_bindings_version` changes.
///
/// Targeting is always [`ParamTarget::Node`] — user bindings can
/// only address a single inner-node parameter (composites and custom
/// fns are static-only).
#[derive(Debug, Clone)]
pub struct UserParamBindingRuntime {
    /// Stable identity. Same ParamId namespace as static bindings;
    /// always `Cow::Owned(String)` here because the source data is
    /// owned. Drivers / Ableton / OSC reference this string.
    pub id: ParamId,
    /// Resolved inner-node id (looked up from the effect's graph
    /// at hydration time via `graph.node_id_by_handle(node_handle)`).
    pub target_node: NodeInstanceId,
    /// Compile-time inner-node parameter name (looked up from
    /// `node.parameters()` at hydration time).
    pub target_param: &'static str,
    pub convert: ParamConvert,
    pub default_value: f32,
}

impl UserParamBindingRuntime {
    pub fn apply(&self, graph: &mut Graph, value: f32) -> Result<(), GraphError> {
        let pv = self.convert.convert(value);
        graph.set_param(self.target_node, self.target_param, pv)
    }
}

/// Hydrate a core-side [`manifold_core::effects::UserParamBinding`]
/// into the renderer-runtime [`UserParamBindingRuntime`] form.
///
/// Returns `None` if the binding's `node_handle` is not registered
/// on the graph (effect refactor dropped the node, or alias resolver
/// didn't catch the rename) or the binding's `inner_param` doesn't
/// match any param on the resolved node. Caller logs and skips —
/// orphan bindings remain in the project file but render inert until
/// they re-bind.
pub fn user_binding_to_runtime(
    core: &manifold_core::effects::UserParamBinding,
    graph: &Graph,
) -> Option<UserParamBindingRuntime> {
    let target_node = graph.node_id_by_handle(&core.node_handle)?;
    let inst = graph.get_node(target_node)?;
    // Find the &'static str on the inner node's ParamDef list that
    // matches `core.inner_param`. We pull the &'static str out of
    // the node's own param list rather than leaking a String —
    // user-exposable params are always declared on shipping nodes,
    // so a matching &'static will always be available when the
    // resolution succeeds.
    let target_param = inst
        .node
        .parameters()
        .iter()
        .map(|p| p.name)
        .find(|name| *name == core.inner_param.as_str())?;
    let convert = match core.convert {
        manifold_core::effects::UserParamConvert::Float => ParamConvert::Float,
        manifold_core::effects::UserParamConvert::IntRound => ParamConvert::IntRound,
        manifold_core::effects::UserParamConvert::BoolThreshold => ParamConvert::BoolThreshold,
        manifold_core::effects::UserParamConvert::EnumRound => ParamConvert::EnumRound,
    };
    Some(UserParamBindingRuntime {
        id: Cow::Owned(core.id.clone()),
        target_node,
        target_param,
        convert,
        default_value: core.default_value,
    })
}

/// Apply static + user bindings against a values slice.
///
/// Static bindings consume `values[0..static_bindings.len()]`. User
/// bindings consume `values[static_bindings.len()..]`. If `values`
/// is shorter than the combined binding count, missing slots fall
/// back to the binding's own default.
///
/// **Per-binding failures are logged, not fatal.** The bindings are
/// built (or hydrated) once at effect construction / version-bump
/// and target a graph that's owned by the same effect — a routing
/// error means the graph has been mutated out from under the binding
/// (target node deleted, param renamed, etc.). That's a developer
/// bug, but it MUST NOT panic the content thread mid-frame: the host
/// runs at production FPS for live performance, and a panic = channel
/// disconnect = entire pipeline stops. Log loudly, skip the broken
/// binding, keep going.
///
/// This is the per-frame routing shim that migrated effects call from
/// their `apply()` implementations.
pub fn apply_param_bindings(
    static_bindings: &[ParamBinding],
    user_bindings: &[UserParamBindingRuntime],
    graph: &mut Graph,
    handle: Option<&CompositeHandle>,
    values: &[ParamSlot],
    last_applied: &mut LastAppliedCache,
) {
    last_applied
        .static_outer
        .resize(static_bindings.len(), BindingCacheEntry::Unset);
    last_applied
        .user_outer
        .resize(user_bindings.len(), BindingCacheEntry::Unset);

    for (i, binding) in static_bindings.iter().enumerate() {
        let value = values
            .get(i)
            .map(|p| p.value)
            .unwrap_or(binding.spec.default_value);
        match last_applied.static_outer[i] {
            BindingCacheEntry::SkipOnce => {
                // Post-hydrate path: the def installed an inner value
                // we must not overwrite. Just record the current outer
                // so next frame's compare-and-skip resumes normally.
                last_applied.static_outer[i] = BindingCacheEntry::Applied(value);
                continue;
            }
            BindingCacheEntry::Applied(prev) if prev == value => {
                continue;
            }
            BindingCacheEntry::Unset | BindingCacheEntry::Applied(_) => {}
        }
        if let Err(err) = binding.apply(graph, handle, value) {
            eprintln!(
                "[manifold-renderer] ParamBinding apply failed: id={} value={} err={:?} — \
                 skipping this binding for the current frame. The graph topology likely \
                 changed without rebuilding the bindings list.",
                binding.id, value, err,
            );
            continue;
        }
        last_applied.static_outer[i] = BindingCacheEntry::Applied(value);
    }
    let n = static_bindings.len();
    for (j, binding) in user_bindings.iter().enumerate() {
        let value = values
            .get(n + j)
            .map(|p| p.value)
            .unwrap_or(binding.default_value);
        match last_applied.user_outer[j] {
            BindingCacheEntry::SkipOnce => {
                last_applied.user_outer[j] = BindingCacheEntry::Applied(value);
                continue;
            }
            BindingCacheEntry::Applied(prev) if prev == value => {
                continue;
            }
            BindingCacheEntry::Unset | BindingCacheEntry::Applied(_) => {}
        }
        if let Err(err) = binding.apply(graph, value) {
            eprintln!(
                "[manifold-renderer] UserParamBinding apply failed: id={} value={} err={:?} — \
                 skipping this user binding for the current frame.",
                binding.id, value, err,
            );
            continue;
        }
        last_applied.user_outer[j] = BindingCacheEntry::Applied(value);
    }
}

/// Per-effect cache of "last outer value applied" parallel to a
/// binding list. Lives on the effect instance (not on the bindings)
/// because `ParamBinding` is `Clone`able / sharable between catalog
/// constructors and tests — adding mutable cache state would force
/// every caller to wrap it in an interior-mutability cell.
///
/// **Why this exists.** The binding apply path runs every frame to
/// power drivers / envelopes / Ableton mappings. Without skip-on-
/// unchanged, that turns inner-node param edits into a 60Hz tug-of-
/// war the outer always wins. With this cache, the binding writes
/// only when the outer slot's value actually changes — inner edits
/// survive when the outer is at rest, and the outer reclaims control
/// the moment it moves.
///
/// Both lists auto-grow on first `apply_param_bindings`; constructors
/// can leave them empty.
#[derive(Debug, Clone, Default)]
pub struct LastAppliedCache {
    pub static_outer: Vec<BindingCacheEntry>,
    pub user_outer: Vec<BindingCacheEntry>,
}

/// One entry in [`LastAppliedCache`]. Tagged because the *first*
/// apply after construction needs different semantics depending on
/// whether the effect was just hydrated from a per-card def:
///
/// - **`Unset`** — never applied. The catalog/default state for a
///   binding: next apply writes outer → inner, then transitions to
///   `Applied(value)`. This is what lets a freshly-constructed
///   effect initialize the inner from the outer slot.
/// - **`SkipOnce`** — set by `apply_graph_def` for params that the
///   per-card def overrode. Next apply does NOT write, just records
///   the current outer value and transitions to `Applied`. This is
///   what makes the hydrated value survive the first frame after a
///   chain rebuild.
/// - **`Applied(v)`** — last value written. Subsequent applies skip
///   when this frame's outer equals `v` and write otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum BindingCacheEntry {
    #[default]
    Unset,
    SkipOnce,
    Applied(f32),
}

impl LastAppliedCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset every tracked outer value so the next apply behaves as
    /// if no value has ever been written. Called after a topology
    /// change that may have moved bindings around (e.g.
    /// [`MirrorFX::apply_graph_def`] rebuilds the bindings list).
    pub fn clear(&mut self) {
        self.static_outer.clear();
        self.user_outer.clear();
    }

    /// Mark a static-binding entry as "skip the next apply, then
    /// behave normally." Used by `apply_graph_def` impls right after
    /// they write a per-card def value into the inner graph — the
    /// next `apply_param_bindings` must not overwrite it, but every
    /// apply after that should resume the normal skip-on-unchanged
    /// behavior.
    ///
    /// Auto-grows the underlying vec to `index + 1` if needed; safe
    /// to call before the first `apply_param_bindings` resizes it.
    pub fn mark_skip_once(&mut self, index: usize) {
        if index >= self.static_outer.len() {
            self.static_outer
                .resize(index + 1, BindingCacheEntry::Unset);
        }
        self.static_outer[index] = BindingCacheEntry::SkipOnce;
    }
}

/// Read a host-visible parameter's current value by stable id,
/// scanning both static and user binding lists. O(n) over the
/// slices — n is typically <10, so the scan is faster than an
/// `AHashMap` lookup at this scale and avoids per-effect allocation.
///
/// Used by effects that need to inspect a param value outside the
/// normal `apply_param_bindings` flow (e.g. `should_skip` predicates).
/// Returns `None` if the id matches nothing or the values slice is
/// shorter than the resolved index.
pub fn binding_value(
    static_bindings: &[ParamBinding],
    user_bindings: &[UserParamBindingRuntime],
    values: &[ParamSlot],
    id: &str,
) -> Option<f32> {
    if let Some(idx) = static_bindings.iter().position(|b| b.id == id) {
        return values.get(idx).map(|p| p.value);
    }
    let n = static_bindings.len();
    if let Some(j) = user_bindings.iter().position(|b| b.id == id) {
        return values.get(n + j).map(|p| p.value);
    }
    None
}

/// Walk a list of static bindings, a composite handle, and the live
/// graph to produce the editor-facing list of
/// [`OuterParamRouting`](crate::node_graph::OuterParamRouting) entries
/// — one per outer slider whose value gets written into an inner-
/// node param every frame.
///
/// Both routing styles are recognized:
/// - [`ParamTarget::Composite`] — resolved via
///   [`CompositeHandle::inner_routing_for`].
/// - [`ParamTarget::Node`] — `(node_id, param)` is taken directly off
///   the binding.
///
/// [`ParamTarget::Custom`] is skipped; it has no introspectable
/// destination, so the editor can't surface it.
pub fn outer_routings_from_bindings(
    bindings: &[ParamBinding],
    handle: Option<&crate::node_graph::composites::CompositeHandle>,
    graph: &Graph,
) -> Vec<crate::node_graph::OuterParamRouting> {
    use crate::node_graph::OuterParamRouting;
    let id_to_handle: ahash::AHashMap<u32, String> = graph
        .handles()
        .map(|(h, id)| (id.0, h.to_string()))
        .collect();
    let mut out = Vec::with_capacity(bindings.len());
    for b in bindings {
        let (node_id, inner_param) = match &b.target {
            ParamTarget::Composite { outer_name } => {
                let Some(h) = handle else {
                    // Composite-target binding on an effect that
                    // doesn't expose a `CompositeHandle` — can't
                    // resolve. Skip rather than guess.
                    continue;
                };
                let Some(route) = h.inner_routing_for(outer_name.as_ref()) else {
                    continue;
                };
                route
            }
            ParamTarget::Node { node, param } => (*node, *param),
            ParamTarget::Custom(_) => continue,
        };
        let Some(handle_str) = id_to_handle.get(&node_id.0) else {
            // Inner node has no stable handle — without it the editor
            // can't match it to a snapshot row, so the routing is
            // un-surfaceable. Skip silently.
            continue;
        };
        out.push(OuterParamRouting {
            outer_label: b.spec.name.to_string(),
            node_handle: handle_str.clone(),
            inner_param: inner_param.to_string(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::boundary_nodes::Source;
    use crate::node_graph::primitives::{FEEDBACK_TYPE_ID, Feedback};

    // ---- Conversion tests ----

    #[test]
    fn float_passes_through_unchanged() {
        assert_eq!(ParamConvert::Float.convert(0.0), ParamValue::Float(0.0));
        assert_eq!(ParamConvert::Float.convert(0.5), ParamValue::Float(0.5));
        assert_eq!(ParamConvert::Float.convert(-1.5), ParamValue::Float(-1.5));
        assert_eq!(ParamConvert::Float.convert(42.0), ParamValue::Float(42.0));
    }

    #[test]
    fn int_round_uses_banker_rounding() {
        // f32::round is half-away-from-zero, NOT banker's rounding.
        // Document the actual behavior so it's expected.
        assert_eq!(ParamConvert::IntRound.convert(0.0), ParamValue::Int(0));
        assert_eq!(ParamConvert::IntRound.convert(0.4), ParamValue::Int(0));
        assert_eq!(ParamConvert::IntRound.convert(0.5), ParamValue::Int(1));
        assert_eq!(ParamConvert::IntRound.convert(0.6), ParamValue::Int(1));
        assert_eq!(ParamConvert::IntRound.convert(-0.5), ParamValue::Int(-1));
        assert_eq!(ParamConvert::IntRound.convert(2.5), ParamValue::Int(3));
    }

    #[test]
    fn bool_threshold_at_half() {
        assert_eq!(
            ParamConvert::BoolThreshold.convert(0.0),
            ParamValue::Bool(false)
        );
        assert_eq!(
            ParamConvert::BoolThreshold.convert(0.5),
            ParamValue::Bool(false)
        );
        assert_eq!(
            ParamConvert::BoolThreshold.convert(0.5001),
            ParamValue::Bool(true)
        );
        assert_eq!(
            ParamConvert::BoolThreshold.convert(1.0),
            ParamValue::Bool(true)
        );
        assert_eq!(
            ParamConvert::BoolThreshold.convert(-0.5),
            ParamValue::Bool(false)
        );
    }

    #[test]
    fn enum_round_clamps_negatives_to_zero() {
        assert_eq!(ParamConvert::EnumRound.convert(0.0), ParamValue::Enum(0));
        assert_eq!(ParamConvert::EnumRound.convert(1.4), ParamValue::Enum(1));
        assert_eq!(ParamConvert::EnumRound.convert(1.6), ParamValue::Enum(2));
        assert_eq!(ParamConvert::EnumRound.convert(-1.0), ParamValue::Enum(0));
        assert_eq!(ParamConvert::EnumRound.convert(-0.4), ParamValue::Enum(0));
    }

    #[test]
    fn enum_remap_looks_up_static_table() {
        // Mirror's case: legacy 0/1/2 → graph FoldX(6) / FoldY(7) / FoldBoth(8)
        let convert = ParamConvert::EnumRemap(Cow::Borrowed(&[6, 7, 8]));
        assert_eq!(convert.convert(0.0), ParamValue::Enum(6));
        assert_eq!(convert.convert(1.0), ParamValue::Enum(7));
        assert_eq!(convert.convert(2.0), ParamValue::Enum(8));
    }

    #[test]
    fn enum_remap_clamps_out_of_range_to_last() {
        let convert = ParamConvert::EnumRemap(Cow::Borrowed(&[6, 7, 8]));
        assert_eq!(convert.convert(3.0), ParamValue::Enum(8));
        assert_eq!(convert.convert(99.0), ParamValue::Enum(8));
        assert_eq!(convert.convert(-1.0), ParamValue::Enum(6)); // negatives → idx 0
    }

    #[test]
    fn enum_remap_empty_table_returns_zero() {
        let convert = ParamConvert::EnumRemap(Cow::Borrowed(&[]));
        // Defensive — would only happen via a buggy effect declaration,
        // but better than panicking.
        assert_eq!(convert.convert(0.0), ParamValue::Enum(0));
        assert_eq!(convert.convert(5.0), ParamValue::Enum(0));
    }

    // ---- apply() routing tests ----

    fn feedback_amount_binding(node: NodeInstanceId) -> ParamBinding {
        ParamBinding {
            id: Cow::Borrowed("amount"),
            spec: ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.5, "F2", ""),
            target: ParamTarget::Node {
                node,
                param: "amount",
            },
            convert: ParamConvert::Float,
        }
    }

    #[test]
    fn apply_node_target_writes_param_to_graph() {
        let mut g = Graph::new();
        let _src = g.add_node(Box::new(Source::new()));
        let feedback = g.add_node(Box::new(Feedback::new()));
        let binding = feedback_amount_binding(feedback);

        binding.apply(&mut g, None, 0.75).unwrap();
        let inst = g.get_node(feedback).unwrap();
        assert_eq!(inst.params.get("amount"), Some(&ParamValue::Float(0.75)));
    }

    #[test]
    fn apply_node_target_doesnt_need_handle() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(Feedback::new()));
        let binding = feedback_amount_binding(feedback);
        // None handle should be fine for Node target.
        assert!(binding.apply(&mut g, None, 0.5).is_ok());
    }

    #[test]
    fn apply_param_bindings_iterates_with_default_fallback() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(Feedback::new()));
        let bindings = vec![
            feedback_amount_binding(feedback),
            ParamBinding {
                id: Cow::Borrowed("zoom"),
                spec: ParamSpec::continuous("zoom", "Zoom", 0.9, 1.1, 0.95, "F2", "Zoom"),
                target: ParamTarget::Node {
                    node: feedback,
                    param: "zoom",
                },
                convert: ParamConvert::Float,
            },
        ];

        // Provide only one value — second falls back to spec default 0.95.
        apply_param_bindings(
            &bindings,
            &[],
            &mut g,
            None,
            &[ParamSlot::exposed(0.5)],
            &mut LastAppliedCache::new(),
        );
        let inst = g.get_node(feedback).unwrap();
        assert_eq!(inst.params.get("amount"), Some(&ParamValue::Float(0.5)));
        assert_eq!(inst.params.get("zoom"), Some(&ParamValue::Float(0.95)));
    }

    #[test]
    fn apply_param_bindings_routes_user_bindings_after_static() {
        let mut g = Graph::new();
        let feedback = g.add_node_named("feedback", Box::new(Feedback::new()));
        let static_bindings = vec![feedback_amount_binding(feedback)];
        // Hydrate a user binding via user_binding_to_runtime.
        let core_ub = manifold_core::effects::UserParamBinding {
            id: "user.feedback.zoom.1".to_string(),
            label: "User Zoom".to_string(),
            node_handle: "feedback".to_string(),
            inner_param: "zoom".to_string(),
            min: 0.9,
            max: 1.1,
            default_value: 0.95,
            convert: manifold_core::effects::UserParamConvert::Float,
        };
        let user_runtime = user_binding_to_runtime(&core_ub, &g).expect("hydrate succeeds");
        // values slice: [static.amount, user.zoom] = [0.5, 1.05]
        apply_param_bindings(
            &static_bindings,
            std::slice::from_ref(&user_runtime),
            &mut g,
            None,
            &[ParamSlot::exposed(0.5), ParamSlot::exposed(1.05)],
            &mut LastAppliedCache::new(),
        );
        let inst = g.get_node(feedback).unwrap();
        assert_eq!(inst.params.get("amount"), Some(&ParamValue::Float(0.5)));
        assert_eq!(inst.params.get("zoom"), Some(&ParamValue::Float(1.05)));
    }

    #[test]
    fn apply_param_bindings_user_default_fallback_when_values_short() {
        let mut g = Graph::new();
        let feedback = g.add_node_named("feedback", Box::new(Feedback::new()));
        let static_bindings = vec![feedback_amount_binding(feedback)];
        let core_ub = manifold_core::effects::UserParamBinding {
            id: "user.feedback.zoom.1".to_string(),
            label: "User Zoom".to_string(),
            node_handle: "feedback".to_string(),
            inner_param: "zoom".to_string(),
            min: 0.9,
            max: 1.1,
            default_value: 0.97,
            convert: manifold_core::effects::UserParamConvert::Float,
        };
        let user_runtime = user_binding_to_runtime(&core_ub, &g).unwrap();
        // values shorter than static + user: user falls back to default_value.
        apply_param_bindings(
            &static_bindings,
            std::slice::from_ref(&user_runtime),
            &mut g,
            None,
            &[ParamSlot::exposed(0.5)],
            &mut LastAppliedCache::new(),
        );
        let inst = g.get_node(feedback).unwrap();
        assert_eq!(inst.params.get("zoom"), Some(&ParamValue::Float(0.97)));
    }

    #[test]
    fn user_binding_to_runtime_returns_none_for_unknown_handle() {
        let mut g = Graph::new();
        let _feedback = g.add_node_named("feedback", Box::new(Feedback::new()));
        let core_ub = manifold_core::effects::UserParamBinding {
            id: "user.nope.zoom.1".to_string(),
            label: "Nope".to_string(),
            node_handle: "no_such_node".to_string(),
            inner_param: "zoom".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.5,
            convert: manifold_core::effects::UserParamConvert::Float,
        };
        assert!(user_binding_to_runtime(&core_ub, &g).is_none());
    }

    #[test]
    fn user_binding_to_runtime_returns_none_for_unknown_inner_param() {
        let mut g = Graph::new();
        let _feedback = g.add_node_named("feedback", Box::new(Feedback::new()));
        let core_ub = manifold_core::effects::UserParamBinding {
            id: "user.feedback.bogus.1".to_string(),
            label: "Bogus".to_string(),
            node_handle: "feedback".to_string(),
            inner_param: "bogus_param".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.5,
            convert: manifold_core::effects::UserParamConvert::Float,
        };
        assert!(user_binding_to_runtime(&core_ub, &g).is_none());
    }

    #[test]
    fn binding_value_finds_user_binding_id() {
        let mut g = Graph::new();
        let feedback = g.add_node_named("feedback", Box::new(Feedback::new()));
        let static_bindings = vec![feedback_amount_binding(feedback)];
        let core_ub = manifold_core::effects::UserParamBinding {
            id: "user.feedback.zoom.1".to_string(),
            label: "User Zoom".to_string(),
            node_handle: "feedback".to_string(),
            inner_param: "zoom".to_string(),
            min: 0.9,
            max: 1.1,
            default_value: 0.95,
            convert: manifold_core::effects::UserParamConvert::Float,
        };
        let user_runtime = user_binding_to_runtime(&core_ub, &g).unwrap();
        let user_slice = std::slice::from_ref(&user_runtime);
        let values = [ParamSlot::exposed(0.5), ParamSlot::exposed(1.07)];
        assert_eq!(
            binding_value(&static_bindings, user_slice, &values, "amount"),
            Some(0.5)
        );
        assert_eq!(
            binding_value(
                &static_bindings,
                user_slice,
                &values,
                "user.feedback.zoom.1"
            ),
            Some(1.07)
        );
        assert_eq!(
            binding_value(&static_bindings, user_slice, &values, "nope"),
            None
        );
    }

    /// First apply writes; second apply with the same outer value
    /// skips. Validates the per-frame no-op invariant that lets inner
    /// edits survive against an unchanging outer routing.
    #[test]
    fn apply_param_bindings_skips_when_outer_value_unchanged() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(Feedback::new()));
        let bindings = vec![feedback_amount_binding(feedback)];
        let values = [ParamSlot::exposed(0.5)];
        let mut cache = LastAppliedCache::new();

        // 1st apply: write should land — amount goes from 0.0 default
        // → 0.5.
        apply_param_bindings(&bindings, &[], &mut g, None, &values, &mut cache);
        assert_eq!(
            g.get_node(feedback).unwrap().params.get("amount"),
            Some(&ParamValue::Float(0.5)),
        );
        assert_eq!(cache.static_outer[0], BindingCacheEntry::Applied(0.5));

        // Simulate the inspector editing the inner directly while
        // the outer slot is at rest.
        g.set_param(feedback, "amount", ParamValue::Float(0.9))
            .unwrap();

        // 2nd apply with the same outer value: skip — inner edit
        // must survive.
        apply_param_bindings(&bindings, &[], &mut g, None, &values, &mut cache);
        assert_eq!(
            g.get_node(feedback).unwrap().params.get("amount"),
            Some(&ParamValue::Float(0.9)),
            "skip-on-unchanged must not overwrite the inner edit",
        );
    }

    /// When the outer slot changes (drag, envelope, driver), the
    /// binding writes again and overwrites any inner edit. Confirms
    /// the outer reclaims control as soon as it moves.
    #[test]
    fn apply_param_bindings_writes_when_outer_value_changes() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(Feedback::new()));
        let bindings = vec![feedback_amount_binding(feedback)];
        let mut cache = LastAppliedCache::new();

        apply_param_bindings(
            &bindings,
            &[],
            &mut g,
            None,
            &[ParamSlot::exposed(0.5)],
            &mut cache,
        );
        // Inner edit.
        g.set_param(feedback, "amount", ParamValue::Float(0.9))
            .unwrap();
        // Outer slot moves: 0.5 → 0.25. Binding writes.
        apply_param_bindings(
            &bindings,
            &[],
            &mut g,
            None,
            &[ParamSlot::exposed(0.25)],
            &mut cache,
        );
        assert_eq!(
            g.get_node(feedback).unwrap().params.get("amount"),
            Some(&ParamValue::Float(0.25)),
            "outer change must overwrite the inner edit",
        );
        assert_eq!(cache.static_outer[0], BindingCacheEntry::Applied(0.25));
    }

    /// Per-frame outer animation (envelope / driver) writes every
    /// frame the value advances. Confirms the cache doesn't trap
    /// active automation.
    #[test]
    fn apply_param_bindings_keeps_writing_under_continuous_automation() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(Feedback::new()));
        let bindings = vec![feedback_amount_binding(feedback)];
        let mut cache = LastAppliedCache::new();

        for (i, v) in [0.10_f32, 0.20, 0.30, 0.40].iter().enumerate() {
            apply_param_bindings(
                &bindings,
                &[],
                &mut g,
                None,
                &[ParamSlot::exposed(*v)],
                &mut cache,
            );
            assert_eq!(
                g.get_node(feedback).unwrap().params.get("amount"),
                Some(&ParamValue::Float(*v)),
                "frame {i}: animated outer must overwrite inner",
            );
        }
    }

    /// `mark_skip_once` + first apply: the value the def installed
    /// is preserved on the next frame even when the outer slot
    /// differs from the value that would otherwise be written.
    /// Mirrors the chain-rebuild-after-edit case end-to-end.
    #[test]
    fn skip_once_preserves_hydrated_inner_against_outer_default() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(Feedback::new()));
        let bindings = vec![feedback_amount_binding(feedback)];
        let mut cache = LastAppliedCache::new();

        // Pretend hydrate just installed inner amount = 0.9 and marked
        // binding 0 as "skip the next outer write."
        g.set_param(feedback, "amount", ParamValue::Float(0.9))
            .unwrap();
        cache.mark_skip_once(0);

        // First apply with the catalog-default outer (0.5): must NOT
        // overwrite the hydrated value.
        apply_param_bindings(
            &bindings,
            &[],
            &mut g,
            None,
            &[ParamSlot::exposed(0.5)],
            &mut cache,
        );
        assert_eq!(
            g.get_node(feedback).unwrap().params.get("amount"),
            Some(&ParamValue::Float(0.9)),
            "SkipOnce must not overwrite the hydrated value on first apply",
        );
        // The skip transitioned to Applied(0.5) so subsequent applies
        // with the same outer skip too, but an outer drag reclaims.
        assert_eq!(
            cache.static_outer[0],
            BindingCacheEntry::Applied(0.5),
        );
    }

    #[test]
    fn skip_once_lets_outer_drag_reclaim_control() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(Feedback::new()));
        let bindings = vec![feedback_amount_binding(feedback)];
        let mut cache = LastAppliedCache::new();

        g.set_param(feedback, "amount", ParamValue::Float(0.9))
            .unwrap();
        cache.mark_skip_once(0);

        // First apply with outer 0.5: SkipOnce → no write.
        apply_param_bindings(
            &bindings,
            &[],
            &mut g,
            None,
            &[ParamSlot::exposed(0.5)],
            &mut cache,
        );
        // Outer moves to 0.2 → write fires, inner gets stomped.
        apply_param_bindings(
            &bindings,
            &[],
            &mut g,
            None,
            &[ParamSlot::exposed(0.2)],
            &mut cache,
        );
        assert_eq!(
            g.get_node(feedback).unwrap().params.get("amount"),
            Some(&ParamValue::Float(0.2)),
            "outer-drag after a SkipOnce frame must reclaim control",
        );
    }

    #[test]
    fn apply_to_unknown_param_returns_graph_error() {
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(Feedback::new()));
        let binding = ParamBinding {
            id: Cow::Borrowed("nonexistent"),
            spec: ParamSpec::continuous("nonexistent", "Nonexistent", 0.0, 1.0, 0.0, "F2", ""),
            target: ParamTarget::Node {
                node: feedback,
                param: "nonexistent",
            },
            convert: ParamConvert::Float,
        };
        let err = binding.apply(&mut g, None, 0.5).unwrap_err();
        assert!(matches!(err, GraphError::ParamNotFound { .. }));
    }

    #[test]
    fn enum_remap_routes_correctly_to_a_real_node() {
        // Verifies the full path: f32 → EnumRemap → ParamValue::Enum →
        // graph.set_param. Using Feedback's mode param (Enum [Screen,
        // Additive, Max]) and a contrived remap that swaps order.
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(Feedback::new()));
        let binding = ParamBinding {
            id: Cow::Borrowed("mode"),
            spec: ParamSpec::whole_labels("mode", "Mode", 0.0, 2.0, 0.0, &["A", "B", "C"], "Mode"),
            target: ParamTarget::Node {
                node: feedback,
                param: "mode",
            },
            // Host idx 0 → graph idx 2, 1 → 1, 2 → 0 (reverse).
            convert: ParamConvert::EnumRemap(Cow::Borrowed(&[2, 1, 0])),
        };
        binding.apply(&mut g, None, 0.0).unwrap();
        assert_eq!(
            g.get_node(feedback).unwrap().params.get("mode"),
            Some(&ParamValue::Enum(2))
        );
        binding.apply(&mut g, None, 2.0).unwrap();
        assert_eq!(
            g.get_node(feedback).unwrap().params.get("mode"),
            Some(&ParamValue::Enum(0))
        );
    }

    #[test]
    fn binding_id_is_independent_of_spec_name_and_target_param() {
        // The ID is the stable mapping key. It's allowed (and expected
        // in some cases) to differ from both the slider label
        // (`spec.name`) and the inner-node param name. Test confirms
        // nothing in the routing code conflates the three.
        let mut g = Graph::new();
        let feedback = g.add_node(Box::new(Feedback::new()));
        let binding = ParamBinding {
            id: Cow::Borrowed("blend_strength"),
            spec: ParamSpec::continuous(
                "blend_strength",
                "Blend Strength",
                0.0,
                1.0,
                0.5,
                "F2",
                "",
            ),
            target: ParamTarget::Node {
                node: feedback,
                param: "amount",
            },
            convert: ParamConvert::Float,
        };
        binding.apply(&mut g, None, 0.42).unwrap();
        // Routed to Feedback's "amount" param (the target.param), not
        // anything keyed off the binding's id or spec.name.
        assert_eq!(
            g.get_node(feedback).unwrap().params.get("amount"),
            Some(&ParamValue::Float(0.42))
        );
        assert!(
            g.get_node(feedback)
                .unwrap()
                .params
                .get("blend_strength")
                .is_none()
        );
    }

    #[test]
    fn unused_type_id_constant_compiles() {
        // Suppress unused-import warning for FEEDBACK_TYPE_ID; this
        // also documents that we chose Feedback as the test fixture
        // because it's the only stateful primitive currently available.
        assert_eq!(FEEDBACK_TYPE_ID, "node.feedback");
    }
}
