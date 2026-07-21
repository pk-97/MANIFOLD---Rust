//! Structural guard for `EDITOR_WINDOW_UNIFICATION_DESIGN.md` D7 (P3).
//!
//! `render_tree_range`/`render_sub_region` are the two low-level tree-scan
//! primitives (`ui_renderer.rs`). Outside a small, named allowlist, a call
//! site to either in `manifold-app`'s source is a structural regression: it
//! means some window forked its own copy of the tree-overlay traversal
//! instead of going through the shared `tree_passes::render_tree_overlay_passes`
//! (D1) — which is exactly what BUG-151 was (the editor's flat
//! `render_tree_range(0, usize::MAX)` root scan swept overlay nodes up at
//! CONTENT depth and never ran the overlay pass at all).
//!
//! Allowlist (D7), by FILE NAME with an EXPECTED CALL COUNT — never by line
//! number, so line drift inside an allowlisted file cannot rot the guard,
//! but a *new, additional* raw call inside an already-allowlisted file still
//! trips it (the count changes):
//!   - `tree_passes.rs` (2 calls): the shared pass itself — the one place
//!     these primitives are meant to be used directly.
//!   - `editor_frame.rs` (1 call): the editor's narrowed base-content scan,
//!     `[0, overlay_region_start)` (D2) — everything past that boundary must
//!     go through the shared pass, never a second raw call here.
//!   - `ui_snapshot/mod.rs` (2 calls): the BUG-097 traversal-semantics proof
//!     (`overlay_fidelity_proof`), which needs raw calls to demonstrate the
//!     root-scan-vs-flat-scan distinction the shared pass depends on.
//!     Everything else in `manifold-app/src/` is expected to have ZERO raw call
//!     sites. `manifold-renderer`'s `ui_cache_manager.rs`/`ui_renderer.rs` (the
//!     internal cache-render path and a renderer unit test) are a different
//!     crate and out of this guard's scope by design (D7).
//!
//! I2 (input-presence, never caller-identity) is folded in here too: the
//! shared pass module must never reference `WorkspaceKind`/
//! `is_graph_editor`/`is_primary` — that would be the caller-identity fork
//! D4 forbids, reborn inside the seam.
//!
//! Precedent: `manifold-core/tests/docs_index_sync.rs`, a repo-discipline
//! meta-test in the default `nextest` sweep that scans the filesystem
//! directly rather than depending on the crate under test.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

fn app_src_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/crates/manifold-app
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` file under `manifold-app/src/`, recursively, as paths
/// relative to `src/` (forward-slash separated, so the allowlist keys below
/// are stable across platforms).
fn all_src_files() -> Vec<(String, PathBuf)> {
    let root = app_src_dir();
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("manifold-app/src should exist") {
            let entry = entry.expect("readable dir entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let rel = path
                    .strip_prefix(&root)
                    .expect("child of src root")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, path));
            }
        }
    }
    out
}

/// Count actual method-call sites (`.render_tree_range(` / `.render_sub_region(`)
/// in a file's text — deliberately requires the leading `.` so doc-comment
/// mentions like `` `render_tree_range(start, end)` `` (no leading dot) don't
/// count as call sites. This file has no `fn render_tree_range`/
/// `fn render_sub_region` definitions (those live in
/// `manifold-renderer::ui_renderer`), so every dotted occurrence here is a
/// real call.
fn count_raw_calls(text: &str) -> usize {
    text.matches(".render_tree_range(").count() + text.matches(".render_sub_region(").count()
}

#[test]
fn tree_render_call_sites_are_allowlisted() {
    let allowlist: BTreeMap<&str, usize> = BTreeMap::from([
        ("tree_passes.rs", 2),
        ("editor_frame.rs", 1),
        ("ui_snapshot/mod.rs", 2),
    ]);

    let mut violations = Vec::new();

    for (rel, path) in all_src_files() {
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let found = count_raw_calls(&text);
        let expected = allowlist.get(rel.as_str()).copied().unwrap_or(0);
        if found != expected {
            violations.push(format!(
                "{rel}: found {found} raw render_tree_range/render_sub_region call site(s), \
                 expected {expected} (allowlist: {allowlist:?})"
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "Structural guard (EDITOR_WINDOW_UNIFICATION_DESIGN.md D7) tripped:\n{}\n\n\
         A raw render_tree_range/render_sub_region call site appeared outside the \
         allowlist, or an allowlisted file's call count changed. Tree-overlay \
         rendering must go through tree_passes::render_tree_overlay_passes — \
         this is the mechanism that keeps BUG-151's bug class impossible by \
         construction. If this file genuinely needs a new raw call (rare — \
         only the shared pass, a window's narrowed base-content scan, and the \
         BUG-097 proof are exempt), update the allowlist above deliberately \
         and explain why in the same commit.",
        violations.join("\n"),
    );
}

#[test]
fn tree_passes_branches_on_input_presence_not_caller_identity() {
    // I2, folded into this guard per the P3 brief: the shared pass must
    // never see a WorkspaceKind or an is-this-window-the-editor flag. Every
    // Option in its signature is INPUT PRESENCE, resolved by the caller
    // (D4) — never caller identity re-derived inside the seam.
    //
    // Scans CODE lines only, not comments: the module doc deliberately
    // *names* this forbidden pattern in prose (documenting the rule this
    // very test enforces), which must not trip the guard. A line is treated
    // as a comment if its trimmed text starts with `//` (covers `//`, `///`,
    // `//!`) — good enough for a single well-behaved module, and it still
    // catches the real hazard: an actual `WorkspaceKind` parameter, match
    // arm, or `is_graph_editor()`/`is_primary()` call in the pass's code.
    let path = app_src_dir().join("tree_passes.rs");
    let text =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let code_lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect();

    for forbidden in ["WorkspaceKind", "is_graph_editor", "is_primary"] {
        let hits: Vec<&&str> = code_lines
            .iter()
            .filter(|line| line.contains(forbidden))
            .collect();
        assert!(
            hits.is_empty(),
            "tree_passes.rs contains {forbidden:?} in code (not just a doc comment): {hits:?} — \
             the shared tree-overlay pass must branch on input presence (Option), never caller \
             identity (D4). This is the caller-identity fork EDITOR_WINDOW_UNIFICATION_DESIGN.md \
             D4 explicitly forbids reintroducing into the seam.",
        );
    }
}

/// Sanity check on the guard's own file inventory: if `manifold-app/src/`
/// stops existing or the walk finds nothing, the two tests above would
/// vacuously "pass" without checking anything. Fails loudly instead.
#[test]
fn guard_walk_finds_source_files() {
    let files = all_src_files();
    assert!(
        files.len() > 50,
        "expected manifold-app/src/ to contain many .rs files; found {} — \
         the directory walk in this guard may be broken",
        files.len()
    );
    assert!(
        files.iter().any(|(rel, _)| rel == "tree_passes.rs"),
        "tree_passes.rs not found by the walk — allowlist keys use paths \
         relative to manifold-app/src/, check the walk logic"
    );
}
