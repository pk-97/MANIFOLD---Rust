//! INV-8 enforcement (`docs/WIDGET_TREE_DESIGN.md` §5b/§6, Peter's standing
//! directive): "Agents must never create their own infra for basic things
//! like rows, sliders, drawers, etc ever again during implementation."
//!
//! Prose alone provably fails here — the scene panel was built against
//! explicit design-doc prohibitions before this test existed. This is the
//! machine check: a repo-wide allowlist scan over `crates/manifold-ui/src/
//! panels/**` for the two shapes a bespoke row/slider re-implementation
//! always needs — a raw `BitmapSlider` construction, or a `Vec<Option<
//! NodeId>>` row-id hoard. Both are legitimate ONLY in the files that already
//! own that infra; the allowlist below is that exact, current set — new
//! files matching either pattern are the violation this test exists to catch.
//!
//! This pins "no NEW bespoke row infra" — it does not (and cannot) prove the
//! EXISTING allowlisted infra is itself minimal; that is `docs/
//! WIDGET_TREE_DESIGN.md`'s job, reviewed at each phase landing.

use std::fs;
use std::path::{Path, PathBuf};

/// Files sanctioned to construct `BitmapSlider` directly — each is either the
/// widget's OWN home, a shared row-builder that assembles the widget-tree row
/// model's slider bundle (`docs/WIDGET_TREE_DESIGN.md` §5b's "the one entry
/// point"), or a plain chrome-slider host predating the row model (D9's scope
/// fence — chrome settings sliders are explicitly out of this design's
/// scope, and never grow families).
const BITMAP_SLIDER_ALLOWLIST: &[&str] = &[
    // The widget's own implementation — draws/updates it, not a caller.
    "slider.rs",
    // The row model's shared per-row builders (`build_param_row`,
    // `build_driver_config`'s decay slider via `drawer::build`, the audio
    // shaping sliders) — THE sanctioned entry point for manifest-backed rows.
    // (`param_slider_shared/builders.rs` since the P-S1 directory-module split.)
    "builders.rs",
    // The row model's card host — relight knobs (D9: out of `RowIndex`
    // scope, but still a manifest-backed per-instance control, built through
    // this file, not a new system).
    "param_card.rs",
    // The generic declarative-drawer API (`docs/AUDIO_MODULATION_DESIGN.md`
    // §10.2) — `DrawerRow::Slider` materialises through the SAME widget, one
    // call site, shared by every drawer kind.
    "drawer.rs",
    // A single gain slider in the layer chrome header — predates the row
    // model, a plain chrome control (D9 scope fence: chrome settings
    // sliders), not a manifest-backed param row.
    "layer_header.rs",
];

/// Files sanctioned to hold a `Vec<Option<NodeId>>` row-id collection —
/// widget-owned per-row bookkeeping (INV-5's explicit exemption: this is
/// state a widget/panel legitimately owns, e.g. for `sync_values`'s
/// positional writes and drag state; it is NOT itself a routing mechanism —
/// see `RowIndex`, which replaced the ROUTING use of these collections).
const NODE_ID_HOARD_ALLOWLIST: &[&str] = &[
    // The row model's card host. `RowIndex` replaced these as a ROUTING
    // mechanism (P2); they remain as widget-owned per-row storage (sync,
    // drag identification via direct field reads, not id-equality scans).
    "param_card.rs",
    // Clip chrome's own per-row audio-shaping controls — a different,
    // non-`ParamRow` surface (D9 scope fence).
    "clip_chrome.rs",
    // The scene panel — explicitly OUT of this design's scope
    // (`docs/WIDGET_TREE_DESIGN.md` §8: "Scene convergence... nothing in
    // P1–P4 touches scene files"). Its own migration is a separate design.
    "scene_setup_panel.rs",
];

fn panels_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/panels")
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read panels dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_bespoke_bitmap_slider_construction_outside_the_allowlist() {
    let mut files = Vec::new();
    rs_files(&panels_dir(), &mut files);

    let mut violations = Vec::new();
    for path in &files {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if BITMAP_SLIDER_ALLOWLIST.contains(&name) {
            continue;
        }
        let text = fs::read_to_string(path).expect("read panel source");
        if text.contains("BitmapSlider::new(") || text.contains("BitmapSlider::build(") {
            violations.push(path.display().to_string());
        }
    }

    assert!(
        violations.is_empty(),
        "bespoke BitmapSlider construction outside the sanctioned row-model \
         entry points (`docs/WIDGET_TREE_DESIGN.md` §5b) — agents must never \
         build their own slider infra; use `build_param_row`/the row model's \
         shared builders instead. Violating files: {violations:?}"
    );
}

#[test]
fn no_bespoke_node_id_row_hoard_outside_the_allowlist() {
    let mut files = Vec::new();
    rs_files(&panels_dir(), &mut files);

    let mut violations = Vec::new();
    for path in &files {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if NODE_ID_HOARD_ALLOWLIST.contains(&name) {
            continue;
        }
        let text = fs::read_to_string(path).expect("read panel source");
        if text.contains("Vec<Option<NodeId>>") {
            violations.push(path.display().to_string());
        }
    }

    assert!(
        violations.is_empty(),
        "a new `Vec<Option<NodeId>>` row-id hoard outside the sanctioned \
         allowlist (`docs/WIDGET_TREE_DESIGN.md` §5b/INV-8) — this is the \
         id-hoard-as-routing shape the widget-tree layer exists to kill; \
         route through `RowIndex` + `row_action` instead. Violating files: \
         {violations:?}"
    );
}
