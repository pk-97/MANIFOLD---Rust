use super::*;

// `docs/UI_WIDGET_UNIFICATION_DESIGN.md` P7.4: prove trim-left/trim-right
// fan-over (incl. locked-clip skip) and region-select extents survive the
// fold of the grabbed-clip-id/geometry fields and the fan-over Vec onto
// `TimelineDrag::TrimLeft(TrimDrag)`/`TrimRight(TrimDrag)`, and the grab
// beat/layer pair onto `TimelineDrag::RegionSelect(RegionDrag)`.

fn trim_left_pos(panel: &TimelineViewportPanel, clip_start_beat: f32) -> Vec2 {
    Vec2::new(
        panel.beat_to_pixel(Beats::from_f32(clip_start_beat)) + 3.0,
        panel.tracks_rect().y + 70.0,
    )
}

fn trim_right_pos(panel: &TimelineViewportPanel, clip_end_beat: f32) -> Vec2 {
    Vec2::new(
        panel.beat_to_pixel(Beats::from_f32(clip_end_beat)) - 3.0,
        panel.tracks_rect().y + 70.0,
    )
}

#[test]
fn build_trim_drag_skips_locked_clips_in_the_fan_over() {
    let host = GestureTestHost::new(&["layer-0"])
        .with_clip("a", 0, 0.0, 8.0)
        .with_locked_clip("b", 0, 2.0, 3.0);
    let mut ui_state = UIState::new();
    ui_state.select_clips(vec![ClipId::new("a"), ClipId::new("b")]);
    let clip_a = host.find_clip_by_id("a").unwrap();

    let trim = InteractionOverlay::build_trim_drag(ClipId::new("a"), &clip_a, &ui_state, &host);

    assert_eq!(trim.clip_id, ClipId::new("a"));
    assert_eq!(trim.originals.len(), 1, "the locked clip must be skipped");
    assert_eq!(trim.originals[0].clip_id, ClipId::new("a"));
}

#[test]
fn trim_left_drag_fans_delta_over_the_whole_selection_and_batches_undo() {
    let mut panel = build_viewport();
    let mut host = GestureTestHost::new(&["layer-0"])
        .with_clip("clip_a", 0, 0.0, 8.0)
        .with_clip("clip_b", 0, 16.0, 4.0);
    let mut ui_state = UIState::new();
    ui_state.select_clips(vec![ClipId::new("clip_a"), ClipId::new("clip_b")]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

    let press = trim_left_pos(&panel, 0.0);
    overlay.on_begin_drag(press, &mut host, &mut ui_state, &panel);
    assert_eq!(
        overlay.drag_mode(),
        DragMode::TrimLeft,
        "pressing the left edge must begin a left trim"
    );

    // Drag right by ~1 beat — shrinks clip_a from the left; the SAME
    // beat delta fans over clip_b too (it's part of the selection).
    overlay.on_drag(
        press
            + Vec2::new(
                panel.beat_to_pixel(Beats::from_f32(1.0)) - panel.beat_to_pixel(Beats::ZERO),
                0.0,
            ),
        &mut host,
        &mut ui_state,
        &mut panel,
    );
    overlay.on_end_drag(&mut host);

    assert_eq!(overlay.drag_mode(), DragMode::None);
    assert_eq!(
        host.trim_records.len(),
        2,
        "both selected clips must be recorded (fan-over)"
    );
    assert_eq!(
        host.committed_batches.len(),
        1,
        "one batched undo entry for the whole trim"
    );
    let a = host.find_clip_by_id("clip_a").unwrap();
    assert!(
        a.start_beat > Beats::ZERO,
        "clip_a's start must have moved right"
    );
}

#[test]
fn trim_left_reveals_source_across_separate_gestures() {
    for command in [false, true] {
        let mut panel = build_viewport();
        let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 4.0, 8.0);
        let mut state = UIState::new();
        state.select_clips(vec![ClipId::new("clip_a")]);
        let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
        overlay.set_modifiers(Modifiers {
            command,
            ..Modifiers::NONE
        });
        let trimmed_start = if command { 6.375 } else { 6.0 };

        // Commit the inward trim, then start fresh gestures to reveal
        // part of it and finally drag beyond the beginning of the source.
        for (requested, expected) in [(trimmed_start, trimmed_start), (5.0, 5.0), (2.0, 4.0)] {
            let clip = host.find_clip_by_id("clip_a").unwrap();
            let trim =
                InteractionOverlay::build_trim_drag(clip.clip_id.clone(), &clip, &state, &host);
            let press = trim_left_pos(&panel, clip.start_beat.as_f32());
            overlay.drag.start(TimelineDrag::TrimLeft(trim), press);
            overlay.on_drag(
                Vec2::new(panel.beat_to_pixel(Beats(requested)), press.y),
                &mut host,
                &mut state,
                &mut panel,
            );
            overlay.on_end_drag(&mut host);
            let clip = host.find_clip_by_id("clip_a").unwrap();
            assert!((clip.start_beat.0 - expected).abs() < 1e-5);
            assert!((clip.duration_beats.0 - (12.0 - expected)).abs() < 1e-5);
            let expected_in = (expected - 4.0) * TEST_SECONDS_PER_BEAT;
            assert!((clip.in_point.0 - expected_in).abs() < 1e-5);
        }
        assert_eq!(host.committed_batches, vec![1, 1, 1]);
    }
}

#[test]
fn trim_left_drag_on_generator_clamps_to_beat_zero() {
    // BUG: generator clips are allowed to extend their left-trim freely
    // (unlike source media, which stops at source time zero), but
    // that freedom must still stop at beat 0 — dragging far enough left
    // must not push start_beat negative.
    let mut panel = build_viewport();
    let mut host = GestureTestHost::new(&["layer-0"]).with_generator_clip("clip_a", 0, 0.0, 8.0);
    let mut ui_state = UIState::new();
    ui_state.select_clips(vec![ClipId::new("clip_a")]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

    let press = trim_left_pos(&panel, 0.0);
    overlay.on_begin_drag(press, &mut host, &mut ui_state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::TrimLeft);

    // Drag far left of beat 0 — must clamp at zero, never go negative.
    overlay.on_drag(
        Vec2::new(panel.tracks_rect().x - 500.0, press.y),
        &mut host,
        &mut ui_state,
        &mut panel,
    );
    let mid = host.find_clip_by_id("clip_a").unwrap().start_beat;
    assert!(
        mid >= Beats::ZERO,
        "start_beat must clamp at zero mid-drag: {mid:?}"
    );

    overlay.on_end_drag(&mut host);
    let end = host.find_clip_by_id("clip_a").unwrap().start_beat;
    assert!(
        end >= Beats::ZERO,
        "start_beat must clamp at zero at commit: {end:?}"
    );
}

#[test]
fn trim_right_drag_clamps_to_minimum_duration() {
    let mut panel = build_viewport();
    let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
    let mut ui_state = UIState::new();
    ui_state.select_clips(vec![ClipId::new("clip_a")]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

    let press = trim_right_pos(&panel, 8.0);
    overlay.on_begin_drag(press, &mut host, &mut ui_state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::TrimRight);

    // Drag far left — must clamp at the 1/16-note (0.25 beat) minimum,
    // never collapse to zero or negative duration.
    overlay.on_drag(
        Vec2::new(panel.tracks_rect().x, press.y),
        &mut host,
        &mut ui_state,
        &mut panel,
    );
    let mid = host.find_clip_by_id("clip_a").unwrap().duration_beats;
    assert!(
        mid >= Beats(0.25),
        "duration must clamp at the 1/16 minimum mid-drag: {mid:?}"
    );

    overlay.on_end_drag(&mut host);
    let end = host.find_clip_by_id("clip_a").unwrap().duration_beats;
    assert!(
        end >= Beats(0.25),
        "duration must clamp at the 1/16 minimum at commit: {end:?}"
    );
}

#[test]
fn drag_readout_clip_id_reports_the_trimmed_clip() {
    let panel = build_viewport();
    let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
    let mut ui_state = UIState::new();
    ui_state.select_clips(vec![ClipId::new("clip_a")]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

    let press = trim_left_pos(&panel, 0.0);
    overlay.on_begin_drag(press, &mut host, &mut ui_state, &panel);
    assert_eq!(overlay.drag_readout_clip_id(), Some(ClipId::new("clip_a")));

    overlay.on_end_drag(&mut host);
    assert_eq!(
        overlay.drag_readout_clip_id(),
        None,
        "readout clears once the drag ends"
    );
}

#[test]
fn region_select_drag_updates_extents_and_clears_prior_selection() {
    let mut panel = build_viewport();
    let mut host = GestureTestHost::new(&["layer-0"])
        .with_clip("clip_a", 0, 0.0, 8.0)
        .with_clip("clip_b", 0, 16.0, 4.0);
    let mut ui_state = UIState::new();
    ui_state.select_clips(vec![ClipId::new("clip_a")]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

    // Press empty area (well past clip_b, at beat ~30) to begin a region drag.
    let press = body_pos_for(&panel, 30.0);
    overlay.on_begin_drag(press, &mut host, &mut ui_state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::RegionSelect);
    assert!(
        ui_state.get_selected_clip_ids().is_empty(),
        "a bare region drag clears prior clip selection"
    );

    // Drag back to beat ~2, spanning most of clip_a's range.
    overlay.on_drag(
        body_pos_for(&panel, 2.0),
        &mut host,
        &mut ui_state,
        &mut panel,
    );
    let region = ui_state
        .current_region()
        .expect("region must be active mid-drag");
    assert!(region.start_beat < region.end_beat);
    assert!(
        region.end_beat >= Beats::from_f32(29.0),
        "region must extend to the drag's far corner"
    );

    overlay.on_end_drag(&mut host);
    assert_eq!(overlay.drag_mode(), DragMode::None);
}

fn body_pos_for(panel: &TimelineViewportPanel, beat_center: f32) -> Vec2 {
    Vec2::new(
        panel.beat_to_pixel(Beats::from_f32(beat_center)),
        panel.tracks_rect().y + 70.0,
    )
}

/// D5 core assertion: drive several synthetic pointer moves, then compare
/// the model's position after the LAST move (the on-screen "preview") to
/// its position after `on_end_drag` commits (the "committed result").
/// Run twice per the gate's "snap on and off": once landing close enough
/// to `clip_b`'s start (16.0) that the neighbor-edge candidate wins over
/// plain grid-snap, once landing far from any clip edge (grid-snap only —
/// grid-snap itself has no "off" state today, per `magnetic_snap`'s
/// half-grid-interval full-coverage threshold; B12's Cmd-bypass toggle is
/// P1.5). Both must show zero further change at commit.
fn assert_preview_equals_committed(landing_beat_center: f32, case: &str) {
    let mut panel = build_viewport();
    let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
    let mut ui_state = UIState::new();
    ui_state.select_clips(vec![ClipId::new("clip_a")]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

    overlay.on_begin_drag(body_pos_for(&panel, 2.0), &mut host, &mut ui_state, &panel);
    assert_eq!(
        overlay.drag_mode(),
        DragMode::Move,
        "case {case}: drag must begin"
    );

    // Several synthetic pointer moves walking toward the landing spot.
    for step in 1..=3 {
        let t = step as f32 / 3.0;
        let beat = 2.0 + (landing_beat_center - 2.0) * t;
        overlay.on_drag(
            body_pos_for(&panel, beat),
            &mut host,
            &mut ui_state,
            &mut panel,
        );
    }

    let preview = host.find_clip_by_id("clip_a").unwrap().start_beat;
    overlay.on_end_drag(&mut host);
    let committed = host.find_clip_by_id("clip_a").unwrap().start_beat;

    assert_eq!(
        preview, committed,
        "case {case}: on-screen preview must already be the committed result (D5)"
    );
}

#[test]
fn preview_equals_committed_neighbor_edge_snap_wins() {
    assert_preview_equals_committed(15.9, "neighbor-edge snap wins over grid");
}

#[test]
fn preview_equals_committed_grid_snap_only() {
    assert_preview_equals_committed(9.37, "grid-snap only, no neighbor edge in range");
}

#[test]
fn escape_mid_drag_restores_byte_identical_state() {
    let mut panel = build_viewport();
    let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
    let mut ui_state = UIState::new();
    ui_state.select_clips(vec![ClipId::new("clip_a")]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

    let before = host.clips.clone();

    overlay.on_begin_drag(body_pos_for(&panel, 2.0), &mut host, &mut ui_state, &panel);
    overlay.on_drag(
        body_pos_for(&panel, 10.0),
        &mut host,
        &mut ui_state,
        &mut panel,
    );
    overlay.on_drag(
        body_pos_for(&panel, 12.37),
        &mut host,
        &mut ui_state,
        &mut panel,
    );

    // Sanity: the drag actually moved the clip before cancelling —
    // otherwise this test would pass for the wrong reason.
    assert_ne!(
        host.find_clip_by_id("clip_a").unwrap().start_beat,
        Beats::from_f32(0.0),
        "sanity: the drag must have actually moved the clip"
    );

    overlay.cancel_drag(&mut host);

    assert_eq!(
        host.clips, before,
        "Escape must restore byte-identical clip state"
    );
    assert_eq!(overlay.drag_mode(), DragMode::None);
    assert!(
        host.committed_batches.is_empty(),
        "cancel must not push an undo entry — restore-and-close-batch, never commit-then-undo"
    );
    assert!(
        host.move_records.is_empty(),
        "cancel must not record a move"
    );
}

#[test]
fn sub_four_px_press_release_moves_nothing() {
    let panel = build_viewport();
    let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
    let mut ui_state = UIState::new();
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

    // A widget under the press position so `UIInputSystem`'s gesture
    // recognizer (the real 4px threshold — `input.rs`'s `DRAG_THRESHOLD`)
    // has something to press/release on. Its identity is irrelevant: only
    // the emitted event kind + `pos` matter downstream — InteractionOverlay
    // does its own beat-space hit-testing independent of the UITree node
    // system, exactly as production does (viewport events are stashed by
    // node-based `UIInputSystem` events but interpreted by beat/pixel math).
    let mut tree = UITree::new();
    let root = tree.add_panel(None, 0.0, 0.0, 1920.0, 1080.0, UIStyle::default());
    let press_pos = body_pos_for(&panel, 2.0);
    tree.add_button(
        Some(root),
        press_pos.x - 50.0,
        press_pos.y - 20.0,
        100.0,
        40.0,
        UIStyle::default(),
        "",
    );

    let mut input_sys = UIInputSystem::new();
    input_sys.process_pointer(&mut tree, press_pos, InputPointerAction::Down, 0.0);
    input_sys.process_pointer(
        &mut tree,
        press_pos + Vec2::new(2.0, 1.0),
        InputPointerAction::Move,
        0.0,
    );
    input_sys.process_pointer(
        &mut tree,
        press_pos + Vec2::new(2.0, 1.0),
        InputPointerAction::Up,
        0.0,
    );
    let events = input_sys.drain_events();

    assert!(
        !events.iter().any(|e| matches!(
            e,
            UIEvent::DragBegin { .. } | UIEvent::Drag { .. } | UIEvent::DragEnd { .. }
        )),
        "sub-4px press-release must not produce a drag event, got {events:?}"
    );

    // Feed exactly what the input layer produced into the overlay — the
    // ONLY entry points that ever mutate a clip's position
    // (`on_begin_drag`/`on_drag`) are never called on this path, by
    // construction, since no DragBegin/Drag event was ever emitted.
    for event in &events {
        if let UIEvent::Click { pos, modifiers, .. } = event {
            overlay.on_pointer_click(
                *pos,
                modifiers.shift,
                modifiers.ctrl || modifiers.command,
                1,
                false,
                &mut host,
                &mut ui_state,
                &panel,
            );
        }
    }

    assert_eq!(
        host.find_clip_by_id("clip_a").unwrap().start_beat,
        Beats::ZERO,
        "start_beat must be unchanged by a sub-threshold press-release"
    );
    assert!(
        host.move_records.is_empty(),
        "no move should be recorded for a sub-threshold press-release"
    );
}

#[test]
fn full_drag_produces_exactly_one_undo_entry() {
    let mut panel = build_viewport();
    let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
    let mut ui_state = UIState::new();
    ui_state.select_clips(vec![ClipId::new("clip_a")]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

    overlay.on_begin_drag(body_pos_for(&panel, 2.0), &mut host, &mut ui_state, &panel);
    overlay.on_drag(
        body_pos_for(&panel, 6.0),
        &mut host,
        &mut ui_state,
        &mut panel,
    );
    overlay.on_drag(
        body_pos_for(&panel, 10.0),
        &mut host,
        &mut ui_state,
        &mut panel,
    );
    overlay.on_end_drag(&mut host);

    assert_eq!(
        host.committed_batches.len(),
        1,
        "exactly one non-empty commit_command_batch call must fire per gesture (B9)"
    );
    assert_eq!(
        host.move_records.len(),
        1,
        "one clip moved -> one record_move call"
    );
}

#[test]
fn drag_toward_zero_clamps_every_frame_not_just_at_release() {
    let mut panel = build_viewport();
    let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 2.0, 4.0);
    let mut ui_state = UIState::new();
    ui_state.select_clips(vec![ClipId::new("clip_a")]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

    overlay.on_begin_drag(body_pos_for(&panel, 4.0), &mut host, &mut ui_state, &panel);

    // Walk the pointer far past beat 0 in several steps; the clamp must
    // hold on EVERY intermediate frame, not just once the drag ends.
    for beat in [3.0, 0.5, -2.0, -8.0, -20.0] {
        overlay.on_drag(
            body_pos_for(&panel, beat),
            &mut host,
            &mut ui_state,
            &mut panel,
        );
        let start = host.find_clip_by_id("clip_a").unwrap().start_beat;
        assert!(
            start >= Beats::ZERO,
            "start_beat went negative mid-drag: {start:?} at pointer beat {beat}"
        );
    }

    overlay.on_end_drag(&mut host);
    let start = host.find_clip_by_id("clip_a").unwrap().start_beat;
    assert!(
        start >= Beats::ZERO,
        "start_beat negative after release: {start:?}"
    );
}

// ── P1.5 drag ergonomics tests ────────────────────────────────────
// `docs/TIMELINE_INTERACTION_P1_SPEC.md` B11 (edge autoscroll),
// B12 (clip-edge/marker snap targets + Cmd-bypass).

/// B11: a pointer parked at the viewport's right edge must keep
/// advancing the (single) horizontal scroll offset frame over frame
/// with NO further pointer movement, and the gesture's beat mapping
/// must track it — the dragged clip keeps moving even though the
/// screen position never changes, because the same screen x now maps
/// to an advancing beat as the content scrolls under it.
#[test]
fn edge_autoscroll_advances_scroll_and_gesture_tracks_it() {
    let mut panel = build_viewport();
    let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
    let mut ui_state = UIState::new();
    ui_state.select_clips(vec![ClipId::new("clip_a")]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

    overlay.on_begin_drag(body_pos_for(&panel, 2.0), &mut host, &mut ui_state, &panel);
    assert_eq!(overlay.drag_mode(), DragMode::Move, "drag must begin");

    // Parked just inside the right edge of the tracks area — within
    // `TimelineViewportPanel::AUTOSCROLL_EDGE_PX` of `tracks_rect.x_max()`.
    let edge_pos = Vec2::new(
        panel.tracks_rect().x_max() - 5.0,
        panel.tracks_rect().y + 70.0,
    );

    let scroll_start = panel.scroll_x_beats();
    let start_begin = host.find_clip_by_id("clip_a").unwrap().start_beat;
    let mut last_scroll = scroll_start;
    let mut last_start = start_begin;

    for frame in 0..8 {
        // `on_drag` here stands in for both a real mouse-move event AND
        // the stationary-pointer poll (`poll_drag`) — both funnel into
        // `handle_move_drag`, which is where `autoscroll_edge` lives.
        overlay.on_drag(edge_pos, &mut host, &mut ui_state, &mut panel);
        let scroll_now = panel.scroll_x_beats();
        let start_now = host.find_clip_by_id("clip_a").unwrap().start_beat;
        assert!(
            scroll_now >= last_scroll,
            "frame {frame}: scroll must never move backward while parked at the edge (was {last_scroll:?}, now {scroll_now:?})"
        );
        assert!(
            start_now >= last_start,
            "frame {frame}: the dragged clip's model position must track the scrolling view (was {last_start:?}, now {start_now:?})"
        );
        last_scroll = scroll_now;
        last_start = start_now;
    }

    assert!(
        last_scroll > scroll_start,
        "scroll must have actually advanced over 8 parked frames (started at {scroll_start:?}, ended at {last_scroll:?})"
    );
    assert!(
        last_start > start_begin,
        "the gesture must have actually advanced the clip over 8 parked frames (started at {start_begin:?}, ended at {last_start:?})"
    );
}

/// B12: with snap on, a dragged clip lands flush against a neighbor's
/// edge; with Cmd held mid-drag, the same gesture must NOT snap (raw
/// position). Fixture: `clip_a` `[0,8)`, `clip_b` `[16,20)` (from
/// `build_viewport`). Press `clip_a` at mouse-beat 2.0 (so
/// `drag_offset_beats` = 2.0 - 0.0 = 2.0) and land at mouse-beat 17.9,
/// so the candidate anchor is 17.9 - 2.0 = 15.9 — within snap range of
/// `clip_b.start_beat` (16.0) but not equal to it, so snapped-vs-raw are
/// distinguishable (verified empirically: snap-on lands exactly at
/// 16.0; Cmd-bypass lands at the raw ~16.0166, never exactly 16.0).
#[test]
fn move_drag_snaps_to_clip_edge_and_cmd_bypasses_it() {
    let mut panel = build_viewport();

    // Snap ON.
    let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
    let mut ui_state = UIState::new();
    ui_state.select_clips(vec![ClipId::new("clip_a")]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.on_begin_drag(body_pos_for(&panel, 2.0), &mut host, &mut ui_state, &panel);
    overlay.on_drag(
        body_pos_for(&panel, 17.9),
        &mut host,
        &mut ui_state,
        &mut panel,
    );
    let snapped = host.find_clip_by_id("clip_a").unwrap().start_beat;
    assert_eq!(
        snapped,
        Beats::from_f32(16.0),
        "snap ON must land clip_a flush against clip_b's start edge"
    );

    // Same gesture, Cmd held: must NOT snap.
    let mut host2 = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
    let mut ui_state2 = UIState::new();
    ui_state2.select_clips(vec![ClipId::new("clip_a")]);
    let mut overlay2 = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay2.set_modifiers(Modifiers {
        command: true,
        ..Modifiers::NONE
    });
    overlay2.on_begin_drag(
        body_pos_for(&panel, 2.0),
        &mut host2,
        &mut ui_state2,
        &panel,
    );
    overlay2.on_drag(
        body_pos_for(&panel, 17.9),
        &mut host2,
        &mut ui_state2,
        &mut panel,
    );
    let raw = host2.find_clip_by_id("clip_a").unwrap().start_beat;
    assert_ne!(
        raw,
        Beats::from_f32(16.0),
        "Cmd held mid-drag must bypass snap entirely (raw position), got exactly the snapped edge"
    );
}

/// B12: a drag landing near a timeline marker snaps to it, exactly like
/// a clip edge — same shared `magnetic_snap`, a marker candidate is just
/// another entry it considers. Marker at beat 10.03 (deliberately off
/// the 0.25 grid at this zoom, so a snap to 10.03 can only be the
/// marker winning, not a coincidental grid line). Press `clip_a` at
/// mouse-beat 2.0, land at mouse-beat 12.02 -> anchor 10.02, within
/// range of the marker.
#[test]
fn move_drag_snaps_to_marker() {
    let mut panel = build_viewport();
    panel.set_markers(vec![crate::UiMarker::new(Beats::from_f32(10.03))]);

    let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
    let mut ui_state = UIState::new();
    ui_state.select_clips(vec![ClipId::new("clip_a")]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay.on_begin_drag(body_pos_for(&panel, 2.0), &mut host, &mut ui_state, &panel);
    overlay.on_drag(
        body_pos_for(&panel, 12.02),
        &mut host,
        &mut ui_state,
        &mut panel,
    );

    let snapped = host.find_clip_by_id("clip_a").unwrap().start_beat;
    assert_eq!(
        snapped,
        Beats::from_f32(10.03),
        "a drag landing near a marker must snap to it"
    );
}

/// a move
/// that actually lands somewhere new fires the flash with the landed
/// beat + layer span; a gesture that never moved stays dark (the flash
/// marks a landing, not a click).
#[test]
fn move_drag_fires_landing_flash_at_commit_only_when_landed() {
    let mut panel = build_viewport();
    let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
    let mut ui_state = UIState::new();
    ui_state.select_clips(vec![ClipId::new("clip_a")]);
    let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

    overlay.on_begin_drag(body_pos_for(&panel, 2.0), &mut host, &mut ui_state, &panel);
    overlay.on_drag(
        body_pos_for(&panel, 10.0),
        &mut host,
        &mut ui_state,
        &mut panel,
    );
    overlay.on_end_drag(&mut host);

    let (_, beat, lo, hi) = overlay
        .landing_flash()
        .expect("flash fires on a landed move");
    let landed = host.find_clip_by_id("clip_a").unwrap().start_beat;
    assert!(
        (beat - landed).abs() < Beats(1e-6),
        "flash beat {beat:?} must be the landed start beat {landed:?}"
    );
    assert_eq!(
        (lo, hi),
        (0, 0),
        "single-layer drag spans exactly its layer"
    );

    // Same gesture, zero displacement: begin then release in place.
    let mut host2 = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
    let mut ui_state2 = UIState::new();
    ui_state2.select_clips(vec![ClipId::new("clip_a")]);
    let mut overlay2 = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
    overlay2.on_begin_drag(
        body_pos_for(&panel, 2.0),
        &mut host2,
        &mut ui_state2,
        &panel,
    );
    overlay2.on_end_drag(&mut host2);
    assert!(
        overlay2.landing_flash().is_none(),
        "no flash when the gesture landed nowhere new"
    );
}
