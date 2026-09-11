//! `generate-preset-thumbnails` — factory-thumbnail one-shot dev bin
//! (`docs/PRESET_LIBRARY_DESIGN.md` P6, D7; `docs/STATIC_THUMBNAILS_DESIGN.md`
//! P2, D2-D4, D7).
//!
//! Walks the bundled stock preset dirs (`assets/effect-presets/`,
//! `assets/generator-presets/` — the SAME dev-stock roots `check_presets.rs`
//! and the real `preset_loader` scan), renders each preset's
//! `<Name>.png` (256×144, over the synthetic test card, deterministic
//! warm-up capture) via `preset_thumbnail::render_preset_thumbnail`, writes
//! it to the committed thumbnails root
//! (`assets/preset-thumbnails/{effects,generators}/<id>.png` —
//! `preset_thumbnail::factory_thumbnail_path`'s dev-resolution target), and
//! writes a `<Name>.hash` sidecar (SHA-256 of the preset JSON bytes) that
//! `factory_thumbnails_fresh` checks on every default-suite run. Finally it
//! composes a plain contact-sheet montage of every thumbnail at
//! `<workspace>/target/thumbnail-contact-sheet.png` and prints the row-major
//! grid order so a reviewer can map cells.
//!
//! Run it once whenever the factory preset set changes and commit the
//! resulting PNGs + hashes; the browser reads them at browse time, never
//! renders.
//!
//! Run: `cargo run -p manifold-renderer --bin generate-preset-thumbnails`

use std::path::{Path, PathBuf};

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::preset_def::PresetKind;
use manifold_gpu::GpuDevice;
use manifold_renderer::preset_thumbnail::{
    THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH, factory_thumbnail_path, render_preset_thumbnail_to_file,
};
use sha2::Digest;

const ASSET_SUBDIRS: &[(&str, PresetKind)] = &[
    ("assets/effect-presets", PresetKind::Effect),
    ("assets/generator-presets", PresetKind::Generator),
];

const CONTACT_SHEET_COLUMNS: usize = 8;

fn main() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let device = std::sync::Arc::new(GpuDevice::new());

    let mut total = 0usize;
    let mut written = 0usize;
    let mut failures: Vec<(String, String)> = Vec::new();
    // (kind, id, png path) in row-major grid order, for the contact sheet.
    let mut grid: Vec<(PresetKind, String, PathBuf)> = Vec::new();

    for (subdir, kind) in ASSET_SUBDIRS {
        let dir = manifest_dir.join(subdir);
        let entries = match sorted_json_entries(&dir) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("error: cannot read {}: {e}", dir.display());
                std::process::exit(2);
            }
        };
        for (path, id) in entries {
            total += 1;
            match render_one(&device, *kind, &path, &id) {
                Ok(out_path) => {
                    written += 1;
                    println!("OK   {id} -> {}", out_path.display());
                    grid.push((*kind, id, out_path));
                }
                Err(msg) => failures.push((id, msg)),
            }
        }
    }

    for (id, msg) in &failures {
        println!("FAIL {id}: {msg}");
    }

    println!("\n{total} presets: {written} thumbnails written, {} failed", failures.len());

    if failures.is_empty() {
        match write_contact_sheet(manifest_dir, &grid) {
            Ok(sheet) => println!("contact sheet -> {}", sheet.display()),
            Err(e) => eprintln!("warning: contact sheet failed: {e}"),
        }
        println!("\nGRID ORDER (row-major):");
        for (_, id, _) in &grid {
            println!("{id}");
        }
    }

    if !failures.is_empty() {
        std::process::exit(1);
    }
}

/// Sorted (path, id) pairs for every `.json` in `dir` — `read_dir` order is
/// filesystem-dependent, and both the contact-sheet grid and reproducible
/// logs need a stable order.
fn sorted_json_entries(dir: &Path) -> std::io::Result<Vec<(PathBuf, String)>> {
    let mut entries: Vec<(PathBuf, String)> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .filter_map(|p| {
            let id = p.file_stem().and_then(|s| s.to_str())?.to_string();
            Some((p, id))
        })
        .collect();
    entries.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(entries)
}

fn render_one(
    device: &std::sync::Arc<GpuDevice>,
    kind: PresetKind,
    json_path: &Path,
    id: &str,
) -> Result<std::path::PathBuf, String> {
    let json_bytes = std::fs::read(json_path).map_err(|e| format!("read: {e}"))?;
    let def: EffectGraphDef =
        serde_json::from_slice(&json_bytes).map_err(|e| format!("parse: {e}"))?;

    let out_path =
        factory_thumbnail_path(kind, id).ok_or_else(|| "no thumbnail root resolved".to_string())?;
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }

    render_preset_thumbnail_to_file(device, kind, &def, THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT, &out_path)?;

    // D7 freshness sidecar: the SHA-256 of the preset JSON bytes. Editing the
    // preset without re-running this bin fails `factory_thumbnails_fresh`.
    let digest = format!("{:x}", sha2::Sha256::digest(&json_bytes));
    std::fs::write(out_path.with_extension("hash"), format!("{digest}\n"))
        .map_err(|e| format!("write hash sidecar: {e}"))?;

    Ok(out_path)
}

/// P2 acceptance demo: a plain grid montage (no labels) of every thumbnail at
/// `<workspace root>/target/thumbnail-contact-sheet.png`, cells in the same
/// row-major order as the GRID ORDER print.
fn write_contact_sheet(
    manifest_dir: &Path,
    grid: &[(PresetKind, String, PathBuf)],
) -> Result<PathBuf, String> {
    use image::{ImageBuffer, Rgba};

    let cell_w = THUMBNAIL_WIDTH as usize;
    let cell_h = THUMBNAIL_HEIGHT as usize;
    let cols = CONTACT_SHEET_COLUMNS;
    let rows = grid.len().div_ceil(cols);
    let mut sheet =
        ImageBuffer::<Rgba<u8>, Vec<u8>>::new((cols * cell_w) as u32, (rows * cell_h) as u32);

    for (i, (_, _, png_path)) in grid.iter().enumerate() {
        let thumb = image::open(png_path)
            .map_err(|e| format!("decode {}: {e}", png_path.display()))?
            .to_rgba8();
        let col = i % cols;
        let row = i / cols;
        // `image::imageops::overlay` pastes at (x, y) without resizing;
        // thumbnails are exactly cell-sized.
        image::imageops::overlay(
            &mut sheet,
            &thumb,
            (col * cell_w) as i64,
            (row * cell_h) as i64,
        );
    }

    let sheet_path = manifest_dir
        .join("..")
        .join("..")
        .join("target")
        .join("thumbnail-contact-sheet.png");
    if let Some(parent) = sheet_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    sheet
        .save(&sheet_path)
        .map_err(|e| format!("save {}: {e}", sheet_path.display()))?;
    Ok(sheet_path)
}
