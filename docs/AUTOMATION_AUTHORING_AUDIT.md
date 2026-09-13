# Automation authoring audit

<!-- index: September 2026 audit of automation authoring, playback, recording, undo, and gaps against an Ableton-style workflow. -->

Dated snapshot: 2026-09-13 at `03c0a4d73b418de3139b58d04f9b2df0209e1ff4`. Review only; no implementation or replacement design approved.

**Implementation follow-up (2026-09-13):** Peter authorized the authoring workstream after this audit. Collision undo, moved-point selection, selection visibility, fine point dragging, accidental empty-space insertion, viewport-edge segment editing, explicit Show Automation, manifest labels, lane resizing, stopped/paused sampling, live drag preview, Escape cancellation, and grouped phrase movement are implemented. Automation clipboard cut/copy/paste and duplication now use undoable commands. Curve recording preserves the exact outside-take curve, including nested `CurvedRange` clipping. Recording gestures flush on stop, pause, disarm, seek, save, and export; export samples arrangement automation while restoring live parameter values. Searchable device/parameter choosers and master/group editors remain unfinished.

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
| Export | [content export](../crates/manifold-app/src/content_export.rs), frame `engine.tick` | Samples arrangement automation in export mode, flushes pending recording gestures, and restores live parameter values afterward. |

Scene-modifier parameters exposed into the owning preset manifest already fit this addressing model. No new envelope engine is needed for Separation, Orbit, Rise or Phase. Modifier removal captures/restores automation in its [command snapshot](../crates/manifold-editing/src/commands/graph/scene_modifier.rs).

## Main authoring gaps

1. **Finding and managing lanes is unfinished.** The path is A/LANES, expand the layer, begin a parameter gesture, then find its strip. There is no device/parameter chooser, search, “+”, independent hide/pin/reorder or lane resize. All enabled lanes are projected, plus one chosen placeholder. Labels use preset type and raw parameter ID instead of the names/sections available on `ParamSurface`. Master effects have backend automation but no master-lane projection; group layers are excluded. Sources: [translation](../crates/manifold-app/src/ui_translate.rs), `layer_automation_lanes_to_ui`; [timeline projection](../crates/manifold-app/src/ui_bridge/projection/timeline.rs), `viewport_lanes`. The [existing design](AUTOMATION_LANES_DESIGN.md) explicitly records chooser/+ as unbuilt.

2. **The editing surface gives too little feedback.** Lanes are fixed at 28px. The renderer receives no selection state and draws identical dots, no selected-point distinction, and no exact value/time readout. There is no point-value editor; the context menu only clears automation or removes the lane. Sources: [constants](../crates/manifold-ui/src/color.rs), `AUTOMATION_LANE_STRIP_HEIGHT`; [lane renderer](../crates/manifold-renderer/src/automation_lane_draw.rs); [dropdowns](../crates/manifold-app/src/ui_root/dropdowns.rs), `AutomationLaneRightClicked`. An inspected historical render shows cramped strips; this is not a current live UI verification.

3. **Phrase operations are now implemented for the supported clipboard/duplication path.** Group movement preserves time/value spacing and uses one undo step; automation cut/copy/paste and duplication are undoable. Time stretch, shape insertion and simplification remain unbuilt.

4. **Pencil and precision gestures are incomplete.** Pencil inserts/replaces only at received pointer samples, leaving intervening old points. Continuous parameters use Linear rather than grid-width Hold steps. Drawing always calls snap without the Cmd bypass used by single-point dragging. Single-point/group drags lack Shift fine-value handling; vertical segment drag has it. A plain click anywhere in a strip adds a point, so attempted deselection can change the envelope. Source: [overlay](../crates/manifold-ui/src/interaction_overlay.rs), `apply_draw_point`, `write_automation_draw_step`, `handle_automation_drag`, `handle_automation_click`.

5. **Authoring preview is split from playback.** Point, segment, bend and pencil previews mutate `local_project`; their content commands are sent on release. The line can move before the content renderer receives the envelope. Automation samples only in `tick_playing`; stopped seek does not evaluate it. Inspecting an exact destroyed pose is therefore awkward even with a correct envelope. Sources: [editing host](../crates/manifold-app/src/editing_host.rs), preview/commit methods; [app render](../crates/manifold-app/src/app_render.rs), `sync_clip_positions`; [engine](../crates/manifold-playback/src/engine.rs), playing/non-playing ticks. This is code-path evidence, not measured live latency.

6. **Override and lane lifecycle controls are incomplete.** Global BACK exists, but no per-parameter re-enable path was found. `SetLaneEnabledCommand` has no production UI caller. Disabled lanes disappear; “Remove Lane” deletes data rather than hiding its editor. B changes an internal mode flag without a dedicated visible pencil-state control. A does not clear automation selections. Sources: [input host](../crates/manifold-app/src/input_host.rs), automation methods; [transport dispatch](../crates/manifold-app/src/ui_bridge/transport.rs); [translation](../crates/manifold-app/src/ui_translate.rs).

## Correctness findings

**P1 — Collisions break exact undo (BUG-xz2w, reproduced).** `insert_sorted` allows duplicate beats; add-undo and move/undo locate the first matching beat. A real-code probe started with `(0,0), (8,1)`, added `(0,0.7)`, then undid: result `(0,0.7), (8,1)`, losing the original value. Moving the beat-8 point onto beat 0 and undoing also failed restoration. UI preview uses the same beat-only lookup. Source: [commands](../crates/manifold-editing/src/commands/automation.rs), `insert_sorted`, `AddAutomationPointCommand::undo`, `MoveAutomationPointCommand::apply`.

**P1 — Recording punch boundaries are repaired.** The join inserts exact representable-beat guard points sampled from the original lane and uses `CurvedRange` for clipped power curves, preserving dense samples outside the take, including nested clips. Source: [core automation](../crates/manifold-core/src/effects/automation.rs), [playback automation](../crates/manifold-playback/src/automation.rs).

**P1 risk — Recording lifecycle/export isolation is repaired.** Explicit flush paths cover stop, pause, disarm, seek, save, and export. Export uses arrangement sampling without consuming live touches, latches, or gestures, then restores live parameter bases. Sources: [playback automation](../crates/manifold-playback/src/automation.rs), [content export](../crates/manifold-app/src/content_export.rs), and the owning engine/content lifecycle callers.

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
