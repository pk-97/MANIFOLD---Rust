# Simplification census: report

The census for BUG-yn979 (Simplification pass) searched the repo for code, content and tools nothing uses, and for dead code that hides a bug. It deleted nothing. Every number here is summed from the table rows in this folder after the reviewers changed grades; some sweep headers are stale. Grade A means every search came back empty. Grade B means the code searches came back empty with one named gap. Grade C needs your call. `DISPOSITIONS.md` has one line for every B and C row.

## Three numbers

- **28,500 lines can go at grade A.** 6,800 of them are app code: 6,300 go tonight (the overnight run deletes the A rows of engine-model 1,458, engine-runtime 849, graph 1,878 and app-ui 2,096), and 550 wait in the patterns, rescreen, water and fallbacks rows. The other 21,700 are scripts, docs and Python: tooling 10,900, tools 10,800.
- **16,500 lines at grade B.** 9,900 are app code, 6,300 are Python in `tools/`, 340 are scripts.
- **18 decisions are yours**, all stage or look calls, plus five off-stage questions only you can answer. Grade C rows hold another 79,000 lines, mostly docs, tools and old-format loading code, that wait on those answers.

Many counts are estimates and a few rows overlap, so read these as good to a few hundred lines.

## Bugs, not slop

31 bugs the reviewers confirmed by reading the code. None has been reproduced yet. Each gets fixed or wired, never deleted by accident.

On stage:

1. **Old shows lose clips on open** (fallbacks sweep, `crates/manifold-io/src/loader.rs:492`). Opening WAYPOINTS, PATTERN or STAGNATE deletes the shorter clip of every overlapping pair, 297 to 3,232 pairs per show. The toast shows only a count, and the first save makes it permanent.
2. **Export eats Shutdown** (fallbacks-020, `crates/manifold-app/src/content_export.rs:440-447`). During an export every command except Cancel is thrown away. Quit mid-export and the window waits for the export to end and may hang; an edit or seek sent meanwhile is lost.
3. **A save can wipe the history** (fallbacks-021, `crates/manifold-io/src/archive.rs:228-237`). Save over a show file whose manifest is damaged and Revert to Snapshot silently goes empty.
4. **The BPM rescale nothing calls** (engine-041, `crates/manifold-editing/src/commands/settings.rs:1275`). CORE_ENGINE_MAP.md line 176 says a BPM change rescales the timeline. Nothing does: clips stay on their bar, which matches the beat contract. Once you confirm, the doc is the half that's wrong.
5. **A uniform buffer filled every frame and never read** (graph-020, `crates/manifold-compositor/src/layer_compositor.rs:120`). Every blend pass copies its uniforms into it for nothing, on the compositor's hot path.
6. **Cold pad black flash** (engine-runtime-007, `crates/manifold-playback/src/live_clip_manager.rs:1098`). Live prewarm is a stub, so the first MIDI hit on a cold video clip can show black. No bead yet.
7. **Dead box Count slider** (content-024, `crates/manifold-nodes-scene/src/node_graph/scene_surface.rs:163`). Count and Spacing sit on the Physics card and do nothing unless a copies body is wired.
8. **Matter stiffness fit is stubbed** (rescreen-014, `crates/manifold-water-gpu-mpm/src/primitives/matter_domain.rs:891-892`). High Stiffness at fine resolution runs with too few substeps and no warning, where the design promises a fit and a node warning.
9. **Exports can drift from the authored look** (patterns review, `crates/manifold-compositor/src/layer_compositor.rs:3745-3749`). The export's "wait for background work" step loops an always-empty list, so MiDaS depth, person segmentation and slow blob frames never wait. An export can lag what you built and differ from run to run. Oracle: export one Wireframe Depth clip twice and diff.
10. **Playback errors never print** (engine-runtime-005, `crates/manifold-playback/src/engine.rs:332-333`). "Trigger delivery stopped", "session control interval stopped" and two more go nowhere, so a dead trigger feed fails silently.
11. **Opening a project shuffles queued commands** (fallbacks-023, `crates/manifold-app/src/content_commands.rs:345-349`). Seek, resize and the project swap come out rotated, and two edits sent while a show opens can run in the wrong order.
12. **Damaged values load as defaults** (fallbacks-028, `crates/manifold-core/src/types.rs:190-215`). A corrupt file opens with a layer turned into Video or an effect into Transform, and the next save keeps it.
13. **Dead fader on a generator card** (fallbacks-029, `crates/manifold-node-engine/src/runtime/build.rs:455-470`). A binding whose node is gone shows a fader that moves nothing; 11 of them in 3 projects. Effects already report this case as an error.
14. **The scene panel builds rows nobody reads** (app-ui-065, app-ui-066, patterns-034 to 037, `crates/manifold-app/src/ui_bridge/projection/inspector.rs:583-756`). Every scene projection builds light, camera, lens, transform, material and modulation rows nothing reads; line 595 lists light types without Spot.
15. **Render Scene reads environment variables every frame** (patterns-026 to 028 and 030, `render_scene.rs:3605, 5642, 5988, 6412`). Four leftover probe switches are checked per frame under a process lock.
16. **Clip triggers compute a value and throw it away** (patterns-033, `crates/manifold-playback/src/live_trigger.rs:250-256`), on every audio hop for every clip trigger.
17. **Snap-back ticks every frame for nothing** (app-ui-043, `crates/manifold-ui/src/panels/param_card/state.rs:565`). Nothing has started it since BUG-061 (param right-click reset folded into SliderReset), so resets are instant, but the tick still runs on every card.
18. **The MIDI backlog warning never prints** (engine-runtime-024, `crates/manifold-playback/src/midi_input.rs:389-395`).

In prep:

19. **Region edits land on layer 0** (fallbacks-022, `crates/manifold-editing/src/service.rs:357-359`). Delete the top and bottom layers of a selected region, then Delete, Split, Duplicate or Copy, and it hits layer 0's clips.
20. **Edits on a stale target leave dead undo steps** (fallbacks-024, `crates/manifold-editing/src/command.rs:49-51`). About a hundred commands do nothing on a missing target and still record a step: the next Cmd+Z undoes nothing and redo is gone.
21. **Undo that can't find its graph skips silently** (fallbacks-025, 33 `do_undo` bodies in `crates/manifold-editing/src/commands/`), and history and project disagree afterwards.
22. **Add Graph Node on a missing target records a dead undo step** (fallbacks-027, `crates/manifold-editing/src/commands/graph/node_edit.rs:135-139`).
23. **Pasted video plays black** (fallbacks-033, `crates/manifold-app/src/project_io.rs:1063-1100`). Cmd+V from Finder adds the clip to the screen's copy only; drag and drop works.
24. **Unreadable audio becomes a 0-beat clip** (fallbacks-034, `crates/manifold-app/src/project_io.rs:1426-1438`), and Replace Audio saves duration 0.
25. **A binding lookup by number can pick the wrong node** (patterns review, `crates/manifold-core/src/effects/bindings.rs:289-300`). Node numbers repeat inside groups in 43 saved projects, while two docs claim they are unique. Narrow reach: imported nodes with an empty scope.

Off stage:

26. **The defer-drop probe leaks when switched on** (app-ui-013, `crates/manifold-gpu/src/metal/defer_drop.rs:159`), the probe for BUG-l7t4 (audition-grid page fault).
27. **The whitewater oracle doesn't compile** (water-019, `crates/manifold-fluids/src/whitewater_oracle.rs`), and no gate builds it.
28. **An unused renderer drops material fields** (content-003, `render_instanced_3d_mesh.rs`). Deleting it is the fix.
29. **A bead tracks a preset nobody can open** (content-018): BUG-9wa4 (ReactionDiffusion runs 4 substeps per display frame) closes with the delete.
30. **A dead rename arm would rename the wrong group** (app-ui-002, `crates/manifold-app/src/text_session.rs:846-849`). Not live; don't revive it as written.
31. **The audio accuracy design counts a 10 ms offset nothing applies** (`docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md:167`). The default lives in settings that are never built; the live offset is zero.

Claimed by the fallbacks sweep but not re-read by its reviewer: fallbacks-026, 030, 031, 037, 038 and 047, two of them owned by BUG-38rup (Effect chains: effects depend on neighbours, silent fallbacks kill params).

## Biggest wins by area

**Engine.** About 1,000 lines of edit commands ported from Unity and never wired (engine-030 and 032 to 036, B, all marked delete), plus nine dead editing helpers (engine-026, 204, A) and three rack commands (engine-031, about 230, A).
Playback's Unity leftovers: the transport and tempo recorder (engine-runtime-009 and 010, about 375, A) and the Max for Live position sender, an unused path (engine-runtime-011, about 345, B).
A Unity percussion settings object nothing ever builds (patterns-032, about 840, B).
In load and edit code, 41 flat V1.0 fields nothing reads (fallbacks-007, about 400) and a project slot that is never empty (fallbacks-032, about 200). Nothing on stage changes.

**Graph.** Five dead modules from before the generators were decomposed: the line and mesh pipelines, compute blit helpers, a particle shader and stateful textures (graph-001 to 005, about 1,470 lines, A, tonight).
An empty plugin-prewarm registry (graph-021, about 160, B) and the write-only buffer (bug 5).
Relight's share, about 900 lines, waits on your call. The skills and docs that still teach the dead modules get fixed in the same landing.

**Water.** Little dead Rust: seven A rows, 111 lines, mostly unused dependencies.
The weight is in C rows: 2,430 lines of C++ coupling probes the app links and never calls (water-018) and the broken whitewater oracle (water-019, 1,180+). Both wait on making the C++ reference proof-only, BUG-65r39 (manifold-fluids proof-only).
No water row is reachable from a show.

**App and UI.** 164 dead theme colours (app-ui-025), three modules nothing imports (app-ui-022 to 024, about 400) and test-only GPU heap code (app-ui-014, about 130).
The scene-panel view models nobody reads (bug 14): 256 lines at A, about 410 more at B.
2,096 lines go tonight.

**Content.** Nodes nothing uses: the old Watercolor wrapper with its bookkeeping (content-001, about 1,065), render_copies (about 520), remove_drift_3d (about 466) and the Krylov pair (about 380); B, all marked delete.
On the Material card, Off and Removed shade the same, so the Enabled row folds into + Add and x (content-025); the 178 import-only fields under Advanced come off the card, and the importer still sets them (content-026).
The 3D geometry atoms (3,669 lines) stay: that's the family you're building.

**Tooling.** `audit_rename.py` (2,255), the crate-split replay data (about 4,400), retired RT probes (about 1,150) and 15 archive docs nothing names (2,847): about 10,900 lines at A.
Nine `dev.py` verbs crash because their script lacks the executable bit; four are the live-UI client.
The rest of the archive folder (16,800) and `tv-led-mirror/` (8,432) wait on you.

**Tools.** The kick research batch from 2026-10-09: 93 Python files, 10,780 lines nothing imports (tools-lab-001, A); deleting it changes no lab run id.
The kick label chain's tail (about 4,300, B).
Waiting on you: the classifier arc (5,328), the accuracy harness (6,664) and `rt_prototype` (2,735; its `compare/` files feed a GPU proof and move to fixtures first). The Python detection backend (5,671) is live product code and counts as zero until you retire it.

## Your decisions

1. **Depth relight.** Turn it back on: every card gets its 3D toggle again, but the look sweep and perf soak never ran and the render hasn't been checked in three months. Or delete it: about 1,800 lines in 84 files, no saved project uses it, and a revert brings it back.
2. **UI shadows.** Turn back on the soft shadows under menus, modals and selected clips (switched off because dark on dark read as smudge), or delete about 100 lines. Neither changes frame time.
3. **Convert old shows once.** Open and resave every show below format 1.11 (Liveschool, WAYPOINTS, PATTERN, STAGNATE) and the 48 online-only 3D projects (the Corrosion and Right Where I Need You videos), and about 7,500 lines of loading code go. Cost: downloading the online-only Dropbox files. Or keep loading them as they are, forever.
4. **Old snapshots inside show files.** Revert to Snapshot runs old snapshots through the same migrations, and local histories still hold shapes that lived for days; converting a show doesn't convert its history. Keep those migrations (about 1,000 lines, and the Dropbox archives were never read), or accept that very old snapshots may stop reverting cleanly.
5. **Overlapping clips in old shows** (bug 1). Keep deleting the shorter clip on open, hundreds per show, or trim the earlier clip to where the next one starts.
6. **Hidden LED presets.** Four retired LED presets load only for old LED shows, and Liveschool V5 is online-only. Delete them once those shows are converted, or keep them.
7. **Test water scenes in the browser.** Move the three MPM water scenes and Particle View to test fixtures and drop "(GPU FLIP)" from two names, or leave them until water presets are designed.
8. **BPM change** (bug 4). Clips keep their bar, as now: the unused rescale goes and the doc is fixed. Or clips keep their time in seconds: wire the rescale.
9. **Light node.** One node for Sun, Point and Spot showing every param, as now, or three light types each showing only its own (a migration for 68 projects).
10. **Shadow Bias and Resolution.** Keep them as params, or make them constants, which changes the shadows in the water presets and Lantern.
11. **The environment's own sun.** Keep a second sun beside Lights > Sun, or have the environment follow the scene's Sun light and retire five params.
12. **Environment mode.** One studio mode (MetallicGlass's look changes) or two environment nodes. Width and Height become a constant either way.
13. **Physics box spawner** (bug 7). Count, Spacing, Columns and Arrangement stay on the world node and its card, or move onto the copies body so they show only when boxes exist.
14. **Water emission settings.** Keep the fourteen FLIP Fluids emission settings in the Scene tab for parity, or cut to Whitewater, Amount and the on/off switches.
15. **Random cue start.** Build it as a perform feature, or delete the setter nothing calls.
16. **Rack bypass and wet/dry.** Tonight deletes the never-wired rack commands, leaving both in the model and the renderer with no control. Give racks those controls, or delete the two fields.
17. **Automation lane on/off.** Build the toggle (the inspector reads the flag, nothing sets it), or delete the command and the flag.
18. **Percussion placement on a video clip.** Make it a feature, or drop the "first clip on the layer" fallback that is the only path today.

Off stage, only you know: which hand-run tools you still use (`tv-led-mirror/`, the analyzer CLI, `MANIFOLD.app`, `typos`, the segmentation tool, the Demucs and debug environment switches, `opencode.json`); which of 13 never-built design docs stay on the roadmap, and whether the archive folder stays or git holds it; whether the audio classifier and accuracy campaigns are parked; when the Python detection backend retires (its models can't ship commercially); and whether the conversation-recall hook is worth 78 ms on every prompt.

## The patterns behind the slop, and the rule that stops each

- **The caller goes, the callee stays.** `pub` hides the leftover from the compiler. Rule: a deletion landing runs `census_refs.py` on every crate it touched.
- **Port the interface, not the need.** Thirteen C# interfaces became traits, four with no implementor. Rule: a trait lands with its second implementor or a named layering seam; ported code lands with its first production caller.
- **Interfaces for one.** `Compositor` has 31 default bodies its one implementor overrides. Rule: one implementor means plain methods, and when a registry's last entry goes, the registry and its tests go too.
- **Switch it off instead of deciding.** Relight, UI shadows, rate of change. Rule: turning a feature off and deleting it are one landing, unless you name a date to turn it back on.
- **Probes left in the instrument.** Bug traces months after the bug closed, some read every frame. Rule: a probe named for a bug goes in the landing that closes the bug.
- **An option every caller sets the same way.** The percussion settings are `None` at every call. Rule: one value at every call site is a constant.
- **The new path lands, the old view model keeps being filled.** Rule: when a surface moves to a new source, the old fields go in the same landing.
- **One question, many channels.** Node params answered four ways, with a test proving two agree. Rule: one registry per question, in the lowest crate that asks; a parity test between two channels means one goes.
- **The helper lives everywhere but on the type.** `max_node_id` 7 times, `output_resource` 30, `frame_time` 73. Rule: a helper over a type goes on the type; search the type's module first.
- **The preset goes, the atoms stay.** Rule: a landing that deletes a preset lists the atoms it orphaned (`node_usage.sh`).
- **Test scenes in the user's browser.** Rule: a preset only tests use lives in test fixtures.
- **Every engine knob reaches the UI.** Shadow bias, solver internals, 178 import-only fields. Rule: a param reaches the UI only when a show would change it.
- **Migrations with five homes that never write back.** Graph renames apply to a copy at every build, so the rename table can never shrink. Rule: one load pipeline in a fixed order, and every pass writes into the project.
- **Permanent migrations for shapes that lived a day.** Rule: a shape that never left your machine is converted by hand with `project_tool`, history included.
- **Repair beside the fix.** Rule: one repair per bug, at the earliest seam.
- **No-op instead of reject.** Rule: a command whose target is gone rejects, and undo drops it.
- **Code and tests for states that can't happen.** Rule: make the state unrepresentable or `expect` it with the reason; never test an unreachable state.
- **Tests that guard the wrong thing.** Tests pin silent fallbacks, test functions production never calls, or sit behind a feature no gate runs. Rule: a test pins errors, not silent defaults, and its subject needs a production caller and a gate that runs it.
- **Crate splits copy dependency lists.** About fifteen unused dependencies across five crates. Rule: the landing gate runs `cargo machete` on touched crates.
- **Tools rot unseen.** Nine verbs crash. Rule: the `dev.py` test runs `--help` for every verb, and every feature-gated check sits on a gate.

## What the census did not cover

- The 67 online-only Dropbox projects (2.1 GB) and the history inside every Dropbox archive were never opened. Every migration verdict turns on them.
- Nothing was built, tested or run on the GPU. Deletion lanes find cascades with `cargo check`. No bug above was reproduced; bugs 2, 11, 19 and 23 come first.
- Reviews were light, with no random sample: about 85 small grade-A rows were not re-run, and tonight's compiler and gate are their proof. The tools-lab sweep had no review; its rows say so in `DISPOSITIONS.md`.
- Not swept: private items, shaders, vendored native code, UI flow scripts, skills, a tests sweep (16 baselined ignored tests), effect and 2D generator params, and the look or quality of real presets.
- Silent fallbacks outside loading, editing and graph build: the per-frame path, compositor, UI, water and scene crates.
- Dead functions hidden by a same-named trait method were checked only in the editing service and the UI host traits.
