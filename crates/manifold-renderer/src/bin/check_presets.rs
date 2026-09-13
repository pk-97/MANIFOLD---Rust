//! `check-presets` — fast pre-launch validator for bundled preset JSON.
//!
//! Walks the selected `assets/{effect,generator,scene-modifier}-presets/`, loads
//! each JSON file from disk, and runs the same load + compile pipeline
//! the runtime / editor take. Catches `UnknownTypeId`, `UnknownParam`,
//! `ParamTypeMismatch`, `InvalidWire`, `RequiredInputUnwired`, cycles,
//! and output-slot sizing failures — exactly the class of error that
//! otherwise only surfaces as "editor shows empty canvas" / "first
//! frame grey, then black" at app launch.
//!
//! For generator presets (`generator-presets/*.json`), the validator
//! also runs the full chain build through `JsonGraphGenerator::
//! from_def_with_device`. This catches the post-compile allocation
//! errors (`UnsizedArrayOutput`, `UnboundArrayResource`) that
//! `compile()` alone misses — the silent-partial-allocation bug class
//! that produced FluidSim2D's all-black output. Adds a real Metal
//! device init + per-preset buffer allocations (~ a couple of seconds
//! total) but stays sub-GPU-dispatch fast.
//!
//! Reads the dev stock dirs (`assets/{effect,generator}-presets`) from
//! disk directly via `CARGO_MANIFEST_DIR` — edit JSON, run this, no
//! rebuild needed. This is the same set the runtime preset loader scans
//! as its dev stock root.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_modifier_preset::{
    SceneNodeRef, SceneTargetSelection, validate_scene_modifier_schema,
};
use manifold_gpu::GpuDevice;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::node_graph::scene_modifier_authoring::prepare_new_scene_modifier;
use manifold_renderer::node_graph::scene_modifier_expand::prepare_scene_modifiers;
use manifold_renderer::node_graph::{
    PrimitiveRegistry, ValidateKind, ValidationReport, validate_def,
};

const EFFECT_SUBDIR: &str = "assets/effect-presets";
const GENERATOR_SUBDIR: &str = "assets/generator-presets";
const SCENE_MODIFIER_SUBDIR: &str = "assets/scene-modifier-presets";
const MUSHROOM_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/cc0___mushroom.glb"
);

#[derive(Clone, Copy)]
enum RequestedKind {
    Effect,
    Generator,
    SceneModifier,
}

impl RequestedKind {
    fn subdir(self) -> &'static str {
        match self {
            Self::Effect => EFFECT_SUBDIR,
            Self::Generator => GENERATOR_SUBDIR,
            Self::SceneModifier => SCENE_MODIFIER_SUBDIR,
        }
    }
}

fn main() {
    let kinds = match requested_kinds(std::env::args().skip(1)) {
        Ok(kinds) => kinds,
        Err(message) => {
            eprintln!("error: {message}");
            eprintln!("usage: check-presets [--kind effect|generator|sceneModifier]");
            std::process::exit(2);
        }
    };
    let start = Instant::now();
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let registry = PrimitiveRegistry::with_builtin();

    // Scene modifier qualification is CPU-only. Avoid constructing a Metal
    // device when the caller selected only the scene modifier catalog.
    let device = kinds
        .iter()
        .any(|kind| !matches!(kind, RequestedKind::SceneModifier))
        .then(|| Arc::new(GpuDevice::new()));
    let scene_host = if kinds
        .iter()
        .any(|kind| matches!(kind, RequestedKind::SceneModifier))
    {
        Some(
            assemble_import_graph(Path::new(MUSHROOM_FIXTURE))
                .map(|(def, _)| def)
                .unwrap_or_else(|error| {
                    eprintln!("error: mushroom validation fixture failed: {error}");
                    std::process::exit(2);
                }),
        )
    } else {
        None
    };

    let mut total = 0usize;
    let mut failures: Vec<(PathBuf, ValidationReport)> = Vec::new();

    for requested_kind in kinds {
        let subdir = requested_kind.subdir();
        let dir = manifest_dir.join(subdir);
        let kind = if subdir == GENERATOR_SUBDIR {
            ValidateKind::Generator
        } else if subdir == SCENE_MODIFIER_SUBDIR {
            ValidateKind::SceneModifier
        } else {
            ValidateKind::Effect
        };
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("error: cannot read {}: {e}", dir.display());
                std::process::exit(2);
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            total += 1;
            let result = if matches!(requested_kind, RequestedKind::SceneModifier) {
                parse_and_validate_scene_modifier(
                    &path,
                    &registry,
                    scene_host.as_ref().expect("scene host initialized"),
                )
            } else {
                parse_and_validate(
                    &path,
                    &registry,
                    device.as_ref().expect("GPU device initialized"),
                    kind,
                )
            };
            match result {
                Ok(report) if report.is_valid() => {}
                Ok(report) => failures.push((path, report)),
                Err(msg) => failures.push((
                    path,
                    ValidationReport {
                        errors: vec![manifold_renderer::node_graph::ValidationIssue {
                            node_id: None,
                            type_id: None,
                            port: None,
                            message: msg,
                        }],
                        warnings: Vec::new(),
                    },
                )),
            }
        }
    }

    for (path, report) in &failures {
        let rel = path.strip_prefix(manifest_dir).unwrap_or(path.as_path());
        println!("FAIL {}", rel.display());
        for issue in &report.errors {
            println!("  {}", issue.message);
        }
    }

    let elapsed = start.elapsed();
    if total == 0 {
        eprintln!("error: selected preset kinds contained no JSON files");
        std::process::exit(2);
    }
    let ok = total - failures.len();
    println!(
        "\n{total} presets: {ok} ok, {} failed ({:.2}s)",
        failures.len(),
        elapsed.as_secs_f32(),
    );

    if !failures.is_empty() {
        std::process::exit(1);
    }
}

fn requested_kinds(mut args: impl Iterator<Item = String>) -> Result<Vec<RequestedKind>, String> {
    let Some(flag) = args.next() else {
        return Ok(vec![
            RequestedKind::Effect,
            RequestedKind::Generator,
            RequestedKind::SceneModifier,
        ]);
    };
    if flag != "--kind" {
        return Err(format!("unknown argument `{flag}`"));
    }
    let value = args
        .next()
        .ok_or_else(|| "--kind requires effect, generator, or sceneModifier".to_string())?;
    if args.next().is_some() {
        return Err("only one --kind selection is supported".to_string());
    }
    match value.as_str() {
        "effect" => Ok(vec![RequestedKind::Effect]),
        "generator" => Ok(vec![RequestedKind::Generator]),
        "sceneModifier" => Ok(vec![RequestedKind::SceneModifier]),
        _ => Err(format!(
            "unknown --kind `{value}`; expected effect, generator, or sceneModifier"
        )),
    }
}

/// Parses `path` from disk and runs it through [`validate_def`] — the
/// same load + compile pipeline the runtime / editor take (and the
/// generator chain-build allocation audit for generator presets).
/// `Err` here means the file itself didn't parse; a parsed-but-invalid
/// graph comes back as an `Ok(report)` whose `errors` are non-empty.
fn parse_and_validate(
    path: &Path,
    registry: &PrimitiveRegistry,
    device: &std::sync::Arc<GpuDevice>,
    kind: ValidateKind,
) -> Result<ValidationReport, String> {
    let bytes = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let def: EffectGraphDef = serde_json::from_str(&bytes).map_err(|e| format!("parse: {e}"))?;
    Ok(validate_def(&def, registry, kind, device))
}

fn parse_and_validate_scene_modifier(
    path: &Path,
    registry: &PrimitiveRegistry,
    host: &EffectGraphDef,
) -> Result<ValidationReport, String> {
    let bytes = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let recipe: EffectGraphDef = serde_json::from_str(&bytes).map_err(|e| format!("parse: {e}"))?;
    validate_scene_modifier_schema(&recipe).map_err(|error| error.to_string())?;
    let scene_node = host
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .ok_or_else(|| "mushroom fixture has no node.render_scene host".to_string())?;
    let instance = prepare_new_scene_modifier(
        host,
        &recipe,
        format!("check:{}", path.display()).into(),
        SceneNodeRef {
            scope: Vec::new(),
            node: scene_node.node_id.clone(),
        },
        SceneTargetSelection::AllObjects,
    )
    .map_err(|error| error.to_string())?;
    let candidate = manifold_core::scene_modifier_edit::insert_scene_modifier(host, host.scene_modifiers.len(), instance)
        .map_err(|error| error.to_string())?;
    prepare_scene_modifiers(&candidate.graph, registry).map_err(|error| error.to_string())?;
    Ok(ValidationReport::default())
}
