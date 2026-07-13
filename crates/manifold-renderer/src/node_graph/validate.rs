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
use manifold_core::preset_def::PresetKind;
use manifold_gpu::{GpuDevice, GpuTextureFormat};

use crate::node_graph::{EffectGraphDefExt, GraphError, LoadError, PrimitiveRegistry, compile};
use crate::preset_runtime::{JsonGeneratorLoadError, PresetRuntime};

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
            Json(_) | MissingGeneratorInput | MissingFinalOutput => ValidationIssue {
                node_id: None,
                type_id: None,
                port: None,
                message: e.to_string(),
            },
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
    device: &GpuDevice,
) -> ValidationReport {
    let mut report = ValidationReport::default();

    report.errors.extend(check_bindings_resolve(def));

    let graph = match def.clone().into_graph(registry) {
        Ok(g) => g,
        Err(e) => {
            report.errors.push(ValidationIssue::from(&e));
            return report;
        }
    };
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
            device,
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
        let device = GpuDevice::new();

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
}
