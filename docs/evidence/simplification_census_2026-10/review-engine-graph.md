# Review: engine-model, engine-runtime, graph, rescreen

- **Review** for BUG-yn979 (Simplification pass), `docs/SIMPLIFICATION_CENSUS.md` section 6 (Review), light form: no random sample.
- **Reviewer:** Opus 5.5. Read-only on code. Main at 090efbfa5; `crates/` is unchanged since the screen commit 37a9c180a.
- **Files:** `engine-model.md`, `engine-runtime.md`, `graph.md`, `rescreen.md` and their `.proof.md` files.
- **Extra search per checked row:** non-Rust files including hidden dirs (`.claude/`), `plugins/` (not vendor), `scripts/`, `tools/`; open beads (full text from `.beads/issues.jsonl`, not only titles); `git log -S NAME`. Corpus re-run as a loading check only.
- Bead parents named once here: BUG-mmct0 (Playback pre-Fable code audit pilot: findings), whose child BUG-mmct0.12 is the playback audit hygiene bead; BUG-5bwzr (Automation lanes: pro-grade editing UX).

## Result

- 2 grade changes, both C to B: engine-runtime-006 and engine-runtime-015. No grade-A row was wrong or turned out to be a bug, so no A row is demoted. The header counts in `engine-runtime.md` are now B 10, C 9 (the header line still says 8 and 11; only grade cells were edited).
- 56 B and C rows get a disposition: delete 26, keep 15, bead 8, fix 6, and one split (engine-039: delete one command, keep the other).
- 7 bugs, not slop: engine-041, graph-020, rescreen-014, engine-runtime-005, engine-runtime-007, engine-runtime-024 (minor), and a design doc built on dead percussion settings (from the patterns cross-check).
- Cross-checks from the patterns sweep: patterns is right about `PercussionPipelineSettings` and engine-model was wrong; `PlaybackEngine::get_seconds_per_beat` conflicts with no engine-runtime A row.

## Grade changes

| id | was | now | why |
|---|---|---|---|
| engine-runtime-006 | C | B | The row asks because CORE_ENGINE_FINDINGS F2 named the two resolvers as the fix point for MIDI launch quantization. F2 has since landed without them: 04301b37b snaps forward in the fallback arm of `compute_trigger_snap_beat` (live_clip_manager.rs:756-779), which calls `host.get_beat_snapped_beat()` and takes the `else` arm. The resolvers are plain dead hooks, already named by BUG-mmct0.12 item (d). Action becomes delete, keeping both `else` arms. Stays B because it sits on the MIDI launch path (stage). CORE_ENGINE_FINDINGS.md F2 still reads "What's broken" and is stale. |
| engine-runtime-015 | C | B | No decision is needed. The live device filter is per layer: `Layer.midi_device` (core layer.rs:150), set from the layer header (ui_bridge/project.rs:169) and applied to note events at clip_launcher.rs:332. The global `MidiInput::set_device_filter` has no caller and duplicates that concept. Action becomes delete. Stays B (stage). |

## Cross-checks from the patterns sweep

**`PercussionPipelineSettings`: patterns-032 is right, engine-model was wrong.** engine-model's "Whole-module follow-up" kept the 12 settings structs in `percussion_settings.rs` as live because they are fields of `PercussionPipelineSettings`. That struct is never built. Every `PercussionImportOrchestrator::new` call passes `None` (manifold-app app.rs:1893-1894, headless_harness.rs:80-81, and the one test at percussion_orchestrator.rs:1380); `pipeline_settings` is set only in the constructor (percussion_orchestrator.rs:164) and has no setter; `rg` finds no struct literal, `::default()` or deserialize site. So every `Some` arm is dead: percussion_orchestrator.rs:306, :1011, :1049 and percussion_import.rs:67-69. Consequences for my rows:

- engine-016 (`reset_to_defaults`, A) is a 4-line subset of patterns-032. No conflict; land it first or fold it in.
- engine-runtime-023 (`apply_detected_bpm`, A) edits the same file as patterns-032 (`new_with_settings` and the confidence threshold in percussion_import.rs). Both are compiler-proven; land them in one batch or in sequence.
- engine-runtime-036 (keep): its note that five `MANIFOLD_DEMUCS_*` knobs duplicate `settings.demucs.*` is now one-sided. The settings half is dead, so the env knobs are the only live Demucs config. The patterns sweep should judge them on that basis.
- The dead settings carry a 10 ms onset compensation that a design doc treats as live; that is bug 7 below.

**`PlaybackEngine::get_seconds_per_beat`: no conflict.** Its other hits are the same-named UI-host trait method (manifold-ui timeline_input_host.rs:45) and its impls (manifold-app input_host.rs:159, input_handler.rs:598). No engine-runtime A row deletes it, calls it, or names it, and nothing in my four files mentions it. One cascade for patterns: its only callee `get_seconds_per_beat_at_beat` (engine.rs:2697) has no other caller, so it goes too, and the doc comments at audio_layer_playback.rs:90 and :104 that cite it as the tempo-priority spec need rewording.

## Grade-A rows checked

Rows over 200 lines, at stage or prep risk, or that looked like a missing wire. Every proof matched on re-run; corpus re-run gave 0 files for all names except `LineGeneratorHelper` (1 file, a node description string, as the proof says).

| id | lines | extra search | verdict |
|---|---|---|---|
| engine-026 | 204 | `rg -w` on the 3 masked names: only the service definitions and the same-named trait methods; the app implements `toggle_mute_clips` and `cut_clips` itself (input_host.rs:537, :780) | A holds. The app's `toggle_mute_clips` is a second copy of the service fn's group-mute rule; the dead one goes. |
| engine-031 | ~229 | non-Rust, beads, git: nothing beyond the port commits | A holds. Note for the lead: these commands are the only writers of `EffectGroup.enabled` and `wet_dry`, which the render path still honours (runtime/groups.rs:45, runtime/core.rs:863, :1504). After the delete, rack bypass and rack wet/dry exist in the model and the renderer with no control. 0 corpus projects carry a `wetDry` below 1. Whether racks get those controls is a product question, not a reason to keep the commands. |
| engine-runtime-009 | ~210 | non-Rust, beads: none; git: the transport-bar port and the clock-authority change (7db075b20) | A holds. |
| engine-runtime-026 | ~35 (prep) | `VideoDecoder_IsPrepared`: only decoder_ffi.rs, decoder.rs and the `.m` file (header comment and body) | A holds. The `.m` body is outside the compiler's proof, so the deleting lane removes it by hand. The proof calls metadata.rs `SUPPORTED_EXTENSIONS` module-private; it is `pub const`, but nothing outside metadata.rs reads it. |
| graph-001 | 542 | `scripts/fixtures/gate-p1.json` names the shader path; it is snapshot data for `scripts/test_gate_readiness.py`, not a file check | A holds. Open bead **BUG-de4i (silent clamps: line/mesh pipeline geometry truncation, no signal)** names `line_pipeline.rs` and `mesh_pipeline.rs`, which are these modules (the `MAX_POSITIONS` / `MAX_INSTANCES` clamps live only in line.rs and mesh/pipeline.rs; the live render_lines.rs has no cap). The sweep did not cite it. After graph-001 and graph-004 land, close BUG-de4i or narrow it to its two unfiled side notes. |
| graph-002 | 278 | `.claude/skills` (effect-development, gpu-metal-patterns) and two docs teach the helpers, as graph.md says | A holds. |
| graph-004 | 352 | same gate fixture; BUG-de4i as above | A holds. |
| graph-005 | 221 | three WGSL files mention it in comments only; the skip entry in wgsl_validation.rs:32 goes with it | A holds. |
| graph-016 | ~54 (prep) | the app's Scale arm calls `projected_drag_delta` with no multiplier (window_input.rs:1047-1048), the same as `SCALE_SENSITIVITY = 1.0`; nothing is lost | A holds. |
| graph-008 | 18 | plan swaps under a live executor: none; plans compile at build time (runtime/build.rs:548, core.rs:1224); the only `self.executor =` is the dead `set_executor` (graph-009) | A holds. The "MUST be called" doc has no remaining trigger. |
| graph-013 | 14 | `kernel_half_width` is the live one; `tap_count` says itself that no caller derives a divisor | A holds. |
| graph-023, engine-runtime-034 | 3, 1 | 3.4 rows, re-run in full per section 6: `png = "0.18"` carries no features, so it does no feature unification for `image`; `manifold_playback` appears once in a doc comment; spectral has no `log` use | A holds. |
| engine-runtime-002, -023 | 44, 85 | missing-wire check: `invalidate_prewarm` (prewarm resets itself from the candidate set, engine.rs:3651-3661); `apply_detected_bpm` (no doc or bead promises auto-applied BPM; the setting is not shown in any UI) | A holds. See the cross-check above for -023's overlap with patterns-032. |
| engine-006 | 114 | missing-wire check: restoring the recorded tempo is the live BPM reset (`TransportController::reset_bpm`, enabled at state_sync.rs:371) | A holds. |

## Disposition of every B and C row

Stage and prep rows were re-run (code counts and corpus); every count matched its proof.

| id | disposition | the fact that decides it |
|---|---|---|
| engine-014 | delete | 3 getters read only by 7 asserts in instance.rs tests; the asserts can read the fields |
| engine-019 | delete | `is_unknown` has one assert caller; `SOFT_FOCUS_GRAPH` has one nodes test (binding_seed_tests.rs:27), which can spell the id |
| engine-028 | delete | `with_forced_def_write` has no caller, so `force_def_write` is always false; BUG-6dh6 (modulation and OSC writes bypass the coupled-row path) mentions it only as history; cite it in the landing |
| engine-029 | bead | BUG-nwjg8 (Physics boundary P6a: rigid add, enable, split and duplicate) is open and owns the add-with-physics path; re-run: `with_physics_world` only in scene/tests.rs (4) |
| engine-030 | delete | `generator_change::build_change` replaced the app caller in 36249aff4; the command has no LED-specific logic, so its LED-lane test covers nothing the product runs |
| engine-032 | delete | the slip UI was removed on purpose (manifold-app dispatch/clip.rs:212-215, 480acf63e); Swap and ClipEffects never had an app constructor |
| engine-033 | delete | the card's expose button builds `ToggleNodeParamExposeCommand` (app_render.rs:2268), which answers the row's open question; the 7 sites in manifold-app/tests/user_param_bindings_e2e.rs go or repoint in the same landing |
| engine-034 | delete | nothing removes an envelope in the product: the UI adds or toggles (dispatch/modulation.rs:529-536) |
| engine-035 | delete | the layer header toggles analysis-only with a deliberate non-undo `project.mutate` (ui_bridge/layer.rs:55-74, "a live perform toggle, not an undo step") |
| engine-036 | delete | selection region is UI state; nothing sends it as a command |
| engine-037 | keep | MULTI_DISPLAY_DESIGN.md is IN PROGRESS and its UI phases are unbuilt; re-run: all 9 appear only in stage.rs |
| engine-038 | keep | SESSION_MODE_DESIGN.md is IN PROGRESS (P4 UI, P5 recording unbuilt) and names all 7; re-run matches |
| engine-039 | split: delete Channels, keep Analysis | the per-send channel picker was deleted on purpose (manifold-ui panels/actions.rs:843-845: capture-fed sends are always stereo); `SendAnalysisConfig.pitch` is the planned activation gate in AUDIO_OBJECT_TRACKING_DESIGN.md:47 and :176. The row's own doc evidence is stale: both docs it cites are SHIPPED |
| engine-040 | bead | `lane.enabled` has no writer except this command (automation.rs:509, :526) yet is read at inspector.rs:1430; AUTOMATION_AUTHORING_AUDIT.md item 6 calls the missing toggle a gap; no bead names it (BUG-5bwzr.14 is the override restore, not enable). Decision: build the lane toggle, or delete the command and the field |
| engine-041 | fix | see the bugs list |
| engine-043 | delete | inline the 3-line wrapper in 3 history_snapshots.rs tests; fix PROJECT_IO_MAP.md:96, :144 |
| engine-045 | keep | multi-display venue file, same reason as engine-037 |
| engine-046 | bead | the relight decision owed by Peter (census section 10); it also decides graph-026 and engine-027 |
| engine-047 | keep | diagnostic switch for the device gate (390a13ca4), documented in its module; the patterns sweep judges knobs |
| engine-048 | keep | UI_INVALIDATION_DESIGN.md names it as the live-run gate |
| engine-049 | keep | the feature exists to keep two wall-clock benches out of the gate (ec14d046d); whether the benches earn their keep is the patterns sweep's call |
| engine-runtime-005 | fix | see the bugs list |
| engine-runtime-006 | bead | BUG-mmct0.12 item (d) owns it; delete, keep the `else` arms (grade change above) |
| engine-runtime-007 | fix | see the bugs list |
| engine-runtime-008 | bead | CORE_ENGINE_FINDINGS F8 is DECISION NEEDED (measure the per-frame query, then wire or delete); no measurement and no bead exist |
| engine-runtime-011 | delete | the M4L path is unused and its deletion was deferred, not refused (ABLETON_TRANSPORT_SYNC_DESIGN.md:65-67); re-run: `enable_sender` has only its definition, `osc_sender_enabled` is written only in the dead `toggle_sync_output`; the SYNC chip display changes in the same landing |
| engine-runtime-014 | delete | `use_native_clock_note_events` only ever holds its initial value |
| engine-runtime-015 | delete | the per-layer `Layer.midi_device` filter is the live one (grade change above) |
| engine-runtime-021 | bead | BUG-mmct0.12 item (d) names `set_randomize_in_point`; keep its fix shape unless Peter wants random cue start as a perform feature |
| engine-runtime-024 | fix | see the bugs list (the backlog warning); the flag itself goes |
| engine-runtime-025 | delete | after engine-runtime-001 |
| engine-runtime-027 | delete | MEDIA_BACKEND_DESIGN folds it into an unbuilt `FrameLease::deliver`, which can be written fresh |
| engine-runtime-030 | delete | only caller is its own test |
| engine-runtime-031 | delete | the profiler guide says no comparison CLI was built |
| engine-runtime-035 | keep | the suppression names its un-suppress condition (a non-macOS encoder, MEDIA_BACKEND_DESIGN P1); cross-platform is a product requirement |
| engine-runtime-036 | keep | patterns sweep; the `settings.demucs.*` half it duplicates is dead (cross-check above), so the env knobs are the live config |
| engine-runtime-037 | keep | live diagnostics; patterns sweep |
| engine-runtime-038 | keep | tool-path overrides; patterns sweep |
| engine-runtime-039 | keep | BUG-ycmj2 (Integrate Signalsmith for live audio warp and matching export) is deferred, not dropped |
| engine-runtime-042 | keep | LED_STRIPS_DESIGN.md:17 plans sACN beside Art-Net, so the one-variant enum is its slot; no saved file carries `outputType` (0 corpus, 0 in prefs.json) |
| graph-020 | fix | see the bugs list; here the fix is the deletion, but it touches the stage blend path, so it does not ride with blind deletes. Drop `UniformArena` from the BUG-bfktg (Rendering-side decomposition epic) crate-move table |
| graph-021 | delete | 0 `PluginPrewarm` submissions and 0 `PostProcessEffect` impls (re-run includes `plugins/`) since the plugin effects became graphs (fb99e0924, 60d4a42de); the plugin-backed nodes warm themselves (depth_estimate_midas.rs:234 `warmup_pending`) |
| graph-022 | delete | its reader went in 66fba804e; the one submission is read only by its own test |
| graph-024 | delete | remove both allows and keep one only where the compiler names a field; this is a compiler-proven change |
| graph-025 | delete | rewrite the 2 shadow-math fixtures to `Light::sun` |
| graph-026 | bead | the relight decision, same as engine-046; re-run: 0 corpus projects contain "relight" |
| graph-027 | keep | patterns sweep; no repo script sets any of the 16 |
| rescreen-001 | delete | 5 wrappers whose inner fns are what the product calls; repoint the tests |
| rescreen-002 | delete | move-to-fixtures: the 3 staged evaluators move into `mod tests`, no product caller |
| rescreen-008 | delete | owner keys are id-derived and stable (`layer_id_owner_key`, `group_id_owner_key` in layer_compositor.rs:1050-1688; 0 for generators), and each runtime call site passes one `ctx.owner_key`, so nothing needs per-owner cleanup; reword the docs at state_store.rs:40 and :54, which describe a hook that does not exist |
| rescreen-009 | delete | both lookups are test-only and named as superseded |
| rescreen-010 | bead | decision: reclassify `node.watercolor` as `FusedBundle`, then delete the ledger |
| rescreen-011 | delete | lands with water-009 |
| rescreen-012 | keep | PHYSICS_ENGINE_BOUNDARY_DESIGN.md (IN PROGRESS) line 36 lists `CompletionLedger` as boundary API |
| rescreen-013 | keep | it is the CPU reference the GPU timestep tests compare against; a move to a testkit is the patterns sweep's call |
| rescreen-014 | fix | see the bugs list |

## Bugs, not slop

1. **engine-041: a doc promises a BPM rescale nothing performs.** CORE_ENGINE_MAP.md:174-176 says a BPM change rescales the timeline through `RescaleBeatsForBpmChangeCommand` (manifold-editing commands/settings.rs:1275). The BPM edits the app makes build `ChangeBpmCommand::master` only: the BPM text field (manifold-app text_session.rs:575) and the BPM reset (manifold-playback transport_controller.rs:270, :304). The command is a port of Unity's percussion-import rescale (its doc says so), the partner of the also-dead `apply_detected_bpm` (engine-runtime-023). The sentence before it in the map says beats are the contract (a clip at bar 33 stays at bar 33), which matches the code. On stage: today a BPM change keeps every clip on its bar; the doc is the wrong half. Fix: correct CORE_ENGINE_MAP.md:176 and delete the command, once Peter confirms that is the behaviour he wants.
2. **graph-020: the uniform arena is written every blend pass and never read.** layer_compositor.rs:120 `let _offset = arena.push(uniforms);` copies the blend uniforms into a shared buffer on every blend pass; the arena is reset and flushed every frame (layer_compositor.rs:1031/1085, 1371/1401; generator_renderer.rs:741/1051, 1680/1734). The only reader, `GpuEncoder::uniform_arena_mut` (manifold-node-engine gpu/gpu_encoder.rs:121), has no caller; the dispatch binds the uniforms inline. On stage: wasted copies on the compositor hot path every frame. Fix: delete the arena, its field and the `blend_pass` parameter.
3. **rescreen-014: the MPM stiffness fit is stubbed, so the substep cap is silent.** manifold-water-gpu-mpm primitives/matter_domain.rs:891-892 sets `let fitted = stiffness; let limited = fitted < stiffness;`, which is always false, then :917 caps `requested.min(MAX_SUBSTEPS)`. GPU_MPM_SOLVER_DESIGN.md:226-228 promises the solver uses the largest value that fits, publishes it on `limited_by_substeps` and raises a node warning ("limited, never silently"). The fit exists unwired: `stiffness_fitting_cap` (matter.rs:296), read only by its test. On stage: a high Stiffness at fine resolution runs with too few substeps and no warning. Fix: wire the fit as designed. No open bead names it.
4. **engine-runtime-005: playback errors are swallowed.** `log_warning` and `log_error` (manifold-playback engine.rs:332-333) are never installed, so these never print: "trigger delivery stopped" (:1310), "session control interval stopped" (:2260), "Restarted stopped player" (:3360), "Drift correction" (:3374). On stage: a dead trigger feed or session clock fails silently. Fix: `log::error!` / `log::warn!` (rate-limit the drift line), delete the fields. BUG-mmct0.12 item (d) lists these hooks as dead code; this row must not land as a plain delete.
5. **engine-runtime-007: live prewarm is a stub, so a cold pad can black-flash.** `LiveClipManager::append_live_prewarm_candidates` (live_clip_manager.rs:1098) is never called, and `compute_prewarm_candidates` (engine.rs:3541) scans only the timeline window. CORE_ENGINE_FINDINGS F10 says the first MIDI fire of a cold video rides only the 0.02 s recently-started gate. No open bead. Fix: the F10 fix shape (MIDI-mapped layers' clips plus recently triggered clips into the prewarm set).
6. **engine-runtime-024 (minor): the MIDI backlog warning never prints.** manifold-playback midi_input.rs:389-395 warns "MIDI note backlog" only when `show_debug_logs` is on, and nothing turns it on. Notes held over to the next frame are never reported. Fix: keep the warning unconditional (rate-limited) when the flag goes.
7. **patterns-032 cross-check: the audio accuracy design reasons from a dead 10 ms offset.** AUDIO_ANALYSIS_ACCURACY_DESIGN.md:167 says one hand-tuned constant, `onset_compensation_seconds` with a 10 ms default (`percussion_settings.rs:42`), is added to every event at plan time; :328 plans to take it to zero. That default lives in the never-built `PercussionPipelineSettings`. The live value comes from the detection config instead: `options.onset_compensation_seconds = config.onset_compensation` (percussion_import.rs:532), default `Seconds::ZERO` (audio_clip_detection.rs:102), added at percussion_planner.rs:85. Nothing breaks on stage, but the accuracy work's model of today's offsets is wrong by 10 ms. Fix: correct the two doc lines when patterns-032 lands.

Not bugs, though they looked like it: rescreen-008 (the per-owner cleanup the doc describes is not needed with id-derived keys), graph-008 (no plan swap remains), engine-runtime-006 (F2 is fixed another way), engine-runtime-023 (no doc or bead promises auto-applied BPM).

## Notes for the lead

- BUG-de4i: close or narrow it after graph-001 and graph-004 land (see above).
- Stale docs found on the way: CORE_ENGINE_FINDINGS.md F2 ("What's broken", fixed in 04301b37b); LIVE_AUDIO_TRIGGERS_DESIGN.md:210 and AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md:640 (engine-039: the first is history, the second describes a picker that was deleted).
- engine-031 leaves rack bypass and rack wet/dry with no control (see above); worth one line in Peter's decisions if the report has room.
- engine-model's method trap generalises: "live by composition" (a type used only as a field of another type) is only as live as the outer type. The sweep checked the inner types, not whether the outer one is ever built.

## What I did not check

- No random sample. Grade-A rows under 200 lines at none or dev risk that did not look like a missing wire were not re-run: engine-model 22 rows, engine-runtime 17, graph 11, rescreen 5. The overnight compiler, clippy and gate runs are their proof.
- Nothing was built, tested or run on the GPU. Cascades are as the sweeps named them.
- Names masked by a same-named trait method outside `service.rs` (engine-model "Second blind spot") were not scanned beyond the `get_seconds_per_beat` cross-check.
- Other "live by composition" claims in engine-model (`GeneratorSnapshot`, `ValidationResult`, `PasteResult`, the `group_edit.rs` types) were not re-checked for whether the outer item is built; their outer items are named as live entry points.
- rescreen-002's manifold-app test files and engine-033's app e2e test were not read in depth.
- rescreen-008: I did not prove that no runtime is ever called with two owner keys; I saw one key per call site.
- The 67 online-only Dropbox projects were not opened (census section 2).
- The "Seen, not judged" items of the four sweeps were not judged, except where a row above needed them.
