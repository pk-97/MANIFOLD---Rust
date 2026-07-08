//! Drive interaction through the REAL input host, then the caller re-syncs and
//! re-renders. `select:<layer>` is sugar (`UI_AUTOMATION_DESIGN.md` §6) for a
//! one-step `AutomationAction::Pointer { target: Query{text}, gesture: Click }`
//! compiled to the §4 core (`super::script::click_by_text`): resolve the
//! target header's rect from the built tree, synthesize a real pointer
//! Down+Up (`UIRoot::pointer_event` → `UIInputSystem::process_pointer`),
//! drain the real `UIEvent`s, and dispatch through the real panel/bridge path
//! — the exact same seam the `--script` runner uses, not a parallel one. No
//! faked state, and no fallback on a miss (D6) — a miss is returned as an
//! `Err` and surfaced loudly by the caller (`mod.rs`'s `--interact` branch).
//! See `docs/HEADLESS_UI_HARNESS.md` §2.

use manifold_core::{Beats, ClipId, LayerId};

use super::fixtures::SceneData;
use crate::ui_root::UIRoot;

/// Outcome of an `--interact` spec: the stdout evidence line plus a
/// STRUCTURAL miss flag. The flag exists because the previous contract —
/// caller greps the description for a "MISS: " prefix — was dead code: no
/// verb emitted that exact string (the one that tried used "MISS —"), so
/// every miss exited 0 and rendered an "after" PNG of an interaction that
/// never happened (found 2026-07-07: `select:EXPANDED` miss exited 0).
pub struct InteractOutcome {
    pub desc: String,
    pub missed: bool,
}

/// Apply one interaction spec. Specs may be chained with `;` — each part is
/// applied in sequence against the same `data`/`ui` (e.g. a click then a
/// shift-click to build a multi-clip selection in one `--interact` string;
/// the CLI only threads one `--interact` value per process, so chaining
/// within the string is how a multi-gesture "before" scene gets built).
/// Mutates `data`'s selection/project so the caller can re-sync + re-render.
pub fn apply(ui: &mut UIRoot, data: &mut SceneData, spec: &str) -> InteractOutcome {
    let mut descs = Vec::new();
    let mut missed = false;
    for part in spec.split(';') {
        let outcome = apply_one(ui, data, part.trim());
        missed |= outcome.missed;
        descs.push(outcome.desc);
    }
    InteractOutcome { desc: descs.join(" | "), missed }
}

/// A verb's miss (target not found / input path didn't resolve). Every miss
/// path funnels through here so the flag can never drift from the text.
fn miss(desc: String) -> InteractOutcome {
    InteractOutcome { desc, missed: true }
}

fn hit(desc: String) -> InteractOutcome {
    InteractOutcome { desc, missed: false }
}

/// Map a verb's `Result` (Ok = applied, Err = target/args didn't resolve)
/// into the outcome the CLI branches on.
fn res(r: Result<String, String>) -> InteractOutcome {
    match r {
        Ok(desc) => hit(desc),
        Err(desc) => miss(desc),
    }
}

fn apply_one(ui: &mut UIRoot, data: &mut SceneData, spec: &str) -> InteractOutcome {
    // A verb's returned description marks a miss when its target didn't
    // resolve; the match below classifies structurally (miss()/hit()), so
    // the caller never string-greps. Verbs whose desc starts with a
    // "<verb>: no ..." / MISS text return via miss().
    match spec.split_once(':') {
        Some(("select", target)) => res(select_layer(ui, data, target)),
        Some(("collapse", target)) => res(collapse_layer(data, target)),
        Some(("collapse_effect", target)) => res(collapse_effect(ui, data, target)),
        Some(("delete", target)) => res(delete_layer(data, target)),
        Some(("open", "settings")) => {
            ui.settings_popup.open();
            hit("open -> settings popup".to_string())
        }
        Some(("open", "audio_setup")) => {
            ui.audio_setup_panel.open();
            hit("open -> audio setup panel".to_string())
        }
        Some(("automation_add", rest)) => res(automation_add_point(data, rest)),
        Some(("automation_move", rest)) => res(automation_move_point(data, rest)),
        Some(("automation_bend", rest)) => res(automation_bend_segment(data, rest)),
        Some(("automation_segment_drag", rest)) => res(automation_segment_drag(data, rest)),
        Some(("automation_group_move", rest)) => res(automation_group_move(data, rest)),
        Some(("automation_group_delete", rest)) => res(automation_group_delete(data, rest)),
        Some(("click_clip", rest)) => res(click_clip(data, rest)),
        Some(("shift_click_clip", rest)) => res(shift_click_clip(data, rest)),
        Some(("cmd_click_clip", rest)) => res(cmd_click_clip(data, rest)),
        Some(("cmd_d", _)) => res(duplicate_selected_clips(data)),
        Some(("drag_clip_toward_zero", rest)) => res(drag_clip_toward_zero(data, rest)),
        Some(("drag_readout", rest)) => res(drag_readout(ui, data, rest)),
        Some(("scroll_inspector", amount)) => res(scroll_inspector(ui, amount)),
        Some((verb, _)) => miss(format!(
            "unknown interact verb '{verb}' (known: select, collapse, collapse_effect, delete, \
             open, automation_add, automation_move, automation_bend, automation_segment_drag, \
             automation_group_move, automation_group_delete, click_clip, shift_click_clip, \
             cmd_click_clip, cmd_d, drag_clip_toward_zero, drag_readout, scroll_inspector)"
        )),
        None if spec == "cmd_d" => res(duplicate_selected_clips(data)),
        None => miss(format!("malformed interact '{spec}' (want verb:target)")),
    }
}

/// P1.0 evidence-gathering verb (`docs/TIMELINE_INTERACTION_P1_SPEC.md` P1.0):
/// `click_clip:<clip_id>:<layer_id>` — a plain click on a clip, driven through
/// `UIState::select_clip` exactly as `ui_bridge/editing.rs`'s
/// `PanelAction::ClipClicked` plain-click arm does (`editing.rs:60`). Selects
/// one clip and clears any region — the anchor for a subsequent
/// `shift_click_clip`/`cmd_click_clip` in a chained spec.
fn click_clip(data: &mut SceneData, rest: &str) -> Result<String, String> {
    let Some((clip_id, layer_id)) = rest.split_once(':') else {
        return Err(format!("click_clip: want clip_id:layer_id, got '{rest}'"));
    };
    data.selection
        .select_clip(ClipId::new(clip_id), LayerId::new(layer_id));
    Ok(format!("click_clip -> '{clip_id}' on '{layer_id}' (selection cleared, this clip selected)"))
}

/// `shift_click_clip:<clip_id>:<layer_id>` drives the clip-click shift path —
/// P1.3b's D2 fix (`docs/TIMELINE_INTERACTION_P1_SPEC.md`):
/// `crate::ui_bridge::select_clip_range_to_with_project`, the same function
/// `ui_bridge/editing.rs`'s `PanelAction::ClipClicked` shift arm now calls.
/// Before this phase this verb drove `select_region_to_with_project`
/// (a REGION) — S1/S3's root; it now drives the clip-RANGE selection D2
/// mandates, so the after-PNG shows the fixed chrome (per-clip highlight
/// across the range, no region band).
fn shift_click_clip(data: &mut SceneData, rest: &str) -> Result<String, String> {
    // `layer_id` isn't used — the clip-range gesture looks up the anchor's
    // and target's own layer/position internally; kept in the spec only for
    // symmetry with `click_clip`/`cmd_click_clip`.
    let Some((clip_id, _layer_id)) = rest.split_once(':') else {
        return Err(format!("shift_click_clip: want clip_id:layer_id, got '{rest}'"));
    };
    let cid = ClipId::new(clip_id);
    crate::ui_bridge::select_clip_range_to_with_project(&cid, &mut data.selection, &data.project);
    Ok(format!(
        "shift_click_clip -> range extended to '{clip_id}' \
         (selected_clip_ids={:?}, has_region={})",
        data.selection.get_selected_clip_ids(),
        data.selection.has_region(),
    ))
}

/// Cmd/ctrl-click path — mirrors `ui_bridge/editing.rs`'s `PanelAction::
/// ClipClicked` cmd arm. Post-D1 this is a pure `toggle_clip_selection`: no
/// region is synthesised from the clip set (the old
/// `update_region_from_clip_selection_inline` sync is deleted), so a multi-clip
/// selection produces a `Clips` selection with no region band — the S1 collapse
/// this verb now exercises.
fn cmd_click_clip(data: &mut SceneData, rest: &str) -> Result<String, String> {
    let Some((clip_id, layer_id)) = rest.split_once(':') else {
        return Err(format!("cmd_click_clip: want clip_id:layer_id, got '{rest}'"));
    };
    data.selection
        .toggle_clip_selection(ClipId::new(clip_id), LayerId::new(layer_id));
    Ok(format!(
        "cmd_click_clip -> toggled '{clip_id}'; selected_clip_ids={:?} has_region={}",
        data.selection.get_selected_clip_ids(),
        data.selection.has_region(),
    ))
}

/// P1.0 evidence-gathering verb: `cmd_d` (no argument) reproduces today's
/// Cmd+D path — `input_host.rs:572-604`'s `duplicate_clips` — using whatever
/// selection/region state a preceding chained verb left in `data.selection`.
/// Calls the same `EditingService::duplicate_clips` + `ui_translate::
/// selection_region_to_core` the real path calls, executes the resulting
/// commands directly on `data.project` (mirrors `input_host.rs:594-597`'s
/// local execute-for-readback), and reports the id churn so the PNG's
/// "before"/"after" clip count and the console's id list agree. S3's root
/// (D3): if a preceding `shift_click_clip` left `region.is_active` true, this
/// takes the REGION-duration offset branch even though 4 *specific* clips
/// were the intent — the "gap after the originals" symptom.
fn duplicate_selected_clips(data: &mut SceneData) -> Result<String, String> {
    let clip_ids = data.selection.get_selected_clip_ids();
    let region = data.selection.current_region().cloned().unwrap_or_default();
    let used_region_mode = region.is_active;
    let region_core = crate::ui_translate::selection_region_to_core(&region);
    let spb = 60.0 / data.project.settings.bpm.0.max(1.0);

    let before_ids: std::collections::HashSet<ClipId> = data
        .project
        .timeline
        .layers
        .iter()
        .flat_map(|l| l.clips.iter().map(|c| c.id.clone()))
        .collect();

    let mut commands = manifold_editing::service::EditingService::duplicate_clips(
        &data.project,
        &clip_ids,
        &region_core,
        spb,
    );
    if commands.is_empty() {
        return Err(format!(
            "cmd_d: no-op — 0 commands (selected_clip_ids={clip_ids:?}, region.is_active={used_region_mode})"
        ));
    }
    for c in commands.iter_mut() {
        c.execute(&mut data.project);
    }

    let new_ids: Vec<ClipId> = data
        .project
        .timeline
        .layers
        .iter()
        .flat_map(|l| l.clips.iter().filter(|c| !before_ids.contains(&c.id)).map(|c| c.id.clone()))
        .collect();

    data.selection.select_clips(new_ids.clone());

    Ok(format!(
        "cmd_d -> mode={} ({} commands) before_ids={} new_ids={new_ids:?}",
        if used_region_mode { "REGION" } else { "individual" },
        commands.len(),
        before_ids.len(),
    ))
}

/// P1.4 evidence verb (D5/D7, `docs/TIMELINE_INTERACTION_P1_SPEC.md`, S2):
/// `drag_clip_toward_zero:<clip_id>:<layer_id>` sets the clip's `start_beat`
/// directly to `(deeply_negative_candidate).max(Beats::ZERO)` — the same
/// clamp `InteractionOverlay::handle_move_drag`'s per-snapshot loop applies
/// on EVERY frame of a real drag (proven per-frame, not just at release, by
/// `interaction_overlay::p1_4_gesture_integrity_tests::
/// drag_toward_zero_clamps_every_frame_not_just_at_release`). Same
/// "drive the data field directly, the level under test is render reaction
/// not input dispatch" convention `collapse_layer`/`automation_add_point`
/// already establish — the drag MATH is unit-tested; this verb renders the
/// committed RESULT (a clip legitimately sitting at beat 0) so the PNG
/// proves D7's structural claim: the clip paints inside the tracks area,
/// never over the header column, regardless of how it got to beat 0.
fn drag_clip_toward_zero(data: &mut SceneData, rest: &str) -> Result<String, String> {
    let Some((clip_id, layer_id)) = rest.split_once(':') else {
        return Err(format!("drag_clip_toward_zero: want clip_id:layer_id, got '{rest}'"));
    };
    let Some(layer) = data.project.timeline.layers.iter_mut().find(|l| l.layer_id == layer_id)
    else {
        return Err(format!("drag_clip_toward_zero: no layer '{layer_id}'"));
    };
    let Some(clip) = layer.clips.iter_mut().find(|c| c.id.as_str() == clip_id) else {
        return Err(format!("drag_clip_toward_zero: no clip '{clip_id}' on layer '{layer_id}'"));
    };
    let requested = Beats(-40.0); // deeply negative — the drag-toward-zero repro
    clip.start_beat = requested.max(Beats::ZERO);
    Ok(format!(
        "drag_clip_toward_zero -> '{clip_id}' requested_beat={:.1} clamped_start_beat={:.3}",
        requested.0, clip.start_beat.0
    ))
}

/// P1.5 evidence verb (B13, `docs/TIMELINE_INTERACTION_P1_SPEC.md`, S5):
/// `drag_readout:<layer_id>:<clip_index>:<position_beat>:<duration_beat>`
/// sets `TimelineViewportPanel::drag_readout` directly to the given
/// (position, duration, layer_index) — the same "drive the data field
/// directly, the level under test is render reaction not input dispatch"
/// convention `drag_clip_toward_zero`/`collapse_layer` already establish.
/// The drag math that WOULD produce a live position/length is unit-tested
/// separately (`interaction_overlay::p1_4_gesture_integrity_tests`'s B11/B12
/// tests); this verb proves the RENDER reacts — the readout label appears
/// with the formatted bars.beats text. Keyed by `layer_id` + clip index
/// (not a clip id) because fixture clip ids are freshly minted UUIDs at
/// build time (`TimelineClip::default()` calls `short_id()`), not stable
/// literals a CLI arg could name.
fn drag_readout(ui: &mut UIRoot, data: &mut SceneData, rest: &str) -> Result<String, String> {
    let parts: Vec<&str> = rest.splitn(4, ':').collect();
    let [layer_id, idx_str, position_str, duration_str] = parts.as_slice() else {
        return Err(format!(
            "drag_readout: want layer_id:clip_index:position_beat:duration_beat, got '{rest}'"
        ));
    };
    let Some(layer_index) = data
        .project
        .timeline
        .layers
        .iter()
        .position(|l| l.layer_id == *layer_id)
    else {
        return Err(format!("drag_readout: no layer '{layer_id}'"));
    };
    let Ok(clip_index) = idx_str.parse::<usize>() else {
        return Err(format!("drag_readout: bad clip_index '{idx_str}'"));
    };
    if data.project.timeline.layers[layer_index].clips.get(clip_index).is_none() {
        return Err(format!(
            "drag_readout: no clip at index {clip_index} on layer '{layer_id}' ({} clips)",
            data.project.timeline.layers[layer_index].clips.len()
        ));
    }
    let Ok(position) = position_str.parse::<f64>() else {
        return Err(format!("drag_readout: bad position_beat '{position_str}'"));
    };
    let Ok(duration) = duration_str.parse::<f64>() else {
        return Err(format!("drag_readout: bad duration_beat '{duration_str}'"));
    };
    ui.viewport
        .set_drag_readout(Some((Beats(position), Beats(duration), layer_index)));
    Ok(format!(
        "drag_readout -> layer '{layer_id}' (index {layer_index}) clip[{clip_index}] \
         position={position:.3} duration={duration:.3}"
    ))
}

/// P4 Unit A evidence-gathering verb (`docs/AUTOMATION_LANES_DESIGN.md` §7):
/// `automation_add:<layer_id>:<param_id>:<beat>` adds a breakpoint at
/// `beat` with value = the param range's midpoint, directly on `data.
/// project` — same "drive the data field directly, the level under test is
/// render reaction not input dispatch" convention `collapse_layer` already
/// establishes. `AddAutomationPointCommand`'s data shape and the click-add
/// path's denormalize math (`InteractionOverlay::handle_automation_click`)
/// are both covered by unit tests instead (`manifold-editing`'s
/// `commands::automation::tests`, `manifold-ui`'s
/// `automation_hit_tester::tests` + `view::automation_lane_tests`); this verb
/// proves the RENDER reacts to a point that lands on an existing lane.
fn automation_add_point(data: &mut SceneData, rest: &str) -> Result<String, String> {
    let parts: Vec<&str> = rest.splitn(3, ':').collect();
    let [layer_id, param_id, beat_str] = parts.as_slice() else {
        return Err(format!("automation_add: want layer:param:beat, got '{rest}'"));
    };
    let Ok(beat) = beat_str.parse::<f64>() else {
        return Err(format!("automation_add: bad beat '{beat_str}'"));
    };
    let Some((min, max)) = lane_param_range(data, layer_id, param_id) else {
        return Err(format!("automation_add: no lane for '{layer_id}':'{param_id}'"));
    };
    let value = (min + max) * 0.5;
    let Some(lane) = find_lane_mut(data, layer_id, param_id) else {
        return Err(format!("automation_add: no lane for '{layer_id}':'{param_id}' (2nd lookup)"));
    };
    lane.points.push(manifold_core::effects::AutomationPoint {
        beat: manifold_core::Beats(beat),
        value,
        shape: manifold_core::effects::SegmentShape::Linear,
    });
    lane.points.sort_by(|a, b| a.beat.partial_cmp(&b.beat).unwrap());
    Ok(format!("automation_add -> '{layer_id}':'{param_id}' beat={beat} value={value:.3} ({} points now)", lane.points.len()))
}

/// P4 Unit A evidence-gathering verb: `automation_move:<layer_id>:<param_id>:
/// <point_index>:<new_beat>` moves an existing point's beat directly,
/// mirroring `set_automation_point_preview`'s live-drag mutation.
fn automation_move_point(data: &mut SceneData, rest: &str) -> Result<String, String> {
    let parts: Vec<&str> = rest.splitn(4, ':').collect();
    let [layer_id, param_id, idx_str, new_beat_str] = parts.as_slice() else {
        return Err(format!("automation_move: want layer:param:index:new_beat, got '{rest}'"));
    };
    let Ok(idx) = idx_str.parse::<usize>() else {
        return Err(format!("automation_move: bad index '{idx_str}'"));
    };
    let Ok(new_beat) = new_beat_str.parse::<f64>() else {
        return Err(format!("automation_move: bad beat '{new_beat_str}'"));
    };
    let Some(lane) = find_lane_mut(data, layer_id, param_id) else {
        return Err(format!("automation_move: no lane for '{layer_id}':'{param_id}'"));
    };
    let Some(point) = lane.points.get_mut(idx) else {
        return Err(format!("automation_move: no point at index {idx} ({} points)", lane.points.len()));
    };
    let old_beat = point.beat.0;
    point.beat = manifold_core::Beats(new_beat);
    lane.points.sort_by(|a, b| a.beat.partial_cmp(&b.beat).unwrap());
    Ok(format!("automation_move -> '{layer_id}':'{param_id}'[{idx}] beat {old_beat} -> {new_beat}"))
}

/// P4 Unit B evidence-gathering verb (`docs/AUTOMATION_LANES_DESIGN.md` §7):
/// `automation_bend:<layer_id>:<param_id>:<point_index>:<bend>` sets the
/// point at `point_index`'s shape to `Curved(bend)`, directly on `data.
/// project` — same "drive the data field, prove the RENDER reacts" level as
/// `automation_move_point`. Mirrors `InteractionOverlay::
/// commit_automation_segment_bend`'s end state (beat/value unchanged, only
/// `shape` differs); the drag math itself (pixel delta -> bend, Alt-gating,
/// whole_numbers gate) is unit-tested separately
/// (`interaction_overlay.rs`/`automation_hit_tester.rs`).
fn automation_bend_segment(data: &mut SceneData, rest: &str) -> Result<String, String> {
    let parts: Vec<&str> = rest.splitn(4, ':').collect();
    let [layer_id, param_id, idx_str, bend_str] = parts.as_slice() else {
        return Err(format!("automation_bend: want layer:param:index:bend, got '{rest}'"));
    };
    let Ok(idx) = idx_str.parse::<usize>() else {
        return Err(format!("automation_bend: bad index '{idx_str}'"));
    };
    let Ok(bend) = bend_str.parse::<f32>() else {
        return Err(format!("automation_bend: bad bend '{bend_str}'"));
    };
    let Some(lane) = find_lane_mut(data, layer_id, param_id) else {
        return Err(format!("automation_bend: no lane for '{layer_id}':'{param_id}'"));
    };
    let Some(point) = lane.points.get_mut(idx) else {
        return Err(format!("automation_bend: no point at index {idx} ({} points)", lane.points.len()));
    };
    point.shape = manifold_core::effects::SegmentShape::Curved(bend);
    Ok(format!("automation_bend -> '{layer_id}':'{param_id}'[{idx}] shape=Curved({bend})"))
}

/// P4 Unit B evidence-gathering verb: `automation_segment_drag:<layer_id>:
/// <param_id>:<left_index>:<right_index>:<value_delta>` moves both segment
/// endpoints' VALUES by `value_delta` (param range, unclamped — caller picks
/// safe numbers), directly on `data.project`. Mirrors `InteractionOverlay::
/// commit_automation_segment_value_drag`'s end state (beats + shapes
/// unchanged, only the two values shift by the same delta).
fn automation_segment_drag(data: &mut SceneData, rest: &str) -> Result<String, String> {
    let parts: Vec<&str> = rest.split(':').collect();
    let [layer_id, param_id, left_str, right_str, delta_str] = parts.as_slice() else {
        return Err(format!(
            "automation_segment_drag: want layer:param:left_index:right_index:delta, got '{rest}'"
        ));
    };
    let Ok(left_idx) = left_str.parse::<usize>() else {
        return Err(format!("automation_segment_drag: bad left_index '{left_str}'"));
    };
    let Ok(right_idx) = right_str.parse::<usize>() else {
        return Err(format!("automation_segment_drag: bad right_index '{right_str}'"));
    };
    let Ok(delta) = delta_str.parse::<f32>() else {
        return Err(format!("automation_segment_drag: bad delta '{delta_str}'"));
    };
    let Some(lane) = find_lane_mut(data, layer_id, param_id) else {
        return Err(format!("automation_segment_drag: no lane for '{layer_id}':'{param_id}'"));
    };
    let len = lane.points.len();
    if left_idx >= len || right_idx >= len {
        return Err(format!("automation_segment_drag: index out of range ({left_idx},{right_idx}) of {len} points"));
    }
    lane.points[left_idx].value += delta;
    lane.points[right_idx].value += delta;
    Ok(format!(
        "automation_segment_drag -> '{layer_id}':'{param_id}' [{left_idx},{right_idx}] both += {delta}"
    ))
}

/// P4 Unit B evidence-gathering verb: `automation_group_move:<layer_id>:
/// <param_id>:<comma_separated_indices>:<value_delta>` moves every listed
/// point's value by `value_delta` (unclamped), directly on `data.project` —
/// mirrors `InteractionOverlay::commit_automation_group_drag`'s end state
/// (a marquee-selected GROUP moved together, beats/shapes unchanged). The
/// grab/marquee-rect math itself is unit-tested separately
/// (`automation_hit_tester::dots_in_rect`).
fn automation_group_move(data: &mut SceneData, rest: &str) -> Result<String, String> {
    let parts: Vec<&str> = rest.splitn(4, ':').collect();
    let [layer_id, param_id, indices_str, delta_str] = parts.as_slice() else {
        return Err(format!("automation_group_move: want layer:param:indices:delta, got '{rest}'"));
    };
    let Ok(delta) = delta_str.parse::<f32>() else {
        return Err(format!("automation_group_move: bad delta '{delta_str}'"));
    };
    let mut indices = Vec::new();
    for s in indices_str.split(',') {
        match s.parse::<usize>() {
            Ok(i) => indices.push(i),
            Err(_) => {
                return Err(format!("automation_group_move: bad index '{s}' in '{indices_str}'"))
            }
        }
    }
    let Some(lane) = find_lane_mut(data, layer_id, param_id) else {
        return Err(format!("automation_group_move: no lane for '{layer_id}':'{param_id}'"));
    };
    let len = lane.points.len();
    for &idx in &indices {
        if idx >= len {
            return Err(format!("automation_group_move: index {idx} out of range ({len} points)"));
        }
    }
    for &idx in &indices {
        lane.points[idx].value += delta;
    }
    Ok(format!("automation_group_move -> '{layer_id}':'{param_id}' {indices:?} all += {delta}"))
}

/// P4 Unit B evidence-gathering verb: `automation_group_delete:<layer_id>:
/// <param_id>:<comma_separated_indices>` removes every listed point,
/// highest-index-first — mirrors `AppInputHost::delete_selected_automation_points`'s
/// per-lane ordering (proven generically by `manifold-editing`'s
/// `composite_group_delete_highest_index_first_survives_execute_and_undo`).
fn automation_group_delete(data: &mut SceneData, rest: &str) -> Result<String, String> {
    let parts: Vec<&str> = rest.splitn(3, ':').collect();
    let [layer_id, param_id, indices_str] = parts.as_slice() else {
        return Err(format!("automation_group_delete: want layer:param:indices, got '{rest}'"));
    };
    let mut indices = Vec::new();
    for s in indices_str.split(',') {
        match s.parse::<usize>() {
            Ok(i) => indices.push(i),
            Err(_) => {
                return Err(format!("automation_group_delete: bad index '{s}' in '{indices_str}'"))
            }
        }
    }
    indices.sort_unstable_by(|a, b| b.cmp(a)); // highest first
    indices.dedup();
    let Some(lane) = find_lane_mut(data, layer_id, param_id) else {
        return Err(format!("automation_group_delete: no lane for '{layer_id}':'{param_id}'"));
    };
    let before = lane.points.len();
    for idx in &indices {
        if *idx < lane.points.len() {
            lane.points.remove(*idx);
        }
    }
    Ok(format!(
        "automation_group_delete -> '{layer_id}':'{param_id}' removed {indices:?}; {before} -> {} points",
        lane.points.len()
    ))
}

/// Find the automation lane for `param_id` among `layer_id`'s effects (checked
/// first, matching the design's effect-then-gen-params walk order).
fn find_lane_mut<'a>(
    data: &'a mut SceneData,
    layer_id: &str,
    param_id: &str,
) -> Option<&'a mut manifold_core::effects::AutomationLane> {
    let layer = data.project.timeline.layers.iter_mut().find(|l| l.layer_id == layer_id)?;
    if let Some(effects) = layer.effects.as_mut() {
        for fx in effects.iter_mut() {
            if let Some(lanes) = fx.automation_lanes.as_mut()
                && let Some(pos) = lanes.iter().position(|l| l.param_id.as_ref() == param_id)
            {
                return lanes.get_mut(pos);
            }
        }
    }
    None
}

/// Resolved `(min, max)` param range for the lane `find_lane_mut` would
/// return — a separate read-only pass (registry lookup needs `&PresetDef`,
/// awkward to thread through the `&mut` walk above) so `automation_add_point`
/// can compute a midpoint value before mutating.
fn lane_param_range(data: &SceneData, layer_id: &str, param_id: &str) -> Option<(f32, f32)> {
    let layer = data.project.timeline.layers.iter().find(|l| l.layer_id == layer_id)?;
    let effects = layer.effects.as_ref()?;
    for fx in effects {
        if let Some(param) = fx.params.get(param_id) {
            return Some((param.spec.min, param.spec.max));
        }
    }
    None
}

/// P0.0 evidence-gathering verb: flip `is_collapsed` directly on the target
/// layer's `Project` data (same field `states_scene` sets directly — see
/// `fixtures.rs`), NOT via a synthesized chevron click. The bug under
/// investigation (`docs/TIMELINE_LAYOUT_P0_SPEC.md` RC1-RC3) lives in the
/// render/sync path's reaction to the resulting state, not in input dispatch,
/// so driving the data field directly is the right level for this phase.
/// `scroll_inspector:<delta>` — calls `UIRoot::try_inspector_scroll` directly,
/// the SAME real, synchronous method `window_input.rs`'s mouse-wheel handler
/// calls when the cursor is over the inspector rect. Not sugar for a
/// `Gesture::Scroll`: the inspector's scroll is a direct call
/// (`try_scroll_in_place`, offsetting the built content nodes in place), not
/// routed through the generic `UIEvent::Scroll` → `pending_events` →
/// `drain_and_dispatch` pipeline `Gesture::Scroll` synthesizes into — that
/// pipeline is real for the dropdown/timeline, but a no-op here (found while
/// building `UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md`'s BUG-060 gate scene: 15
/// chained `Gesture::Scroll`s at the inspector moved content ~13px total,
/// clamped, and stayed there — the event was never reaching
/// `try_inspector_scroll` at all). `cursor_x` is the inspector rect's
/// horizontal center, matching whichever column (master/layer) is showing.
/// Positive `delta` scrolls DOWN (reveals later content) — the same sign
/// `window_input.rs` passes `dy` in.
fn scroll_inspector(ui: &mut UIRoot, amount: &str) -> Result<String, String> {
    let delta: f32 = amount
        .parse()
        .map_err(|_| format!("scroll_inspector: '{amount}' is not a number"))?;
    let rect = ui.layout.inspector();
    let cursor_x = rect.x + rect.width * 0.5;
    let handled = ui.try_inspector_scroll(delta, cursor_x);
    if !handled {
        // No scroll container built at this cursor_x (e.g. inspector empty) —
        // structurally a miss, not a silent no-op.
        return Err(format!(
            "scroll_inspector: try_inspector_scroll({delta}, {cursor_x}) found no scroll container"
        ));
    }
    Ok(format!("scroll_inspector -> delta={delta} cursor_x={cursor_x}"))
}

fn collapse_layer(data: &mut SceneData, target: &str) -> Result<String, String> {
    let Some(layer) = data.project.timeline.layers.iter_mut().find(|l| l.layer_id == target)
    else {
        return Err(format!("collapse: no layer with id '{target}'"));
    };
    layer.is_collapsed = !layer.is_collapsed;
    Ok(format!("collapse -> layer '{target}' is_collapsed={}", layer.is_collapsed))
}

/// P2 "caret rotate" evidence verb (`docs/UI_CRAFT_AND_MOTION_PLAN.md` P2):
/// `collapse_effect:<layer-id>` toggles `.collapsed` on `target`'s FIRST
/// effect, both on the model (data field, same "drive the model directly"
/// convention as `collapse_layer` above) AND directly on the already-built
/// card panel via `ParamCardPanel::set_collapsed` — the same "test/
/// automation harness drives it directly, snapping `collapse_anim`, no
/// ease" setter the panel's own doc comment names for exactly this use.
/// Driving the model alone isn't enough for a SINGLE headless frame: the
/// panel already exists from the base render, so the caller's follow-up
/// `sync_build` → `configure()` call sees a card that's configured once
/// already and EASES toward the new collapsed state instead of snapping
/// (matching a real live toggle) — with no per-frame tick loop in this
/// one-shot tool, the chevron would still show mid-flight (near its
/// expanded angle), not the fully-collapsed rotation this verb exists to
/// prove. Snapping the panel here first means the follow-up `configure()`
/// finds it already at the target and leaves it alone.
fn collapse_effect(ui: &mut UIRoot, data: &mut SceneData, target: &str) -> Result<String, String> {
    let Some(layer) = data.project.timeline.layers.iter_mut().find(|l| l.layer_id == target)
    else {
        return Err(format!("collapse_effect: no layer with id '{target}'"));
    };
    let Some(effects) = layer.effects.as_mut() else {
        return Err(format!("collapse_effect: layer '{target}' has no effects"));
    };
    let Some(fx) = effects.first_mut() else {
        return Err(format!("collapse_effect: layer '{target}' has an empty effect list"));
    };
    fx.collapsed = !fx.collapsed;
    let new_collapsed = fx.collapsed;
    if let Some(card) = ui.inspector.layer_effect_mut(0) {
        card.set_collapsed(new_collapsed);
    }
    Ok(format!("collapse_effect -> layer '{target}' effect[0] collapsed={new_collapsed}"))
}

/// P0.0 evidence-gathering verb: remove the target layer (and any children
/// parented to it) from the `Project`, mirroring what `EditingService`'s
/// delete command achieves at the data level. No synthesized click/menu.
fn delete_layer(data: &mut SceneData, target: &str) -> Result<String, String> {
    let before = data.project.timeline.layers.len();
    data.project
        .timeline
        .layers
        .retain(|l| l.layer_id != target && !l.parent_layer_id.as_ref().is_some_and(|pid| *pid == target));
    let after = data.project.timeline.layers.len();
    if before == after {
        return Err(format!("delete: no layer with id '{target}' (or children) found"));
    }
    Ok(format!("delete -> removed '{target}' and any children; {before} -> {after} layers"))
}

/// `select:<layer_id>` sugar (§6): looks up the layer's display name (the
/// CLI's existing `select:<id>` contract, unchanged) and compiles to a
/// one-step `Pointer { target: Query{text}, gesture: Click }` against the §4
/// core (`super::script::click_by_text`) — the same resolver + gesture
/// dispatch the `--script` runner uses, not a parallel reimplementation. A
/// synthesized-click miss is no longer silently patched over with an id
/// lookup (D6, the seam this phase removes): it comes back as `Err` and the
/// caller (`mod.rs`'s `--interact` branch) fails the run loudly with the dump.
fn select_layer(ui: &mut UIRoot, data: &mut SceneData, target: &str) -> Result<String, String> {
    let Some(idx) = data
        .project
        .timeline
        .layers
        .iter()
        .position(|l| l.layer_id == target)
    else {
        return Err(format!("select: no layer with id '{target}'"));
    };
    let name = data.project.timeline.layers[idx].name.clone();

    match super::script::click_by_text(ui, data, &name) {
        // `apply_panel_actions` (script.rs) already set `data.active` from the
        // real `PanelAction::LayerClicked` the click produced — `idx` (the
        // pre-click lookup) is reported here only for the human-readable
        // description, not as a stand-in for it.
        Ok(()) => Ok(format!(
            "select -> layer {idx} '{name}' (real click dispatched through the automation core)"
        )),
        Err(e) => Err(format!(
            "select: MISS — synthesized click on '{name}' did not resolve ({e}); the real input \
             path was NOT exercised and no fallback selection was applied"
        )),
    }
}
