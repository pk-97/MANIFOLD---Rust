//! [`ChainSpec`] — declarative effect contribution to a chain graph.
//!
//! Every effect submits one `ChainSpec` via inventory alongside its
//! [`EffectMetadata`]. The chain consumes specs uniformly: there is no
//! separate "atomic" vs "composite", "primitive-direct" vs "wrapped",
//! or "canonical" vs "divergent" code path. Every effect, every frame,
//! every chain rebuild — single path.
//!
//! ## Splice model
//!
//! Each effect is **a contribution to the chain**, not a self-contained
//! mini-factory. Given a chain graph and the endpoint where the previous
//! effect's output lives, the effect's `splice` function adds its worker
//! nodes directly to the chain graph and returns where its own output
//! ends up.
//!
//! - Atomic effect (one worker): adds one primitive, wires source into
//!   its input, returns its output port.
//! - Composite effect (multiple workers like Mirror): adds two or more
//!   primitives, wires them up internally, returns the terminal node's
//!   output port.
//!
//! No Source/FinalOutput boundary nodes per effect — those exist only
//! at the chain's start and end.
//!
//! ## Handles
//!
//! Each effect's `splice` returns a list of internal `(handle, node_id)`
//! pairs. These names are **local to that effect** — two effects sharing
//! one chain can both use "mix" without conflict because bindings and
//! user-bindings always resolve the handle within the effect they live
//! on.
//!
//! The handles drive two things:
//! 1. [`ParamBinding`] — outer-card sliders writing to inner-worker params.
//! 2. User-param bindings — per-instance V2 bindings hydrated at chain
//!    build time, addressed by handle string from the project file.

use std::borrow::Cow;

use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::effects::EffectInstance;

use crate::node_graph::boundary_nodes::{
    FINAL_OUTPUT_TYPE_ID, FinalOutput, SOURCE_TYPE_ID, Source,
};
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::metadata::metadata_by_id;
use crate::node_graph::param_binding::ParamBinding;
use crate::node_graph::persistence::{PrimitiveRegistry, SerializedParamValue};

/// Static spec — one per effect, registered via `inventory::submit!`.
pub struct ChainSpec {
    pub type_id: EffectTypeId,

    /// Place this effect's workers into `graph`, wired so they read
    /// from `source` (where the previous effect's output lives).
    /// Returns the output endpoint + effect-local handle map.
    pub splice: fn(graph: &mut Graph, source: (NodeInstanceId, &'static str)) -> SpliceResult,

    /// Host-visible parameter bindings. Each [`ParamBinding`] carries
    /// both the outer slider's [`ParamSpec`] and the inner routing
    /// target ([`ParamTarget::HandleNode`]) in one place.
    ///
    /// Per-frame value flow uses
    /// [`apply_param_bindings`](crate::node_graph::apply_param_bindings)
    /// with a [`LastAppliedCache`](crate::node_graph::LastAppliedCache)
    /// seeded from these bindings' defaults — so per-card edits to
    /// inner-node params survive when the outer slot is at rest, and
    /// the outer reclaims control as soon as it moves.
    pub bindings: &'static [ParamBinding],

    /// When the chain should drop this effect entirely (no workers
    /// added, no cost). Previous output flows directly to next effect.
    pub skip: SkipMode,
}

/// Outcome of a single [`ChainSpec::splice`] call.
pub struct SpliceResult {
    /// `(node, port)` where this effect's output lives. Port names come
    /// from primitive port declarations and are always `&'static str`.
    pub output: (NodeInstanceId, &'static str),

    /// Effect-local handle map. Names are scoped to this effect;
    /// bindings + user-bindings look up nodes here, never on the chain
    /// graph globally.
    ///
    /// `Cow<'static, str>` so canonical splices (compile-time literals)
    /// stay zero-allocation via `Cow::Borrowed("mix")`, while
    /// user-edited divergent defs use `Cow::Owned(handle_string)`
    /// for names that come off disk at runtime.
    pub handles: Vec<(Cow<'static, str>, NodeInstanceId)>,
}

pub enum SkipMode {
    /// Effect always contributes its workers.
    Never,
    /// Skip when the param identified by `param_id` is ≤ 0.
    OnZero { param_id: &'static str },
}

impl ChainSpec {
    /// Build a standalone graph containing only this effect, wrapped
    /// in Source / FinalOutput boundaries. Used for the editor canvas
    /// snapshot — the chain itself never builds graphs this way.
    ///
    /// Effect-local handles from the splice result are also projected
    /// onto the graph globally so the editor's outer-routing gray-out
    /// can match `OuterParamRouting.node_handle` against the snapshot's
    /// `NodeSnapshot.node_handle`. Canonical splices always return
    /// `Cow::Borrowed` handles (compile-time literals), so no
    /// allocation is required here.
    pub fn build_canonical_graph(&self) -> Graph {
        let mut graph = Graph::new();
        let src = graph.add_node_named("source", Box::new(Source::new()));
        let result = (self.splice)(&mut graph, (src, "out"));
        for (handle, node_id) in &result.handles {
            if let Cow::Borrowed(name) = handle {
                graph.register_handle(name, *node_id);
            }
            // Owned handles only flow in via `splice_def_into_chain`,
            // which doesn't go through this path. Defensive skip.
        }
        let final_out = graph.add_node_named("final_output", Box::new(FinalOutput::new()));
        graph
            .connect(result.output, (final_out, "in"))
            .expect("wire splice output → final_output");
        graph
    }

    /// Should the chain drop this effect for this instance?
    pub fn is_skipped(&self, fx: &EffectInstance) -> bool {
        match self.skip {
            SkipMode::Never => false,
            SkipMode::OnZero { param_id } => {
                let Some(metadata) = metadata_by_id(&self.type_id) else {
                    return false;
                };
                let Some(idx) = metadata.params.iter().position(|p| p.id == param_id) else {
                    return false;
                };
                fx.param_values
                    .get(idx)
                    .map(|s| s.value <= 0.0)
                    .unwrap_or(false)
            }
        }
    }
}

inventory::collect!(ChainSpec);

/// Compress the splice + inventory submission for an atomic effect —
/// one primitive, one input port, one handle, one output port. About
/// two thirds of the shipping effects fit this shape; the macro lets
/// them declare only the host-visible information (type id, primitive
/// type, handle name, routings, skip rule) and emits the boilerplate.
///
/// Composite effects (Mirror, SoftFocus, …) still hand-write their
/// splice — the wiring between their inner workers is what gives them
/// their shape.
///
/// ## Optional fields
///
/// - `input_port: "name"` — defaults to `"in"`. Override when a
///   primitive declares its input under a different name (e.g.
///   `Transform` uses `"source"`).
///
/// ## Example
///
/// ```ignore
/// crate::atomic_chain_spec! {
///     type_id: EffectTypeId::INVERT_COLORS,
///     primitive: Invert,
///     handle: "invert",
///     bindings: &[
///         ParamBinding {
///             id: Cow::Borrowed("amount"),
///             spec: ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),
///             target: ParamTarget::HandleNode { handle: "invert", param: "intensity" },
///             convert: ParamConvert::Float,
///         },
///     ],
///     skip: SkipMode::OnZero { param_id: "amount" },
/// }
/// ```
#[macro_export]
macro_rules! atomic_chain_spec {
    (
        type_id: $type_id:expr,
        primitive: $prim:ty,
        handle: $handle:literal,
        $(input_port: $input:literal,)?
        bindings: $bindings:expr,
        skip: $skip:expr $(,)?
    ) => {
        ::inventory::submit! {
            $crate::node_graph::ChainSpec {
                type_id: $type_id,
                splice: {
                    fn splice(
                        graph: &mut $crate::node_graph::Graph,
                        source: (
                            $crate::node_graph::NodeInstanceId,
                            &'static str,
                        ),
                    ) -> $crate::node_graph::SpliceResult {
                        let node = graph.add_node(::std::boxed::Box::new(<$prim>::new()));
                        graph
                            .connect(
                                source,
                                (node, $crate::atomic_chain_spec!(@port $($input)?)),
                            )
                            .expect(concat!(
                                "wire source → ",
                                stringify!($prim),
                                ".",
                                $crate::atomic_chain_spec!(@port $($input)?),
                            ));
                        $crate::node_graph::SpliceResult {
                            output: (node, "out"),
                            handles: ::std::vec![(
                                ::std::borrow::Cow::Borrowed($handle),
                                node,
                            )],
                        }
                    }
                    splice
                },
                bindings: $bindings,
                skip: $skip,
            }
        }
    };
    (@port) => { "in" };
    (@port $input:literal) => { $input };
}


/// Look up a chain spec by effect type id. Built once at first call.
pub fn chain_spec_by_id(id: &EffectTypeId) -> Option<&'static ChainSpec> {
    use std::sync::OnceLock;
    static MAP: OnceLock<ahash::AHashMap<EffectTypeId, &'static ChainSpec>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m: ahash::AHashMap<EffectTypeId, &'static ChainSpec> = ahash::AHashMap::default();
        for spec in inventory::iter::<ChainSpec> {
            if m.insert(spec.type_id.clone(), spec).is_some() {
                eprintln!(
                    "[manifold-renderer] duplicate ChainSpec submission for {:?}",
                    spec.type_id
                );
            }
        }
        m
    })
    .get(id)
    .copied()
}

/// Resolve a handle within an effect's local map.
pub fn lookup_handle(
    handles: &[(Cow<'static, str>, NodeInstanceId)],
    name: &str,
) -> Option<NodeInstanceId> {
    handles
        .iter()
        .find(|(h, _)| h.as_ref() == name)
        .map(|(_, id)| *id)
}

/// Startup-time invariant check — runs every registered spec's
/// `splice` against a throwaway graph and verifies that every
/// routing's `target_handle` (and every binding's `HandleNode.handle`)
/// appears in the resulting handle map. Catches typos at process boot
/// rather than at first render.
///
/// Returns the list of spec/routing pairs that failed. Empty result
/// = every spec is internally consistent. Callers may log + skip the
/// broken specs or panic, depending on policy.
pub fn validate_all_specs() -> Vec<SpecValidationError> {
    use crate::node_graph::param_binding::ParamTarget;
    let mut errors = Vec::new();
    for spec in inventory::iter::<ChainSpec> {
        let mut probe = Graph::new();
        let probe_src = probe.add_node(Box::new(Source::new()));
        let result = (spec.splice)(&mut probe, (probe_src, "out"));
        let handle_set: ahash::AHashSet<&str> = result
            .handles
            .iter()
            .map(|(name, _)| name.as_ref())
            .collect();
        for binding in spec.bindings {
            if let ParamTarget::HandleNode { handle, .. } = &binding.target
                && !handle_set.contains(handle)
            {
                // Static bindings declared on a `ChainSpec` always carry
                // `Cow::Borrowed` ids; owned-id bindings only flow through
                // user-edited graphs and don't reach this validator.
                let id: &'static str = match &binding.id {
                    Cow::Borrowed(s) => s,
                    Cow::Owned(_) => "<owned>",
                };
                errors.push(SpecValidationError {
                    effect_id: spec.type_id.clone(),
                    routing_param_id: id,
                    missing_handle: handle,
                });
            }
        }
    }
    errors
}

/// Parity check: when a spec declares both `bindings` and the matching
/// effect's `EffectMetadata.params` is non-empty, every binding's
/// `spec` must equal the corresponding `ParamSpec` in the metadata
/// (matched by `id`). Drift here means the card UI / OSC / save path
/// would see different metadata than the runtime apply path.
///
/// Empty result = no drift. During Phase 1 migration, effects flip
/// from declaring `routings` to declaring `bindings`; this guard
/// guarantees that flipping doesn't change the outer surface.
///
/// Run automatically as part of [`validate_all_specs`]; also exposed
/// directly for fine-grained tests.
pub fn validate_binding_spec_parity() -> Vec<BindingParityError> {
    let mut errors = Vec::new();
    for spec in inventory::iter::<ChainSpec> {
        if spec.bindings.is_empty() {
            continue;
        }
        let Some(metadata) = metadata_by_id(&spec.type_id) else {
            continue;
        };
        if metadata.params.is_empty() {
            continue;
        }
        // Match each binding to its metadata param by id; assert
        // every metadata field matches.
        for binding in spec.bindings {
            let bid = binding.id.as_ref();
            let Some(meta_param) = metadata.params.iter().find(|p| p.id == bid) else {
                errors.push(BindingParityError {
                    effect_id: spec.type_id.clone(),
                    param_id: bid.to_string(),
                    reason: "binding id has no matching EffectMetadata.params entry".into(),
                });
                continue;
            };
            if !param_specs_equal(&binding.spec, meta_param) {
                errors.push(BindingParityError {
                    effect_id: spec.type_id.clone(),
                    param_id: bid.to_string(),
                    reason: format!(
                        "binding.spec differs from EffectMetadata.params entry: \
                         binding={:?} metadata={:?}",
                        binding.spec, meta_param,
                    ),
                });
            }
        }
        // Note: we deliberately do NOT flag metadata params that have
        // no matching binding. Some effects intentionally leave a
        // metadata param unrouted (e.g. edge_detect's `mode` is folded
        // into the always-on shader path; voronoi_prism's `beat` is
        // ctx-driven by `apply_ctx_params_at`, not by a static binding).
        // The metadata-without-binding direction is therefore allowed;
        // only the binding-without-metadata direction is actual drift.
    }
    errors
}

fn param_specs_equal(
    a: &manifold_core::generator_registration::ParamSpec,
    b: &manifold_core::generator_registration::ParamSpec,
) -> bool {
    a.id == b.id
        && a.name == b.name
        && a.min == b.min
        && a.max == b.max
        && a.default_value == b.default_value
        && a.whole_numbers == b.whole_numbers
        && a.is_toggle == b.is_toggle
        && a.value_labels == b.value_labels
        && a.format_string == b.format_string
        && a.osc_suffix == b.osc_suffix
}

#[derive(Debug, Clone)]
pub struct BindingParityError {
    pub effect_id: EffectTypeId,
    pub param_id: String,
    pub reason: String,
}

impl std::fmt::Display for BindingParityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ChainSpec[{}].bindings[{}]: {}",
            self.effect_id.as_str(),
            self.param_id,
            self.reason,
        )
    }
}

impl std::error::Error for BindingParityError {}

#[derive(Debug, Clone)]
pub struct SpecValidationError {
    pub effect_id: EffectTypeId,
    pub routing_param_id: &'static str,
    pub missing_handle: &'static str,
}

impl std::fmt::Display for SpecValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ChainSpec[{}]: routing `{}` references handle `{}` that `splice` does not register",
            self.effect_id.as_str(),
            self.routing_param_id,
            self.missing_handle
        )
    }
}

impl std::error::Error for SpecValidationError {}

// ─── Outer-vs-inner ParamSpec audit ──────────────────────────────────
//
// Each `ParamBinding` declares an outer-card `ParamSpec` (range,
// default, labels, format) alongside its inner-node target. The
// outer spec is authored separately from the inner node's
// `ParamDef`, which means it can drift — wider/narrower range,
// fewer/different enum labels, mismatched defaults. After the V2
// panel unification the per-node panel exposes the inner ParamDef
// directly, so any divergence between the two shows up as a visible
// inconsistency (Mirror's "Mode" outer slider lists 3 options;
// opening Transform inside the graph reveals 9).
//
// This audit walks every `ChainSpec`, resolves each binding's
// inner-node target on a throwaway canonical graph, and compares
// the outer `binding.spec` against the inner node's actual
// `ParamDef`. Findings are categorized:
//
// - **`[DRIFT]`** — accidental mismatch: no conversion in the
//   binding explains the divergence. Range, labels, or default
//   simply don't line up. Author neglect; fix in place.
// - **`[CURATED]`** — intentional restriction or remap. Triggered
//   by `ParamConvert::EnumRemap` (Mirror's mode subset) or
//   `ParamConvert::FloatTransform` (Transform.rot degrees↔radians,
//   Strobe.rate note-rate index↔strobes-per-beat). The mismatch is
//   load-bearing for save compat or unit translation; categorize
//   so it shows up explicitly when reviewing the curation surface.

/// One finding produced by [`audit_outer_inner_drift`]. Categorizes
/// the divergence and carries enough context to fix it in place.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct OuterInnerDrift {
    pub effect_id: EffectTypeId,
    pub outer_param_id: String,
    pub inner_handle: &'static str,
    pub inner_param: &'static str,
    pub category: DriftCategory,
    /// Field-by-field differences. Empty when only category-level
    /// info (e.g., "inner-node missing") is meaningful.
    pub diffs: Vec<DriftField>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftCategory {
    /// Author neglect. Outer spec contradicts inner ParamDef and
    /// no conversion explains the difference. Should be fixed.
    Drift,
    /// Intentional narrowing or remap. `ParamConvert::EnumRemap` or
    /// `ParamConvert::FloatTransform` is doing the translation.
    Curated,
    /// Inner-node target couldn't be resolved against the canonical
    /// graph — binding references a handle/param that doesn't exist
    /// after splice. Bug, but a different one than drift.
    Unresolved,
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub enum DriftField {
    /// Outer claims one type; inner declares another. `expected_for_inner`
    /// is the outer shape we'd expect given the inner's `ParamType`
    /// (e.g. `whole_numbers=true` for `Int`).
    TypeMismatch {
        inner_ty: &'static str,
        outer_flag_summary: String,
    },
    /// Numeric range disagreement. `None` on the inner side means the
    /// inner ParamDef declared no range — outer-side range is then
    /// purely an outer-UI choice and the test treats it as drift-free.
    Range {
        outer: (f32, f32),
        inner: Option<(f32, f32)>,
    },
    /// Default-value disagreement after coercing the outer's f32 onto
    /// the inner's type (round for Int, threshold for Bool, etc.).
    Default {
        outer: f32,
        inner: String,
    },
    /// Enum label set divergence. Both sides hold an ordered list;
    /// curated effects have matching length but differing strings, or
    /// shorter outer list. Drift is "outer has different length AND
    /// no `EnumRemap`".
    EnumLabels {
        outer: Vec<String>,
        inner: Vec<String>,
    },
}

#[cfg(test)]
impl std::fmt::Display for OuterInnerDrift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tag = match self.category {
            DriftCategory::Drift => "[DRIFT]",
            DriftCategory::Curated => "[CURATED]",
            DriftCategory::Unresolved => "[UNRESOLVED]",
        };
        writeln!(
            f,
            "{tag} {effect}::{outer_id}  →  {handle}.{inner}",
            effect = self.effect_id.as_str(),
            outer_id = self.outer_param_id,
            handle = self.inner_handle,
            inner = self.inner_param,
        )?;
        for d in &self.diffs {
            match d {
                DriftField::TypeMismatch { inner_ty, outer_flag_summary } => {
                    writeln!(
                        f,
                        "    type: inner={inner_ty}, outer={outer_flag_summary}",
                    )?;
                }
                DriftField::Range { outer, inner } => match inner {
                    Some(i) => writeln!(
                        f,
                        "    range: outer=({}..={}), inner=({}..={})",
                        outer.0, outer.1, i.0, i.1,
                    )?,
                    None => writeln!(
                        f,
                        "    range: outer=({}..={}), inner=<none declared>",
                        outer.0, outer.1,
                    )?,
                },
                DriftField::Default { outer, inner } => {
                    writeln!(f, "    default: outer={outer}, inner={inner}")?;
                }
                DriftField::EnumLabels { outer, inner } => {
                    writeln!(
                        f,
                        "    enum_labels: outer={:?}, inner={:?}",
                        outer, inner,
                    )?;
                }
            }
        }
        Ok(())
    }
}

/// Run the outer-vs-inner audit across every registered `ChainSpec`.
/// Returns findings sorted into `[DRIFT]` first, then `[CURATED]`,
/// then `[UNRESOLVED]` — drift is what we want to fix; curated and
/// unresolved are informational.
#[cfg(test)]
pub fn audit_outer_inner_drift() -> Vec<OuterInnerDrift> {
    use crate::node_graph::param_binding::{ParamConvert, ParamTarget};
    use crate::node_graph::parameters::{ParamType, ParamValue};

    let mut findings: Vec<OuterInnerDrift> = Vec::new();

    for spec in inventory::iter::<ChainSpec> {
        // Build the canonical graph so we can resolve `HandleNode`
        // targets against a real handle map. Mirror the construction
        // pattern from `validate_all_specs` so we don't depend on its
        // accidental implementation details.
        let mut probe = Graph::new();
        let probe_src = probe.add_node_named("source", Box::new(
            crate::node_graph::boundary_nodes::Source::new(),
        ));
        let result = (spec.splice)(&mut probe, (probe_src, "out"));
        let handle_map: ahash::AHashMap<&str, NodeInstanceId> = result
            .handles
            .iter()
            .map(|(h, id)| (h.as_ref(), *id))
            .collect();

        for binding in spec.bindings {
            let ParamTarget::HandleNode { handle, param } = &binding.target else {
                // Composite/Node/Custom variants don't address a single
                // inner ParamDef — skip. The existing parity validator
                // already covers Custom; Composite resolution lives
                // inside the CompositeHandle indirection.
                continue;
            };
            let Some(&node_id) = handle_map.get(handle) else {
                findings.push(OuterInnerDrift {
                    effect_id: spec.type_id.clone(),
                    outer_param_id: binding.id.to_string(),
                    inner_handle: handle,
                    inner_param: param,
                    category: DriftCategory::Unresolved,
                    diffs: Vec::new(),
                });
                continue;
            };
            let Some(node) = probe.get_node(node_id) else {
                findings.push(OuterInnerDrift {
                    effect_id: spec.type_id.clone(),
                    outer_param_id: binding.id.to_string(),
                    inner_handle: handle,
                    inner_param: param,
                    category: DriftCategory::Unresolved,
                    diffs: Vec::new(),
                });
                continue;
            };
            let Some(inner_pd) = node.node.parameters().iter().find(|p| p.name == *param) else {
                findings.push(OuterInnerDrift {
                    effect_id: spec.type_id.clone(),
                    outer_param_id: binding.id.to_string(),
                    inner_handle: handle,
                    inner_param: param,
                    category: DriftCategory::Unresolved,
                    diffs: Vec::new(),
                });
                continue;
            };

            let curated = matches!(
                binding.convert,
                ParamConvert::EnumRemap(_) | ParamConvert::FloatTransform(_),
            );

            // Build the expected outer shape from the inner ParamDef.
            // Walk each field and record divergences. The categoriser
            // at the bottom decides whether the bag of diffs is
            // intentional (curated) or accidental (drift).
            let mut diffs: Vec<DriftField> = Vec::new();

            // Type vs flags.
            let (expected_whole, expected_toggle, expected_enum) = match inner_pd.ty {
                ParamType::Float => (false, false, false),
                ParamType::Int => (true, false, false),
                ParamType::Bool => (false, true, false),
                ParamType::Enum => (true, false, true),
                ParamType::Vec2 | ParamType::Vec3 | ParamType::Vec4 | ParamType::Color => {
                    // Multi-component types can't bind to a single outer
                    // slot. If a binding declares one anyway, surface as
                    // an outright type mismatch rather than try to compare
                    // a phantom range/default.
                    findings.push(OuterInnerDrift {
                        effect_id: spec.type_id.clone(),
                        outer_param_id: binding.id.to_string(),
                        inner_handle: handle,
                        inner_param: param,
                        category: DriftCategory::Drift,
                        diffs: vec![DriftField::TypeMismatch {
                            inner_ty: match inner_pd.ty {
                                ParamType::Vec2 => "Vec2",
                                ParamType::Vec3 => "Vec3",
                                ParamType::Vec4 => "Vec4",
                                ParamType::Color => "Color",
                                _ => "?",
                            },
                            outer_flag_summary: format!(
                                "whole_numbers={} is_toggle={} value_labels={}",
                                binding.spec.whole_numbers,
                                binding.spec.is_toggle,
                                binding.spec.value_labels.len(),
                            ),
                        }],
                    });
                    continue;
                }
            };
            let flags_match = binding.spec.whole_numbers == expected_whole
                && binding.spec.is_toggle == expected_toggle
                && (binding.spec.value_labels.is_empty() != expected_enum
                    || (expected_enum && !binding.spec.value_labels.is_empty()));
            if !flags_match {
                diffs.push(DriftField::TypeMismatch {
                    inner_ty: match inner_pd.ty {
                        ParamType::Float => "Float",
                        ParamType::Int => "Int",
                        ParamType::Bool => "Bool",
                        ParamType::Enum => "Enum",
                        _ => "?",
                    },
                    outer_flag_summary: format!(
                        "whole_numbers={} is_toggle={} enum_labels={}",
                        binding.spec.whole_numbers,
                        binding.spec.is_toggle,
                        binding.spec.value_labels.len(),
                    ),
                });
            }

            // Range. Inner declares a range only on Float/Int (and
            // sometimes Enum). When inner declares None we have no
            // ground truth — skip the diff.
            if let Some(inner_range) = inner_pd.range
                && matches!(inner_pd.ty, ParamType::Float | ParamType::Int)
                && (binding.spec.min != inner_range.0 || binding.spec.max != inner_range.1)
            {
                diffs.push(DriftField::Range {
                    outer: (binding.spec.min, binding.spec.max),
                    inner: Some(inner_range),
                });
            }

            // Default. Coerce the outer f32 onto the inner's type
            // before comparing — IntRound, BoolThreshold, EnumRound
            // do the same coercion at runtime, so the audit should
            // match that.
            let default_diff: Option<String> = match (inner_pd.ty, &inner_pd.default) {
                (ParamType::Float, ParamValue::Float(v)) => {
                    if (binding.spec.default_value - *v).abs() > f32::EPSILON {
                        Some(format!("Float({v})"))
                    } else {
                        None
                    }
                }
                (ParamType::Int, ParamValue::Int(v)) => {
                    let coerced = binding.spec.default_value.round() as i32;
                    if coerced != *v {
                        Some(format!("Int({v})"))
                    } else {
                        None
                    }
                }
                (ParamType::Bool, ParamValue::Bool(v)) => {
                    let coerced = binding.spec.default_value > 0.5;
                    if coerced != *v {
                        Some(format!("Bool({v})"))
                    } else {
                        None
                    }
                }
                (ParamType::Enum, ParamValue::Enum(v)) => {
                    let coerced = binding.spec.default_value.round().max(0.0) as u32;
                    // For EnumRemap, the outer default must go through
                    // the remap table to land on a meaningful inner
                    // value. Audit treats this as expected curation.
                    let mapped = if let ParamConvert::EnumRemap(table) = &binding.convert {
                        let idx = coerced as usize;
                        if table.is_empty() {
                            coerced
                        } else if idx < table.len() {
                            table[idx]
                        } else {
                            table[table.len() - 1]
                        }
                    } else {
                        coerced
                    };
                    if mapped != *v {
                        Some(format!("Enum({v})"))
                    } else {
                        None
                    }
                }
                _ => None,
            };
            if let Some(inner_repr) = default_diff {
                diffs.push(DriftField::Default {
                    outer: binding.spec.default_value,
                    inner: inner_repr,
                });
            }

            // Enum labels. Compare the outer label slice against the
            // inner's enum_values when the inner is an Enum.
            if matches!(inner_pd.ty, ParamType::Enum) {
                let outer: Vec<String> =
                    binding.spec.value_labels.iter().map(|s| s.to_string()).collect();
                let inner: Vec<String> =
                    inner_pd.enum_values.iter().map(|s| s.to_string()).collect();
                if outer != inner {
                    diffs.push(DriftField::EnumLabels { outer, inner });
                }
            }

            if diffs.is_empty() {
                continue;
            }
            findings.push(OuterInnerDrift {
                effect_id: spec.type_id.clone(),
                outer_param_id: binding.id.to_string(),
                inner_handle: handle,
                inner_param: param,
                category: if curated {
                    DriftCategory::Curated
                } else {
                    DriftCategory::Drift
                },
                diffs,
            });
        }
    }

    // Sort: Drift first, then Curated, then Unresolved; within each
    // bucket alphabetical by effect id for readability.
    findings.sort_by(|a, b| {
        let bucket = |c: &DriftCategory| match c {
            DriftCategory::Drift => 0,
            DriftCategory::Curated => 1,
            DriftCategory::Unresolved => 2,
        };
        bucket(&a.category)
            .cmp(&bucket(&b.category))
            .then_with(|| a.effect_id.as_str().cmp(b.effect_id.as_str()))
            .then_with(|| a.outer_param_id.cmp(&b.outer_param_id))
    });
    findings
}

/// Splice an [`EffectGraphDef`] (user-edited per-card divergence) into
/// the chain graph. Returns the same shape as the canonical splice
/// path, so the caller doesn't care whether the contribution came from
/// `spec.splice` or from a user's saved wiring.
///
/// The def's `Source` boundary disappears — every wire fanning out
/// from it is re-anchored to `source` (the chain's previous endpoint).
/// The def's `FinalOutput` boundary also disappears — the wire feeding
/// into it identifies the def's output endpoint, which becomes the
/// chain's next source.
///
/// Per-node params encoded in the def (the user's slider edits) are
/// applied via `graph.set_param` before returning. Effect-local
/// handles (named nodes in the def) flow into [`SpliceResult::handles`]
/// so routings + user-bindings resolve uniformly with the canonical
/// path.
///
/// Returns `None` on malformed input (no Source / no FinalOutput /
/// unknown type id / orphan wire); the caller falls back to the
/// canonical splice so the chain still renders.
pub fn splice_def_into_chain(
    graph: &mut Graph,
    source: (NodeInstanceId, &'static str),
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Option<SpliceResult> {
    // First pass: identify the def's Source and FinalOutput ids so we
    // know which wires to re-anchor / treat as the output.
    let mut def_source_id: Option<u32> = None;
    let mut def_final_id: Option<u32> = None;
    for n in &def.nodes {
        if n.type_id == SOURCE_TYPE_ID {
            def_source_id = Some(n.id);
        } else if n.type_id == FINAL_OUTPUT_TYPE_ID {
            def_final_id = Some(n.id);
        }
    }
    let def_source_id = def_source_id?;
    let def_final_id = def_final_id?;

    // Second pass: instantiate every non-boundary node. Track
    // (def_id → chain_node_id) so wires can be translated.
    let mut def_to_chain: AHashMap<u32, NodeInstanceId> = AHashMap::default();
    let mut handles: Vec<(Cow<'static, str>, NodeInstanceId)> = Vec::new();
    for n in &def.nodes {
        if n.id == def_source_id || n.id == def_final_id {
            continue;
        }
        let node = registry.construct(&n.type_id)?;
        let chain_id = graph.add_node(node);
        def_to_chain.insert(n.id, chain_id);
        if let Some(handle_name) = n.handle.as_deref() {
            handles.push((Cow::Owned(handle_name.to_owned()), chain_id));
        }
    }

    // Apply per-node params from the def.
    for n in &def.nodes {
        if let Some(&chain_id) = def_to_chain.get(&n.id) {
            for (param_name, value) in &n.params {
                let pv = match value {
                    SerializedParamValue::Float { value } => {
                        Some(crate::node_graph::ParamValue::Float(*value))
                    }
                    SerializedParamValue::Int { value } => {
                        Some(crate::node_graph::ParamValue::Int(*value))
                    }
                    SerializedParamValue::Bool { value } => {
                        Some(crate::node_graph::ParamValue::Bool(*value))
                    }
                    SerializedParamValue::Enum { value } => {
                        Some(crate::node_graph::ParamValue::Enum(*value))
                    }
                    // Vec2/Vec3/Vec4/Color are not yet plumbed through
                    // the runtime `ParamValue` enum. Skip for now —
                    // the primitive keeps its declared default.
                    SerializedParamValue::Vec2 { .. }
                    | SerializedParamValue::Vec3 { .. }
                    | SerializedParamValue::Vec4 { .. }
                    | SerializedParamValue::Color { .. } => None,
                };
                if let Some(pv) = pv
                    && let Some(static_name) = resolve_param_name(graph, chain_id, param_name)
                {
                    let _ = graph.set_param(chain_id, static_name, pv);
                }
            }
        }
    }

    // Third pass: translate wires. Port names need to be resolved into
    // the primitive's declared `&'static str` references — those are
    // what `graph.connect` accepts, and looking them up on the just-
    // instantiated nodes is cleaner than leaking heap strings.
    let mut output_endpoint: Option<(NodeInstanceId, &'static str)> = None;
    for w in &def.wires {
        if w.from_node == def_source_id {
            let to_chain = *def_to_chain.get(&w.to_node)?;
            let to_port = resolve_input_port(graph, to_chain, &w.to_port)?;
            graph.connect(source, (to_chain, to_port)).ok()?;
            continue;
        }
        if w.to_node == def_final_id {
            let from_chain = *def_to_chain.get(&w.from_node)?;
            let from_port = resolve_output_port(graph, from_chain, &w.from_port)?;
            output_endpoint = Some((from_chain, from_port));
            continue;
        }
        let from_chain = *def_to_chain.get(&w.from_node)?;
        let to_chain = *def_to_chain.get(&w.to_node)?;
        let from_port = resolve_output_port(graph, from_chain, &w.from_port)?;
        let to_port = resolve_input_port(graph, to_chain, &w.to_port)?;
        graph.connect((from_chain, from_port), (to_chain, to_port)).ok()?;
    }

    Some(SpliceResult {
        output: output_endpoint?,
        handles,
    })
}

fn resolve_param_name(graph: &Graph, node: NodeInstanceId, name: &str) -> Option<&'static str> {
    graph
        .get_node(node)?
        .node
        .parameters()
        .iter()
        .map(|p| p.name)
        .find(|n| *n == name)
}

fn resolve_input_port(graph: &Graph, node: NodeInstanceId, name: &str) -> Option<&'static str> {
    graph
        .get_node(node)?
        .node
        .inputs()
        .iter()
        .map(|p| p.name)
        .find(|n| *n == name)
}

fn resolve_output_port(graph: &Graph, node: NodeInstanceId, name: &str) -> Option<&'static str> {
    graph
        .get_node(node)?
        .node
        .outputs()
        .iter()
        .map(|p| p.name)
        .find(|n| *n == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every registered `ChainSpec` must pass `splice` validation —
    /// every routing's `target_handle` and every binding's
    /// `HandleNode.handle` must resolve against the splice's handle map.
    /// Regressions here break per-frame param refresh and surface as
    /// "the slider does nothing" in production.
    #[test]
    fn all_specs_validate_handles_at_startup() {
        let errors = validate_all_specs();
        assert!(
            errors.is_empty(),
            "ChainSpec handle validation failed:\n{}",
            errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    /// For every effect that has migrated to `bindings`, the bindings'
    /// `spec` field must equal the effect's `EffectMetadata.params`
    /// entry of the same id, byte-for-byte. This is the gate that
    /// catches drift during the Phase 1 migration: declaring a
    /// binding with different metadata than the hand-written
    /// `ParamSpec` would silently change the card UI / OSC / save
    /// surface.
    #[test]
    fn migrated_effects_have_binding_spec_parity_with_metadata() {
        let errors = validate_binding_spec_parity();
        assert!(
            errors.is_empty(),
            "ParamBinding spec ↔ EffectMetadata.params parity failed:\n{}",
            errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    /// Audit-mode survey: dump every divergence between a binding's
    /// outer `ParamSpec` and its inner-node `ParamDef`, separated into
    /// `[DRIFT]` (accidental, fix in place) and `[CURATED]` (intentional
    /// via `ParamConvert::EnumRemap` / `FloatTransform`). Run with
    /// `cargo test -- --nocapture audit_outer_inner_param_drift_report`
    /// to see the full categorized list.
    ///
    /// First-pass policy: print, don't fail. Once we've decided what to
    /// do with each finding the assertion below tightens to `drift = 0`.
    #[test]
    fn audit_outer_inner_param_drift_report() {
        let findings = audit_outer_inner_drift();
        if findings.is_empty() {
            return;
        }
        let drift = findings
            .iter()
            .filter(|f| f.category == DriftCategory::Drift)
            .count();
        let curated = findings
            .iter()
            .filter(|f| f.category == DriftCategory::Curated)
            .count();
        let unresolved = findings
            .iter()
            .filter(|f| f.category == DriftCategory::Unresolved)
            .count();
        let mut report = String::new();
        report.push_str(&format!(
            "\nOuter-vs-inner ParamSpec audit — {} drift, {} curated, {} unresolved\n",
            drift, curated, unresolved,
        ));
        report.push_str(
            "──────────────────────────────────────────────────────────────────────\n",
        );
        for f in &findings {
            report.push_str(&format!("{f}"));
        }
        // Print whether passing or failing — the categorized list is
        // useful diagnostic output even in the green case.
        eprintln!("{report}");
    }
}
