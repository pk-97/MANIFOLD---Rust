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
use std::time::Instant;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_gpu::GpuDevice;
use manifold_renderer::node_graph::{PrimitiveRegistry, ValidateKind, ValidationReport, validate_def};

const ASSET_SUBDIRS: &[&str] = &["assets/effect-presets", "assets/generator-presets"];
const GENERATOR_SUBDIR: &str = "assets/generator-presets";

fn main() {
    let start = Instant::now();
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let registry = PrimitiveRegistry::with_builtin();

    // Initialise once; reuse across all generator presets. Device
    // creation is ~50ms; reusing it saves that per preset.
    let device = std::sync::Arc::new(GpuDevice::new());

    let mut total = 0usize;
    let mut failures: Vec<(PathBuf, ValidationReport)> = Vec::new();

    for subdir in ASSET_SUBDIRS {
        let dir = manifest_dir.join(subdir);
        let kind = if *subdir == GENERATOR_SUBDIR {
            ValidateKind::Generator
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
            match parse_and_validate(&path, &registry, &device, kind) {
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
