# Overnight 2026-10-19: morning report

Plan: `docs/evidence/overnight_2026-10-19/PLAN.md` at 25af1c932. All 27 lanes ran, then the continuation queue: the rescreen, tooling and patterns sweeps, the test suite review design (child 1 of BUG-hkbdp (renderer crate split)) and BUG-bkfv2 (split bulky gate scopes). Every commit below is on origin/main. What only you can do is in `PETER.md` next to this file.

## Commits landed (first-parent on main)
- 7eae46856 lane 4a, rendering coupling census; 471a78141 lane 4b, app and UI coupling census (docs only).
- d71aa9afe lane 10, process-layer deletions (land_wave.py, three dead guards, verdict stamping), landed alone, beads 1761dcb08; 6fcb708d2 lane 13, rule sources down to CLAUDE.md plus AGENT_ROUTING.md, beads 299f55b4d.
- eff154097 batch 1: lane 1 green main, lane 22 data and edit bugs, lanes 2a and 2b engine dead code; beads 51dc18a4b; a04906014 fixes a docs red batch 1 landed past.
- 0e15c9f53 batch 2: lanes 2c and 2d dead code, 23 per-frame waste, 27 pre-1.11 loading dropped, 25 switched-off features deleted, deleted-API doc sweep; beads 5972c5b14.
- 4713b8bfc batch 3a: lanes 5 playback bugs, 6 one param lookup, 7 effect cards by id, 8 one typing path; beads 8622ae06b. 7742ea27d batch 3b: lane 9 repaint flags onto the frame planner.
- 9067a49a5 batch 4: lanes 15 light and environment gates, 14 scene params drawn by kind, 16 sky sun follows the Sun light, 26 box spawner onto the body, 3 render_scene.rs split (9,726 to 3,076 lines); beads f7fa40bbf.
- 89a6e39e3 batch 5: lanes 17 colour row, 19 audio defaults, 11 godfile ratchet as one JSON, 24 rack bypass and wet/dry, 20 Scene tab in two panes, 21 materials preset and add-feature menus, 18 small fixes; beads 77f8c7844.
- 3a0aa0c0e batch 6: lane 12 board, register and beads (docs/archive deleted), the rescreen, tooling and patterns sweeps, the gate-scope split, the test suite design doc; beads 3caad3144. Then the commit that adds this report, PETER.md additions and renders.

## Lines per crate, 25af1c932 to 3a0aa0c0e (moved = removed lines re-added elsewhere)
| crate | deleted | moved | added | net |
|---|---|---|---|---|
| manifold-node-engine | 2156 | 99 | 93 | -2162 |
| manifold-io | 2245 | 46 | 284 | -2007 |
| manifold-nodes-image | 769 | 21 | 4 | -786 |
| manifold-core | 1638 | 46 | 1027 | -657 |
| manifold-editing | 568 | 17 | 179 | -406 |
| manifold-playback | 878 | 15 | 510 | -383 |
| manifold-nodes-scene | 2178 | 5243 | 7044 | -377 |
| manifold-ui | 2945 | 199 | 2876 | -268 |
| manifold-nodes | 656 | 15 | 477 | -194 |
| manifold-gpu, compositor, ui-paint, foundation, media | 753 | 12 | 63 | -702 |
| manifold-water-rigid | 29 | 9 | 67 | +29 |
| manifold-app | 1868 | 317 | 2548 | +363 |
| native, fluids, audio, led, nodes-water | 51 | 0 | 6 | -45 |
| **Rust total** | 16734 | 6039 | 15178 | -7595 |
| docs, scripts, .claude, plugins, root | 31391 | 140 | 2543 | -28988 |

Unified, by lane: 6 one node-param lookup (core `stamped_node_params`; NodeParamProvider and SceneExposureSource gone); 7 effect cards addressed by id (ui, app); 8 nine text-entry kinds commit through one `text_session::commit` (ui, app); 9 repaint writes onto the frame planner, 59 writes gone (ui, app); 14 one draw decision per scene param row, `ParamRow::kind` (ui, app, core); 17 one colour row for every RGB triplet (ui); 26 one migrate-then-retire helper at all three retire sites (nodes, core); 11 one godfile ceiling JSON (app tests, scripts).

## Reds
Before: the plan's 13 known reds. Fixed 9: BUG-fqj7s (workspace nextest), BUG-ryf9u (feature matrix), BUG-gol9y (flow sentinel selection), BUG-n5jsg (RT dynamic export fixture), BUG-3a09y (rt_dynamic_export envmap), BUG-tv0yy (render-mode-solid flow), BUG-s6efv (whitewater fuse regions), BUG-cavux (Math View migration journeys), BUG-7uo11 (angular wrap journey). BUG-c0e04 (LEAK under nextest) did not reproduce and is closed.
Still red, 3: BUG-rqsaa (five work- flows over frame budget, expected_fail; reopened this morning, it had been closed with no fix), BUG-gm08t (two flows, expected_fail, needs a design call), BUG-kqz05 (code_terminal GPU proof, nondeterministic).
Found tonight: BUG-pl747 (whitewater golden), BUG-nfk6b (Math View historical-snapshots journey), BUG-xf63z (Math View grid journey), both behind the journey-proofs feature and outside the gate, BUG-3hohk (gate reap test flake), BUG-umopp (gpu-scope test reads the live diff; green on main). Last gates: batch 5 46 of 46; batch 6 75 of 76, its one red (doc citations broken between two lanes) fixed before landing.

## Rows skipped, with reasons
- 2a: set_recorded_bpm (app tests use it), TEXT/HDR_BOOST (unnamed), map_value (now test-only). 2b: apply_detected_bpm (cascades into a field and another file), the spectral row (out of scope). 2c: relight (lane 25 deleted it). 2d: create_texture_memoryless (no census row).
- 23 (b): condition() keeps prev_raw for its rate-of-change edge, not waste. 27: LED presets kept (1.14 shows use them; you ruled leave), BUG-b5syk (typed pre-1.11 readers still compile in) kept.
- 9: ten repaint writes kept with reasons (six D8, three composite gate, one BUG-qxhgn (display retarget writes offscreen_dirty by hand)). 3: stopped where the remaining structs need wider visibility (census in BUG-trh7 (render_scene.rs decomposition) notes).
- Rescreen 011 (water-009 owns it). Tooling 003 cargo-check-on-stop.sh (workers may not delete hooks; BUG-qfi9a (lane 10 leftovers)), 027 MANIFOLD.app (BUG-8fsrs (launcher: delete or keep)), 029 opencode.json. Patterns 029 (BUG-o6jkf (RT accel probe prints read by a GPU proof)). Gate scopes: dependency-only narrowing stopped, it needs a resolved-feature compare (BUG-o8m7v (Cargo.lock-only change selects nothing)).

## Bugs fixed, with the test that proves each
- BUG-2ac6b (save over a damaged manifest): `save_over_damaged_manifest_keeps_snapshot_history`. BUG-vli73 (warmup command order): `commands_queued_during_warmup_keep_arrival_order`. BUG-71xde (region delete): `region_with_deleted_edge_layers_touches_no_clips`. BUG-evh52 (Finder video paste): `finder_paste_of_a_video_never_writes_the_screen_copy_only`.
- BUG-fkv3n (commands during export) and BUG-v2o9y (every click but Cancel and Quit dropped during export): `commands_during_export_are_held_and_a_quit_cancels_it`, the exhaustive `export_disposition` match and the publish-at-end cure test.
- Child 3 of BUG-mmct0 (playback audit), the snapshot ignoring the tempo map (32 s off at beat 128): `snapshot_beat_to_time_follows_tempo_map`. Child 1, a new video clip missing its in-point: `new_video_clip_seeks_to_in_point_when_ready`, `untrimmed_clip_visible_at_later_of_ready_and_start_gate`.
- Lane 7 mapping target lost on effect reorder: `effect_target_follows_the_effect_across_a_reorder`. Lane 9 picker arrows and chips not repainting: the lane's six flows.
- BUG-1nb7h (right-click mapping menu on switch and pick-list rows): `button_rows_open_the_mapping_menu_on_right_click`; stored modulators keep their row: `a_stored_modulator_keeps_a_choice_a_slider_until_deleted`, `hidden_row_with_a_disarmed_modulator_stays_until_deleted`.
- Edited Physics Boxes shows panicked on load (duplicate param ids): `edited_physics_boxes_keep_box_values_and_drivers_through_load`. BUG-7p7x9 (library presets skipped the sky-sun migration): the migration tests in sky_sun.rs. Add Fluid value nodes had no owner: `scene_physics_add_fluid_is_content_owned_selectable_and_reloadable`.
- Lane 19 audio defaults: flows audio-new-source-starts-on-none, audio-add-source-keeps-system-default, audio-new-layer-routes-to-sole-source. Lane 18: trigger-lane-sibling and card-collapse-all; BUG-wx1ke (split drag rebuilt every event): the pane tests.
- Code removed, no cure test: BUG-9s1v4 (per-tick alloc while a clip prepares), BUG-5eu1c (uniform arena never read), BUG-sq4bo (playback errors never logged), BUG-cj3y9 (MIDI backlog warning never printed), BUG-cwus2 (scene rows nothing reads). BUG-gbil0 (stale doc paths): docs_cited_paths green.

## UI changes, before and after (`renders/`, left is before)
- lanes14-15_sun_light.jpg: Type, Falloff and cone sliders become a Type pick list with only the rows a Sun reads; Cast Shadows a switch; Advanced Shadows folded. lanes14-16_world.jpg: Mode leads Environment as a pick list, Gradient-only rows hide under Softbox, Sun X/Y/Z gone (the light drives the sun).
- lane17_colour_row.jpg: three colour sliders become one Base Colour row with a swatch, folded and open. lane21_materials.jpg: five preset buttons become one Preset pick list, the Add feature menu lists eight features; lane21_preset_final.jpg shows the Preset field after its follow-up.
- lane20_two_panes.jpg and lane20_split_dragged.jpg: the tree stays visible while settings scroll; the split drags. lane24_rack_controls.jpg: Bypass and Wet/Dry under the rack header.
- lane18c_tree_headings.jpg, lane18d_open_in_scene.jpg, lane18e_collapse_all.jpg, lane18g_transport_tooltips.jpg. Not renderable headless, on your live list: 18 (a) graph editor scroll, (f) lattice readout. Not UI, checked by render: the sun disc follows the light (lane 16); Physics Boxes identical across the spawner move (lane 26).

## The two coupling files
- `coupling_rendering.md`: node-engine, nodes-scene and compositor edges, evidence for BUG-bfktg (rendering-side decomposition). `coupling_app_ui.md`: app, ui and playback edges, evidence for BUG-brw4f (app and UI decomposition). Evidence only, no recommendations.

## What needs you
`PETER.md`: BUG-akqgc (quitting loses unsaved changes, P1) first, then designs waiting on you, 35 open decision beads, the test suite cut list and the live test list.

## Conservative readings and deviations
- Read-only lanes counted toward the three-live cap. Batch 3 split in two so the trunk could start (one extra gate). Lane 10 landed alone. Batch 2 ran GPU proofs although the plan said skip: lane 25 touched fusion and presets.
- Batch 1 and lanes 10 and 13 landed past a red they caused, because `--named-red` waives every red. Fixed in a04906014; every later gate ran `--keep-going` first.
- Lane 27's converted show fixtures stay untracked: the repo is public. My lane 18 brief had item (b) backwards; the lane followed the plan. Lane 18 (c) and (e) were committed on the batch 5 branch; the continuation lanes started from the reviewed batch 5 tip before it landed.
- Lane 3 proved its moves by still hashes with ray tracing off, so the RT path moves rest on compile plus a move-only diff check. I finished lane 12's archive cleanup in scripts, hooks and tests (108e0958b) and fixed the batch 6 merge red (a58fc9e7b).
