use super::*;

fn overlay() -> InteractionOverlay {
    InteractionOverlay::new(6.0)
}

/// A minimal `MoveDrag` payload for driving the motion tests below —
/// only `snapshot_clip_ids` (what `tick()` copies into the visual-tween
/// target set) and `duplicate_on_release` (the ghost-alpha target) vary
/// per test; everything else is an inert placeholder.
fn move_drag_for(snapshot_clip_ids: &[&str], duplicate_on_release: bool) -> MoveDrag {
    MoveDrag {
        anchor_clip_id: ClipId::new("anchor"),
        start_layer_index: 0,
        snapshots: Vec::new(),
        snapshot_clip_ids: snapshot_clip_ids.iter().map(|id| ClipId::new(*id)).collect(),
        selection_min_start_beat: Beats::ZERO,
        selection_min_layer: 0,
        selection_max_layer: 0,
        layer_blocked: false,
        duplicate_on_release,
        start_beat: Beats::ZERO,
        offset_beats: Beats::ZERO,
    }
}

#[test]
fn lift_and_ghost_target_zero_and_one_when_idle() {
    let mut ov = overlay();
    assert_eq!(ov.drag_mode(), DragMode::None);
    ov.tick(16.0);
    assert_eq!(ov.lift_amount(), 0.0, "no lift while not dragging");
    assert_eq!(ov.ghost_alpha(), 1.0, "fully solid while not dragging");
    assert_eq!(ov.error_shake_offset_px(), 0.0, "no shake while idle");
    assert!(ov.landing_flash().is_none(), "no landing flash while idle");
}

#[test]
fn move_drag_ramps_lift_up_and_settles_to_one() {
    let mut ov = overlay();
    ov.drag.start(TimelineDrag::Move(move_drag_for(&["clip-a"], false)), Vec2::ZERO);

    ov.tick(16.0);
    assert!(ov.lift_amount() > 0.0, "lift starts rising the first tick of a move drag");
    assert!(ov.is_drag_visual_target(&ClipId::new("clip-a")));
    assert!(!ov.is_drag_visual_target(&ClipId::new("clip-b")));

    // Drive it to settle at the full MOTION_MED_MS duration.
    ov.tick(color::MOTION_MED_MS);
    assert_eq!(ov.lift_amount(), 1.0, "settles fully lifted while still dragging");
}

#[test]
fn alt_duplicate_drag_dims_ghost_then_solidifies_on_release() {
    let mut ov = overlay();
    ov.drag.start(TimelineDrag::Move(move_drag_for(&["clip-a"], true)), Vec2::ZERO);

    ov.tick(color::MOTION_MED_MS);
    assert!(
        ov.ghost_alpha() < 1.0 && ov.ghost_alpha() > 0.0,
        "alt-dragging dims toward the ghost target: {}",
        ov.ghost_alpha()
    );

    // Release: drag_mode drops, ghost eases back up ("solidifies").
    ov.drag.cancel();
    ov.tick(color::MOTION_MED_MS);
    assert_eq!(ov.ghost_alpha(), 1.0, "fully solid once settled post-release");
    // The clip stays a visual target until every tween has caught up —
    // by now they all have, so the memory is dropped.
    assert!(!ov.is_drag_visual_target(&ClipId::new("clip-a")));
}

#[test]
fn error_shake_fires_and_decays_to_zero() {
    let mut ov = overlay();
    ov.error_shake.fire(240.0);
    ov.tick(1.0);
    assert!(ov.error_shake_offset_px().abs() <= 3.0001, "amplitude capped near 3px");

    // Run past the full duration — decays back to inert.
    ov.tick(240.0);
    assert_eq!(ov.error_shake_offset_px(), 0.0, "shake settles to zero once finished");
}

#[test]
fn landing_flash_reports_geometry_while_active() {
    let mut ov = overlay();
    ov.landing_flash_beat = Beats(4.0);
    ov.landing_flash_layers = (1, 3);
    ov.landing_flash.fire(color::MOTION_MED_MS);

    let (progress, beat, min_layer, max_layer) = ov.landing_flash().expect("active");
    assert!((0.0..=1.0).contains(&progress));
    assert_eq!(beat, Beats(4.0));
    assert_eq!((min_layer, max_layer), (1, 3));

    ov.tick(color::MOTION_MED_MS * 2.0);
    assert!(ov.landing_flash().is_none(), "flash finishes and reports idle");
}
