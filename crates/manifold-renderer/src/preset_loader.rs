//! Runtime preset catalog loader.
//!
//! Stock effect and generator presets are JSON files scanned from disk
//! at startup — the same way user presets load (the Ableton factory-pack
//! model). The binary embeds NO preset JSON; there is no compile-time
//! knowledge of which effects/generators exist.
//!
//! ## Scan roots
//!
//! For each kind (`effects` / `generators`) two roots are scanned:
//!
//! - **STOCK** — resolved in this order, first existing wins:
//!   1. Packaged macOS bundle: `<dir-of-exe>/../Resources/presets/{effects,generators}`
//!   2. Dev workspace: `<CARGO_MANIFEST_DIR>/assets/{effect-presets,generator-presets}`
//!      (manifold-renderer's manifest dir, baked at compile time).
//! - **USER** — `~/Library/Application Support/MANIFOLD/presets/{effects,generators}`
//!   (same base dir as `prefs.json`). Optional; absent is fine.
//!
//! ## Type ids
//!
//! The type id of a preset is its **filename stem**, exactly as the old
//! `build.rs` did it. Type ids are forever — save files reference them.
//!
//! ## Collision rule
//!
//! A user preset with the same type id (stem) OVERRIDES the stock one.
//! Each override is logged.
//!
//! ## Fail-loud
//!
//! This is a live-performance rig. If a STOCK root cannot be resolved,
//! or it scans to ZERO presets, the loader `panic!`s at startup with the
//! paths it tried. The app must never come up with an empty catalog.
//!
//! A malformed individual JSON file is a loud per-file error (logged,
//! naming the file) and is skipped — the rest still load. That is error
//! reporting, not a silent fallback.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// One scanned preset: type id (filename stem) + owned JSON content.
struct PresetFile {
    type_id: String,
    json: String,
}

/// A fully-loaded, read-only preset catalog for one kind (effects or
/// generators). Built once at first access via [`LazyLock`]; the owned
/// strings live for the lifetime of the process, so borrows handed out
/// to callers are `'static`.
pub struct PresetCatalog {
    /// `(type_id, json)` pairs, sorted by type id for stable iteration.
    entries: Vec<(String, String)>,
}

impl PresetCatalog {
    /// Raw JSON for `type_id`, or `None` if no preset has that id.
    pub fn json(&self, type_id: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(id, _)| id == type_id)
            .map(|(_, json)| json.as_str())
    }

    /// Every type id in the catalog, in sorted order.
    pub fn type_ids(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(id, _)| id.as_str())
    }

    /// Every `(type_id, json)` pair, in sorted order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(id, json)| (id.as_str(), json.as_str()))
    }

    /// Number of presets loaded.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Which sub-directory names a preset kind uses under each root.
struct KindDirs {
    /// Human label for log + panic messages ("effect" / "generator").
    label: &'static str,
    /// Sub-dir under the packaged-bundle `Resources/presets/` and under
    /// the user `presets/` root (e.g. `"effects"`).
    bundle_subdir: &'static str,
    /// Sub-dir under the dev workspace assets root
    /// (`<CARGO_MANIFEST_DIR>/...`, e.g. `"assets/effect-presets"`).
    dev_subdir: &'static str,
}

const EFFECT_DIRS: KindDirs = KindDirs {
    label: "effect",
    bundle_subdir: "effects",
    dev_subdir: "assets/effect-presets",
};

const GENERATOR_DIRS: KindDirs = KindDirs {
    label: "generator",
    bundle_subdir: "generators",
    dev_subdir: "assets/generator-presets",
};

/// The effect preset catalog. Built once; fail-loud if the stock root is
/// missing or empty.
pub static EFFECT_CATALOG: LazyLock<PresetCatalog> =
    LazyLock::new(|| load_catalog(&EFFECT_DIRS));

/// The generator preset catalog. Built once; fail-loud if the stock root
/// is missing or empty.
pub static GENERATOR_CATALOG: LazyLock<PresetCatalog> =
    LazyLock::new(|| load_catalog(&GENERATOR_DIRS));

/// Resolve the STOCK root for a kind. First existing directory wins:
/// packaged bundle `Resources/presets/<subdir>`, then the dev workspace
/// assets dir baked from `CARGO_MANIFEST_DIR`. Returns the resolved path
/// plus the full list of candidates that were tried (for the fail-loud
/// message).
fn resolve_stock_root(dirs: &KindDirs) -> (Option<PathBuf>, Vec<PathBuf>) {
    let mut tried = Vec::new();

    // (a) Packaged macOS .app bundle: <dir-of-exe>/../Resources/presets/<subdir>
    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        let bundle = exe_dir
            .join("..")
            .join("Resources")
            .join("presets")
            .join(dirs.bundle_subdir);
        tried.push(bundle.clone());
        if bundle.is_dir() {
            return (Some(bundle), tried);
        }
    }

    // (b) Dev workspace: <CARGO_MANIFEST_DIR>/assets/<...>
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(dirs.dev_subdir);
    tried.push(dev.clone());
    if dev.is_dir() {
        return (Some(dev), tried);
    }

    (None, tried)
}

/// Resolve the optional USER root for a kind:
/// `~/Library/Application Support/MANIFOLD/presets/<subdir>`. `None` if
/// `$HOME` is unset or the directory doesn't exist.
fn resolve_user_root(dirs: &KindDirs) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let dir = PathBuf::from(home)
        .join("Library/Application Support/MANIFOLD/presets")
        .join(dirs.bundle_subdir);
    dir.is_dir().then_some(dir)
}

/// Scan a directory for `*.json` files, reading each into an owned
/// `PresetFile`. A file that fails to read or fails a structural JSON
/// parse is logged loudly (naming the file) and skipped — the rest load.
fn scan_dir(dir: &Path) -> Vec<PresetFile> {
    let mut out = Vec::new();
    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            log::error!(
                "[presets] cannot read preset directory {}: {e}",
                dir.display()
            );
            return out;
        }
    };

    for entry in read_dir {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                log::error!("[presets] dir entry error in {}: {e}", dir.display());
                continue;
            }
        };
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }

        let Some(type_id) = path.file_stem().and_then(|s| s.to_str()) else {
            log::error!("[presets] preset file has no valid UTF-8 stem, skipping: {}", path.display());
            continue;
        };
        let type_id = type_id.to_string();

        let json = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                log::error!("[presets] failed to read preset file {}: {e} — skipping", path.display());
                continue;
            }
        };

        // Structural sanity: must parse as JSON. Deeper validation
        // (typeIds, bindings) happens when the def hits the graph loader.
        // A malformed file is reported loudly and skipped, not fatal.
        if let Err(e) = serde_json::from_str::<serde_json::Value>(&json) {
            log::error!(
                "[presets] preset file {} is not valid JSON: {e} — skipping",
                path.display()
            );
            continue;
        }

        out.push(PresetFile { type_id, json });
    }

    out
}

/// Build a catalog for one kind: resolve + scan the stock root (fail-loud
/// if missing or empty), then overlay the optional user root (overrides
/// log).
fn load_catalog(dirs: &KindDirs) -> PresetCatalog {
    let (stock_root, tried) = resolve_stock_root(dirs);
    let Some(stock_root) = stock_root else {
        let candidates = tried
            .iter()
            .map(|p| format!("    - {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n");
        panic!(
            "[presets] FATAL: could not resolve the stock {} preset directory. \
             Tried (first existing wins):\n{candidates}\n\
             The app cannot start with an empty preset catalog. For a packaged \
             build, ensure presets were copied into \
             `<App>.app/Contents/Resources/presets/{}/`. For a dev build, ensure \
             `{}/{}` exists.",
            dirs.label,
            dirs.bundle_subdir,
            env!("CARGO_MANIFEST_DIR"),
            dirs.dev_subdir,
        );
    };

    let user_root = resolve_user_root(dirs);
    build_catalog(dirs.label, &stock_root, user_root.as_deref())
}

/// The catalog assembly + fail-loud-on-empty logic, factored out of the
/// path-resolution so the empty-scan panic can be unit-tested against a
/// real (empty) directory without touching `CARGO_MANIFEST_DIR`.
///
/// `stock_root` is the resolved stock directory (already known to exist).
/// `user_root`, if `Some`, is overlaid on top (override on stem match).
fn build_catalog(label: &str, stock_root: &Path, user_root: Option<&Path>) -> PresetCatalog {
    log::info!(
        "[presets] scanning stock {label} presets from {}",
        stock_root.display()
    );
    let stock = scan_dir(stock_root);

    if stock.is_empty() {
        panic!(
            "[presets] FATAL: stock {label} preset directory {} scanned to ZERO presets. \
             The app cannot start with an empty preset catalog. Check that the \
             directory contains valid `*.json` preset files (malformed files are \
             skipped with a logged error — look above for per-file errors).",
            stock_root.display(),
        );
    }

    // Merge: start with stock, then overlay user (override on stem match).
    let mut merged: Vec<(String, String)> =
        stock.into_iter().map(|f| (f.type_id, f.json)).collect();

    if let Some(user_root) = user_root {
        log::info!(
            "[presets] scanning user {label} presets from {}",
            user_root.display()
        );
        for f in scan_dir(user_root) {
            if let Some(slot) = merged.iter_mut().find(|(id, _)| *id == f.type_id) {
                log::info!(
                    "[presets] user {label} preset `{}` overrides the stock one",
                    f.type_id
                );
                slot.1 = f.json;
            } else {
                log::info!(
                    "[presets] user {label} preset `{}` added (no stock equivalent)",
                    f.type_id
                );
                merged.push((f.type_id, f.json));
            }
        }
    }

    // Stable sort by type id — same iteration order every launch, which
    // the catalog/drift consumers rely on.
    merged.sort_by(|a, b| a.0.cmp(&b.0));

    log::info!("[presets] loaded {} {label} presets", merged.len());

    PresetCatalog { entries: merged }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dev stock root must resolve (via `CARGO_MANIFEST_DIR`) and
    /// scan to a non-empty set when no packaged bundle is present.
    #[test]
    fn dev_effect_catalog_is_non_empty() {
        assert!(
            !EFFECT_CATALOG.is_empty(),
            "effect catalog must load from the dev assets dir",
        );
    }

    #[test]
    fn dev_generator_catalog_is_non_empty() {
        assert!(
            !GENERATOR_CATALOG.is_empty(),
            "generator catalog must load from the dev assets dir",
        );
    }

    /// Type ids are filename stems and the catalog is sorted.
    #[test]
    fn catalog_is_sorted_by_type_id() {
        let ids: Vec<&str> = EFFECT_CATALOG.type_ids().collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "effect catalog must be sorted by type id");
    }

    /// Unique scratch dir per test, cleaned up at the end.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "manifold-preset-{tag}-{}-{}",
            std::process::id(),
            // monotonic-ish suffix so concurrent tests don't collide
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn write_preset(dir: &Path, stem: &str, name: &str) {
        let json = format!(
            r#"{{"version":2,"nodes":[],"wires":[],"name":"{name}"}}"#
        );
        fs::write(dir.join(format!("{stem}.json")), json).expect("write preset");
    }

    /// FAIL-LOUD: a stock root that exists but scans to zero presets must
    /// panic with the documented message. Exercises the real
    /// `build_catalog` empty-scan branch against a real empty directory.
    #[test]
    #[should_panic(expected = "scanned to ZERO presets")]
    fn empty_stock_dir_panics() {
        let dir = scratch("empty");
        // Real empty dir → build_catalog must panic.
        build_catalog("effect", &dir, None);
    }

    /// A malformed JSON file is skipped (logged), not fatal — the rest
    /// load. Verifies the per-file error-reporting contract.
    #[test]
    fn malformed_file_is_skipped_not_fatal() {
        let dir = scratch("malformed");
        write_preset(&dir, "Good", "Good");
        fs::write(dir.join("Bad.json"), "{ this is not json").expect("write bad");
        let cat = build_catalog("effect", &dir, None);
        let ids: Vec<&str> = cat.type_ids().collect();
        assert_eq!(ids, vec!["Good"], "malformed Bad.json must be skipped");
        let _ = fs::remove_dir_all(&dir);
    }

    /// User preset with a colliding stem OVERRIDES the stock one; a
    /// non-colliding user preset is added.
    #[test]
    fn user_preset_overrides_stock_by_stem() {
        let stock = scratch("stock");
        let user = scratch("user");
        write_preset(&stock, "Shared", "StockVersion");
        write_preset(&stock, "StockOnly", "StockOnly");
        write_preset(&user, "Shared", "UserVersion");
        write_preset(&user, "UserOnly", "UserOnly");

        let cat = build_catalog("effect", &stock, Some(&user));

        // Shared resolves to the user JSON.
        let shared = cat.json("Shared").expect("Shared present");
        assert!(
            shared.contains("UserVersion"),
            "user preset must override stock by stem, got: {shared}",
        );
        // Both unique ids survive; catalog sorted.
        let ids: Vec<&str> = cat.type_ids().collect();
        assert_eq!(ids, vec!["Shared", "StockOnly", "UserOnly"]);

        let _ = fs::remove_dir_all(&stock);
        let _ = fs::remove_dir_all(&user);
    }
}
