//! `generate-preset-thumbnails` — factory-thumbnail one-shot dev bin
//! (`docs/PRESET_LIBRARY_DESIGN.md` P6, D7).
//!
//! Walks the bundled stock preset dirs (`assets/effect-presets/`,
//! `assets/generator-presets/` — the SAME dev-stock roots `check_presets.rs`
//! and the real `preset_loader` scan), renders each preset's
//! `<Name>.png` via `preset_thumbnail::render_preset_thumbnail`, and writes
//! it to the committed thumbnails root
//! (`assets/preset-thumbnails/{effects,generators}/<id>.png` —
//! `preset_thumbnail::factory_thumbnail_path`'s dev-resolution target).
//! Run it once whenever the factory preset set changes and commit the
//! resulting PNGs; the browser reads them at browse time, never renders.
//!
//! Run: `cargo run -p manifold-renderer --bin generate-preset-thumbnails`

use std::path::Path;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::preset_def::PresetKind;
use manifold_gpu::GpuDevice;
use manifold_renderer::preset_thumbnail::{
    THUMBNAIL_SIZE, factory_thumbnail_path, render_preset_thumbnail_to_file,
};

const ASSET_SUBDIRS: &[(&str, PresetKind)] = &[
    ("assets/effect-presets", PresetKind::Effect),
    ("assets/generator-presets", PresetKind::Generator),
];

fn main() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let device = std::sync::Arc::new(GpuDevice::new());

    let mut total = 0usize;
    let mut written = 0usize;
    let mut failures: Vec<(String, String)> = Vec::new();

    for (subdir, kind) in ASSET_SUBDIRS {
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
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            total += 1;
            match render_one(&device, *kind, &path, id) {
                Ok(out_path) => {
                    written += 1;
                    println!("OK   {id} -> {}", out_path.display());
                }
                Err(msg) => failures.push((id.to_string(), msg)),
            }
        }
    }

    for (id, msg) in &failures {
        println!("FAIL {id}: {msg}");
    }

    println!("\n{total} presets: {written} thumbnails written, {} failed", failures.len());

    if !failures.is_empty() {
        std::process::exit(1);
    }
}

fn render_one(
    device: &std::sync::Arc<GpuDevice>,
    kind: PresetKind,
    json_path: &Path,
    id: &str,
) -> Result<std::path::PathBuf, String> {
    let bytes = std::fs::read_to_string(json_path).map_err(|e| format!("read: {e}"))?;
    let def: EffectGraphDef = serde_json::from_str(&bytes).map_err(|e| format!("parse: {e}"))?;

    let out_path =
        factory_thumbnail_path(kind, id).ok_or_else(|| "no thumbnail root resolved".to_string())?;
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }

    render_preset_thumbnail_to_file(device, kind, &def, THUMBNAIL_SIZE, &out_path)?;
    Ok(out_path)
}
