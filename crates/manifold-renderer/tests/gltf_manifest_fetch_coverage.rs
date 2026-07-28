//! Guards against `tests/fixtures/gltf/khronos/manifest.json` drifting ahead
//! of `scripts/fetch-gltf-conformance.sh` — a manifest row naming an asset
//! the fetch script has no mechanism to ever produce, which would leave that
//! row permanently `SKIPPED` in `glb_conformance.rs` (D1's documented
//! skip-if-absent behavior) with no signal it's dead rather than merely
//! unfetched (BUG-7ijw: SimpleSparseAccessor hit exactly this — the fetch
//! script did have a mechanism for it, it just hadn't been run in that
//! worktree, but nothing would have caught it if the mechanism had been
//! missing).
//!
//! Offline and file-presence-blind by design: this never touches
//! `tests/fixtures/gltf/khronos/` itself (gitignored, populated on demand),
//! only `manifest.json` (tracked) and the fetch script's source text. It
//! must NOT become a "does the file exist" check — that would re-break D1's
//! "CI and a fresh worktree stay green without network" invariant that
//! `glb_conformance.rs`'s skip-if-absent path exists to preserve.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn khronos_manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf/khronos/manifest.json")
}

fn fetch_script_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/fetch-gltf-conformance.sh")
}

#[derive(serde::Deserialize)]
struct ManifestEntry {
    asset: String,
}

/// The set of top-level asset directory/file-stem names the fetch script
/// knows how to produce, parsed from its three fetch mechanisms: the flat
/// `ASSETS="..."` list (fetched as `<name>.glb`), the `GP7_TABLE` heredoc
/// (tab-separated `asset\tfile`, one asset per subdirectory), and the
/// hand-written `TextureTransformTest` block.
fn fetchable_asset_names(script_src: &str) -> HashSet<String> {
    let mut names = HashSet::new();

    let assets_block = script_src
        .split("ASSETS=\"\n")
        .nth(1)
        .and_then(|rest| rest.split_once("\"\n"))
        .map(|(block, _)| block)
        .expect("fetch script must define ASSETS=\"...\" block — format changed?");
    for line in assets_block.lines() {
        let line = line.trim();
        if !line.is_empty() {
            names.insert(line.to_string());
        }
    }

    let gp7_block = script_src
        .split("<<'GP7_TABLE'\n")
        .nth(1)
        .and_then(|rest| rest.split_once("\nGP7_TABLE"))
        .map(|(block, _)| block)
        .expect("fetch script must define a GP7_TABLE heredoc — format changed?");
    for line in gp7_block.lines() {
        if let Some((asset, _file)) = line.split_once('\t') {
            names.insert(asset.to_string());
        }
    }

    names.insert("TextureTransformTest".to_string());
    names
}

/// The manifest's `asset` field is either a bare `<Name>.glb`/`<Name>.gltf`
/// (flat ASSETS fetch) or `<Name>/<file>` (subdirectory fetch, GP7_TABLE or
/// TextureTransformTest) — derive the top-level name the fetch script would
/// key on either way.
fn manifest_top_level_name(asset: &str) -> &str {
    match asset.split_once('/') {
        Some((dir, _rest)) => dir,
        None => asset
            .strip_suffix(".glb")
            .or_else(|| asset.strip_suffix(".gltf"))
            .unwrap_or(asset),
    }
}

#[test]
fn every_manifest_asset_has_a_fetch_mechanism() {
    let script_src = std::fs::read_to_string(fetch_script_path()).expect("read fetch script");
    let fetchable = fetchable_asset_names(&script_src);

    // Sanity: a parsing regression (e.g. the script renames ASSETS or
    // GP7_TABLE) must fail loudly here, never pass vacuously with an empty
    // set that happens to reject nothing because the manifest loop below
    // never ran either.
    assert!(
        fetchable.len() > 100,
        "parsed only {} fetchable asset names out of the fetch script — \
         ASSETS/GP7_TABLE parsing likely broke, not that the script shrank",
        fetchable.len()
    );
    assert!(
        fetchable.contains("Box"),
        "expected sentinel `Box` (flat ASSETS entry) missing — ASSETS parsing broke"
    );
    assert!(
        fetchable.contains("SimpleSparseAccessor"),
        "expected sentinel `SimpleSparseAccessor` (GP7_TABLE entry) missing — GP7_TABLE parsing broke"
    );
    assert!(
        fetchable.contains("TextureTransformTest"),
        "expected sentinel `TextureTransformTest` missing"
    );

    let manifest_json = std::fs::read_to_string(khronos_manifest_path()).expect("read manifest.json");
    let entries: Vec<ManifestEntry> =
        serde_json::from_str(&manifest_json).expect("parse manifest.json");
    assert!(!entries.is_empty(), "manifest.json must name at least one asset");

    let dead: Vec<&str> = entries
        .iter()
        .map(|e| manifest_top_level_name(&e.asset))
        .filter(|name| !fetchable.contains(*name))
        .collect();

    assert!(
        dead.is_empty(),
        "manifest.json names asset(s) scripts/fetch-gltf-conformance.sh has NO mechanism to \
         fetch — these can never move past SKIPPED no matter how many times the fetch script \
         runs (either add a fetch mechanism or drop the manifest row): {:?}",
        dead
    );
}
