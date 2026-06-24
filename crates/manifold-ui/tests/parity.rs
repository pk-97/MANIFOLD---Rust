/// Parity tests: verify that Rust constants match the Unity spec exactly.
///
/// These tests catch regressions where someone changes a constant
/// without checking the Unity USER_GUIDE.md or UIConstants.cs.
use manifold_ui::color;
use manifold_ui::node::Color32;

// ── Layout constants (from USER_GUIDE.md §2) ───────────────────

#[test]
fn layout_transport_bar_height() {
    assert_eq!(color::TRANSPORT_BAR_HEIGHT, 36.0);
}

#[test]
fn layout_header_height() {
    assert_eq!(color::HEADER_HEIGHT, 40.0);
}

#[test]
fn layout_footer_height() {
    // Raised 29→36 when the footer became global full-width chrome pinned to the
    // screen bottom (commit a52b2997).
    assert_eq!(color::FOOTER_HEIGHT, 36.0);
}

#[test]
fn layout_ruler_height() {
    assert_eq!(color::RULER_HEIGHT, 40.0); // Unity UIConstants.cs, NOT USER_GUIDE
}

#[test]
fn layout_track_height() {
    assert_eq!(color::TRACK_HEIGHT, 140.0);
}

#[test]
fn layout_layer_controls_width() {
    assert_eq!(color::LAYER_CONTROLS_WIDTH, 200.0); // Fixed, not resizable
}

#[test]
fn layout_inspector_width_range() {
    assert_eq!(color::MIN_INSPECTOR_WIDTH, 232.0);
    assert_eq!(color::MAX_INSPECTOR_WIDTH, 900.0);
}

#[test]
fn layout_default_inspector_width() {
    assert_eq!(color::DEFAULT_INSPECTOR_WIDTH, 500.0);
}

// ── Accent colors (from USER_GUIDE.md §32.3) ───────────────────

#[test]
fn color_accent_blue() {
    // Selection / focus: #5994EB
    assert_eq!(color::ACCENT_BLUE, Color32::new(89, 148, 235, 255));
}

#[test]
fn color_play_active() {
    // Play green: #40B852
    assert_eq!(color::PLAY_ACTIVE, Color32::new(64, 184, 82, 255));
}

#[test]
fn color_stop_red() {
    // Stop: #803333
    assert_eq!(color::STOP_RED, Color32::new(128, 51, 51, 255));
}

#[test]
fn color_record_inactive() {
    // Record inactive: #6B2626
    assert_eq!(color::RECORD_RED, Color32::new(107, 38, 38, 255));
}

#[test]
fn color_record_active() {
    // Record active: #D12E2E
    assert_eq!(color::RECORD_ACTIVE, Color32::new(209, 46, 46, 255));
}

#[test]
fn color_paused_yellow() {
    // Paused: #D1A626
    assert_eq!(color::PAUSED_YELLOW, Color32::new(209, 166, 38, 255));
}

#[test]
fn color_link_orange() {
    // Link (Ableton): #BF7A14
    assert_eq!(color::LINK_ORANGE, Color32::new(191, 122, 20, 255));
}

#[test]
fn color_midi_purple() {
    // CLK (MIDI): #944D94
    assert_eq!(color::MIDI_PURPLE, Color32::new(148, 77, 148, 255));
}

#[test]
fn color_sync_teal() {
    // Sync (ArtNet): #389E85
    assert_eq!(color::SYNC_ACTIVE, Color32::new(56, 158, 133, 255));
}

#[test]
fn color_export_marker() {
    // Export range: #4D8DEB
    assert_eq!(color::EXPORT_MARKER_COLOR, Color32::new(77, 141, 235, 255));
}

// ── Clip colors (from USER_GUIDE.md §9.3) ───────────────────────

#[test]
fn color_video_clip_normal() {
    // (0.68, 0.66, 0.64) = (173, 168, 163)
    assert_eq!(color::CLIP_NORMAL, Color32::new(173, 168, 163, 255));
}

#[test]
fn color_video_clip_hover() {
    // (0.74, 0.72, 0.70) = (189, 184, 179)
    assert_eq!(color::CLIP_HOVER, Color32::new(189, 184, 179, 255));
}

#[test]
fn color_video_clip_selected() {
    // (0.85, 0.82, 0.78) = (217, 209, 199)
    assert_eq!(color::CLIP_SELECTED, Color32::new(217, 209, 199, 255));
}

#[test]
fn color_generator_clip_normal() {
    // (0.396, 0.988, 1.0) = (101, 252, 255)
    assert_eq!(color::CLIP_GEN_NORMAL, Color32::new(101, 252, 255, 255));
}

#[test]
fn color_generator_clip_hover() {
    // (0.30, 0.38, 0.60) = (77, 97, 153)
    assert_eq!(color::CLIP_GEN_HOVER, Color32::new(77, 97, 153, 255));
}

#[test]
fn color_generator_clip_selected() {
    // (0.40, 0.55, 0.88) = (102, 140, 224)
    assert_eq!(color::CLIP_GEN_SELECTED, Color32::new(102, 140, 224, 255));
}

// ── Text colors (from USER_GUIDE.md §32.4) ──────────────────────

#[test]
fn color_text_primary() {
    // Unity TextNormal = Color(0.88, 0.88, 0.90, 1) → blue channel 230
    assert_eq!(color::TEXT_NORMAL, Color32::new(224, 224, 230, 255));
}

#[test]
fn color_text_primary_c32() {
    assert_eq!(color::TEXT_PRIMARY_C32, Color32::new(224, 224, 230, 255));
}

// ── Elevation hierarchy (from USER_GUIDE.md §32.2) ──────────────

#[test]
fn color_track_background_deep_level() {
    // Phase 3 design tokens re-pointed TRACK_BG to the BG_2 grey-ramp step
    // (31, 31, 33), replacing the old Unity-ported (36, 36, 37).
    assert_eq!(color::TRACK_BG, Color32::new(31, 31, 33, 255));
}

// Generator param-count parity tests moved to
// `manifold-core/tests/generator_param_counts.rs` in the Phase 5 layering
// inversion: they exercise the engine's preset registries, not the UI, and the
// UI crate no longer depends on `manifold-core`. See docs/UI_LAYERING_INVERSION.md.
