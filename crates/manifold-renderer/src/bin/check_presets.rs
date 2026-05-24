//! `check-presets` — fast pre-launch validator for bundled preset JSON.
//!
//! Walks `assets/effect-presets/` and `assets/generator-presets/`, loads
//! each JSON file from disk, and runs the same load + compile pipeline
//! the runtime / editor take. Catches `UnknownTypeId`, `UnknownParam`,
//! `ParamTypeMismatch`, `InvalidWire`, `RequiredInputUnwired`, cycles,
//! and output-slot sizing failures — exactly the class of error that
//! otherwise only surfaces as "editor shows empty canvas" / "first
//! frame grey, then black" at app launch.
//!
//! Reads from disk, not the build.rs-baked bundle — edit JSON, run this,
//! no rebuild needed.

use std::path::{Path, PathBuf};
use std::time::Instant;

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_renderer::node_graph::{EffectGraphDefExt, PrimitiveRegistry, compile};

const ASSET_SUBDIRS: &[&str] = &["assets/effect-presets", "assets/generator-presets"];

fn main() {
    let start = Instant::now();
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let registry = PrimitiveRegistry::with_builtin();

    let mut total = 0usize;
    let mut failures: Vec<(PathBuf, String)> = Vec::new();

    for subdir in ASSET_SUBDIRS {
        let dir = manifest_dir.join(subdir);
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
            if let Err(msg) = check_one(&path, &registry) {
                failures.push((path, msg));
            }
        }
    }

    for (path, msg) in &failures {
        let rel = path
            .strip_prefix(manifest_dir)
            .unwrap_or(path.as_path());
        println!("FAIL {}", rel.display());
        for line in msg.lines() {
            println!("  {line}");
        }
    }

    let elapsed = start.elapsed();
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

fn check_one(path: &Path, registry: &PrimitiveRegistry) -> Result<(), String> {
    let bytes = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let def: EffectGraphDef =
        serde_json::from_str(&bytes).map_err(|e| format!("parse: {e}"))?;

    check_bindings_resolve(&def)?;

    let graph = def
        .clone()
        .into_graph(registry)
        .map_err(|e| e.to_string())?;
    compile(&graph).map_err(|e| e.to_string())?;
    Ok(())
}

/// Mirrors `every_bundled_preset_binding_resolves_to_an_outer_param` —
/// each `bindings[i].id` must match some `params[j].id`. Bindings whose
/// id has no matching outer param sit forever on `default_value` at
/// runtime (silent failure mode).
fn check_bindings_resolve(def: &EffectGraphDef) -> Result<(), String> {
    let Some(meta) = def.preset_metadata.as_ref() else {
        return Ok(());
    };
    let param_ids: ahash::AHashSet<&str> =
        meta.params.iter().map(|p| p.id.as_str()).collect();
    let mut bad: Vec<String> = Vec::new();
    for binding in &meta.bindings {
        if !param_ids.contains(binding.id.as_str()) {
            let target = match &binding.target {
                BindingTarget::HandleNode { handle, param } => {
                    format!("handleNode {handle}.{param}")
                }
                other => format!("{other:?}"),
            };
            bad.push(format!(
                "binding id='{}' (target {target}) has no matching outer-card param id",
                binding.id
            ));
        }
    }
    if bad.is_empty() {
        Ok(())
    } else {
        Err(bad.join("\n"))
    }
}
