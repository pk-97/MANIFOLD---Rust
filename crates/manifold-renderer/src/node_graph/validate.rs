//! The validate seam — one function every consumer of "does this graph
//! load and compile" calls through: `check_presets` (bundled-dirs
//! walker), `graph_tool validate` (arbitrary file path), the .glb
//! importer (P3), and eventually the MCP `validate_graph` tool (D7).
//!
//! `validate_def` is `check_presets::check_one`'s body MOVED here
//! verbatim (GRAPH_TOOLING_DESIGN P1, D1) — not a reimplementation.
//! It runs the exact pipeline the runtime loader takes
//! (`EffectGraphDefExt::into_graph` → `compile`, plus the generator
//! chain build via `PresetRuntime::from_def_with_device` for
//! `ValidateKind::Generator`), so a validator pass is fidelity by
//! construction: it can never approve a graph the loader rejects,
//! because it *is* the loader up to the point checked.
//!
//! See `docs/GRAPH_TOOLING_DESIGN.md` §2 D1–D2, §3 "Committed shapes".

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::id::NodeId;
use manifold_core::preset_def::PresetKind;
use manifold_gpu::{GpuDevice, GpuTextureFormat};

use crate::node_graph::parameters::ParamType;
use crate::node_graph::{EffectGraphDefExt, GraphError, LoadError, PrimitiveRegistry, compile};
use crate::preset_runtime::{JsonGeneratorLoadError, PresetRuntime};

/// Blend/mix-family atoms whose declared param is a continuous
/// crossfade weight between two authored "looks" — the structural
/// identification the P4 brief calls for, derived from the registry:
/// every `category: Composite` primitive whose param is a [0,1]-ranged
/// blend factor between two texture inputs. `node.feedback` (Composite,
/// but a single-input temporal loop, no blend weight) and
/// `node.texture_sum_5` (Composite, but `divisor` is a sum-vs-average
/// switch, not a two-look crossfade) are deliberately excluded.
const BLEND_FAMILY_PARAMS: &[(&str, &str)] = &[
    ("node.mix", "amount"),
    ("node.masked_mix", "amount"),
    ("node.wet_dry", "wet_dry"),
    ("node.hdr_mix", "retention"),
];

/// Small canvas + fp16 format matches `check_presets`' allocation
/// budget — cheap enough to validate every generator preset without a
/// real render target. Canvas-sized array outputs (scatter
/// accumulators etc.) scale by w×h, so 256×256 stays well under the
/// per-preset budget even for particle-density graphs.
const VALIDATE_CANVAS_W: u32 = 256;
const VALIDATE_CANVAS_H: u32 = 256;
const VALIDATE_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

/// Which pipeline [`validate_def`] runs. Effects stop after
/// `compile`; generators additionally run the `PresetRuntime` chain
/// build, which catches the post-compile `Array<T>` allocation errors
/// (`UnsizedArrayOutput`, `UnboundArrayResource`) that `compile()`
/// alone misses — the silent-partial-allocation bug class that
/// produced FluidSim2D's all-black output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidateKind {
    Effect,
    Generator,
}

impl From<PresetKind> for ValidateKind {
    fn from(kind: PresetKind) -> Self {
        match kind {
            PresetKind::Effect => ValidateKind::Effect,
            PresetKind::Generator => ValidateKind::Generator,
        }
    }
}

/// One problem found in a graph document. `node_id` / `type_id` /
/// `port` are populated whenever the source error carries that
/// context as a doc-id / registered type / port-or-param name — a
/// `None` field means the underlying error variant didn't name one,
/// not that the check is imprecise. `message` is always
/// self-contained (safe to print alone).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ValidationIssue {
    pub node_id: Option<u32>,
    pub type_id: Option<String>,
    pub port: Option<String>,
    pub message: String,
}

/// Result of [`validate_def`]. Any non-empty `errors` means the graph
/// is invalid. `warnings` is populated from P4 onward (card idiom
/// lints, D8) — a graph with warnings but no errors still passes.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct ValidationReport {
    pub errors: Vec<ValidationIssue>,
    pub warnings: Vec<ValidationIssue>,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

impl From<&LoadError> for ValidationIssue {
    fn from(e: &LoadError) -> Self {
        use LoadError::*;
        let (node_id, type_id, port) = match e {
            UnsupportedVersion { .. } | DuplicateNodeId(_) | InvalidWire { .. } => {
                (None, None, None)
            }
            UnknownTypeId { node_id, type_id } => (Some(*node_id), Some(type_id.clone()), None),
            UnknownNodeRef { node_id, .. } => (Some(*node_id), None, None),
            UnknownParam {
                node_id,
                type_id,
                param,
            } => (Some(*node_id), Some(type_id.clone()), Some(param.clone())),
            ParamTypeMismatch {
                node_id,
                type_id,
                param,
                ..
            } => (Some(*node_id), Some(type_id.clone()), Some(param.clone())),
            UnknownOutputFormat {
                node_id,
                type_id,
                port,
                ..
            } => (Some(*node_id), Some(type_id.clone()), Some(port.clone())),
            OutputFormatNotSupported {
                node_id,
                type_id,
                port,
                ..
            } => (Some(*node_id), Some(type_id.clone()), Some(port.clone())),
            // `node_id` here is a serialized handle string, not the
            // u32 doc id every other variant carries (see
            // `BindingConvertTypeMismatch`'s field docs) — left out of
            // the structured field, kept in the message text.
            BindingConvertTypeMismatch { param, .. } => (None, None, Some(param.clone())),
            Flatten(_) => (None, None, None),
        };
        ValidationIssue {
            node_id,
            type_id,
            port,
            message: e.to_string(),
        }
    }
}

impl From<&GraphError> for ValidationIssue {
    fn from(e: &GraphError) -> Self {
        use GraphError::*;
        let (node_id, port) = match e {
            NodeNotFound(id) => (Some(id.0), None),
            PortNotFound { node, port } => (Some(node.0), Some(port.clone())),
            PortKindMismatch { node, port, .. } => (Some(node.0), Some(port.clone())),
            PortTypeMismatch { .. } => (None, None),
            ChannelMismatch(info) => (Some(info.from_node.0), Some(info.from_port.clone())),
            TextureChannelMismatch(info) => {
                (Some(info.from_node.0), Some(info.from_port.clone()))
            }
            RequiredInputUnwired { node, port } => (Some(node.0), Some(port.clone())),
            ParamNotFound { node, param } => (Some(node.0), Some(param.clone())),
            CycleDetected { involves } => (involves.first().map(|n| n.0), None),
            PortFormatMismatch {
                from_node,
                from_port,
                ..
            } => (Some(from_node.0), Some(from_port.clone())),
            ConditionalRequirementUnmet {
                node,
                missing_input,
                ..
            } => (Some(node.0), Some(missing_input.clone())),
        };
        ValidationIssue {
            node_id,
            type_id: None,
            port,
            message: e.to_string(),
        }
    }
}

impl From<&JsonGeneratorLoadError> for ValidationIssue {
    fn from(e: &JsonGeneratorLoadError) -> Self {
        use JsonGeneratorLoadError::*;
        match e {
            Load(load_err) => {
                let mut issue = ValidationIssue::from(load_err);
                issue.message = e.to_string();
                issue
            }
            Compile(graph_err) => {
                let mut issue = ValidationIssue::from(graph_err);
                issue.message = e.to_string();
                issue
            }
            UnsizedArrayOutput { node_type, port } | UnsizedTexture3DOutput { node_type, port } => {
                ValidationIssue {
                    node_id: None,
                    type_id: Some(node_type.clone()),
                    port: Some(port.clone()),
                    message: e.to_string(),
                }
            }
            UnboundArrayResource {
                producer_node_type,
                producer_port,
                ..
            } => ValidationIssue {
                node_id: None,
                type_id: Some(producer_node_type.clone()),
                port: Some(producer_port.clone()),
                message: e.to_string(),
            },
            Json(_) | MissingGeneratorInput | MissingFinalOutput | MultipleFinalOutputs { .. } => {
                ValidationIssue {
                    node_id: None,
                    type_id: None,
                    port: None,
                    message: e.to_string(),
                }
            }
        }
    }
}

/// THE validation entry point. Same pipeline the runtime loader
/// takes: `into_graph` → `compile`, plus (for `ValidateKind::Generator`)
/// the `PresetRuntime` chain build for the allocation-error class
/// `compile()` alone misses. Parsing the JSON is the caller's job —
/// this takes the already-deserialized def.
pub fn validate_def(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    kind: ValidateKind,
    device: &std::sync::Arc<GpuDevice>,
) -> ValidationReport {
    let mut report = ValidationReport::default();

    report.errors.extend(check_bindings_resolve(def));

    // Card lints (D8) resolve binding targets against the real,
    // post-flatten, post-wgsl-parse graph (see `check_card_lints`'s
    // doc comment for why a bare registry lookup isn't fidelity-safe
    // here) — so they run against the same `graph` this function
    // builds for `compile`, not a second parse.
    let graph = match def.clone().into_graph(registry) {
        Ok(g) => g,
        Err(e) => {
            let (card_errors, card_warnings) = check_card_lints(def, None);
            report.errors.extend(card_errors);
            report.warnings.extend(card_warnings);
            report.errors.push(ValidationIssue::from(&e));
            return report;
        }
    };
    let (card_errors, card_warnings) = check_card_lints(def, Some(&graph));
    report.errors.extend(card_errors);
    report.warnings.extend(card_warnings);

    if let Err(e) = compile(&graph) {
        report.errors.push(ValidationIssue::from(&e));
        return report;
    }

    // Generator-side post-compile audit. `compile()` covers static
    // validation (types, cycles, required inputs) but doesn't
    // allocate the Array<T> buffer pool. The full chain build path
    // catches `UnsizedArrayOutput` at the cause site and
    // `UnboundArrayResource` at the catch-all audit. Effect presets
    // don't run the same Array allocation path (yet); the
    // load/compile check above is sufficient for them.
    if kind == ValidateKind::Generator
        && let Err(e) = PresetRuntime::from_def_with_device(
            def.clone(),
            registry,
            std::sync::Arc::clone(device),
            VALIDATE_CANVAS_W,
            VALIDATE_CANVAS_H,
            VALIDATE_FORMAT,
            None,
        )
    {
        report.errors.push(ValidationIssue::from(&e));
    }

    report
}

/// Mirrors `every_bundled_preset_binding_resolves_to_an_outer_param` —
/// each `bindings[i].id` must match some `params[j].id`. Bindings
/// whose id has no matching outer param sit forever on
/// `default_value` at runtime (silent failure mode).
fn check_bindings_resolve(def: &EffectGraphDef) -> Vec<ValidationIssue> {
    let Some(meta) = def.preset_metadata.as_ref() else {
        return Vec::new();
    };
    let param_ids: ahash::AHashSet<&str> = meta.params.iter().map(|p| p.id.as_str()).collect();
    let mut issues = Vec::new();
    for binding in &meta.bindings {
        if !param_ids.contains(binding.id.as_str()) {
            let (target, port) = match &binding.target {
                BindingTarget::Node { node_id, param } => {
                    (format!("node {node_id}.{param}"), Some(param.clone()))
                }
                other => (format!("{other:?}"), None),
            };
            issues.push(ValidationIssue {
                node_id: None,
                type_id: None,
                port,
                message: format!(
                    "binding id='{}' (target {target}) has no matching outer-card param id",
                    binding.id
                ),
            });
        }
    }
    issues
}

/// One resolved binding target: the concrete node's type_id and the
/// [`ParamDef`] the inner param name resolved to. No document `id: u32`
/// field here — the graph's [`NodeInstanceId`](crate::node_graph::graph::NodeInstanceId)
/// is a fresh runtime-assigned id, not the JSON document's stable
/// numeric id, so callers report location via the `node_id` STRING
/// (already in every message) rather than a numeric `ValidationIssue::node_id`.
struct ResolvedTargetParam {
    type_id: String,
    param_def: crate::node_graph::parameters::ParamDef,
    /// The inner param's [`RangeContract`](manifold_core::effects::RangeContract),
    /// if any — read via `EffectNode::param_contract` (PARAM_RANGE_CONTRACT_DESIGN.md
    /// P1). `None` for the overwhelming majority of params, which carry only a
    /// display hint (`param_def.range`).
    contract: Option<manifold_core::effects::RangeContract>,
}

/// Resolve a `BindingTarget::Node { node_id, param }` against the
/// already-built, already-flattened live [`Graph`] — NOT a throwaway
/// `registry.construct()` of the bare type_id. Two reasons this has to
/// go through the real graph: (1) a bound node may live only inside an
/// embedded group body, invisible in `def.nodes` before flatten (the
/// graph is post-flatten by construction, `into_graph` flattens
/// first); (2) `node.wgsl_compute`'s param list is per-instance,
/// parsed from that specific node's embedded `wgsl_source` — a fresh
/// default-constructed `wgsl_compute` declares none of it, so a
/// registry-only lookup false-positived on every WGSL-authored preset
/// (StrangeAttractor, Plasma, BlackHole, FluidSim3D) during this
/// phase's own gate run. `None` covers both "node_id not in this
/// graph" and "node has no such param" — callers distinguish by
/// re-checking `graph.instance_by_node_id` if they need to tell those
/// apart (only (a) does).
fn resolve_target_param(
    graph: &crate::node_graph::Graph,
    node_id: &NodeId,
    param: &str,
) -> Option<ResolvedTargetParam> {
    let instance_id = graph.instance_by_node_id(node_id)?;
    let inst = graph.get_node(instance_id)?;
    let param_def = inst.node.parameters().iter().find(|p| p.name == param)?.clone();
    let contract = inst.node.param_contract(param);
    Some(ResolvedTargetParam {
        type_id: inst.node.type_id().as_str().to_string(),
        param_def,
        contract,
    })
}

/// D8 card lints (GRAPH_TOOLING_DESIGN P4) — the checks beyond
/// `check_bindings_resolve` that make sure a card never lies to the
/// performer. Split by severity per D8: errors are structural breakage
/// (a dead slider, a dangling target, a mislabeled mode, a
/// trigger-type mismatch, a duplicate OSC address); warnings are
/// idiom/consistency lints an authoring agent corrects in-session.
///
/// Takes the already-built `graph` (post `into_graph`, so post-flatten
/// and post-wgsl-parse) for every check that needs to resolve a
/// binding target to a concrete inner param — see
/// [`resolve_target_param`]. `graph` is `None` when the caller
/// couldn't build one (the main `into_graph`/`compile` call already
/// reports that failure); node-shaped checks are skipped in that case,
/// not duplicated.
///
/// `BindingTarget::Composite` targets are skipped for every check that
/// needs to resolve a target to a concrete inner param — composite
/// routing (`CompositeHandle::inner_routing_for`, see
/// `node_graph/composites/mod.rs`) is built at live-graph construction
/// time from a runtime handle, not statically derivable from the
/// `EffectGraphDef` alone. No bundled preset uses a Composite target
/// today, so this is a documented gap, not an observed miss.
pub(crate) fn check_card_lints(
    def: &EffectGraphDef,
    graph: Option<&crate::node_graph::Graph>,
) -> (Vec<ValidationIssue>, Vec<ValidationIssue>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let Some(meta) = def.preset_metadata.as_ref() else {
        return (errors, warnings);
    };

    // (a) ERROR — binding target doesn't resolve to a node+param in the graph.
    if let Some(graph) = graph {
        for binding in &meta.bindings {
            let BindingTarget::Node { node_id, param } = &binding.target else {
                continue; // Composite — see doc comment above.
            };
            if graph.instance_by_node_id(node_id).is_none() {
                errors.push(ValidationIssue {
                    node_id: None,
                    type_id: None,
                    port: Some(param.clone()),
                    message: format!(
                        "card binding '{}' targets node_id '{node_id}', which does not exist in this graph",
                        binding.id
                    ),
                });
                continue;
            }
            if resolve_target_param(graph, node_id, param).is_none() {
                let inst = graph
                    .get_node(graph.instance_by_node_id(node_id).expect("checked above"));
                let type_id = inst.map(|i| i.node.type_id().as_str().to_string());
                errors.push(ValidationIssue {
                    node_id: None,
                    type_id: type_id.clone(),
                    port: Some(param.clone()),
                    message: format!(
                        "card binding '{}' targets param '{param}' on node '{node_id}' ({}), which declares no such param",
                        binding.id,
                        type_id.as_deref().unwrap_or("?")
                    ),
                });
            }
        }
    }

    // (b) ERROR — card param with no binding referencing its id (dead slider).
    let bound_ids: ahash::AHashSet<&str> = meta.bindings.iter().map(|b| b.id.as_str()).collect();
    for param in &meta.params {
        if !bound_ids.contains(param.id.as_str()) {
            errors.push(ValidationIssue {
                node_id: None,
                type_id: None,
                port: Some(param.id.clone()),
                message: format!(
                    "card param '{}' has no binding — moving this slider does nothing (dead slider)",
                    param.id
                ),
            });
        }
    }

    // (c) ERROR — mode param: value_labels count disagrees with the
    // integer range [min, max].
    for param in &meta.params {
        if param.whole_numbers && !param.value_labels.is_empty() {
            let expected = (param.max - param.min).round() as i64 + 1;
            let got = param.value_labels.len() as i64;
            if got != expected {
                errors.push(ValidationIssue {
                    node_id: None,
                    type_id: None,
                    port: Some(param.id.clone()),
                    message: format!(
                        "card param '{}' has {got} value_labels but its integer range [{}, {}] has {expected} steps",
                        param.id, param.min, param.max
                    ),
                });
            }
        }
    }

    // (d) ERROR — is_trigger card param bound to a non-trigger-typed
    // inner param.
    if let Some(graph) = graph {
        for param in meta.params.iter().filter(|p| p.is_trigger) {
            for binding in meta.bindings.iter().filter(|b| b.id == param.id) {
                let BindingTarget::Node { node_id, param: inner } = &binding.target else {
                    continue;
                };
                let Some(resolved) = resolve_target_param(graph, node_id, inner) else {
                    continue; // reported by (a).
                };
                if resolved.param_def.ty != ParamType::Trigger {
                    errors.push(ValidationIssue {
                        node_id: None,
                        type_id: Some(resolved.type_id.clone()),
                        port: Some(inner.clone()),
                        message: format!(
                            "card param '{}' is marked is_trigger but its binding targets '{node_id}.{inner}' ({}), which is not a trigger-typed param",
                            param.id, resolved.type_id
                        ),
                    });
                }
            }
        }
    }

    // (e) ERROR — duplicate non-empty osc_suffix across one card's params.
    let mut seen_suffixes: ahash::AHashSet<&str> = ahash::AHashSet::default();
    for param in &meta.params {
        if param.osc_suffix.is_empty() {
            continue;
        }
        if !seen_suffixes.insert(param.osc_suffix.as_str()) {
            errors.push(ValidationIssue {
                node_id: None,
                type_id: None,
                port: Some(param.id.clone()),
                message: format!(
                    "card param '{}' reuses osc_suffix '{}' already claimed by another param on this card",
                    param.id, param.osc_suffix
                ),
            });
        }
    }

    // (f) WARNING — discrete card control bound to a continuous blend
    // param on a mix/blend-family node.
    if let Some(graph) = graph {
        for param in &meta.params {
            let discrete =
                param.is_toggle || (param.whole_numbers && !param.value_labels.is_empty());
            if !discrete {
                continue;
            }
            for binding in meta.bindings.iter().filter(|b| b.id == param.id) {
                let BindingTarget::Node { node_id, param: inner } = &binding.target else {
                    continue;
                };
                let Some(resolved) = resolve_target_param(graph, node_id, inner) else {
                    continue;
                };
                if BLEND_FAMILY_PARAMS
                    .iter()
                    .any(|(t, p)| *t == resolved.type_id && *p == inner)
                {
                    warnings.push(ValidationIssue {
                        node_id: None,
                        type_id: Some(resolved.type_id.clone()),
                        port: Some(inner.clone()),
                        message: format!(
                            "card param '{}' is a discrete control bound to '{node_id}.{inner}', a continuous blend factor on {} — a mux switches branches and skips the dead one; blend renders both every frame. Use a mux select for a discrete look-switch, or keep this a continuous morph if that's the intent",
                            param.id, resolved.type_id
                        ),
                    });
                }
            }
        }
    }

    // (g) WARNING — card param default_value disagrees with its
    // binding's default_value.
    for binding in &meta.bindings {
        if let Some(param) = meta.params.iter().find(|p| p.id == binding.id)
            && param.default_value != binding.default_value
        {
            warnings.push(ValidationIssue {
                node_id: None,
                type_id: None,
                port: Some(param.id.clone()),
                message: format!(
                    "card param '{}' default_value ({}) disagrees with its binding's default_value ({})",
                    param.id, param.default_value, binding.default_value
                ),
            });
        }
    }

    // (h) ERROR — card [min, max] mapped through scale/offset escapes the
    // inner param's declared RangeContract (PARAM_RANGE_CONTRACT_DESIGN.md
    // D4). A contract names a real physical/mathematical boundary, so a
    // card escaping it is always a bug. A param with no contract — the
    // overwhelming majority — produces NOTHING here: its `range` is a
    // display hint only, and a card is free to remap past a hint (that's
    // the design working, not a gap). One-sided contracts (`min` or `max`
    // alone) are checked independently.
    if let Some(graph) = graph {
        for binding in &meta.bindings {
            let Some(param) = meta.params.iter().find(|p| p.id == binding.id) else {
                continue;
            };
            let BindingTarget::Node { node_id, param: inner } = &binding.target else {
                continue;
            };
            let Some(resolved) = resolve_target_param(graph, node_id, inner) else {
                continue;
            };
            let Some(contract) = &resolved.contract else {
                continue; // no contract — a hint disagreement is not a finding.
            };
            let mapped_a = param.min * binding.scale + binding.offset;
            let mapped_b = param.max * binding.scale + binding.offset;
            let (lo, hi) = (mapped_a.min(mapped_b), mapped_a.max(mapped_b));
            const EPS: f32 = 1e-4;
            let mut escapes = false;
            if let Some(cmin) = contract.min
                && lo < cmin - EPS
            {
                escapes = true;
            }
            if let Some(cmax) = contract.max
                && hi > cmax + EPS
            {
                escapes = true;
            }
            if escapes {
                errors.push(ValidationIssue {
                    node_id: None,
                    type_id: Some(resolved.type_id.clone()),
                    port: Some(inner.clone()),
                    message: format!(
                        "card param '{}' range [{}, {}] maps through scale={}/offset={} to [{lo}, {hi}], escaping inner param '{node_id}.{inner}''s RangeContract [{:?}, {:?}] ({:?})",
                        param.id, param.min, param.max, binding.scale, binding.offset,
                        contract.min, contract.max, contract.reason
                    ),
                });
            }
        }
    }

    (errors, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::node_graph::PrimitiveRegistry;

    const ASSET_SUBDIRS: &[(&str, ValidateKind)] = &[
        ("assets/effect-presets", ValidateKind::Effect),
        ("assets/generator-presets", ValidateKind::Generator),
    ];

    /// Every bundled preset JSON on disk validates clean through
    /// `validate_def` — the same set `check_presets` walks. Kept as a
    /// disk walk (not `bundled_preset_def`/inventory) so this test
    /// exercises `validate_def` exactly the way `graph_tool validate
    /// <file.json>` will: parse-from-disk, then validate.
    #[test]
    fn every_bundled_preset_validates_clean() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let registry = PrimitiveRegistry::with_builtin();
        let device = std::sync::Arc::new(GpuDevice::new());

        let mut total = 0usize;
        let mut failures: Vec<(std::path::PathBuf, ValidationReport)> = Vec::new();

        for (subdir, kind) in ASSET_SUBDIRS {
            let dir = manifest_dir.join(subdir);
            let entries = std::fs::read_dir(&dir)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                total += 1;
                let bytes = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("{}: read failed: {e}", path.display()));
                let def: EffectGraphDef = serde_json::from_str(&bytes)
                    .unwrap_or_else(|e| panic!("{}: parse failed: {e}", path.display()));
                let report = validate_def(&def, &registry, *kind, &device);
                if !report.is_valid() {
                    failures.push((path, report));
                }
            }
        }

        assert!(total > 0, "expected to find bundled preset JSON files");
        assert!(
            failures.is_empty(),
            "{} of {total} bundled presets failed validate_def:\n{}",
            failures.len(),
            failures
                .iter()
                .map(|(p, r)| format!(
                    "{}: {}",
                    p.display(),
                    r.errors
                        .iter()
                        .map(|i| i.message.clone())
                        .collect::<Vec<_>>()
                        .join("; ")
                ))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    /// Every bundled preset's card-lint WARNING count, printed for
    /// Peter's triage per the P4 gate (D8: warnings are reported
    /// verbatim, never auto-fixed or suppressed in this phase). Run
    /// with `--nocapture` to see the counts; never fails on its own —
    /// `every_bundled_preset_validates_clean` above is the pass/fail
    /// gate for errors.
    #[test]
    fn bundled_preset_card_warning_counts() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let registry = PrimitiveRegistry::with_builtin();
        let device = std::sync::Arc::new(GpuDevice::new());

        for (subdir, kind) in ASSET_SUBDIRS {
            let dir = manifest_dir.join(subdir);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let bytes = std::fs::read_to_string(&path).unwrap();
                let def: EffectGraphDef = serde_json::from_str(&bytes).unwrap();
                let report = validate_def(&def, &registry, *kind, &device);
                if !report.warnings.is_empty() {
                    eprintln!(
                        "WARN-REPORT {}: {} warning(s)",
                        path.display(),
                        report.warnings.len()
                    );
                    for w in &report.warnings {
                        eprintln!("  - {}", w.message);
                    }
                }
            }
        }
    }

    // ── PARAM_RANGE_CONTRACT_DESIGN.md P1 fixture ───────────────────
    //
    // A test-only primitive (`node.__` prefix — excluded from every
    // registry-walking meta-test, per the `every_boundary_atom_declares_its_reason`
    // precedent) declaring a one-sided-min and a both-sided RangeContract,
    // so lint (h)'s contract-escape check has something real to fire
    // against without adding any contract to production code (forbidden
    // this phase — D6, P2's job).
    crate::primitive! {
        name: RangeContractFixture,
        type_id: "node.__range_contract_fixture",
        purpose: "Internal lint(h) test fixture — not registered in production.",
        inputs: {
            in: Texture2D required,
        },
        outputs: {
            out: Texture2D,
        },
        params: [
            crate::node_graph::parameters::ParamDef {
                name: std::borrow::Cow::Borrowed("both_sided"),
                label: "Both Sided",
                ty: crate::node_graph::parameters::ParamType::Float,
                default: crate::node_graph::parameters::ParamValue::Float(0.5),
                range: Some((0.0, 1.0)),
                enum_values: &[],
            },
            crate::node_graph::parameters::ParamDef {
                name: std::borrow::Cow::Borrowed("min_only"),
                label: "Min Only",
                ty: crate::node_graph::parameters::ParamType::Float,
                default: crate::node_graph::parameters::ParamValue::Float(1.0),
                range: Some((0.01, 1000.0)),
                enum_values: &[],
            },
        ],
        depth_rule: Terminal,
        composition_notes: "Used by tests; do not reference from real code.",
        param_contracts: [
            ("both_sided", manifold_core::effects::RangeContract {
                min: Some(0.0),
                max: Some(1.0),
                reason: manifold_core::effects::RangeReason::NormalizedDomain,
            }),
            ("min_only", manifold_core::effects::RangeContract {
                min: Some(0.01),
                max: None,
                reason: manifold_core::effects::RangeReason::DegenerateFloor,
            }),
        ],
    }

    impl crate::node_graph::primitive::Primitive for RangeContractFixture {
        fn run(&mut self, _ctx: &mut crate::node_graph::effect_node::EffectNodeContext<'_, '_>) {}
    }

    // ── D8 card lint fixtures (P4) ──────────────────────────────────
    //
    // Each test below parses a minimal, hand-authored JSON graph
    // document exercising exactly one lint — the "held-out invalid-card
    // fixture" the P4 brief calls for. Kept as inline JSON literals
    // (rather than files under a fixtures dir) because P1 left no
    // physical fixture directory to mirror; each literal is still a
    // fresh, single-purpose document nobody wrote the lint against.

    fn parse(json: &str) -> EffectGraphDef {
        serde_json::from_str(json).unwrap_or_else(|e| panic!("fixture parse failed: {e}"))
    }

    fn registry() -> PrimitiveRegistry {
        PrimitiveRegistry::with_builtin()
    }

    /// Build the live graph the same way `validate_def` does, then run
    /// just the card lints against it — the fixture-test equivalent of
    /// `validate_def`'s card-lint step.
    fn lint(def: &EffectGraphDef) -> (Vec<ValidationIssue>, Vec<ValidationIssue>) {
        let registry = registry();
        let graph = def
            .clone()
            .into_graph(&registry)
            .unwrap_or_else(|e| panic!("fixture into_graph failed: {e}"));
        check_card_lints(def, Some(&graph))
    }

    /// (a) ERROR — binding target node_id doesn't exist in the graph.
    #[test]
    fn card_lint_error_dangling_binding_target_node() {
        let def = parse(
            r#"{
              "version": 2,
              "presetMetadata": {
                "id": "Fixture",
                "displayName": "Fixture",
                "category": "Color",
                "oscPrefix": "fixture",
                "params": [
                  {"id": "amount", "name": "Amount", "min": 0.0, "max": 1.0, "defaultValue": 1.0}
                ],
                "bindings": [
                  {"id": "amount", "label": "Amount", "defaultValue": 1.0,
                   "target": {"kind": "node", "nodeId": "does_not_exist", "param": "intensity"}}
                ]
              },
              "nodes": [
                {"id": 0, "nodeId": "invert", "typeId": "node.invert"}
              ],
              "wires": []
            }"#,
        );
        let (errors, _) = lint(&def);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("does_not_exist") && e.message.contains("does not exist")),
            "expected a dangling-target error, got: {errors:?}"
        );
    }

    /// (a) ERROR — binding target param doesn't exist on the resolved node.
    #[test]
    fn card_lint_error_dangling_binding_target_param() {
        let def = parse(
            r#"{
              "version": 2,
              "presetMetadata": {
                "id": "Fixture",
                "displayName": "Fixture",
                "category": "Color",
                "oscPrefix": "fixture",
                "params": [
                  {"id": "amount", "name": "Amount", "min": 0.0, "max": 1.0, "defaultValue": 1.0}
                ],
                "bindings": [
                  {"id": "amount", "label": "Amount", "defaultValue": 1.0,
                   "target": {"kind": "node", "nodeId": "invert", "param": "no_such_param"}}
                ]
              },
              "nodes": [
                {"id": 0, "nodeId": "invert", "typeId": "node.invert"}
              ],
              "wires": []
            }"#,
        );
        let (errors, _) = lint(&def);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("no_such_param") && e.message.contains("declares no such param")),
            "expected a dangling-param error, got: {errors:?}"
        );
    }

    /// (b) ERROR — card param with no binding referencing its id (dead slider).
    #[test]
    fn card_lint_error_dead_slider() {
        let def = parse(
            r#"{
              "version": 2,
              "presetMetadata": {
                "id": "Fixture",
                "displayName": "Fixture",
                "category": "Color",
                "oscPrefix": "fixture",
                "params": [
                  {"id": "amount", "name": "Amount", "min": 0.0, "max": 1.0, "defaultValue": 1.0},
                  {"id": "orphan", "name": "Orphan", "min": 0.0, "max": 1.0, "defaultValue": 0.0}
                ],
                "bindings": [
                  {"id": "amount", "label": "Amount", "defaultValue": 1.0,
                   "target": {"kind": "node", "nodeId": "invert", "param": "intensity"}}
                ]
              },
              "nodes": [
                {"id": 0, "nodeId": "invert", "typeId": "node.invert"}
              ],
              "wires": []
            }"#,
        );
        let (errors, _) = lint(&def);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("orphan") && e.message.contains("dead slider")),
            "expected a dead-slider error, got: {errors:?}"
        );
    }

    /// (c) ERROR — mode param whole_numbers + value_labels count
    /// disagrees with the integer range.
    #[test]
    fn card_lint_error_mode_label_count_mismatch() {
        let def = parse(
            r#"{
              "version": 2,
              "presetMetadata": {
                "id": "Fixture",
                "displayName": "Fixture",
                "category": "Color",
                "oscPrefix": "fixture",
                "params": [
                  {"id": "mode", "name": "Mode", "min": 0.0, "max": 2.0, "defaultValue": 0.0,
                   "wholeNumbers": true, "valueLabels": ["A", "B"]}
                ],
                "bindings": [
                  {"id": "mode", "label": "Mode", "defaultValue": 0.0,
                   "target": {"kind": "node", "nodeId": "invert", "param": "intensity"}}
                ]
              },
              "nodes": [
                {"id": 0, "nodeId": "invert", "typeId": "node.invert"}
              ],
              "wires": []
            }"#,
        );
        let (errors, _) = lint(&def);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("mode") && e.message.contains("value_labels")),
            "expected a mode-label-mismatch error, got: {errors:?}"
        );
    }

    /// (d) ERROR — is_trigger card param bound to a non-trigger-typed
    /// inner param.
    #[test]
    fn card_lint_error_trigger_bound_to_non_trigger_param() {
        let def = parse(
            r#"{
              "version": 2,
              "presetMetadata": {
                "id": "Fixture",
                "displayName": "Fixture",
                "category": "Color",
                "oscPrefix": "fixture",
                "params": [
                  {"id": "fire", "name": "Fire", "min": 0.0, "max": 1.0, "defaultValue": 0.0,
                   "isTrigger": true}
                ],
                "bindings": [
                  {"id": "fire", "label": "Fire", "defaultValue": 0.0,
                   "target": {"kind": "node", "nodeId": "invert", "param": "intensity"}}
                ]
              },
              "nodes": [
                {"id": 0, "nodeId": "invert", "typeId": "node.invert"}
              ],
              "wires": []
            }"#,
        );
        let (errors, _) = lint(&def);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("is_trigger") && e.message.contains("not a trigger-typed param")),
            "expected a trigger-type-mismatch error, got: {errors:?}"
        );
    }

    /// (d) negative control — is_trigger bound to an actual
    /// trigger-typed inner param produces no error.
    #[test]
    fn card_lint_trigger_bound_to_trigger_param_is_clean() {
        let def = parse(
            r#"{
              "version": 2,
              "presetMetadata": {
                "id": "Fixture",
                "displayName": "Fixture",
                "category": "Color",
                "oscPrefix": "fixture",
                "params": [
                  {"id": "advance", "name": "Advance", "min": 0.0, "max": 1.0, "defaultValue": 0.0,
                   "isTrigger": true}
                ],
                "bindings": [
                  {"id": "advance", "label": "Advance", "defaultValue": 0.0,
                   "target": {"kind": "node", "nodeId": "folder", "param": "next"}}
                ]
              },
              "nodes": [
                {"id": 0, "nodeId": "folder", "typeId": "node.image_folder"}
              ],
              "wires": []
            }"#,
        );
        let (errors, _) = lint(&def);
        assert!(
            errors.is_empty(),
            "expected no error for a trigger-to-trigger binding, got: {errors:?}"
        );
    }

    /// (e) ERROR — duplicate non-empty osc_suffix across one card's params.
    #[test]
    fn card_lint_error_duplicate_osc_suffix() {
        let def = parse(
            r#"{
              "version": 2,
              "presetMetadata": {
                "id": "Fixture",
                "displayName": "Fixture",
                "category": "Color",
                "oscPrefix": "fixture",
                "params": [
                  {"id": "amount", "name": "Amount", "min": 0.0, "max": 1.0, "defaultValue": 1.0,
                   "oscSuffix": "amt"},
                  {"id": "level", "name": "Level", "min": 0.0, "max": 1.0, "defaultValue": 1.0,
                   "oscSuffix": "amt"}
                ],
                "bindings": [
                  {"id": "amount", "label": "Amount", "defaultValue": 1.0,
                   "target": {"kind": "node", "nodeId": "invert", "param": "intensity"}},
                  {"id": "level", "label": "Level", "defaultValue": 1.0,
                   "target": {"kind": "node", "nodeId": "invert", "param": "intensity"}}
                ]
              },
              "nodes": [
                {"id": 0, "nodeId": "invert", "typeId": "node.invert"}
              ],
              "wires": []
            }"#,
        );
        let (errors, _) = lint(&def);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("osc_suffix") && e.message.contains("amt")),
            "expected a duplicate-osc-suffix error, got: {errors:?}"
        );
    }

    /// (f) WARNING — discrete control (is_toggle) bound to a
    /// continuous blend param on a blend-family node (`node.mix.amount`).
    #[test]
    fn card_lint_warning_toggle_bound_to_blend_param() {
        let def = parse(
            r#"{
              "version": 2,
              "presetMetadata": {
                "id": "Fixture",
                "displayName": "Fixture",
                "category": "Color",
                "oscPrefix": "fixture",
                "params": [
                  {"id": "which", "name": "Which", "min": 0.0, "max": 1.0, "defaultValue": 0.0,
                   "isToggle": true}
                ],
                "bindings": [
                  {"id": "which", "label": "Which", "defaultValue": 0.0,
                   "target": {"kind": "node", "nodeId": "mix", "param": "amount"}}
                ]
              },
              "nodes": [
                {"id": 0, "nodeId": "mix", "typeId": "node.mix"}
              ],
              "wires": []
            }"#,
        );
        let (errors, warnings) = lint(&def);
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
        assert!(
            warnings.iter().any(|w| w.message.contains("mux switches branches and skips the dead one")
                && w.message.contains("blend renders both every frame")),
            "expected the mux-vs-blend warning, got: {warnings:?}"
        );
    }

    /// (g) WARNING — card param default_value disagrees with its
    /// binding's default_value.
    #[test]
    fn card_lint_warning_default_value_disagreement() {
        let def = parse(
            r#"{
              "version": 2,
              "presetMetadata": {
                "id": "Fixture",
                "displayName": "Fixture",
                "category": "Color",
                "oscPrefix": "fixture",
                "params": [
                  {"id": "amount", "name": "Amount", "min": 0.0, "max": 1.0, "defaultValue": 1.0}
                ],
                "bindings": [
                  {"id": "amount", "label": "Amount", "defaultValue": 0.5,
                   "target": {"kind": "node", "nodeId": "invert", "param": "intensity"}}
                ]
              },
              "nodes": [
                {"id": 0, "nodeId": "invert", "typeId": "node.invert"}
              ],
              "wires": []
            }"#,
        );
        let (errors, warnings) = lint(&def);
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
        assert!(
            warnings
                .iter()
                .any(|w| w.message.contains("default_value") && w.message.contains("disagrees")),
            "expected a default-value-disagreement warning, got: {warnings:?}"
        );
    }

    /// (h) — card range mapped through scale/offset lands outside the
    /// inner param's declared `range` (a display hint, not a contract).
    /// PARAM_RANGE_CONTRACT_DESIGN.md D4: hint disagreement is not a
    /// finding at all — a card is free to remap past a hint. `node.invert`'s
    /// `intensity` carries no `RangeContract`, so this produces NOTHING.
    #[test]
    fn card_lint_hint_escape_after_remap_produces_no_finding() {
        let def = parse(
            r#"{
              "version": 2,
              "presetMetadata": {
                "id": "Fixture",
                "displayName": "Fixture",
                "category": "Color",
                "oscPrefix": "fixture",
                "params": [
                  {"id": "amount", "name": "Amount", "min": 0.0, "max": 1.0, "defaultValue": 1.0}
                ],
                "bindings": [
                  {"id": "amount", "label": "Amount", "defaultValue": 1.0,
                   "target": {"kind": "node", "nodeId": "invert", "param": "intensity"},
                   "scale": 2.0}
                ]
              },
              "nodes": [
                {"id": 0, "nodeId": "invert", "typeId": "node.invert"}
              ],
              "wires": []
            }"#,
        );
        let (errors, warnings) = lint(&def);
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
        assert!(warnings.is_empty(), "expected no warnings, got: {warnings:?}");
    }

    /// (h) ERROR — card range mapped through scale/offset escapes a
    /// both-sided `RangeContract`.
    #[test]
    fn card_lint_error_both_sided_contract_escaped_after_remap() {
        let def = parse(
            r#"{
              "version": 2,
              "presetMetadata": {
                "id": "Fixture",
                "displayName": "Fixture",
                "category": "Color",
                "oscPrefix": "fixture",
                "params": [
                  {"id": "amount", "name": "Amount", "min": 0.0, "max": 1.0, "defaultValue": 1.0}
                ],
                "bindings": [
                  {"id": "amount", "label": "Amount", "defaultValue": 1.0,
                   "target": {"kind": "node", "nodeId": "fixture", "param": "both_sided"},
                   "scale": 2.0}
                ]
              },
              "nodes": [
                {"id": 0, "nodeId": "fixture", "typeId": "node.__range_contract_fixture"}
              ],
              "wires": []
            }"#,
        );
        let (errors, _warnings) = lint(&def);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("RangeContract") && e.message.contains("both_sided")),
            "expected a RangeContract-escape error, got: {errors:?}"
        );
    }

    /// (h) ERROR — card range escapes a ONE-SIDED (min-only) `RangeContract`.
    /// The card's own [0,1] range maps (scale=0.001) to [0, 0.001], which
    /// dips below the contract's `min: Some(0.01)` floor — `max: None` means
    /// no ceiling exists to escape, only the floor.
    #[test]
    fn card_lint_error_one_sided_min_contract_escaped_after_remap() {
        let def = parse(
            r#"{
              "version": 2,
              "presetMetadata": {
                "id": "Fixture",
                "displayName": "Fixture",
                "category": "Color",
                "oscPrefix": "fixture",
                "params": [
                  {"id": "amount", "name": "Amount", "min": 0.0, "max": 1.0, "defaultValue": 1.0}
                ],
                "bindings": [
                  {"id": "amount", "label": "Amount", "defaultValue": 1.0,
                   "target": {"kind": "node", "nodeId": "fixture", "param": "min_only"},
                   "scale": 0.001}
                ]
              },
              "nodes": [
                {"id": 0, "nodeId": "fixture", "typeId": "node.__range_contract_fixture"}
              ],
              "wires": []
            }"#,
        );
        let (errors, _warnings) = lint(&def);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("RangeContract") && e.message.contains("min_only")),
            "expected a RangeContract-escape error, got: {errors:?}"
        );
    }

    /// Negative control: a clean card with no lint-worthy issues
    /// produces zero errors and zero warnings.
    #[test]
    fn card_lint_clean_card_produces_nothing() {
        let def = parse(
            r#"{
              "version": 2,
              "presetMetadata": {
                "id": "Fixture",
                "displayName": "Fixture",
                "category": "Color",
                "oscPrefix": "fixture",
                "params": [
                  {"id": "amount", "name": "Amount", "min": 0.0, "max": 1.0, "defaultValue": 1.0}
                ],
                "bindings": [
                  {"id": "amount", "label": "Amount", "defaultValue": 1.0,
                   "target": {"kind": "node", "nodeId": "invert", "param": "intensity"}}
                ]
              },
              "nodes": [
                {"id": 0, "nodeId": "invert", "typeId": "node.invert"}
              ],
              "wires": []
            }"#,
        );
        let (errors, warnings) = lint(&def);
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
        assert!(warnings.is_empty(), "expected no warnings, got: {warnings:?}");
    }
}
