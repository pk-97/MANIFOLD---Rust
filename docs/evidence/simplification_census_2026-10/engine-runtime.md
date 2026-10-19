# Simplification census: engine-runtime sweep

- Sweep: `engine-runtime`, for BUG-yn979 (Simplification pass). Model: Sonnet 5.5, medium effort.
- Main commit: `37a9c180a` (`raw/main_commit.txt`). `crates/` is unchanged between it and the checkout read (`git diff --stat 37a9c180a HEAD -- crates` is empty).
- Scope: `crates/manifold-{playback,compositor,audio,spectral,media,recording,profiler,native,led}`.
- Method: `docs/SIMPLIFICATION_CENSUS.md` sections 2, 3.1 to 3.4, 3.9 and 4 (The candidate record), brief 11.1b. Saved projects are a loading check only (section 2): every corpus search below answers "would deleting this break loading a project", never "is it used".
- Raw files used: `raw/pub_refs/manifold-{playback,compositor,audio,spectral,media,recording,profiler,native,led}.tsv` and `summary.txt`, `raw/suppressions.txt`, `raw/machete.txt`, `raw/features.tsv`, `raw/const_bools.txt`, `raw/env_knobs.txt`, `raw/open_cleanup_beads.txt`, `raw/main_commit.txt`. Corpus: `/tmp/simplification_census/corpus/projects` (182 projects).
- Proof: `engine-runtime.proof.md` has one section per row, every command and a one-line result.
- Not covered: see the end of this file.

Counts: 42 rows. Grade A 23 rows, about 849 lines. Grade B 8 rows, about 780 lines. Grade C 11 rows, about 1021 lines. Line counts are spans read from the files with doc comments, `~` where estimated. No `cargo check` was run (read-only rules), so cascades beyond the ones named are for the deleting lane's compiler to find.

| id | location | category | proof | risk | grade | action | lines |
|---|---|---|---|---|---|---|---|
| engine-runtime-001 | `crates/manifold-playback/src/engine.rs:1928` | 3.1 | `sync_clip_loop_state`, `compute_clip_playback_rate`, `apply_clip_playback_rate`: 0 callers repo-wide; BUG-mmct0.12 (Playback audit hygiene) item d names the first two; its only call into `video_time::compute_video_time` makes that dead (025) | none | A | delete | ~66 |
| engine-runtime-002 | `crates/manifold-playback/src/engine.rs:601` | 3.1 | 7 uncalled methods (`invalidate_prewarm`, `clear_pending_pauses`, `get_timeline_active_clips_at_current_beat`, `get_clip_end_time_seconds`, `live_clip_manager_mut`, `replace_renderer`, `set_clock`); field `to_pause_list` is cleared and never pushed; `set_clock` was read as used because of two comment mentions | none | A | delete | ~44 |
| engine-runtime-003 | `crates/manifold-playback/src/engine.rs:29` | 3.1 | trait `PlaybackNotifier`: no impl, no `dyn`, no bound anywhere (also listed in 3.10) | none | A | delete | ~7 |
| engine-runtime-004 | `crates/manifold-playback/src/engine.rs:331` | 3.1 | fields `log`, `replenish_warm_cache`, `on_drift_corrected` never read; assignment search over every crate finds no installer; BUG-mmct0.12 item d | none | A | delete | ~8 |
| engine-runtime-005 | `crates/manifold-playback/src/engine.rs:332` | 3.1 | `log_warning`, `log_error` never installed, read at 4 sites, so two error messages are swallowed today | dev | B | simplify: call `log::error!` / `log::warn!`, delete the fields | ~20 |
| engine-runtime-006 | `crates/manifold-playback/src/engine.rs:338` | 3.1 | `beat_snapped_beat_resolver`, `absolute_tick_resolver` never installed; CORE_ENGINE_FINDINGS F2 names them as the fix point for MIDI launch quantization | stage | B | ask | ~12 |
| engine-runtime-007 | `crates/manifold-playback/src/live_clip_manager.rs:1098` | 3.1 | `append_live_prewarm_candidates` stub, never called, plus 3 constants only it would use (`engine.rs:51-53`); F10 and map section 13 item 10 plan to finish it | stage | C | ask | ~29 |
| engine-runtime-008 | `crates/manifold-playback/src/active_window.rs` | 3.2 | `ActiveTimelineClipWindow`: 0 uses outside its own file and tests; module has no outside reference; F8 says wire or delete, DECISION NEEDED; includes `reset_active_clip_window` stub | dev | C | ask | ~697 |
| engine-runtime-009 | `crates/manifold-playback/src/transport_controller.rs:48` | 3.1 | 9 uncalled methods (6 DEAD rows + `set_bpm`, `apply_authority_exclusively`, `get_clock_authority`), 3 private helpers only they call, `InsertCursorState`, `osc_sender_port`; BUG-mmct0.12 item d names 7 of them; `toggle_midi_clock` is live and stays | none | A | delete | ~210 |
| engine-runtime-010 | `crates/manifold-playback/src/tempo_recorder.rs:28` | 3.1 | `track_clip_start`, `finalize_clip`, `remove_clip_start`, `clear_clip_starts`, `update_session_state`, `RecordingClipStartInfo` and the `clip_starts` map: 0 callers; `LiveClipManager::finalize_recording_clip` is the live twin; BUG-mmct0.12 item d | none | A | delete | ~165 |
| engine-runtime-011 | `crates/manifold-playback/src/osc_sender.rs` | 3.1 | `OscPositionSender`: `enable_sender` has 0 callers so `late_update` returns at once; SYNC button now drives AbletonOSC only; Peter's ruling 2026-07-07 (ABLETON_TRANSPORT_SYNC_DESIGN lines 65-67) says the M4L path is unused and deletion is deferred; includes `clear_ownership` and app wiring | stage | B | delete | ~345 |
| engine-runtime-012 | `crates/manifold-playback/src/sync.rs:120` | 3.1 | `current_authority`, `set_manifold_owns`: 0 callers (`set_manifold_owns_at` is the live one) | none | A | delete | ~12 |
| engine-runtime-013 | `crates/manifold-playback/src/midi_clock_sync.rs:268` | 3.1 | `is_clock_transport_playing`, `available_source_count`, `available_source_name`, `available_source_names`: 0 callers | none | A | delete | ~20 |
| engine-runtime-014 | `crates/manifold-playback/src/midi_input.rs:154` | 3.9 | `has_device` 0 callers; `use_native_clock_note_events` starts true and its only writer is the uncalled setter | dev | B | simplify: delete flag, setter, `has_device` | ~15 |
| engine-runtime-015 | `crates/manifold-playback/src/midi_input.rs:189` | 3.1 | `set_device_filter` has 0 callers so `device_filter` is always None; ported live-rig feature with no UI | stage | B | ask | ~55 |
| engine-runtime-016 | `crates/manifold-playback/src/osc_param_router.rs:104` | 3.1 | `route_count`: 0 callers | none | A | delete | ~4 |
| engine-runtime-017 | `crates/manifold-playback/src/osc_sync.rs:264` | 3.1 | `toggle_osc`: 0 callers | none | A | delete | ~7 |
| engine-runtime-018 | `crates/manifold-playback/src/ableton_bridge.rs:473` | 3.1 | `has_send_socket`: 0 callers | none | A | delete | ~3 |
| engine-runtime-019 | `crates/manifold-playback/src/link_sync.rs:63` | 3.1 | `with_tempo`: 0 callers in src; the census's 10 test hits belong to `FakeAbleton::with_tempo` in a test file | none | A | delete | ~4 |
| engine-runtime-020 | `crates/manifold-playback/src/audio_decoder.rs:31` | 3.1 | `DecodedAudio::from_static_sound_data`, the only fn of its impl: 0 callers | none | A | delete | ~18 |
| engine-runtime-021 | `crates/manifold-playback/src/clip_launcher.rs:67` | 3.9 | `randomize_in_point` starts false, only writer `set_randomize_in_point` has 0 callers, so `compute_in_point` always returns 0.0; BUG-mmct0.12 item d names the setter; random cue start is a performance feature | stage | C | ask | ~50 |
| engine-runtime-022 | `crates/manifold-playback/src/percussion_orchestrator.rs:191` | 3.1 | `status_animating`, `is_import_in_progress`, `nudge_step_beats`, `escape_json_value` and its constant: 0 callers | none | A | delete | ~24 |
| engine-runtime-023 | `crates/manifold-playback/src/percussion_import.rs:273` | 3.1 | `apply_detected_bpm`: 0 callers; leaves `PercussionBpmDecision` and the confidence threshold field unused; setting not stored in any corpus project | none | A | delete | ~85 |
| engine-runtime-024 | `crates/manifold-playback/src/clip_launcher.rs:71` | 3.9 | `show_debug_logs` on 5 structs, false in every constructor, both setters have 0 callers, trait method `show_debug_logs()` has 0 callers; 14 gated log blocks never run | dev | B | simplify: delete flag, keep or drop the log lines (about 150 if dropped) | ~60 |
| engine-runtime-025 | `crates/manifold-playback/src/video_time.rs:8` | 3.2 | `compute_video_time_beats` is called only by its own test; `compute_video_time` has one caller, inside the dead `sync_clip_loop_state` (001); `PrewarmCandidate` stays | dev | B | delete (after 001) | ~105 |
| engine-runtime-026 | `crates/manifold-media/src/decode_scheduler.rs:184` | 3.1 | `recv_results_blocking`, `is_prepared`, `is_supported_video_file`: 0 callers; leaves `SUPPORTED_EXTENSIONS` (metadata.rs), the `VideoDecoder_IsPrepared` extern and its `.m` body unused; MEDIA_EXPORT_MAP names the first as used (stale) | prep | A | delete | ~35 |
| engine-runtime-027 | `crates/manifold-media/src/decoder.rs:125` | 3.1 | `DecoderPool::copy_frame_to_texture`: 0 callers; MEDIA_BACKEND_DESIGN plans to fold it into the unbuilt `FrameLease::deliver` | none | B | delete | 23 |
| engine-runtime-028 | `crates/manifold-compositor/src/compositor.rs:117` | 3.1 | `CompositorFrame::is_occluded`: 0 callers; the field is read directly | none | A | delete | ~7 |
| engine-runtime-029 | `crates/manifold-compositor/src/compositor.rs:309` | 3.1 | `graph_snapshot_for`, `outer_routings_for` trait methods and impls: still 0 callers; BUG-axzlz (Delete unused Compositor::graph_snapshot_for and outer_routings_for) owns the work | none | A | delete | 37 |
| engine-runtime-030 | `crates/manifold-audio/src/analysis.rs:2025` | 3.2 | `push_with_callback`: only caller is the test `push_callback_matches_enabled_scope_columns` | dev | B | delete fn and test | ~24 |
| engine-runtime-031 | `crates/manifold-profiler/src/compare.rs` | 3.2 | `compare_sessions`, `SessionComparison`, `PassDelta`: only caller is one test; the profiler guide says no CLI was built | dev | B | delete (or ask if the comparison bin is wanted) | ~188 |
| engine-runtime-032 | `crates/manifold-native/src/depth_estimator.rs:4` | 3.1 | `FlowPixel`, `FfiDepthEstimator::supports_parallel`: 0 callers; crate loads bundles with libloading, no `extern "C"` exports, native-source search empty | none | A | delete | ~19 |
| engine-runtime-033 | `crates/manifold-led/src/types.rs:20` | 3.1 | `STRIPS_PER_SIDE`, `ARTNET_OP_POLL`, `ARTNET_PROTOCOL_VERSION`: 0 uses (one doc mention) | none | A | delete | ~6 |
| engine-runtime-034 | `crates/manifold-spectral/Cargo.toml:17` | 3.4 | `log`: machete flags it; no `log::`, `use log`, macro, feature entry or `build.rs` in the crate | none | A | delete | 1 |
| engine-runtime-035 | `crates/manifold-media/src/export_session.rs:43` | 3.3 | `#[allow(dead_code)]` on `ExportError::UnsupportedPlatform`, never constructed; condition (a non-macOS encoder) has not happened | none | C | ask | ~8 |
| engine-runtime-036 | `crates/manifold-playback/src/percussion_backend.rs:326` | 3.9 | nine `MANIFOLD_DEMUCS_*` env knobs, five overlap `settings.demucs.*`; listed for the patterns sweep | none | C | ask | 0 |
| engine-runtime-037 | `crates/manifold-compositor/src/layer_compositor.rs:1874` | 3.9 | `MANIFOLD_LOG_REBUILD_REASON` (and `MANIFOLD_ODF_DEBUG` in manifold-audio): live diagnostics, listed for the patterns sweep | none | C | ask | 0 |
| engine-runtime-038 | `crates/manifold-playback/src/audio_sync.rs:113` | 3.9 | `FFPROBE_PATH`, `FFMPEG_PATH`, `MANIFOLD_PYTHON_PATH`, `MANIFOLD_{NAME}_PLUGIN`: tool-path overrides, listed for the patterns sweep | none | C | ask | 0 |
| engine-runtime-039 | `crates/manifold-playback/src/audio_warp.rs` | 3.2 | `warp_interleaved` has only its own tests as callers; module has no outside reference; BUG-ycmj2 (Integrate Signalsmith for live audio warp and matching export) says Peter deferred it on 2026-10-09 | dev | C | ask (keep) | ~161 |
| engine-runtime-040 | `crates/manifold-playback/src/osc_receiver.rs:371` | 3.1 | `subscribe_keyed`, `unsubscribe_keyed`: named only in comments (the screen counted them); `enable_osc` deliberately uses plain `subscribe` | none | A | delete | ~19 |
| engine-runtime-041 | `crates/manifold-media/src/video_renderer.rs:191` | 3.1 | `request_clip_poster`: named only in a doc comment and a design doc; the filmstrip variant is the live path | none | A | delete | ~48 |
| engine-runtime-042 | `crates/manifold-led/src/types.rs:28` | 3.9 | `ExternalOutputType` has one variant (`ArtNet`) and its field `output_type` is never read; saved as `outputType`, so the load must ignore the key | prep | C | ask | ~9 |

## Order dependencies

- 025 depends on 001: the engine's only call into the free `compute_video_time` is inside `sync_clip_loop_state`.
- 011 removes `clear_ownership` (sync.rs) and 009's `toggle_sync_output`; the two rows touch `osc_sender_enabled` from both sides, so land them together or 009 first.
- 023 leaves `PercussionGlobalSettings::bpm_auto_apply_confidence` (manifold-core) unused: engine-model's crate.
- 029 may leave `loaded_preset_view_by_id`, `snapshot_for_view`, `outer_routings_from_view` (manifold-node-engine) without a caller: graph sweep.
- 009 leaves `ClockAuthority::next()` (manifold-core) and possibly `ChangeBpmCommand::master` callers to re-check; `ClearTempoMapCommand` stays live (`ui_bridge/transport.rs:54`).

## Rows reviewed and kept

TEST-ONLY rows that are test infrastructure, not candidates (details in the proof file): `set_video_sync_interval`, `get_timeline_fallback_bpm`, `recently_started_time`, `set_port`, `start_count_for`, `stop_count_for`, `last_edge_flag_for`, `is_playing_layer`, `playing_count`, `drain_out`, `i32_scalar`. Suppressions kept: `midi_input.rs:60` (`expect(dead_code)`, ownership-only field, checks itself) and `manifold-audio/src/directory/fallback.rs:13` (non-macOS fallback, cross-platform requirement). Feature `project-lend` is live (ui-snapshot harness). No machete row other than 034, no const-bool switch in these crates.

## Seen, not judged

- `PlaybackEngine` hooks `log_warning` and `log_error` are never installed, so "trigger delivery stopped" and "session control interval stopped" are never logged (005). Fallbacks sweep.
- `midi_input.rs:389` "MIDI note backlog" warning only prints when `show_debug_logs` is on, which never happens (024).
- `osc_sync.rs`: `enable_osc` never registers a subscription (CORE_ENGINE_MAP section 13 item 1), so the OSC timecode input is inert; with Peter's M4L ruling (011) the whole `osc_sync.rs` (892 lines) and the receiver's timecode path may be next. Ask.
- `TransportAction::CycleClockAuthority` survives only as an inert match arm (`manifold-app/src/ui_bridge/transport.rs:100`). App-ui sweep.
- Duplicates (3.11): `append_demucs_defaults` repeats the settings-driven argument list in `percussion_backend.rs`; `manifold-media/src/metadata.rs` `SUPPORTED_EXTENSIONS` repeats `manifold_core::video::SUPPORTED_EXTENSIONS`; `TempoRecorder::finalize_clip` repeats `LiveClipManager::finalize_recording_clip` (row 010).
- Per census text, the compositor's `plugin_warmups` list is filled from a registry with no submissions (3.10); not re-checked here.
- After row 023, `ChangeBpmCommand::with_tempo_map` (manifold-editing) may have no caller: engine-model sweep.

## What this sweep did not cover

- No compiler: no `cargo check`, build, test or clippy ran (read-only rules), so cascades are derived by `rg`; the deleting lane's `cargo check -p <crate> --all-targets` is the final proof.
- The pub screen lists items with `pub` visibility only. Private items, struct fields and trait methods were looked at only where a row led to them (hooks, `to_pause_list`, `clip_starts`, flags).
- Names that are common words (`new`, `get`, `title`) were not chased (3.1 trap). The 440 CRATE-ONLY and USED rows were re-counted once with comment lines dropped and the 1 to 2 hit survivors read; deeper chains (an item used only by another item used only by a test) beyond the ones named were not followed.
- manifold-recording and manifold-spectral have no DEAD or TEST-ONLY rows; spectral's only finding is the `log` dependency. manifold-led and manifold-native were read for their DEAD rows and the one enum (042); the LED generators and the LED rig are `ask` by census rule and were not judged.
- Online-only Dropbox projects were not opened (census section 2); the corpus is the 182 readable projects, and it was used only as a loading check.
- Files outside `crates/` were searched for names (scripts, docs, tools, `.claude`) but not judged: `tv-led-mirror`, `plugins/`, `tools/` belong to other sweeps.
