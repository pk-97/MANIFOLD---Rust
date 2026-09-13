# Automation authoring audit

<!-- index: September 2026 audit of automation authoring, playback, recording, undo, and gaps against an Ableton-style workflow. -->

Dated snapshot: 2026-09-13 at `03c0a4d73b418de3139b58d04f9b2df0209e1ff4`. Review only; no implementation or replacement design approved.

**Implementation follow-up (2026-09-13):** Peter authorized the authoring workstream after this audit. The first implementation slice repairs collision undo, moved-point selection, selection visibility, fine point dragging, accidental empty-space insertion, and viewport-edge segment editing. The next authoring slice adds explicit Show Automation, manifest labels, individual lane resizing, and stopped/paused sampling. The audit findings below remain the historical baseline. Searchable device/parameter choosers, master/group editors, clipboard/time transforms, authoritative live drag preview, and recording lifecycle work remain unfinished.

**Conclusion:** retain the automation model and extend the existing editor. The UI is the main obstacle to Corrosion's “separate → hold → burst” sequence, but the backend also has correctness gaps. This needs an automation-authoring workstream with targeted reliability fixes.

## Existing foundation

| Piece | Source | Assessment |
|---|---|---|
| Persistent envelopes | [core automation](../crates/manifold-core/src/effects/automation.rs), `AutomationLane` | Per-instance, param-ID-addressed, absolute arrangement beats; linear, hold and curved segments. Optional serialization and orphan restoration have tests. |
| Evaluation | [playback automation](../crates/manifold-playback/src/automation.rs), `evaluate_all_automation` | Master effects, layer effects and generator parameters sample before modulation. Values hold outside the first/last point. Periodic parameters can wrap. |
| Modulation | [modulation](../crates/manifold-playback/src/modulation.rs) | Automation writes base; downstream modulation determines effective output. Absolute LFO/audio modulation can mask the envelope. |
| Commands | [editing commands](../crates/manifold-editing/src/commands/automation.rs) | Add/move/delete, clear/remove/enable lane and whole-gesture commits exist. Segment/group edits batch undo. Collision handling is unsafe. |
| Gestures | [overlay](../crates/manifold-ui/src/interaction_overlay.rs), `handle_automation_click`, `begin_automation_drag` | Point edits, segment lift/bend, marquee, vertical group movement and pencil. A shows lanes; B toggles pencil. |
| Entry point | [scrub dispatch](../crates/manifold-app/src/ui_bridge/scrub.rs), `ScrubPhase::Begin` | Beginning a layer-scoped parameter gesture chooses that parameter; an unautomated parameter gets an implicit flat lane. |
| Indicators | [projection](../crates/manifold-app/src/ui_bridge/projection/inspector.rs), [slider builder](../crates/manifold-ui/src/panels/param_slider_shared/builders.rs) | Automation dots and overridden coloring exist. Transport exposes LANES/BACK/ARM. |
| Export | [content export](../crates/manifold-app/src/content_export.rs), frame `engine.tick` | Reaches the same sampler, but live-state isolation and unfinished recording need attention. |

Scene-modifier parameters exposed into the owning preset manifest already fit this addressing model. No new envelope engine is needed for Separation, Orbit, Rise or Phase. Modifier removal captures/restores automation in its [command snapshot](../crates/manifold-editing/src/commands/graph/scene_modifier.rs).

## Main authoring gaps

1. **Finding and managing lanes is unfinished.** The path is A/LANES, expand the layer, begin a parameter gesture, then find its strip. There is no device/parameter chooser, search, “+”, independent hide/pin/reorder or lane resize. All enabled lanes are projected, plus one chosen placeholder. Labels use preset type and raw parameter ID instead of the names/sections available on `ParamSurface`. Master effects have backend automation but no master-lane projection; group layers are excluded. Sources: [translation](../crates/manifold-app/src/ui_translate.rs), `layer_automation_lanes_to_ui`; [timeline projection](../crates/manifold-app/src/ui_bridge/projection/timeline.rs), `viewport_lanes`. The [existing design](AUTOMATION_LANES_DESIGN.md) explicitly records chooser/+ as unbuilt.

2. **The editing surface gives too little feedback.** Lanes are fixed at 28px. The renderer receives no selection state and draws identical dots, no selected-point distinction, and no exact value/time readout. There is no point-value editor; the context menu only clears automation or removes the lane. Sources: [constants](../crates/manifold-ui/src/color.rs), `AUTOMATION_LANE_STRIP_HEIGHT`; [lane renderer](../crates/manifold-renderer/src/automation_lane_draw.rs); [dropdowns](../crates/manifold-app/src/ui_root/dropdowns.rs), `AutomationLaneRightClicked`. An inspected historical render shows cramped strips; this is not a current live UI verification.

3. **A phrase cannot be manipulated as a phrase.** Marquee selects dots, but group dragging only changes values. No automation-specific clipboard, duplicate, time-selection editing, horizontal group move, stretch, shape insertion or simplification was found. Cmd+A/C/V/D route to clips/effects/layers. Arrangement lanes remain at absolute beats; clip move/duplicate commands do not carry the corresponding lane interval. Sources: [group drag](../crates/manifold-ui/src/interaction_overlay.rs), `handle_automation_group_drag`; [keyboard routing](../crates/manifold-app/src/input_handler.rs). This blocks shifting the entire burst to another drop or reusing a transformation across parameters.

4. **Pencil and precision gestures are incomplete.** Pencil inserts/replaces only at received pointer samples, leaving intervening old points. Continuous parameters use Linear rather than grid-width Hold steps. Drawing always calls snap without the Cmd bypass used by single-point dragging. Single-point/group drags lack Shift fine-value handling; vertical segment drag has it. A plain click anywhere in a strip adds a point, so attempted deselection can change the envelope. Source: [overlay](../crates/manifold-ui/src/interaction_overlay.rs), `apply_draw_point`, `write_automation_draw_step`, `handle_automation_drag`, `handle_automation_click`.

5. **Authoring preview is split from playback.** Point, segment, bend and pencil previews mutate `local_project`; their content commands are sent on release. The line can move before the content renderer receives the envelope. Automation samples only in `tick_playing`; stopped seek does not evaluate it. Inspecting an exact destroyed pose is therefore awkward even with a correct envelope. Sources: [editing host](../crates/manifold-app/src/editing_host.rs), preview/commit methods; [app render](../crates/manifold-app/src/app_render.rs), `sync_clip_positions`; [engine](../crates/manifold-playback/src/engine.rs), playing/non-playing ticks. This is code-path evidence, not measured live latency.

6. **Override and lane lifecycle controls are incomplete.** Global BACK exists, but no per-parameter re-enable path was found. `SetLaneEnabledCommand` has no production UI caller. Disabled lanes disappear; “Remove Lane” deletes data rather than hiding its editor. B changes an internal mode flag without a dedicated visible pencil-state control. A does not clear automation selections. Sources: [input host](../crates/manifold-app/src/input_host.rs), automation methods; [transport dispatch](../crates/manifold-app/src/ui_bridge/transport.rs); [translation](../crates/manifold-app/src/ui_translate.rs).

## Correctness findings

**P1 — Collisions break exact undo (BUG-xz2w, reproduced).** `insert_sorted` allows duplicate beats; add-undo and move/undo locate the first matching beat. A real-code probe started with `(0,0), (8,1)`, added `(0,0.7)`, then undid: result `(0,0.7), (8,1)`, losing the original value. Moving the beat-8 point onto beat 0 and undoing also failed restoration. UI preview uses the same beat-only lookup. Source: [commands](../crates/manifold-editing/src/commands/automation.rs), `insert_sorted`, `AddAutomationPointCommand::undo`, `MoveAutomationPointCommand::apply`.

**P1 — Recording changes the curve outside its punch interval (BUG-jxg7, reproduced).** With `(0,0) → (8,1)`, recording one touch at beat 2/value .9 and closing changes beat 1 from .125 to .45 and beat 3 from .375 to approximately .9167. The join preserves old points, not the original curve outside the recorded interval; no boundary anchors are inserted. Curved segments need preservation too. Source: [playback automation](../crates/manifold-playback/src/automation.rs), `close_expired_gestures`.

**P1 risk — Recording lifecycle/export isolation (BUG-jxg7, static evidence).** Recording accumulates privately until two arrangement beats of inactivity elapse. Stop/pause/disarm do not explicitly finalize; stopped ticks do not close gestures. Save cannot include an uncommitted private gesture. `initialize` does not clear automation gestures/latches despite replacing the project. Export retains live automation state and discards pending gesture commits rather than executing them. Focused stop/save/reload/loop/export reproductions are still required. Sources: [engine](../crates/manifold-playback/src/engine.rs), `initialize`, `stop`, `pause`, `set_automation_armed`; [export](../crates/manifold-app/src/content_export.rs), frame tick/reclaim; [normal content tick](../crates/manifold-app/src/content_thread.rs), gesture commit drain.

**P2 — Selection becomes stale after moving a point in time (static evidence).** Selection keeps the original beat; dragging updates private last-beat state but not `selected_automation_point`. Delete looks up the original beat, potentially doing nothing or targeting a different point. Sources: [overlay](../crates/manifold-ui/src/interaction_overlay.rs), point drag begin/update/end; [input host](../crates/manifold-app/src/input_host.rs), `delete_selected_automation_point`.

**P2 — Visible segments lose editability at viewport boundaries (static evidence).** The line samples the full viewport, but dots are culled to visible beats. Segment hit-testing only uses consecutive visible dots. A visible segment with an off-screen endpoint becomes an empty-strip hit: drag starts marquee instead of segment movement/bending. Sources: [viewport](../crates/manifold-ui/src/panels/viewport.rs), `automation_lane_screens`; [hit tester](../crates/manifold-ui/src/automation_hit_tester.rs), `hit_test_automation`.

**Additional debt:** sampling and UI geometry allocate temporary vectors; no performance measurement was made. Out-of-range points are clamped during UI normalization and drag baselines are reconstructed from that clamped value, so the editor cannot faithfully represent every multi-turn curve the sampler supports. Sources: evaluator local vectors; [translation](../crates/manifold-app/src/ui_translate.rs), `push_instance_automation_lanes`; overlay drag capture.

## Reference and recommended scope

The current design overstates Ableton parity. Live 12 documents device/parameter choosers, independent lane hide/show, exact values, automation clipboard/time operations, selection transforms and per-parameter re-enable. It also distinguishes Automation Arm from Arrangement Record; Manifold records while armed and playing without checking its recording flag. Several current Live mouse gestures differ from the older Manifold spec. Agree the interaction contract using the [current Ableton manual](https://www.ableton.com/en/live-manual/12/automation-and-editing-envelopes/), not the old spec's parity claims.

The existing approved choice to put strips **under** layers need not change.

Recommended first scope: make Corrosion's three-state sequence comfortable to author and revise. Provide explicit “show automation” without changing a value, a named/resizable lane, selection feedback and exact values, clean ramp/hold/bend editing, phrase movement/copy in time, and authoritative stopped inspection/live preview. Repair collision undo and recording boundaries alongside this work. Reuse `ParamSurface`, parameter identity, undo commands and content-thread ownership.

## Verification and limits

- `cargo test -p manifold-core -p manifold-editing -p manifold-playback -p manifold-ui --lib automation`: **84 passed**, none failed/ignored. This filter includes generic UI test-driver tests, not just musical automation. First attempt was sandbox-blocked by sccache; the permitted retry passed.
- A temporary Rust probe linked the actual core/editing libraries and included the current playback automation module. It confirmed both collision/undo cases and the punch-boundary failure. No app source changed.
- Inspected [historical lane evidence](evidence/automation-ui/automation_group_move.after.png). App inventory exposed no running Manifold window. No current live UI, GPU/render test, Corrosion authoring session, export or controller recording was performed.
- Existing first-click/point-drag UI scripts were reviewed, not rerun. Their coordinate assertions do not establish collision undo, exact values, recording finalization or content preview during a drag.
- Remaining UX/audit follow-ups are recorded on **BUG-i70**; correctness work is **BUG-xz2w** and **BUG-jxg7**. No implementation is part of this audit.
