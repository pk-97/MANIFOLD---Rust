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
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwap;
use manifold_core::project::EmbeddedOrigin;

/// Monotonic catalog generation counter. Starts at 0 and is bumped by the
/// hot-reload watcher (after both the catalog snapshots and the core
/// registry have been refreshed) so live consumers can detect that the
/// preset data changed.
///
/// The render path reads this **once per chain dispatch** with a single
/// relaxed atomic load and compares it to the generation it last built
/// against. At rest the value never changes, the comparison is equal, and
/// the rebuild slow-path is skipped — preserving the byte-identical
/// at-rest invariant. Authoring-time edits bump it, which forces the
/// affected chains to rebuild from the new defs on the next frame.
static CATALOG_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Current catalog generation. One relaxed atomic load; safe to call on the
/// per-frame path.
#[inline]
pub fn catalog_generation() -> u64 {
    CATALOG_GENERATION.load(Ordering::Relaxed)
}

/// Bump the catalog generation. Called by the watcher thread **after** the
/// catalog snapshots and the core registry have been swapped, so a reader
/// that observes the new generation is guaranteed to see the new data when
/// it rebuilds. `Release` here pairs with the `Acquire`/`Relaxed` reads on
/// the consumer side (the data it gates was published by prior `store`s).
pub fn bump_catalog_generation() -> u64 {
    CATALOG_GENERATION.fetch_add(1, Ordering::Release) + 1
}

/// One scanned preset: type id (filename stem) + owned JSON content,
/// stored as `Arc<str>` so the catalog snapshot can share unchanged
/// strings across reload swaps and hand out cheap refcount clones.
struct PresetFile {
    type_id: Arc<str>,
    json: Arc<str>,
}

/// A fully-loaded, read-only preset catalog for one kind (effects or
/// generators).
///
/// ## Hot-reload (step 10)
///
/// The catalog is no longer a `LazyLock<PresetCatalog>` borrowed for the
/// process lifetime. It now lives behind an [`ArcSwap`] snapshot
/// ([`EFFECT_CATALOG`] / [`GENERATOR_CATALOG`] are
/// `LazyLock<ArcSwap<Arc<PresetCatalog>>>`). A read takes a cheap
/// `load_full` of the current `Arc<PresetCatalog>` snapshot and clones the
/// owned `String`/`Arc<str>` it needs; the watcher thread re-scans the
/// dirs and `store`s a fresh snapshot when a file changes.
///
/// This is RCU, not a lock: readers never block and never see a torn
/// catalog. **At rest** (no file changing) the only cost over the old
/// `LazyLock` borrow is one atomic pointer load per read plus the owned
/// clone, and no read happens on the per-frame render path — the catalog
/// is consulted only at chain (re)build, which the prime directive already
/// permits to do work.
///
/// JSON is stored as `Arc<str>` so a snapshot swap shares the unchanged
/// strings with the previous snapshot (only the changed file's `Arc<str>`
/// is freshly allocated) and so cloning out a value is a refcount bump,
/// not a byte copy.
pub struct PresetCatalog {
    /// `(type_id, json)` pairs, sorted by type id for stable iteration.
    entries: Vec<(Arc<str>, Arc<str>)>,
}

impl PresetCatalog {
    /// Raw JSON for `type_id`, or `None` if no preset has that id. The
    /// returned `Arc<str>` is a cheap refcount clone of the snapshot's
    /// stored string, owned by the caller — so a concurrent reload that
    /// swaps the snapshot can't invalidate it.
    pub fn json(&self, type_id: &str) -> Option<Arc<str>> {
        self.entries
            .iter()
            .find(|(id, _)| id.as_ref() == type_id)
            .map(|(_, json)| json.clone())
    }

    /// Every type id in the catalog, in sorted order (owned clones).
    pub fn type_ids(&self) -> impl Iterator<Item = Arc<str>> + '_ {
        self.entries.iter().map(|(id, _)| id.clone())
    }

    /// Every `(type_id, json)` pair, in sorted order (owned clones).
    pub fn entries(&self) -> impl Iterator<Item = (Arc<str>, Arc<str>)> + '_ {
        self.entries
            .iter()
            .map(|(id, json)| (id.clone(), json.clone()))
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

/// The effect preset catalog. Built once on first access; fail-loud if
/// the stock root is missing or empty. Lives behind an [`ArcSwap`] so the
/// watcher thread can swap in a freshly-scanned snapshot at authoring time
/// without a restart (step 10). At rest the snapshot is never re-scanned.
pub static EFFECT_CATALOG: LazyLock<ArcSwap<PresetCatalog>> =
    LazyLock::new(|| ArcSwap::from(load_catalog(&EFFECT_DIRS)));

/// The generator preset catalog. Same shape as [`EFFECT_CATALOG`].
pub static GENERATOR_CATALOG: LazyLock<ArcSwap<PresetCatalog>> =
    LazyLock::new(|| ArcSwap::from(load_catalog(&GENERATOR_DIRS)));

/// Project-scoped preset overlay (Phase 4; split into two origin tiers by
/// PRESET_LIBRARY_DESIGN D5/P2). `(type_id, json)` for the currently-loaded
/// project's `embedded_presets`, split by [`EmbeddedOrigin`] and merged in
/// [`build_catalog`] at two different tiers:
///
/// - **Saved** (`_SAVED` statics) — explicit project-scoped forks / imports /
///   Save-to-Project entries (D4/D9). Deliberate, so they merge on TOP of
///   stock+user, same as every project-overlay entry did pre-P2.
/// - **Snapshot** (`_SNAPSHOT` statics) — auto-captured at save for
///   self-containment (D5). Merges BELOW stock+user: disk wins over a stale
///   snapshot, and the snapshot is only the fallback when the library file is
///   gone.
///
/// Empty when no project is loaded or the project has no embedded presets of
/// that tier. Set via [`set_project_presets`] (which re-merges both catalogs +
/// rebuilds the core registry through [`apply_reload`]), so the per-frame
/// render path is unaffected — resolution stays a catalog read.
/// `(type_id, json)` overlay entries for one preset kind.
type OverlayEntries = Vec<(Arc<str>, Arc<str>)>;
static PROJECT_EFFECT_PRESETS_SAVED: LazyLock<ArcSwap<OverlayEntries>> =
    LazyLock::new(|| ArcSwap::from_pointee(Vec::new()));
static PROJECT_GENERATOR_PRESETS_SAVED: LazyLock<ArcSwap<OverlayEntries>> =
    LazyLock::new(|| ArcSwap::from_pointee(Vec::new()));
static PROJECT_EFFECT_PRESETS_SNAPSHOT: LazyLock<ArcSwap<OverlayEntries>> =
    LazyLock::new(|| ArcSwap::from_pointee(Vec::new()));
static PROJECT_GENERATOR_PRESETS_SNAPSHOT: LazyLock<ArcSwap<OverlayEntries>> =
    LazyLock::new(|| ArcSwap::from_pointee(Vec::new()));

/// Install the loaded project's embedded presets as the catalog overlay and
/// re-derive both catalogs + the core registry. Call on project load; call
/// with empty vecs (or [`clear_project_presets`]) on project close/switch so a
/// stale project's forks never leak into the next one. Returns the new catalog
/// generation.
///
/// `(type_id, json, origin)` triples — `json` is each preset's full
/// `EffectGraphDef` JSON (graph + `presetMetadata`); `origin` picks which
/// merge tier the entry resolves at (see the overlay doc comment above).
pub fn set_project_presets(
    effect: Vec<(String, String, EmbeddedOrigin)>,
    generator: Vec<(String, String, EmbeddedOrigin)>,
) -> u64 {
    fn split(v: Vec<(String, String, EmbeddedOrigin)>) -> (OverlayEntries, OverlayEntries) {
        let mut saved = Vec::new();
        let mut snapshot = Vec::new();
        for (id, json, origin) in v {
            let entry = (Arc::from(id.as_str()), Arc::from(json.as_str()));
            match origin {
                EmbeddedOrigin::Saved => saved.push(entry),
                EmbeddedOrigin::Snapshot => snapshot.push(entry),
            }
        }
        (saved, snapshot)
    }
    let (effect_saved, effect_snapshot) = split(effect);
    let (generator_saved, generator_snapshot) = split(generator);
    PROJECT_EFFECT_PRESETS_SAVED.store(Arc::new(effect_saved));
    PROJECT_EFFECT_PRESETS_SNAPSHOT.store(Arc::new(effect_snapshot));
    PROJECT_GENERATOR_PRESETS_SAVED.store(Arc::new(generator_saved));
    PROJECT_GENERATOR_PRESETS_SNAPSHOT.store(Arc::new(generator_snapshot));
    apply_reload()
}

/// Clear the project overlay (project close / switch). Equivalent to
/// [`set_project_presets`] with empty lists.
pub fn clear_project_presets() -> u64 {
    set_project_presets(Vec::new(), Vec::new())
}

/// The `Saved`-tier project overlay entries for a catalog kind (by
/// `KindDirs::label`) — merges on top of stock+user in [`build_catalog`].
fn project_saved_overlay_for(label: &str) -> Arc<OverlayEntries> {
    if label == EFFECT_DIRS.label {
        PROJECT_EFFECT_PRESETS_SAVED.load_full()
    } else {
        PROJECT_GENERATOR_PRESETS_SAVED.load_full()
    }
}

/// The `Snapshot`-tier project overlay entries for a catalog kind — merges
/// BELOW stock+user in [`build_catalog`] (disk wins; this is the fallback).
fn project_snapshot_overlay_for(label: &str) -> Arc<OverlayEntries> {
    if label == EFFECT_DIRS.label {
        PROJECT_EFFECT_PRESETS_SNAPSHOT.load_full()
    } else {
        PROJECT_GENERATOR_PRESETS_SNAPSHOT.load_full()
    }
}

/// Re-scan the effect preset dirs and swap in a fresh snapshot.
///
/// Crash-safe: if the re-scan resolves no stock root, or scans to zero
/// presets (a transient empty / all-malformed edit), the previous good
/// snapshot is **kept** and the failure is logged loudly — never swapped
/// to empty. Returns `true` if a new snapshot was installed.
pub fn reload_effect_catalog() -> bool {
    reload_into(&EFFECT_CATALOG, &EFFECT_DIRS)
}

/// Re-scan the generator preset dirs and swap in a fresh snapshot. Same
/// crash-safe last-good-snapshot contract as [`reload_effect_catalog`].
pub fn reload_generator_catalog() -> bool {
    reload_into(&GENERATOR_CATALOG, &GENERATOR_DIRS)
}

/// Shared reload body. Re-runs the same resolve+scan as startup but, on a
/// transient empty/broken scan, keeps the last-good snapshot instead of
/// panicking (startup panics; a live reload must not take the show down).
fn reload_into(slot: &ArcSwap<PresetCatalog>, dirs: &KindDirs) -> bool {
    match try_load_catalog(dirs) {
        Ok(catalog) => {
            slot.store(catalog);
            true
        }
        Err(reason) => {
            log::error!(
                "[presets] reload of {} presets failed ({reason}); keeping the last-good catalog",
                dirs.label,
            );
            false
        }
    }
}

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
        let type_id: Arc<str> = Arc::from(type_id);

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

        out.push(PresetFile {
            type_id,
            json: Arc::from(json),
        });
    }

    out
}

/// Build a catalog for one kind at STARTUP: resolve + scan the stock root
/// (fail-loud if missing or empty), then overlay the optional user root
/// (overrides log). Panics on failure — the app must never come up with an
/// empty catalog. The crash-safe reload path uses [`try_load_catalog`]
/// instead, which returns `Err` rather than panicking.
fn load_catalog(dirs: &KindDirs) -> Arc<PresetCatalog> {
    match try_load_catalog(dirs) {
        Ok(catalog) => catalog,
        Err(reason) => panic!(
            "[presets] FATAL: {reason}\n\
             The app cannot start with an empty preset catalog. For a packaged \
             build, ensure presets were copied into \
             `<App>.app/Contents/Resources/presets/{}/`. For a dev build, ensure \
             `{}/{}` exists.",
            dirs.bundle_subdir,
            env!("CARGO_MANIFEST_DIR"),
            dirs.dev_subdir,
        ),
    }
}

/// Fallible catalog build shared by startup and reload. Resolves the stock
/// root and scans it; returns `Err(reason)` if the stock root can't be
/// resolved or scans to zero presets. Startup turns that `Err` into a
/// panic; reload logs it and keeps the last-good snapshot.
fn try_load_catalog(dirs: &KindDirs) -> Result<Arc<PresetCatalog>, String> {
    let (stock_root, tried) = resolve_stock_root(dirs);
    let Some(stock_root) = stock_root else {
        let candidates = tried
            .iter()
            .map(|p| format!("    - {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!(
            "could not resolve the stock {} preset directory. Tried (first existing wins):\n{candidates}",
            dirs.label,
        ));
    };

    let user_root = resolve_user_root(dirs);
    build_catalog(dirs.label, &stock_root, user_root.as_deref())
}

/// The catalog assembly + empty-scan check, factored out of the
/// path-resolution so the empty-scan behaviour can be unit-tested against a
/// real (empty) directory without touching `CARGO_MANIFEST_DIR`.
///
/// `stock_root` is the resolved stock directory (already known to exist).
/// `user_root`, if `Some`, is overlaid on top (override on stem match).
/// Returns `Err` (rather than panicking) when the stock root scans to zero
/// presets, so the reload path can keep its last-good snapshot; the startup
/// path converts that `Err` into the fail-loud panic.
fn build_catalog(
    label: &str,
    stock_root: &Path,
    user_root: Option<&Path>,
) -> Result<Arc<PresetCatalog>, String> {
    log::info!(
        "[presets] scanning stock {label} presets from {}",
        stock_root.display()
    );
    let stock = scan_dir(stock_root);

    if stock.is_empty() {
        return Err(format!(
            "stock {label} preset directory {} scanned to ZERO presets. Check that the \
             directory contains valid `*.json` preset files (malformed files are \
             skipped with a logged error — look above for per-file errors).",
            stock_root.display(),
        ));
    }

    // Merge order (PRESET_LIBRARY_DESIGN D5/P2): Snapshot overlay (bottom) →
    // stock → user → Saved overlay (top). Snapshot entries are save-time
    // self-containment plumbing (D5) — disk (stock/user) always wins over
    // one, so an improved/restored library file is never shadowed by a
    // stale cache; Snapshot only serves when disk has nothing for that id.
    // Saved entries are deliberate, explicit project-scoped forks/imports
    // (D4/D9) and keep today's on-top-of-everything behavior unchanged.
    let snapshot_overlay = project_snapshot_overlay_for(label);
    let snapshot_ids: std::collections::HashSet<Arc<str>> =
        snapshot_overlay.iter().map(|(id, _)| id.clone()).collect();
    let mut merged: Vec<(Arc<str>, Arc<str>)> = snapshot_overlay.iter().cloned().collect();
    if !snapshot_overlay.is_empty() {
        log::info!(
            "[presets] starting from {} project snapshot {label} preset(s) (D5 self-containment fallback)",
            snapshot_overlay.len()
        );
    }

    // Stock overrides any Snapshot entry with the same id (disk wins).
    let mut disk_ids: std::collections::HashSet<Arc<str>> = std::collections::HashSet::new();
    for f in stock {
        disk_ids.insert(f.type_id.clone());
        if let Some(slot) = merged.iter_mut().find(|(id, _)| *id == f.type_id) {
            slot.1 = f.json;
        } else {
            merged.push((f.type_id, f.json));
        }
    }

    if let Some(user_root) = user_root {
        log::info!(
            "[presets] scanning user {label} presets from {}",
            user_root.display()
        );
        for f in scan_dir(user_root) {
            disk_ids.insert(f.type_id.clone());
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

    // Loud log: a Snapshot entry whose library file is gone from BOTH stock
    // and user disk. This is the exact D5 fallback firing — a saved show
    // still resolves its preset from the project's own cache instead of
    // stranding. Logged every catalog build (startup + every hot-reload /
    // overlay install), deliberately — it's meant to be seen, not buried.
    for id in &snapshot_ids {
        if !disk_ids.contains(id) {
            log::warn!(
                "[presets] `{id}` has no {label} file on disk — resolving `{id}` from the \
                 project's saved snapshot (D5 self-containment fallback)"
            );
        }
    }

    // Saved-tier project overlay (Phase 4 / D4 / D9): explicit project-scoped
    // forks / imports / Save-to-Project entries, on top of stock+user
    // (override on id match) — unchanged from pre-P2 behavior.
    let saved_overlay = project_saved_overlay_for(label);
    if !saved_overlay.is_empty() {
        log::info!("[presets] merging {} project {label} preset(s)", saved_overlay.len());
        for (id, json) in saved_overlay.iter() {
            if let Some(slot) = merged.iter_mut().find(|(i, _)| i == id) {
                slot.1 = json.clone();
            } else {
                merged.push((id.clone(), json.clone()));
            }
        }
    }

    // Stable sort by type id — same iteration order every launch, which
    // the catalog/drift consumers rely on.
    merged.sort_by(|a, b| a.0.cmp(&b.0));

    log::info!("[presets] loaded {} {label} presets", merged.len());

    Ok(Arc::new(PresetCatalog { entries: merged }))
}

// ─── Hot-reload watcher (step 10) ───
//
// A single background thread that polls the preset directories' mtimes
// every ~1s. It does **no work on the render or content tick path** — it's
// a normal detached thread. When it detects any added / removed / changed
// `*.json` under the watched roots, it reloads both catalog snapshots,
// rebuilds the core definition registry from the freshly-loaded metadata,
// then bumps `CATALOG_GENERATION`. Live chains observe the new generation
// on their next dispatch (a single atomic load) and rebuild from the new
// defs; the catalog default also updates so newly-created instances use
// the new JSON.
//
// mtime-poll, not the `notify` crate: simpler, no extra dependency, and
// lower-risk for a live rig (no FS-event backend quirks). 1s latency is
// fine for authoring.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

/// Guards against starting the watcher twice (idempotent startup).
static WATCHER_STARTED: AtomicBool = AtomicBool::new(false);

/// Every directory the watcher polls: the resolved stock + optional user
/// roots for both effects and generators. Returns only directories that
/// currently exist.
fn watched_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for kind in [&EFFECT_DIRS, &GENERATOR_DIRS] {
        if let (Some(stock), _) = resolve_stock_root(kind) {
            dirs.push(stock);
        }
        if let Some(user) = resolve_user_root(kind) {
            dirs.push(user);
        }
    }
    dirs
}

/// A cheap fingerprint of a directory's `*.json` files: the count plus the
/// max mtime. Catches add / remove (count changes) and edit (mtime moves).
/// Returns `(count, latest_mtime_nanos)`.
fn dir_fingerprint(dir: &Path) -> (usize, u128) {
    let mut count = 0usize;
    let mut latest = 0u128;
    let Ok(rd) = fs::read_dir(dir) else {
        return (0, 0);
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        count += 1;
        if let Ok(meta) = entry.metadata()
            && let Ok(modified) = meta.modified()
            && let Ok(dur) = modified.duration_since(SystemTime::UNIX_EPOCH)
        {
            latest = latest.max(dur.as_nanos());
        }
    }
    (count, latest)
}

/// Combined fingerprint across all watched dirs.
fn snapshot_fingerprint(dirs: &[PathBuf]) -> Vec<(usize, u128)> {
    dirs.iter().map(|d| dir_fingerprint(d)).collect()
}

/// Reload both catalogs, rebuild the core registry from the freshly-loaded
/// metadata, then bump the generation. Returns the new generation.
///
/// Order matters: catalogs first (they feed the metadata loaders the
/// registry rebuild reads), then the registry, then the generation bump —
/// so any reader that observes the new generation is guaranteed to see the
/// new catalog + registry data.
fn apply_reload() -> u64 {
    let effect_changed = reload_effect_catalog();
    let generator_changed = reload_generator_catalog();
    if !effect_changed && !generator_changed {
        // Every reload attempt failed (transient empty/broken edit); the
        // last-good snapshots were kept. Don't bump — nothing changed for
        // consumers, and bumping would force a needless rebuild against
        // identical data.
        log::warn!("[presets] hot-reload: all reload attempts failed; keeping last-good, not bumping generation");
        return catalog_generation();
    }

    // Rebuild the core definition registry from the reloaded catalog's
    // metadata. The metadata loaders read the current (just-swapped)
    // catalog snapshot.
    let effect_meta = crate::node_graph::loaded_presets_from_bundled();
    let generator_meta =
        crate::generators::bundled_generator_presets::loaded_generator_presets_from_bundled();
    // ONE atomic swap of the merged store — both kinds' metadata in a single
    // rebuild so a reader observing the new generation never sees a
    // half-merged registry.
    manifold_core::preset_definition_registry::rebuild_preset_definitions(
        &effect_meta,
        &generator_meta,
    );
    // the Add-effect/Add-generator
    // browser popup reads `preset_type_registry`, a SEPARATE store from
    // `PRESET_DEFINITIONS` above — it used to be a `LazyLock` computed once
    // and never refreshed, so a preset saved into the user dir at runtime
    // never appeared in the browser without a restart. Rebuilt from the same
    // freshly-reloaded metadata, in the same reload pass.
    //
    // `effect_meta`/`generator_meta` come from the FULL merged catalog
    // (stock + user + project overlay — `build_catalog`'s merge order), but
    // the registry must stay STOCK + USER only: project-embedded presets
    // (Saved and Snapshot) are already surfaced separately as the "Project"
    // category from `Project.embedded_presets` (`ui_root.rs`'s browser-open
    // handlers). Feeding them into the registry too would list the same
    // preset twice in the Add browser, so the current project's overlay ids
    // are excluded here.
    let effect_overlay_ids: std::collections::HashSet<Arc<str>> = PROJECT_EFFECT_PRESETS_SAVED
        .load_full()
        .iter()
        .chain(PROJECT_EFFECT_PRESETS_SNAPSHOT.load_full().iter())
        .map(|(id, _)| id.clone())
        .collect();
    let generator_overlay_ids: std::collections::HashSet<Arc<str>> = PROJECT_GENERATOR_PRESETS_SAVED
        .load_full()
        .iter()
        .chain(PROJECT_GENERATOR_PRESETS_SNAPSHOT.load_full().iter())
        .map(|(id, _)| id.clone())
        .collect();
    let effect_meta_for_registry: Vec<_> = effect_meta
        .iter()
        .filter(|m| !effect_overlay_ids.contains(m.id.as_str()))
        .cloned()
        .collect();
    let generator_meta_for_registry: Vec<_> = generator_meta
        .iter()
        .filter(|m| !generator_overlay_ids.contains(m.id.as_str()))
        .cloned()
        .collect();
    manifold_core::preset_type_registry::rebuild(&effect_meta_for_registry, &generator_meta_for_registry);

    let generation = bump_catalog_generation();
    log::info!("[presets] hot-reload applied; catalog generation = {generation}");
    generation
}

/// Start the preset hot-reload watcher. Idempotent — only the first call
/// spawns the thread. Call once at app startup (after the catalogs have
/// been force-loaded so the watcher's first fingerprint is taken against
/// the real, validated catalog). The thread is detached and runs for the
/// process lifetime; it never touches the render or content tick path.
pub fn start_preset_watcher() {
    if WATCHER_STARTED.swap(true, Ordering::SeqCst) {
        return; // already running
    }

    // Force the catalogs to load now (fail-loud happens here if a stock
    // root is missing) so the watcher baseline reflects a valid catalog.
    LazyLock::force(&EFFECT_CATALOG);
    LazyLock::force(&GENERATOR_CATALOG);

    let dirs = watched_dirs();
    if dirs.is_empty() {
        log::warn!("[presets] hot-reload watcher: no preset directories resolved; not starting");
        return;
    }
    log::info!(
        "[presets] hot-reload watcher polling {} preset dir(s) every 1s",
        dirs.len()
    );

    std::thread::Builder::new()
        .name("preset-watcher".into())
        .spawn(move || {
            let mut last = snapshot_fingerprint(&dirs);
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let current = snapshot_fingerprint(&dirs);
                if current != last {
                    log::info!("[presets] hot-reload: preset directory change detected");
                    apply_reload();
                    // Re-fingerprint AFTER reload so a reload that itself
                    // touches mtimes doesn't retrigger.
                    last = snapshot_fingerprint(&dirs);
                }
            }
        })
        .expect("failed to spawn preset-watcher thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dev stock root must resolve (via `CARGO_MANIFEST_DIR`) and
    /// scan to a non-empty set when no packaged bundle is present.
    #[test]
    fn dev_effect_catalog_is_non_empty() {
        assert!(
            !EFFECT_CATALOG.load().is_empty(),
            "effect catalog must load from the dev assets dir",
        );
    }

    #[test]
    fn dev_generator_catalog_is_non_empty() {
        assert!(
            !GENERATOR_CATALOG.load().is_empty(),
            "generator catalog must load from the dev assets dir",
        );
    }

    /// Type ids are filename stems and the catalog is sorted.
    #[test]
    fn catalog_is_sorted_by_type_id() {
        let ids: Vec<Arc<str>> = EFFECT_CATALOG.load().type_ids().collect();
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

    /// FAIL-LOUD: at startup a stock root that exists but scans to zero
    /// presets must panic. Reproduced through the startup wrapper
    /// `load_catalog` semantics: `build_catalog` returns the `Err` that
    /// `load_catalog` turns into a panic. Here we assert the `Err`.
    #[test]
    fn empty_stock_dir_errors() {
        let dir = scratch("empty");
        let err = match build_catalog("effect", &dir, None) {
            Ok(_) => panic!("empty stock dir must produce an Err (startup turns it into a panic)"),
            Err(e) => e,
        };
        assert!(
            err.contains("scanned to ZERO presets"),
            "error must name the empty-scan condition, got: {err}",
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// A malformed JSON file is skipped (logged), not fatal — the rest
    /// load. Verifies the per-file error-reporting contract.
    #[test]
    fn malformed_file_is_skipped_not_fatal() {
        let dir = scratch("malformed");
        write_preset(&dir, "Good", "Good");
        fs::write(dir.join("Bad.json"), "{ this is not json").expect("write bad");
        let cat = build_catalog("effect", &dir, None).expect("good file must load");
        let ids: Vec<Arc<str>> = cat.type_ids().collect();
        assert_eq!(
            ids.iter().map(|s| s.as_ref()).collect::<Vec<_>>(),
            vec!["Good"],
            "malformed Bad.json must be skipped",
        );
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

        let cat = build_catalog("effect", &stock, Some(&user)).expect("merge loads");

        // Shared resolves to the user JSON.
        let shared = cat.json("Shared").expect("Shared present");
        assert!(
            shared.contains("UserVersion"),
            "user preset must override stock by stem, got: {shared}",
        );
        // Both unique ids survive; catalog sorted.
        let ids: Vec<Arc<str>> = cat.type_ids().collect();
        assert_eq!(
            ids.iter().map(|s| s.as_ref()).collect::<Vec<_>>(),
            vec!["Shared", "StockOnly", "UserOnly"],
        );

        let _ = fs::remove_dir_all(&stock);
        let _ = fs::remove_dir_all(&user);
    }

    /// HOT-RELOAD: editing a preset file and reloading swaps the catalog
    /// snapshot — the new value is visible, the old `Arc` snapshot held
    /// across the swap still reads the old value (RCU, no torn reads).
    #[test]
    fn reload_swaps_catalog_snapshot() {
        let stock = scratch("reload-stock");
        write_preset(&stock, "Alpha", "AlphaV1");

        let slot = ArcSwap::from(build_catalog("effect", &stock, None).expect("initial load"));

        // Hold the pre-edit snapshot — proves RCU: the swap must not
        // mutate it underneath us.
        let pre = slot.load_full();
        assert!(pre.json("Alpha").unwrap().contains("AlphaV1"));

        // Edit the file on disk and reload (same body the watcher calls).
        write_preset(&stock, "Alpha", "AlphaV2");
        let reloaded = build_catalog("effect", &stock, None).expect("reload load");
        slot.store(reloaded);

        // New snapshot sees the edit; the retained old snapshot does not.
        assert!(
            slot.load().json("Alpha").unwrap().contains("AlphaV2"),
            "reload must surface the edited JSON",
        );
        assert!(
            pre.json("Alpha").unwrap().contains("AlphaV1"),
            "the pre-swap snapshot must still read the old bytes (RCU)",
        );

        let _ = fs::remove_dir_all(&stock);
    }

    /// HOT-RELOAD crash-safety: a reload whose scan comes back empty (every
    /// file deleted, or all malformed) must NOT swap an empty catalog in —
    /// the last-good snapshot is kept. Mirrors `reload_into`'s contract.
    #[test]
    fn reload_keeps_last_good_on_empty_or_malformed() {
        let stock = scratch("reload-keep-stock");
        write_preset(&stock, "Beta", "BetaGood");

        let slot = ArcSwap::from(build_catalog("effect", &stock, None).expect("initial load"));

        // Simulate a transient bad edit: delete the only good file and
        // leave only a malformed one (scan_dir skips it → zero presets).
        fs::remove_file(stock.join("Beta.json")).expect("remove good");
        fs::write(stock.join("Beta.json"), "{ broken").expect("write broken");

        // The reload body (build_catalog) must Err on the empty scan; the
        // `reload_into` contract keeps the prior snapshot.
        let attempt = build_catalog("effect", &stock, None);
        assert!(attempt.is_err(), "all-malformed scan must Err, not swap empty");
        // Caller keeps last-good — emulate `reload_into` not storing on Err.
        assert!(
            slot.load().json("Beta").unwrap().contains("BetaGood"),
            "last-good snapshot must survive a broken reload",
        );

        let _ = fs::remove_dir_all(&stock);
    }
}
