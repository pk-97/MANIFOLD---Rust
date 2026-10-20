# engine-runtime sweep: proof

Proof for `engine-runtime.md` (BUG-yn979, Simplification pass). One section per row id, then the rows reviewed and kept, the suppressions, the dependency and feature checks.

- Main commit `37a9c180a`; `crates/` is unchanged at the checkout read. Model Sonnet 5.5.
- `<repo>` and `$REPO` both mean `/Users/peterkiemann/MANIFOLD - Rust`. Paths in results are repo-relative.
- Standard pair per name, from `docs/SIMPLIFICATION_CENSUS.md` section 3.1 (Dead pub items): `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F NAME <repo>` and `rg -l -w -F NAME /tmp/simplification_census/corpus/projects`. The result line gives the total hit count (the definition included), then the definition, then any other code hit, then the files that only mention the name (hits under `docs/`, `.beads/`, `.claude/` or in a `.md` file). The whole-repo search covers strings, serde attributes, macros that spell the name, and `.m`, `.h`, `.swift` and `.cpp` files.
- Other commands are shown as run. `"$REPO"` inside them is the repo path above.
- The corpus line is a loading check only (census section 2): `0 file(s)` means no readable saved project carries the name, so deleting needs no load path.
- Spans and `lines` are read from the files with a brace-matching script that includes doc comments; `~` marks an estimate that adds banners, separators, imports or call-site edits.
- Method checks after the per-row work, in the last section: module scan, types used only by their own file, and a recount with comment lines dropped.

### engine-runtime-001

Cluster in `crates/manifold-playback/src/engine.rs`: `sync_clip_loop_state` (lines 1928-1969, 42 lines with doc), `compute_clip_playback_rate` (private, 1971-1983, 13 lines), `apply_clip_playback_rate` (3112-3119, 8 lines). Sum 63, plus separator blanks, `~66`. Bead `BUG-mmct0.12` (Playback audit hygiene) item (d) names the first two; `apply_clip_playback_rate` is the same Unity-port leftover and gets the same action.
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F sync_clip_loop_state <repo>`: 3 hit(s); definition at crates/manifold-playback/src/engine.rs:1930; no other code hit; mentions only in .beads/issues.jsonl, docs/SIMPLIFICATION_CENSUS.md.
- `rg -l -w -F sync_clip_loop_state /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F compute_clip_playback_rate <repo>`: 3 hit(s); definition at crates/manifold-playback/src/engine.rs:1972; other code hits: crates/manifold-playback/src/engine.rs:1943; mentions only in .beads/issues.jsonl.
- `rg -l -w -F compute_clip_playback_rate /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F apply_clip_playback_rate <repo>`: 1 hit(s); definition at crates/manifold-playback/src/engine.rs:3114; no other code hit.
- `rg -l -w -F apply_clip_playback_rate /tmp/simplification_census/corpus/projects`: 0 file(s).
- Cascade: the only caller of `compute_clip_playback_rate` is `sync_clip_loop_state` (engine.rs:1943, shown above), so it dies with it. `get_clip_playback_rate` (engine.rs:3017) stays: it has 9 uses. `looping_clip_ids` stays: engine.rs:1808, 1831, 1865 maintain it. Second cascade: `sync_clip_loop_state` (engine.rs:1957) is the only non-test caller of the free function `video_time::compute_video_time`; see engine-runtime-025.
- Also searched: serde string attributes, macro-built names, `extern "C"` / `.m` / `.swift` / `.h` names: the whole-repo `-w -F` search covers every text file, so a string or native use would have printed. None did.
- Verdict: grade A, action delete, risk none (zero callers; the live clip start/stop path does not use these).

### engine-runtime-002

Seven uncalled methods in `crates/manifold-playback/src/engine.rs` plus the one field they leave dead. Spans: `invalidate_prewarm` 1919-1924 (6), `clear_pending_pauses` 1987-1992 (6), `get_timeline_active_clips_at_current_beat` 1996-2001 (6), `get_clip_end_time_seconds` 2978-2982 (5), `live_clip_manager_mut` 601-603 (3), `replace_renderer` 662-665 (4), `set_clock` 760-765 (6), the `to_pause_list` field (declaration, initialiser: 3), plus the section banners that become empty, `~44`. These are not named by an open bead. `set_clock` is a comment-excluded finding: the pub screen counted two comment mentions (engine.rs:613 and :2166) as uses.
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F invalidate_prewarm <repo>`: 1 hit(s); definition at crates/manifold-playback/src/engine.rs:1921; no other code hit.
- `rg -l -w -F invalidate_prewarm /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F clear_pending_pauses <repo>`: 1 hit(s); definition at crates/manifold-playback/src/engine.rs:1989; no other code hit.
- `rg -l -w -F clear_pending_pauses /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F get_timeline_active_clips_at_current_beat <repo>`: 1 hit(s); definition at crates/manifold-playback/src/engine.rs:1998; no other code hit.
- `rg -l -w -F get_timeline_active_clips_at_current_beat /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F get_clip_end_time_seconds <repo>`: 2 hit(s); definition at crates/manifold-playback/src/engine.rs:2980; no other code hit; mentions only in docs/FRAME_EXACT_TRANSPORT_DESIGN.md.
- `rg -l -w -F get_clip_end_time_seconds /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F live_clip_manager_mut <repo>`: 1 hit(s); definition at crates/manifold-playback/src/engine.rs:601; no other code hit.
- `rg -l -w -F live_clip_manager_mut /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F replace_renderer <repo>`: 1 hit(s); definition at crates/manifold-playback/src/engine.rs:663; no other code hit.
- `rg -l -w -F replace_renderer /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F set_clock <repo>`: 3 hit(s); definition at crates/manifold-playback/src/engine.rs:762; other code hits: crates/manifold-playback/src/engine.rs:613 crates/manifold-playback/src/engine.rs:2166.
- `rg -l -w -F set_clock /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n -w set_clock "$REPO/crates" --glob "*.rs" | rg -v "^\S+:[0-9]+:\s*//"`: 3 line(s): crates/manifold-playback/src/engine.rs:613:    /// `set_clock()` for pre-first-tick callers) — the epoch every | crates/manifold-playback/src/engine.rs:762:    pub fn set_clock(&mut self, realtime_now: Seconds, frame_count: u64) { | crates/manifold-playback/src/engine.rs:2166:        // ctx.realtime_now.0`), and `set_clock` covers pre-first-tick callers.
- `rg -n -w to_pause_list "$REPO/crates" --glob "*.rs"`: 3 line(s): crates/manifold-playback/src/engine.rs:347:    to_pause_list: Vec<ClipId>, | crates/manifold-playback/src/engine.rs:442:            to_pause_list: Vec::with_capacity(8), | crates/manifold-playback/src/engine.rs:1991:        self.to_pause_list.clear();
- Cascade: `to_pause_list` is declared (engine.rs:347), initialised (442) and cleared (1991, inside `clear_pending_pauses`) but never pushed to, so it is dead once the method goes. `query_active_timeline_clips` and `timeline_active_scratch` stay: engine.rs:3476-3479 use them. `get_clip_end_time_seconds` is named in docs/FRAME_EXACT_TRANSPORT_DESIGN.md as a design example only (mention, not a caller).
- Also searched: strings, macros, native names: covered by the whole-repo `-w -F` runs above, nothing.
- Verdict: grade A, action delete, risk none.

### engine-runtime-003

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F PlaybackNotifier <repo>`: 2 hit(s); definition at crates/manifold-playback/src/engine.rs:29; no other code hit; mentions only in docs/SIMPLIFICATION_CENSUS.md.
- `rg -l -w -F PlaybackNotifier /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n "impl[^{]*PlaybackNotifier|dyn PlaybackNotifier|: PlaybackNotifier|PlaybackNotifier>" "$REPO/crates" --glob "*.rs"`: no output.
- The trait (engine.rs:27-32, 6 lines, `~7` with its banner) has no implementor, no `dyn` use and no bound anywhere. The live equivalent is `LiveClipHost::mark_compositor_dirty`, which the engine implements itself (engine.rs:3760 region). `docs/SIMPLIFICATION_CENSUS.md` section 3.10 (Traits and registries with no or one implementor) lists it; the Opus patterns sweep judges the trait question, this row only records that the 3.1 searches are empty.
- Verdict: grade A, action delete (together with any `use` that becomes unused: `Layer`, `PresetTypeId` if only this trait used them; cargo check names them), risk none.

### engine-runtime-004

Three `pub` fields on `PlaybackEngine` (engine.rs:331, 336, 337) that nothing reads or writes: `log`, `replenish_warm_cache`, `on_drift_corrected`. Declaration plus `None` initialiser plus the two "Port of C#" comment lines: `~8`. Named by `BUG-mmct0.12` (d) ("six never-installed engine hooks") and by docs/FRAME_EXACT_TRANSPORT_DESIGN.md section 1.9 (Dead and stale things found on the way).
- `rg -n "self\.log\b|\.log\s*=\s|\.log\b.*Some\(" "$REPO/crates/manifold-playback/src/engine.rs"`: no output.
- `rg -n -w "replenish_warm_cache|on_drift_corrected" "$REPO/crates" --glob "*.rs"`: 4 line(s): crates/manifold-playback/src/engine.rs:336:    pub replenish_warm_cache: Option<Box<dyn Fn(&[TimelineClip]) + Send>>, | crates/manifold-playback/src/engine.rs:337:    pub on_drift_corrected: Option<Box<dyn Fn(&str, f32) + Send>>, | crates/manifold-playback/src/engine.rs:437:            replenish_warm_cache: None, | crates/manifold-playback/src/engine.rs:438:            on_drift_corrected: None,
- `rg -n "\.(log|log_warning|log_error|replenish_warm_cache|on_drift_corrected|beat_snapped_beat_resolver|absolute_tick_resolver)\s*=\s" "$REPO/crates" --glob "*.rs"`: no output.
- The third command is the assignment search over every crate: no code anywhere installs any of the seven hooks. The first shows `log` is never read either (field declaration and `None` initialiser only). `replenish_warm_cache` and `on_drift_corrected` appear only as declaration and initialiser.
- Verdict: grade A, action delete the three fields. Risk none.

### engine-runtime-005

`log_warning` and `log_error` (engine.rs:332-333) are read at four sites but never installed, so the branches never run. Sites: 1310-1312 (trigger delivery stopped), 2260-2262 (session control interval stopped), 3360-3364 (restarted stopped player), 3374-3378 (drift correction). Two declarations, two initialisers and the sixteen lines at the four sites: `~20`. Same bead and design-doc cites as engine-runtime-004.
- `rg -n -w "log_warning|log_error" "$REPO/crates" --glob "*.rs"`: 10 line(s): crates/manifold-playback/src/engine.rs:332:    pub log_warning: Option<Box<dyn Fn(&str) + Send>>, | crates/manifold-playback/src/engine.rs:333:    pub log_error: Option<Box<dyn Fn(&str) + Send>>, | crates/manifold-playback/src/engine.rs:435:            log_warning: None, | crates/manifold-playback/src/engine.rs:436:            log_error: None, | crates/manifold-playback/src/engine.rs:1310:            if !was_failed && let Some(log_error) = &self.log_error { | crates/manifold-playback/src/engine.rs:1311:                log_error(&format!("[PlaybackEngine] trigger delivery stopped: {error}")); | crates/manifold-playback/src/engine.rs:2260:                if let Some(log_error) = &self.log_error { | crates/manifold-playback/src/engine.rs:2261:                    log_error(&format!("[PlaybackEngine] session control interval stopped: {error}")); | ...
- `rg -n "\.(log_warning|log_error)\s*=\s" "$REPO/crates" --glob "*.rs"`: no output.
- Not a plain delete: the two error sites report a failed trigger delivery and a stopped session control interval. Because the closure is never installed, those errors are swallowed today (seen, not judged: that is a fallbacks-sweep finding). The fix shape is to call the `log` crate (`log::error!`, `log::warn!`) at those four sites and drop the fields, which both removes the dead hooks and makes the messages appear.
- Verdict: grade B (the one gap is that the replacement turns two currently silent messages on, a behavior change a reviewer should see), action simplify (use `log::error!` / `log::warn!`, delete the two fields), risk dev.

### engine-runtime-006

`beat_snapped_beat_resolver` and `absolute_tick_resolver` (engine.rs:338-339), read in `LiveClipHost::get_beat_snapped_beat` (engine.rs:3741) and `get_current_absolute_tick` (3748), never installed, so both always take the `else` arm. Net removable if the else arms stay: the two fields, two initialisers and the `if let` wrappers, `~12`.
- `rg -n -w "beat_snapped_beat_resolver|absolute_tick_resolver" "$REPO/crates" --glob "*.rs"`: 6 line(s): crates/manifold-playback/src/engine.rs:338:    pub beat_snapped_beat_resolver: Option<Box<dyn Fn() -> f32 + Send>>, | crates/manifold-playback/src/engine.rs:339:    pub absolute_tick_resolver: Option<Box<dyn Fn() -> i32 + Send>>, | crates/manifold-playback/src/engine.rs:439:            beat_snapped_beat_resolver: None, | crates/manifold-playback/src/engine.rs:440:            absolute_tick_resolver: None, | crates/manifold-playback/src/engine.rs:3741:        if let Some(ref resolver) = self.beat_snapped_beat_resolver { | crates/manifold-playback/src/engine.rs:3749:        if let Some(ref resolver) = self.absolute_tick_resolver {
- Why this is not a delete: `docs/CORE_ENGINE_FINDINGS.md` F2 ("Make MIDI launch quantization real") names these two hooks as the intended wiring that would make pad-launch start positions quantize. Today `QuantizeMode` quarter/beat/bar looks like it works for durations but not for starts. Deleting the hooks removes the planned fix point. docs/FRAME_EXACT_TRANSPORT_DESIGN.md section 1.9 lists them as never installed without a ruling.
- Verdict: grade C, action ask (wire per F2, or delete and keep the `else` arms as the only behavior). Risk stage (MIDI launch timing).

### engine-runtime-007

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F append_live_prewarm_candidates <repo>`: 3 hit(s); definition at crates/manifold-playback/src/live_clip_manager.rs:1098; no other code hit; mentions only in docs/CORE_ENGINE_FINDINGS.md, docs/CORE_ENGINE_MAP.md.
- `rg -l -w -F append_live_prewarm_candidates /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F LIVE_PREWARM_MAX_UNIQUE_CLIPS <repo>`: 3 hit(s); definition at crates/manifold-playback/src/engine.rs:51; no other code hit; mentions only in docs/CORE_ENGINE_FINDINGS.md, docs/CORE_ENGINE_MAP.md.
- `rg -l -w -F LIVE_PREWARM_MAX_UNIQUE_CLIPS /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F LIVE_PREWARM_RECENT_PRIORITY_COUNT <repo>`: 2 hit(s); definition at crates/manifold-playback/src/engine.rs:52; no other code hit; mentions only in docs/CORE_ENGINE_FINDINGS.md.
- `rg -l -w -F LIVE_PREWARM_RECENT_PRIORITY_COUNT /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F COMBINED_PREWARM_MAX_UNIQUE_CLIPS <repo>`: 3 hit(s); definition at crates/manifold-playback/src/engine.rs:53; no other code hit; mentions only in docs/CORE_ENGINE_FINDINGS.md.
- `rg -l -w -F COMBINED_PREWARM_MAX_UNIQUE_CLIPS /tmp/simplification_census/corpus/projects`: 0 file(s).
- Size: `append_live_prewarm_candidates` spans live_clip_manager.rs:1089-1114 (26 lines with doc); three one-line constants at engine.rs:51-53. `~29`.
- Not a plain delete: `docs/CORE_ENGINE_MAP.md` section 13 item 10 and `docs/CORE_ENGINE_FINDINGS.md` F10 call this the unfinished live prewarm ("first MIDI fire of a cold video clip rides only the 0.02s recently-started gate, black-frame risk on stage") and list the three constants as its parameters. The function is a stub with a TODO, never called by `compute_prewarm_candidates`. Whether to finish or drop it is Peter's call on a stage-facing feature.
- Verdict: grade C, action ask. Risk stage.

### engine-runtime-008

`crates/manifold-playback/src/active_window.rs`, `ActiveTimelineClipWindow`: an incremental boundary-cursor active-clip query, 694 lines with 7 tests, plus `pub mod active_window;` (lib.rs:2) and the empty `PlaybackEngine::reset_active_clip_window` stub (engine.rs:757-758). `~697`. The pub screen lists only `get_active_clips` as TEST-ONLY; the struct reads CRATE-ONLY because its own tests instantiate it.
- `rg -n -w ActiveTimelineClipWindow "$REPO/crates" --glob "*.rs" | rg -v "crates/manifold-playback/src/active_window.rs"`: no output.
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F get_active_clips <repo>`: 15 hit(s); definition at crates/manifold-playback/src/active_window.rs:67; other code hits: crates/manifold-playback/src/active_window.rs:493 crates/manifold-playback/src/active_window.rs:507 crates/manifold-playback/src/active_window.rs:517 crates/manifold-playback/src/active_window.rs:523 crates/manifold-playback/src/active_window.rs:533 crates/manifold-playback/src/active_window.rs:549.
- `rg -l -w -F get_active_clips /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F reset_active_clip_window <repo>`: 3 hit(s); definition at crates/manifold-playback/src/engine.rs:758; no other code hit; mentions only in docs/CORE_ENGINE_FINDINGS.md, docs/CORE_ENGINE_MAP.md.
- `rg -l -w -F reset_active_clip_window /tmp/simplification_census/corpus/projects`: 0 file(s).
- The first command (every use of the struct outside its own file) prints nothing: no engine, app or test constructs it. The `get_active_clips` hits are all inside active_window.rs's own test module (14 calls).
- Why this is not a plain delete: `docs/CORE_ENGINE_FINDINGS.md` F8 and `docs/CORE_ENGINE_MAP.md` section 13 item 8 mark it DECISION NEEDED: measure the per-frame full-layer query at project scale, then wire the window in or delete the file. No measurement is recorded.
- Verdict: grade C, action ask (needs the F8 measurement or Peter's ruling), risk dev. `reset_active_clip_window` goes with it either way.

### engine-runtime-009

`crates/manifold-playback/src/transport_controller.rs` (347 lines). The census lists six DEAD methods; the dead cluster is nine methods, three private helpers that only they call, `InsertCursorState` and one field. Spans: `cycle_authority` 54-64 (11), `apply_authority_exclusively` 66-97 (32), `disable_non_authority_sources` 99-132 (34), `is_authority_source_enabled` 134-144 (11), `enable_authority_source` 146-165 (20), `get_clock_authority` 48-52 (5), `toggle_play_pause` 169-181 (13), `stop_playback` 183-191 (9), `toggle_record` 193-196 (4), `toggle_sync_output` 233-247 (15), `set_bpm` 251-274 (24), `clear_tempo_map` 312-328 (17), `InsertCursorState` 15-20 (6), `osc_sender_port` (field and initialiser, 3). Sum 204, plus separators and banners, `~210`. `BUG-mmct0.12` (d) names "eight uncalled TransportController methods"; today seven of the eight are still uncalled (cycle_authority, toggle_play_pause, stop_playback, toggle_record, toggle_sync_output, clear_tempo_map, set_bpm). The eighth, `toggle_midi_clock`, is now called (content_commands.rs:1626) and stays, together with `toggle_link` and `reset_bpm`.
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F cycle_authority <repo>`: 1 hit(s); definition at crates/manifold-playback/src/transport_controller.rs:56; no other code hit.
- `rg -l -w -F cycle_authority /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F toggle_play_pause <repo>`: 1 hit(s); definition at crates/manifold-playback/src/transport_controller.rs:171; no other code hit.
- `rg -l -w -F toggle_play_pause /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F stop_playback <repo>`: 1 hit(s); definition at crates/manifold-playback/src/transport_controller.rs:185; no other code hit.
- `rg -l -w -F stop_playback /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F toggle_record <repo>`: 1 hit(s); definition at crates/manifold-playback/src/transport_controller.rs:194; no other code hit.
- `rg -l -w -F toggle_record /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F toggle_sync_output <repo>`: 1 hit(s); definition at crates/manifold-playback/src/transport_controller.rs:234; no other code hit.
- `rg -l -w -F toggle_sync_output /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F clear_tempo_map <repo>`: 1 hit(s); definition at crates/manifold-playback/src/transport_controller.rs:314; no other code hit.
- `rg -l -w -F clear_tempo_map /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F toggle_midi_clock <repo>`: 2 hit(s); definition at crates/manifold-playback/src/transport_controller.rs:219; other code hits: crates/manifold-app/src/content_commands.rs:1626.
- `rg -l -w -F toggle_midi_clock /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n "TransportController::set_bpm|transport_controller\.set_bpm|\.set_bpm\(" "$REPO/crates" --glob "*.rs"`: no output.
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F apply_authority_exclusively <repo>`: 5 hit(s); definition at crates/manifold-playback/src/transport_controller.rs:68; other code hits: crates/manifold-playback/src/transport_controller.rs:62; mentions only in docs/CORE_ENGINE_F5_SCOPING.md, docs/CORE_ENGINE_FINDINGS.md, docs/CORE_ENGINE_MAP.md.
- `rg -l -w -F apply_authority_exclusively /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F get_clock_authority <repo>`: 2 hit(s); definition at crates/manifold-playback/src/transport_controller.rs:48; other code hits: crates/manifold-playback/src/transport_controller.rs:59.
- `rg -l -w -F get_clock_authority /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F InsertCursorState <repo>`: 3 hit(s); definition at crates/manifold-playback/src/transport_controller.rs:17; other code hits: crates/manifold-playback/src/transport_controller.rs:171 crates/manifold-playback/src/transport_controller.rs:185.
- `rg -l -w -F InsertCursorState /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F osc_sender_port <repo>`: 3 hit(s); definition at crates/manifold-playback/src/transport_controller.rs:32; other code hits: crates/manifold-playback/src/transport_controller.rs:42 crates/manifold-playback/src/transport_controller.rs:243.
- `rg -l -w -F osc_sender_port /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n -w "toggle_link|reset_bpm|toggle_midi_clock|write_clock_authority" "$REPO/crates/manifold-app" --glob "*.rs"`: 4 line(s): crates/manifold-app/src/content_thread.rs:1587:            manifold_playback::transport_controller::write_clock_authority(&mut self.engine, auto); | crates/manifold-app/src/content_commands.rs:1622:                self.transport_controller.toggle_link(&mut self.engine); | crates/manifold-app/src/content_commands.rs:1626:                    .toggle_midi_clock(&mut self.engine); | crates/manifold-app/src/content_commands.rs:1638:                TransportController::reset_bpm(&mut self.engine, &mut self.editing_service);
- Last command: the app calls `toggle_link`, `toggle_midi_clock` (a multi-line call at content_commands.rs:1625-1627), `reset_bpm` and the free function `write_clock_authority`; those stay, as do the fields `link_sync`, `midi_clock_sync`, `osc_enabled` and `osc_sender_enabled`. `set_bpm` has no caller (`rg -n -w set_bpm crates` finds only this definition and an unrelated `Settings::set_bpm` in manifold-core).
- The UI buttons that look like the dead methods never reach them: the SYNC button (`TransportAction::ToggleSyncOutput`) goes to `ContentCommand::ToggleOscSyncMode` (app_render.rs:1108); `CycleClockAuthority` is removed from the toolbar (app_render.rs:1099 comment) and survives only as an inert match arm (ui_bridge/transport.rs:100, seen for the app-ui sweep).
- `ClearTempoMapCommand` (manifold-editing) stays live: crates/manifold-app/src/ui_bridge/transport.rs:54 builds it. `ChangeBpmCommand::master` stays: `reset_bpm` uses it.
- Verdict: grade A (every search empty), action delete, risk none (zero callers). The deleting lane's cargo check removes the imports that go unused (`Project`, `Bpm`, `Beats`, `ClockAuthority` partly).

### engine-runtime-010

`crates/manifold-playback/src/tempo_recorder.rs`: the per-clip provenance half of `TempoRecorder`. Items: `RecordingClipStartInfo` (28-43, 16), the `clip_starts` map (field and initialiser, 2), `clear_clip_starts` (74-76, 3), `update_session_state` (88-115, 28), `track_clip_start` (232-270, 39), `finalize_clip` (272-335, 64), `remove_clip_start` (337-344, 8), the imports they alone use (2). `~165`. `BUG-mmct0.12` (d) names `track_clip_start` and `finalize_clip`; the other three are the same Unity-port leftover.
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F track_clip_start <repo>`: 3 hit(s); definition at crates/manifold-playback/src/tempo_recorder.rs:234; no other code hit; mentions only in .beads/issues.jsonl, docs/SIMPLIFICATION_CENSUS.md.
- `rg -l -w -F track_clip_start /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F finalize_clip <repo>`: 3 hit(s); definition at crates/manifold-playback/src/tempo_recorder.rs:274; no other code hit; mentions only in .beads/issues.jsonl, docs/SIMPLIFICATION_CENSUS.md.
- `rg -l -w -F finalize_clip /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F remove_clip_start <repo>`: 1 hit(s); definition at crates/manifold-playback/src/tempo_recorder.rs:339; no other code hit.
- `rg -l -w -F remove_clip_start /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F clear_clip_starts <repo>`: 1 hit(s); definition at crates/manifold-playback/src/tempo_recorder.rs:74; no other code hit.
- `rg -l -w -F clear_clip_starts /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F update_session_state <repo>`: 1 hit(s); definition at crates/manifold-playback/src/tempo_recorder.rs:91; no other code hit.
- `rg -l -w -F update_session_state /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n -w "clip_starts|RecordingClipStartInfo" "$REPO/crates/manifold-playback/src/tempo_recorder.rs"`: 8 line(s): 19:    clip_starts: HashMap<ClipId, RecordingClipStartInfo>, | 28:struct RecordingClipStartInfo { | 56:            clip_starts: HashMap::new(), | 75:        self.clip_starts.clear(); | 256:        self.clip_starts.insert( | 258:            RecordingClipStartInfo { | 287:        let start = match self.clip_starts.remove(live_clip_id) { | 343:        self.clip_starts.remove(clip_id);
- `rg -n -w "RecordedClipProvenance" "$REPO/crates/manifold-playback" --glob "*.rs"`: 4 line(s): crates/manifold-playback/src/live_clip_manager.rs:5:use manifold_core::recording::RecordedClipProvenance; | crates/manifold-playback/src/live_clip_manager.rs:1055:        let entry = RecordedClipProvenance { | crates/manifold-playback/src/tempo_recorder.rs:4:use manifold_core::recording::{RecordedClipProvenance, RecordedTempoChange, RecordingProvenance}; | crates/manifold-playback/src/tempo_recorder.rs:316:        let entry = RecordedClipProvenance {
- Cascade: `clip_starts` is written only by `track_clip_start` and read only by `finalize_clip` / `remove_clip_start` / `clear_clip_starts`, so the whole map and its info struct go together. The live equivalent already exists: `LiveClipManager::finalize_recording_clip` (live_clip_manager.rs:1013) builds the `RecordedClipProvenance` entry itself (live_clip_manager.rs:1055), so this is a dead duplicate (3.11, noted only). `TempoRecorder::try_record_tempo_point`, `append_tempo_change`, `end_session_if_active`, `capture_*` stay: content_thread.rs:2099, 2105, 2143 call them.
- Verdict: grade A, action delete, risk none (zero callers).

### engine-runtime-011

`crates/manifold-playback/src/osc_sender.rs` (314 lines): `OscPositionSender`, the legacy Max4Live sender. Plus `SyncArbiter::clear_ownership` (sync.rs:147-149, only called by `disable_sender`) and the app-side wiring: `osc_sender` field and construction (content_thread.rs:22, 123; app.rs:1917; headless_harness.rs:98), the `else if osc_sender_enabled` branch (content_thread.rs:688-701), the three `is_sender_enabled() ||` tests (content_commands.rs:887, 905, 918), the `osc_sender_enabled` state fields (content_state.rs:215, 650; frame_plan.rs:173, 843; state_sync.rs:323) and the two fields on `TransportController`. `~345`.
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F enable_sender <repo>`: 3 hit(s); definition at crates/manifold-playback/src/osc_sender.rs:63; no other code hit; mentions only in docs/CORE_ENGINE_FINDINGS.md.
- `rg -l -w -F enable_sender /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F disable_sender <repo>`: 1 hit(s); definition at crates/manifold-playback/src/osc_sender.rs:101; no other code hit.
- `rg -l -w -F disable_sender /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F clear_ownership <repo>`: 3 hit(s); definition at crates/manifold-playback/src/sync.rs:147; other code hits: crates/manifold-playback/src/osc_sender.rs:106 crates/manifold-playback/src/sync.rs:128.
- `rg -l -w -F clear_ownership /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n "osc_sender_enabled\s*=|osc_sender_enabled:\s*(true|self)" "$REPO/crates" --glob "*.rs"`: 3 line(s): crates/manifold-app/src/content_thread.rs:1412:            osc_sender_enabled: self.transport_controller.osc_sender_enabled, | crates/manifold-playback/src/transport_controller.rs:236:            self.osc_sender_enabled = false; | crates/manifold-playback/src/transport_controller.rs:244:            self.osc_sender_enabled = true;
- `rg -n -w "ToggleOscSyncMode" "$REPO/crates" --glob "*.rs"`: 3 line(s): crates/manifold-app/src/content_command.rs:222:    ToggleOscSyncMode, | crates/manifold-app/src/content_commands.rs:1758:            ContentCommand::ToggleOscSyncMode => { | crates/manifold-app/src/app_render.rs:1109:                    self.send_content_cmd(ContentCommand::ToggleOscSyncMode);
- `for f in /tmp/simplification_census/corpus/projects/*.json; do jq -r ".settings.oscSyncMode // \"absent\"" "$f"; done | sort | uniq -c`: 3 line(s):   82 0 |   15 1 |   85 absent
- `enable_sender` is the only way `is_enabled` becomes true and has no caller (git: it was wired to the old ToggleSyncOutput command in 03c1c502f, since replaced). The SYNC button (`TransportAction::ToggleSyncOutput`, app_render.rs:1108) now sends `ContentCommand::ToggleOscSyncMode`, which only toggles the AbletonOSC bridge (content_commands.rs:1758-1776). `osc_sender_enabled` is assigned only inside the dead `TransportController::toggle_sync_output` (transport_controller.rs:236, 244). So `late_update` returns at its first line, always.
- Peter's ruling, docs/ABLETON_TRANSPORT_SYNC_DESIGN.md lines 65-67: the M4L device path is "not used anymore"; deletion is "a separate cleanup, deferred". docs/CORE_ENGINE_MAP.md line 303: "legacy, Peter no longer uses it. Deletion is a deferred cleanup". The same design doc's line 94 and line 433 say do not touch it for that session only.
- Corpus: `oscSyncMode` is 0 (M4L, the default) in 82 projects, 1 (AbletonOsc) in 15, absent in 85. The enum value and its saved spelling stay for file compatibility; only the sender goes. The saved value must keep loading (census section 2, Saved projects: a loading check, never a usage check); it does, because only the sender goes.
- Gaps keeping this at B: the SYNC chip shows "Off" for M4L-mode projects through `osc_sender_enabled` (state_sync.rs:323), so the display code changes with the deletion; and the separate M4L timecode input path (`osc_sync.rs`, `OscReceiver`) is not part of this row (seen, not judged: CORE_ENGINE_MAP section 13 item 1 says `enable_osc` never registers a subscription).
- Verdict: grade B, action delete, risk stage (transport chip display, M4L-mode project load).

### engine-runtime-012

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F current_authority <repo>`: 1 hit(s); definition at crates/manifold-playback/src/sync.rs:120; no other code hit.
- `rg -l -w -F current_authority /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F set_manifold_owns <repo>`: 1 hit(s); definition at crates/manifold-playback/src/sync.rs:126; no other code hit.
- `rg -l -w -F set_manifold_owns /tmp/simplification_census/corpus/projects`: 0 file(s).
- `set_manifold_owns_at` (sync.rs, the timestamped variant) is the live one: content_commands.rs:889-891 calls it. `SyncArbiter::current_authority` duplicates `TransportController::get_clock_authority` and is itself unused. Lines: 5 + 5 + separator, `~12`.
- Verdict: grade A, action delete, risk none.

### engine-runtime-013

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F is_clock_transport_playing <repo>`: 1 hit(s); definition at crates/manifold-playback/src/midi_clock_sync.rs:268; no other code hit.
- `rg -l -w -F is_clock_transport_playing /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F available_source_count <repo>`: 1 hit(s); definition at crates/manifold-playback/src/midi_clock_sync.rs:319; no other code hit.
- `rg -l -w -F available_source_count /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F available_source_name <repo>`: 1 hit(s); definition at crates/manifold-playback/src/midi_clock_sync.rs:324; no other code hit.
- `rg -l -w -F available_source_name /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F available_source_names <repo>`: 2 hit(s); definition at crates/manifold-playback/src/midi_clock_sync.rs:329; no other code hit; mentions only in .beads/issues.jsonl.
- `rg -l -w -F available_source_names /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n -w "source_count|source_name" "$REPO/crates/manifold-playback/src" --glob "*.rs"`: 11 line(s): crates/manifold-playback/src/rig_log.rs:177:fn source_name(s: TransportSource) -> &'static str { | crates/manifold-playback/src/rig_log.rs:267:            source_name(source), | crates/manifold-playback/src/midi_clock_sync.rs:175:    fn source_count() -> usize { | crates/manifold-playback/src/midi_clock_sync.rs:186:    fn source_name(index: i32) -> String { | crates/manifold-playback/src/midi_clock_sync.rs:320:        MidiClockReceiver::source_count() | crates/manifold-playback/src/midi_clock_sync.rs:325:        MidiClockReceiver::source_name(index) | ...
- Four wrapper accessors on `MidiClockSyncController` (spans 268-270 (3), 318-321 (4), 323-326 (4), 328-334 (7); 18 lines, `~20` with separators). The one mention of `available_source_names` is `.beads/issues.jsonl` (BUG-dl16 (Investigate Corrosion V9 GPU stalls after completed warmup), an unrelated note). The receiver's own `source_count` / `source_name` stay: `enable_midi_clock` and `change_source` call them (midi_clock_sync.rs:351, 363, 396), as the command above shows. `last_is_playing` stays: midi_clock_sync.rs:496-506 use it.
- Verdict: grade A, action delete, risk none.

### engine-runtime-014

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F has_device <repo>`: 1 hit(s); definition at crates/manifold-playback/src/midi_input.rs:173; no other code hit.
- `rg -l -w -F has_device /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F set_use_native_clock_note_events <repo>`: 1 hit(s); definition at crates/manifold-playback/src/midi_input.rs:154; no other code hit.
- `rg -l -w -F set_use_native_clock_note_events /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n -w use_native_clock_note_events "$REPO/crates" --glob "*.rs"`: 5 line(s): crates/manifold-playback/src/midi_input.rs:81:    use_native_clock_note_events: bool, | crates/manifold-playback/src/midi_input.rs:122:            use_native_clock_note_events: true, | crates/manifold-playback/src/midi_input.rs:155:        self.use_native_clock_note_events = value; | crates/manifold-playback/src/midi_input.rs:601:    /// In Rust: true when CLK is clock authority and use_native_clock_note_events is set. | ...
- `has_device` has no caller (4 lines). `use_native_clock_note_events` is initialised to `true` (midi_input.rs:122), its only writer is the uncalled setter (155), and its one read (604) is an early return that therefore never fires. The flag has exactly one value. Removing it: setter 3, field 1, initialiser 1, early return 3, doc 2, `has_device` 4 = `~15`.
- Verdict: grade B (the flag deletion changes one branch that is dead by construction; needs a reviewer to see it is the same), action simplify (delete the flag and the setter, `has_device`), risk dev.

### engine-runtime-015

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F set_device_filter <repo>`: 1 hit(s); definition at crates/manifold-playback/src/midi_input.rs:192; no other code hit.
- `rg -l -w -F set_device_filter /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n "device_filter" "$REPO/crates/manifold-playback/src/midi_input.rs"`: 10 line(s): 87:    device_filter: Option<String>, | 125:            device_filter: None, | 159:    pub fn device_filter(&self) -> Option<&str> { | 160:        self.device_filter.as_deref() | 192:    pub fn set_device_filter(&mut self, product_name: Option<&str>) { | 193:        self.device_filter = match product_name { | 204:                !Self::matches_device_filter_name(&dev.name, self.device_filter.as_deref()) | 221:            self.device_filter.as_deref().unwrap_or("All"), | 228:    fn matches_device_filter_name(name: &str, filter: Option<&str>) -> bool { | 429:            if !Self::matches_device_filter_name(&port_name, self.device_filter.as_deref()) {
- `rg -n "\.device_filter\(\)|set_device_filter\(" "$REPO/crates" --glob "*.rs"`: 1 line(s): crates/manifold-playback/src/midi_input.rs:192:    pub fn set_device_filter(&mut self, product_name: Option<&str>) {
- `set_device_filter` (189-224, 36 lines) is the only writer of `device_filter` besides the `None` initialiser, and nothing calls it, so the filter is always `None` and `matches_device_filter_name` (226-233) always returns true. With the getter `device_filter()` (158-161), the field, its initialiser and the check at midi_input.rs:429-431, about `~55` lines go.
- Why this is not a plain delete: it is a live-rig feature (restrict MIDI note input to one named controller) that was ported from Unity and never given a UI. A rig with two controllers plugged in may want it. Peter's call.
- Verdict: grade C, action ask, risk stage (MIDI input).

### engine-runtime-016

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F route_count <repo>`: 1 hit(s); definition at crates/manifold-playback/src/osc_param_router.rs:104; no other code hit.
- `rg -l -w -F route_count /tmp/simplification_census/corpus/projects`: 0 file(s).
- Four lines (103-106) with its doc. `registered_addresses` stays.
- Verdict: grade A, action delete, risk none. `~4`.

### engine-runtime-017

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F toggle_osc <repo>`: 1 hit(s); definition at crates/manifold-playback/src/osc_sync.rs:264; no other code hit.
- `rg -l -w -F toggle_osc /tmp/simplification_census/corpus/projects`: 0 file(s).
- Seven lines (264-270). `enable_osc` / `disable_osc` are separate and not judged here (seen: `enable_osc` never registers a subscription, CORE_ENGINE_MAP section 13 item 1).
- Verdict: grade A, action delete, risk none. `~7`.

### engine-runtime-018

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F has_send_socket <repo>`: 1 hit(s); definition at crates/manifold-playback/src/ableton_bridge.rs:473; no other code hit.
- `rg -l -w -F has_send_socket /tmp/simplification_census/corpus/projects`: 0 file(s).
- Three lines (473-475). `send_socket` stays.
- Verdict: grade A, action delete, risk none. `~3`.

### engine-runtime-019

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F with_tempo <repo>`: 11 hit(s); definition at crates/manifold-playback/src/link_sync.rs:63; other code hits: crates/manifold-playback/tests/ableton_transport_sync.rs:98 crates/manifold-playback/tests/ableton_transport_sync.rs:362 crates/manifold-playback/tests/ableton_transport_sync.rs:404 crates/manifold-playback/tests/ableton_transport_sync.rs:447 crates/manifold-playback/tests/ableton_transport_sync.rs:487 crates/manifold-playback/tests/ableton_transport_sync.rs:523.
- `rg -l -w -F with_tempo /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n "\.with_tempo\(|fn with_tempo" "$REPO/crates/manifold-playback/src"`: 1 line(s): crates/manifold-playback/src/link_sync.rs:63:    pub fn with_tempo(&self, at: Instant, tempo: f64) -> Self {
- The census counted 10 test uses because `crates/manifold-playback/tests/ableton_transport_sync.rs:98` defines its own `FakeAbleton::with_tempo`; that is a different function. Inside src nothing calls the link_sync one (the last command shows only its definition). Four lines (62-65).
- Verdict: grade A, action delete, risk none. `~4`.

### engine-runtime-020

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F from_static_sound_data <repo>`: 1 hit(s); definition at crates/manifold-playback/src/audio_decoder.rs:31; no other code hit.
- `rg -l -w -F from_static_sound_data /tmp/simplification_census/corpus/projects`: 0 file(s).
- The only function in `impl DecodedAudio` (lines 27-44, 18 with the impl wrapper). `DecodedAudio` itself stays (decode path, `rg` shows 4 own-crate uses).
- Verdict: grade A, action delete, risk none. `~18`.

### engine-runtime-021

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F set_randomize_in_point <repo>`: 2 hit(s); definition at crates/manifold-playback/src/clip_launcher.rs:67; no other code hit; mentions only in .beads/issues.jsonl.
- `rg -l -w -F set_randomize_in_point /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n -w randomize_in_point "$REPO/crates" --glob "*.rs"`: 7 line(s): crates/manifold-playback/src/clip_launcher.rs:26:    randomize_in_point: bool, | crates/manifold-playback/src/clip_launcher.rs:58:            randomize_in_point: false, | crates/manifold-playback/src/clip_launcher.rs:68:        self.randomize_in_point = value; | crates/manifold-playback/src/clip_launcher.rs:139:            self.randomize_in_point, | crates/manifold-playback/src/clip_launcher.rs:476:            self.randomize_in_point, | crates/manifold-playback/src/clip_launcher.rs:608:        randomize_in_point: bool, | crates/manifold-playback/src/clip_launcher.rs:613:        if !randomize_in_point  |  |  clip_duration <= 1.0 {
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F compute_in_point <repo>`: 3 hit(s); definition at crates/manifold-playback/src/clip_launcher.rs:607; other code hits: crates/manifold-playback/src/clip_launcher.rs:138 crates/manifold-playback/src/clip_launcher.rs:475.
- `rg -l -w -F compute_in_point /tmp/simplification_census/corpus/projects`: 0 file(s).
- `ClipLauncher.randomize_in_point` starts `false` (clip_launcher.rs:58) and its only writer, `set_randomize_in_point`, has no caller, so `compute_in_point` always returns 0.0 and the random-start code is unreachable. `BUG-mmct0.12` (d) names `set_randomize_in_point`. Removable: setter 3, field 2, `compute_in_point` 18, `unit_from_seed` 4, `pseudo_random_unit` 14 (its only caller is `compute_in_point`, clip_launcher.rs:622), plus the argument at two call sites: `~50`.
- Why this is not a plain delete: "random in-point per pad hit" is a performance feature (Resolume-style random cue start) that was ported and never exposed. The bead calls it dead code; whether Peter wants it on a switch is his call.
- Verdict: grade C, action ask, risk stage (MIDI clip trigger start position).

### engine-runtime-022

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F status_animating <repo>`: 1 hit(s); definition at crates/manifold-playback/src/percussion_orchestrator.rs:191; no other code hit.
- `rg -l -w -F status_animating /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F is_import_in_progress <repo>`: 1 hit(s); definition at crates/manifold-playback/src/percussion_orchestrator.rs:203; no other code hit.
- `rg -l -w -F is_import_in_progress /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F nudge_step_beats <repo>`: 1 hit(s); definition at crates/manifold-playback/src/percussion_orchestrator.rs:207; no other code hit.
- `rg -l -w -F nudge_step_beats /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F escape_json_value <repo>`: 1 hit(s); definition at crates/manifold-playback/src/percussion_orchestrator.rs:1161; no other code hit.
- `rg -l -w -F escape_json_value /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F PERCUSSION_NUDGE_STEP_BEATS <repo>`: 2 hit(s); definition at crates/manifold-playback/src/percussion_orchestrator.rs:56; other code hits: crates/manifold-playback/src/percussion_orchestrator.rs:208.
- `rg -l -w -F PERCUSSION_NUDGE_STEP_BEATS /tmp/simplification_census/corpus/projects`: 0 file(s).
- Three one-line accessors (3 lines each) and `escape_json_value` (1160-1170, 11), plus `PERCUSSION_NUDGE_STEP_BEATS` (line 56), whose only reader is `nudge_step_beats`. The fields behind the accessors stay (many readers). `~24`.
- Verdict: grade A, action delete, risk none.

### engine-runtime-023

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F apply_detected_bpm <repo>`: 1 hit(s); definition at crates/manifold-playback/src/percussion_import.rs:273; no other code hit.
- `rg -l -w -F apply_detected_bpm /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F PercussionBpmDecision <repo>`: 9 hit(s); definition at crates/manifold-playback/src/percussion_import.rs:30; other code hits: crates/manifold-playback/src/percussion_import.rs:26 crates/manifold-playback/src/percussion_import.rs:28 crates/manifold-playback/src/percussion_import.rs:278 crates/manifold-playback/src/percussion_import.rs:281 crates/manifold-playback/src/percussion_import.rs:286 crates/manifold-playback/src/percussion_import.rs:292.
- `rg -l -w -F PercussionBpmDecision /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F bpm_auto_apply_confidence_threshold <repo>`: 5 hit(s); definition at crates/manifold-playback/src/percussion_import.rs:57; other code hits: crates/manifold-playback/src/percussion_import.rs:63 crates/manifold-playback/src/percussion_import.rs:73 crates/manifold-playback/src/percussion_import.rs:296 crates/manifold-playback/src/percussion_import.rs:303.
- `rg -l -w -F bpm_auto_apply_confidence_threshold /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n -i "bpmAutoApplyConfidence|bpm_auto_apply_confidence" "$REPO/crates" "/tmp/simplification_census/corpus/projects" --glob "*.rs" --glob "*.json" -l`: 2 line(s): crates/manifold-core/src/percussion_settings.rs | crates/manifold-playback/src/percussion_import.rs
- `apply_detected_bpm` (273-338, 66 lines) is the whole "auto-apply the detected tempo when confidence is high" step of `PercussionImportService`; nothing calls it. After it goes, `PercussionBpmDecision` (enum, 26-34, ~9 lines with banner) is unused and so are the confidence threshold field, its constant and the `new_with_settings` branch that fills it (~10 lines). The setting behind it, `PercussionGlobalSettings::bpm_auto_apply_confidence` (manifold-core/src/percussion_settings.rs:33, 44), is stored in no corpus project (last command finds it only in .rs files); that part belongs to the engine-model sweep's crate. `~85` in playback.
- The service itself is live: percussion_orchestrator.rs:1049 builds it for placement. This row removes only the tempo step.
- Verdict: grade A, action delete, risk none (no caller).

### engine-runtime-024

`show_debug_logs`, a debug switch that is false everywhere. Five structs carry the field (`PlaybackEngine` engine.rs:342, `OscSyncController` osc_sync.rs:77, `MidiInputController` midi_input.rs:78, `OscReceiver` osc_receiver.rs:79, `ClipLauncher` clip_launcher.rs:27), the `LiveClipHost` trait carries a `show_debug_logs()` method that nothing calls, with four implementations (declared live_clip_manager.rs:25; implemented live_clip_manager.rs:1146, midi_input.rs:931, engine.rs:3702, tests/live_clip.rs:60), and 14 `if ... show_debug_logs` blocks gate `log::debug!` / `log::warn!` lines. Estimated `~60` lines if the log lines stay as plain `log::debug!`, `~150` if they are deleted.
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F set_show_debug_logs <repo>`: 2 hit(s); definition at crates/manifold-playback/src/clip_launcher.rs:71; other code hits: crates/manifold-playback/src/midi_input.rs:150.
- `rg -l -w -F set_show_debug_logs /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n "show_debug_logs\s*=\s*true|show_debug_logs:\s*true|set_show_debug_logs\(\s*true|set_show_debug_logs\(" "$REPO/crates" --glob "*.rs"`: 2 line(s): crates/manifold-playback/src/midi_input.rs:150:    pub fn set_show_debug_logs(&mut self, value: bool) { | crates/manifold-playback/src/clip_launcher.rs:71:    pub fn set_show_debug_logs(&mut self, value: bool) {
- `rg -c "show_debug_logs" "$REPO/crates" --glob "*.rs"`: 7 line(s): crates/manifold-playback/src/clip_launcher.rs:8 | crates/manifold-playback/src/live_clip_manager.rs:2 | crates/manifold-playback/src/engine.rs:4 | crates/manifold-playback/src/midi_input.rs:12 | crates/manifold-playback/src/osc_receiver.rs:3 | crates/manifold-playback/src/osc_sync.rs:4 | crates/manifold-playback/tests/live_clip.rs:1
- The second command looks for any place the flag is ever set true or any call to the two setters: it prints only the two setter definitions (clip_launcher.rs:71, midi_input.rs:150), no call and no `true`. Every constructor sets `false`. So all 14 gated blocks (osc_sync 2, osc_receiver 1, clip_launcher 4, midi_input 7) are unreachable, including one real diagnostic: midi_input.rs:389-396 warns "MIDI note backlog" only when the flag is on.
- Judgment left open: delete the flag and keep the messages as ordinary `log::debug!` (they only print at debug level), or delete the messages. Env knobs are a patterns-sweep call, so this is `simplify` with the choice named.
- Verdict: grade B (the gap is the choice above and the `LiveClipHost` trait change touching test doubles in tests/live_clip.rs), action simplify, risk dev. Category 3.9.

### engine-runtime-025

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F compute_video_time_beats <repo>`: 2 hit(s); definition at crates/manifold-playback/src/video_time.rs:46; other code hits: crates/manifold-playback/src/video_time.rs:106.
- `rg -l -w -F compute_video_time_beats /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n -w "compute_video_time|video_time::" "$REPO/crates" --glob "*.rs" | rg -v "crates/manifold-playback/src/video_time.rs"`: 4 line(s): crates/manifold-playback/src/engine.rs:1957:                let video_time = crate::video_time::compute_video_time( | crates/manifold-playback/src/engine.rs:3082:    pub fn compute_video_time(&mut self, clip: &TimelineClip, clip_id: &str) -> f32 { | crates/manifold-playback/src/engine.rs:3174:            let video_time = self.compute_video_time(&clip, clip_id); | crates/manifold-playback/src/engine.rs:3435:            let video_time = self.compute_video_time(&clip, clip_id);
- `crates/manifold-playback/src/video_time.rs` (116 lines) holds `PrewarmCandidate` (lines 1-6, live: engine.rs:104, 3544, 3628) and two free functions. `compute_video_time_beats` (45-71, 27) is TEST-ONLY: its one caller is `test_beats_overload` (103-109), which tests only that wrapper. `compute_video_time` (8-43) has exactly one non-test caller outside this file, engine.rs:1957, and that call sits inside `sync_clip_loop_state`, the dead function of engine-runtime-001. The engine's live path uses its own method `PlaybackEngine::compute_video_time` (engine.rs:3082, called at 3174 and 3435), a separate implementation. So once engine-runtime-001 lands, both free functions and their six unit tests (`test_simple_playback` and the rest, lines 73-115) have no live caller.
- Removable: everything in video_time.rs except `PrewarmCandidate` and its doc, `~105` lines. The row depends on engine-runtime-001; `compute_video_time_beats` and its test alone are `~34` and can go first.
- Verdict: grade B (TEST-ONLY plus a cascade that depends on another row), action delete, risk dev.

### engine-runtime-026

`crates/manifold-media`: three uncalled functions and what only they use. `recv_results_blocking` (decode_scheduler.rs:182-196, 15), `is_prepared` (decoder.rs:247-250, 4), `is_supported_video_file` (metadata.rs:34-38, 5), the module-private `SUPPORTED_EXTENSIONS` const that only `is_supported_video_file` reads (metadata.rs:15, 1), the `VideoDecoder_IsPrepared` extern declaration (decoder_ffi.rs:54-55, 3) and its Objective-C implementation (MetalVideoDecoderPlugin.m:712-716 plus header comment line 21, 7). `~35`.
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F recv_results_blocking <repo>`: 3 hit(s); definition at crates/manifold-media/src/decode_scheduler.rs:184; no other code hit; mentions only in docs/MEDIA_EXPORT_MAP.md, BUG_BACKLOG_CLOSED.md (deleted).
- `rg -l -w -F recv_results_blocking /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F is_prepared <repo>`: 1 hit(s); definition at crates/manifold-media/src/decoder.rs:248; no other code hit.
- `rg -l -w -F is_prepared /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F is_supported_video_file <repo>`: 1 hit(s); definition at crates/manifold-media/src/metadata.rs:35; no other code hit.
- `rg -l -w -F is_supported_video_file /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n -w "SUPPORTED_EXTENSIONS|VideoDecoder_IsPrepared" "$REPO/crates/manifold-media" "$REPO/crates/manifold-core/src/video.rs" --glob "*.rs" --glob "*.m" --glob "*.h"`: 7 line(s): crates/manifold-core/src/video.rs:64:    pub const SUPPORTED_EXTENSIONS: &'static [&'static str] = &[".mp4", ".mov", ".webm", ".avi"]; | crates/manifold-media/src/decoder_ffi.rs:55:    pub fn VideoDecoder_IsPrepared(handle: *mut c_void) -> i32; | crates/manifold-media/src/metadata.rs:15:pub const SUPPORTED_EXTENSIONS: &[&str] = &[".mp4", ".mov", ".webm", ".avi"]; | crates/manifold-media/src/metadata.rs:37:    SUPPORTED_EXTENSIONS.iter().any( | ext |  lower.ends_with(ext)) | crates/manifold-media/src/decoder.rs:249:        unsafe { decoder_ffi::VideoDecoder_IsPrepared(self.handle) != 0 } | crates/manifold-media/native/MetalVideoDecoderPlugin.m:21://   VideoDecoder_IsPrepared(handle)     -> 1 if first frame decoded | crates/manifold-media/native/MetalVideoDecoderPlugin.m:712:int VideoDecoder_IsPrepared(void* handle)
- `rg -n -w recv_results_timeout "$REPO/crates/manifold-media/src/video_renderer.rs"`: 1 line(s): 909:            let results = self.scheduler.recv_results_timeout(remaining);
- `recv_results_blocking` is named in docs/MEDIA_EXPORT_MAP.md line 235 as what the export loop blocks on; the code has no caller (the neighbouring `recv_results_timeout` is the one used), so that doc line is stale and needs a fix with the deletion. `SUPPORTED_EXTENSIONS` in metadata.rs duplicates `manifold_core::video::SUPPORTED_EXTENSIONS` (core video.rs:64): the core copy is the live one (seen, not judged: 3.11). `decoder_ffi` is a private module, so once `is_prepared` goes the extern is flagged by rustc; the `.m` function must go with it or stays as dead native code.
- Verdict: grade A for the Rust items; the `.m` edit rides along. Action delete, risk prep (video import pipeline code; no show path calls these).

### engine-runtime-027

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F copy_frame_to_texture <repo>`: 2 hit(s); definition at crates/manifold-media/src/decoder.rs:125; no other code hit; mentions only in docs/MEDIA_BACKEND_DESIGN.md.
- `rg -l -w -F copy_frame_to_texture /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n -w "VideoDecoder_CopyFrameToTexture|FrameLease|copy_frame_to_texture" "$REPO/crates" --glob "*.rs"`: 5 line(s): crates/manifold-media/src/decoder_ffi.rs:33:    pub fn VideoDecoder_CopyFrameToTexture( | crates/manifold-media/src/video_renderer.rs:478:            decoder_ffi::VideoDecoder_CopyFrameToTexture(pool.raw_handle(), handle_ptr, dest_ptr) | crates/manifold-media/src/decode_scheduler.rs:68:/// `VideoDecoder_CopyFrameToTexture` with the destination Metal texture. | crates/manifold-media/src/decoder.rs:125:    pub unsafe fn copy_frame_to_texture( | crates/manifold-media/src/decoder.rs:131:            decoder_ffi::VideoDecoder_CopyFrameToTexture(
- `DecoderPool::copy_frame_to_texture` (decoder.rs:119-141, 23 lines) is an unsafe wrapper nothing calls; `video_renderer.rs:478` calls the FFI function directly. docs/MEDIA_BACKEND_DESIGN.md line 210 lists it as "unused wrapper ... deleted; its body becomes `FrameLease::deliver`'s VT arm". `FrameLease` does not exist yet (no Rust hit), so the design's P1 is unbuilt and the body would be re-derived from git.
- Verdict: grade B (the gap is the unbuilt design that reuses the body), action delete, risk none.

### engine-runtime-028

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F is_occluded <repo>`: 1 hit(s); definition at crates/manifold-compositor/src/compositor.rs:117; no other code hit.
- `rg -l -w -F is_occluded /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n -w occluded_layers "$REPO/crates/manifold-compositor/src" --glob "*.rs"`: 8 line(s): crates/manifold-compositor/src/compositor.rs:86:    pub occluded_layers: &'a [i32], | crates/manifold-compositor/src/compositor.rs:87:    /// Subset of `occluded_layers` whose generators AND effect chains are | crates/manifold-compositor/src/compositor.rs:118:        self.occluded_layers.contains(&layer_index) | crates/manifold-compositor/src/layer_compositor.rs:2413:    /// `occluded_layers` (from `CompositorFrame`) lists layers hidden by a | ...
- Seven lines (113-119 with doc). The field `occluded_layers` stays: `layer_compositor.rs:2427` and `:3039` read it directly.
- Verdict: grade A, action delete, risk none. `~7`.

### engine-runtime-029

- `rg -n -w "graph_snapshot_for|outer_routings_for" "$REPO/crates" --glob "*.rs"`: 4 line(s): crates/manifold-compositor/src/compositor.rs:309:    fn graph_snapshot_for( | crates/manifold-compositor/src/compositor.rs:321:    fn outer_routings_for( | crates/manifold-compositor/src/layer_compositor.rs:3760:    fn graph_snapshot_for( | crates/manifold-compositor/src/layer_compositor.rs:3768:    fn outer_routings_for(
- `BUG-axzlz` (Delete unused Compositor::graph_snapshot_for and outer_routings_for) already names this; fix shape kept: delete both trait methods (compositor.rs:305-314 (10) and 316-326 (11)) and both `LayerCompositor` impls (layer_compositor.rs:3760-3766 (7) and 3768-3776 (9)). The command above, re-run now on 37a9c180a, still shows only those four definitions and no call. The three mentions in docs (PRIMITIVE_LIBRARY_DESIGN.md, archive/PRESET_FORK_INVENTORY.md) are history. 37 lines.
- Cascade for the graph sweep: `loaded_preset_view_by_id`, `snapshot_for_view` and `outer_routings_from_view` in manifold-node-engine may lose their last caller.
- Verdict: grade A, action delete, risk none. Row exists only to carry the line count; the bead owns the work.

### engine-runtime-030

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F push_with_callback <repo>`: 2 hit(s); definition at crates/manifold-audio/src/analysis.rs:2025; other code hits: crates/manifold-audio/src/analysis.rs:3249.
- `rg -l -w -F push_with_callback /tmp/simplification_census/corpus/projects`: 0 file(s).
- The one caller is the test `push_callback_matches_enabled_scope_columns` (analysis.rs:3241-3257, 17 lines), which compares the callback's columns with the scope buffer's. `push_with_callback` (2021-2027, 7) is a three-line wrapper over `push_with_hops`, which is live and has its own tests (`queued_kick_fires_land_on_their_hop...`). The scope-column path keeps its other test (`scope buffers columns when enabled`). `~24`.
- Verdict: grade B (TEST-ONLY; the test also gives a second check of the scope buffer, which a reviewer may want to keep), action delete function and test, risk dev.

### engine-runtime-031

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F compare_sessions <repo>`: 4 hit(s); definition at crates/manifold-profiler/src/compare.rs:39; other code hits: crates/manifold-profiler/tests/profiler_smoke.rs:12 crates/manifold-profiler/tests/profiler_smoke.rs:21; mentions only in .claude/skills/profiler-guide/SKILL.md.
- `rg -l -w -F compare_sessions /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F SessionComparison <repo>`: 3 hit(s); definition at crates/manifold-profiler/src/compare.rs:10; other code hits: crates/manifold-profiler/src/compare.rs:39 crates/manifold-profiler/src/compare.rs:136.
- `rg -l -w -F SessionComparison /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F PassDelta <repo>`: 3 hit(s); definition at crates/manifold-profiler/src/compare.rs:30; other code hits: crates/manifold-profiler/src/compare.rs:22 crates/manifold-profiler/src/compare.rs:77.
- `rg -l -w -F PassDelta /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n "compare::|mod compare" "$REPO/crates" "$REPO/scripts" "$REPO/tools" --glob "*.rs" --glob "*.py" --glob "*.sh"`: 5 line(s): crates/manifold-profiler/src/lib.rs:13:pub mod compare; | crates/manifold-profiler/tests/profiler_smoke.rs:12:    let comparison = compare::compare_sessions(&paths[0], &paths[1]).unwrap(); | crates/manifold-profiler/tests/profiler_smoke.rs:21:    assert!(compare::compare_sessions(&paths[0], &paths[1]).is_err()); | crates/manifold-app/src/ui_snapshot/mod.rs:9:mod compare; | ...
- `crates/manifold-profiler/src/compare.rs` (165 lines): `compare_sessions` (38-147, 110), `SessionComparison` (8-26, 19), `PassDelta` (28-36, 9) and `load_summary`. Only caller: the test `comparisons_do_not_invent_percentages_for_zero_baselines` (tests/profiler_smoke.rs:4-23, 20 lines; it covers nothing else, the `frame` helper below it stays for the other tests). `.claude/skills/profiler-guide/SKILL.md:190` says "Not exposed as a CLI tool yet - would need a small bin": no bin was ever built. `~188` with the skill paragraph.
- Verdict: grade B (the gap: the skill documents it as the intended comparison tool; if Peter wants before/after comparison the right move is the small bin, not deletion), action delete, risk dev. Fallback if the bin is wanted: ask.

### engine-runtime-032

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F FlowPixel <repo>`: 1 hit(s); definition at crates/manifold-native/src/depth_estimator.rs:4; no other code hit.
- `rg -l -w -F FlowPixel /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F supports_parallel <repo>`: 1 hit(s); definition at crates/manifold-native/src/ffi/depth_ffi.rs:168; no other code hit.
- `rg -l -w -F supports_parallel /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n -w "FlowPixel|supports_parallel|DepthEstimator_SupportsParallel" "$REPO" --hidden -g "!.git/" -g "*.m" -g "*.mm" -g "*.swift" -g "*.h" -g "*.c" -g "*.cpp"`: no output.
- manifold-native loads its bundles with `libloading` (depth_ffi.rs:50 and siblings) and exports no `extern "C"` symbols, so there is no Objective-C or Swift caller by name; the third command (all native source types, whole repo) is empty. `FlowPixel` is a 6-line struct (lines 1-9 with its doc) that "matches DepthEstimatorPlugin.cpp"; no code builds it. `supports_parallel` (167-176, 10) is a probe over symbols the three `new_*_only` constructors resolve themselves. The `fn_create_*_only` fields stay: the constructors use them.
- Verdict: grade A, action delete, risk none. `~19`.

### engine-runtime-033

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F STRIPS_PER_SIDE <repo>`: 2 hit(s); definition at crates/manifold-led/src/types.rs:20; no other code hit; mentions only in docs/LED_STRIPS_DESIGN.md.
- `rg -l -w -F STRIPS_PER_SIDE /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F ARTNET_OP_POLL <repo>`: 1 hit(s); definition at crates/manifold-led/src/types.rs:22; no other code hit.
- `rg -l -w -F ARTNET_OP_POLL /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F ARTNET_PROTOCOL_VERSION <repo>`: 1 hit(s); definition at crates/manifold-led/src/types.rs:24; no other code hit.
- `rg -l -w -F ARTNET_PROTOCOL_VERSION /tmp/simplification_census/corpus/projects`: 0 file(s).
- Three unused constants, types.rs:19-24 (6 lines with docs). `STRIPS_PER_SIDE` is quoted in docs/LED_STRIPS_DESIGN.md line 30 (mention). The LED rig's generators are `ask` by census rule, but these are protocol and layout constants no code reads, not generators.
- Verdict: grade A, action delete, risk none. `~6`.

### engine-runtime-034

- `rg -n "\blog::|use log\b|extern crate log|\b(trace|debug|info|warn|error)!\(" "$REPO/crates/manifold-spectral" --glob "*.rs"`: no output.
- `rg -n "log" "$REPO/crates/manifold-spectral/Cargo.toml"`: 1 line(s): 17:log = { workspace = true }
- `rg -n "dep:log|\"log/" "$REPO/crates/manifold-spectral/Cargo.toml"`: no output.
- `fd -t f "build.rs" "$REPO/crates/manifold-spectral"`: no output.
- `cargo machete` (raw/machete.txt) flags `log` in manifold-spectral. The 3.4 searches are empty: no `log::` path, no `use log`, no logging macro anywhere in the crate (tests and benches included, since the glob is every `.rs` file), no feature entry names it, no `build.rs`. The only mention is the dependency line (Cargo.toml:17). Final proof is the deleting lane's `cargo check -p manifold-spectral --all-targets --all-features`.
- Other machete rows in this sweep's crates: none (the rest are nodes-water, whitewater, node-engine, nodes-scene, analyzer plugin, tv-led-mirror).
- Verdict: grade A, action delete (one line), risk none. `1`.

### engine-runtime-035

- `rg -n -w UnsupportedPlatform "$REPO/crates" --glob "*.rs"`: 2 line(s): crates/manifold-media/src/export_session.rs:44:    UnsupportedPlatform, | crates/manifold-media/src/export_session.rs:58:            Self::UnsupportedPlatform => write!(f, "native encoding not supported"),
- `git -C "$REPO" blame -L 43,43 --date=short crates/manifold-media/src/export_session.rs`: 1 line(s): d754eb08ed (Peter Kiemann 2026-03-24 43)     #[allow(dead_code)]
- `ExportError::UnsupportedPlatform` (export_session.rs:40-44, plus the Display arm at :58): never constructed. The suppression reads "Unconstructed on every platform today - MetalEncoder is the only encoder this crate has; a non-macOS encoder path is unbuilt ... Un-suppresses when that path returns this." Blame: 2026-03-24. Its named condition (a second encoder path) is the Vulkan work CLAUDE.md calls approved but not built, so it has not happened and may.
- Verdict: grade C, action ask (delete now and re-add with the Vulkan encoder, or keep). Risk none. `~8`.

### engine-runtime-036

Environment knobs in `crates/manifold-playback/src/percussion_backend.rs`, listed for the patterns sweep and not judged: `MANIFOLD_DEMUCS_MODEL` (326, 388), `MANIFOLD_DEMUCS_SHIFTS` (329, 390), `MANIFOLD_DEMUCS_OVERLAP` (334, 392), `MANIFOLD_DEMUCS_DEVICE` (338, 394), `MANIFOLD_DEMUCS_JOBS` (341, 396), `MANIFOLD_DEMUCS_SEGMENT` (345, 398), `MANIFOLD_DEMUCS_NO_SPLIT` (350, 403), `MANIFOLD_DEMUCS_CACHE_DIR` (484, 488), `MANIFOLD_DEMUCS_CACHE` (508-509). All go through the helpers `env_or` / `env_opt` / `env_bool` (percussion_backend.rs:457-481).
- `rg -n -o "MANIFOLD_DEMUCS_[A-Z_]+|MANIFOLD_PYTHON_PATH" "$REPO/crates/manifold-playback/src/percussion_backend.rs" | sort -u`: 21 line(s): 110:MANIFOLD_PYTHON_PATH | 113:MANIFOLD_PYTHON_PATH | 326:MANIFOLD_DEMUCS_MODEL | 329:MANIFOLD_DEMUCS_SHIFTS | 334:MANIFOLD_DEMUCS_OVERLAP | 338:MANIFOLD_DEMUCS_DEVICE | 341:MANIFOLD_DEMUCS_JOBS | 345:MANIFOLD_DEMUCS_SEGMENT | 350:MANIFOLD_DEMUCS_NO_SPLIT | 388:MANIFOLD_DEMUCS_MODEL | 390:MANIFOLD_DEMUCS_SHIFTS | 392:MANIFOLD_DEMUCS_OVERLAP | 394:MANIFOLD_DEMUCS_DEVICE | 396:MANIFOLD_DEMUCS_JOBS | ...
- `rg -l "MANIFOLD_DEMUCS" "$REPO" --hidden -g "!.git/"`: 2 line(s): tools/audio_analysis/README.md | crates/manifold-playback/src/percussion_backend.rs
- Seen: model, shifts, overlap, jobs and no_split overlap `settings.demucs.*` fields (percussion_backend.rs:324-350 reads the setting and lets the env var win), and `append_demucs_defaults` (384-405) repeats the same argument list with constants: a duplicate for the patterns sweep (3.11). Peter may set these in his launch command, which is not in the repo.
- Verdict: grade C, action ask (patterns sweep), lines 0.

### engine-runtime-037

Diagnostic env knobs read in this sweep's crates, listed for the patterns sweep: `MANIFOLD_LOG_REBUILD_REASON` (manifold-compositor layer_compositor.rs:1874; the same name is read in six more places: five in node-engine, one in manifold-gpu) and `MANIFOLD_ODF_DEBUG` (manifold-audio analysis.rs:1681).
- `rg -n -w "MANIFOLD_LOG_REBUILD_REASON|MANIFOLD_ODF_DEBUG" "$REPO" --hidden -g "!.git/" -g "!.beads/**"`: 14 line(s): docs/EFFECT_CHAIN_LIFECYCLE.md:69:For rebuild diagnostics, `MANIFOLD_LOG_REBUILD_REASON=1` logs topology and pool | docs/WARMUP_DESIGN.md:419:The Corrosion Music Video probe (rt-capture + cold-touch counter, `MANIFOLD_LOG_REBUILD_REASON=1`) proved P1–P4 insufficient on a real 3D/RT s | BUG_BACKLOG_CLOSED.md (deleted):2988:**Symptom:** on dense material (`self_render/edm_kit_128bpm`, 196 truth hits) the causal trigger path fires only ~21% of hits at t | docs/evidence/simplification_census_2026-10/graph.md:53: |  `MANIFOLD_LOG_REBUILD_REASON`  |  exec/execution.rs:1812, graph.rs:472, graph.rs:612, runtime/build.rs:535, runtim | tools/audio_analysis/eval/odf_attribution.py:3:Parses a MANIFOLD_ODF_DEBUG dump from `causal_events` and attributes every | crates/manifold-app/tests/renderer_contracts/gpu_proofs/generator_provider.rs:27:        .env("MANIFOLD_LOG_REBUILD_REASON", "1") | crates/manifold-compositor/src/layer_compositor.rs:1874:        if !stale.is_empty() && std::env::var("MANIFOLD_LOG_REBUILD_REASON").is_ok() { | crates/manifold-gpu/src/metal/device.rs:61:    *ENABLED.get_or_init( |  |  std::env::var_os("MANIFOLD_LOG_REBUILD_REASON").is_some()) | crates/manifold-audio/src/analysis.rs:1681:            if std::env::var_os("MANIFOLD_ODF_DEBUG").is_some() { | crates/manifold-node-engine/src/exec/execution.rs:1812:            && std::env::var("MANIFOLD_LOG_REBUILD_REASON").is_ok() | crates/manifold-node-engine/src/graph.rs:472:            if std::env::var("MANIFOLD_LOG_REBUILD_REASON").is_ok() { | crates/manifold-node-engine/src/graph.rs:612:                if std::env::var("MANIFOLD_LOG_REBUILD_REASON").is_ok() { | crates/manifold-node-engine/src/runtime/build.rs:535:        if std::env::var("MANIFOLD_LOG_REBUILD_REASON").is_ok() { | crates/manifold-node-engine/src/runtime/chain_dispatch.rs:372:        if std::env::var("MANIFOLD_LOG_REBUILD_REASON").is_ok() {
- `MANIFOLD_LOG_REBUILD_REASON` is documented (docs/EFFECT_CHAIN_LIFECYCLE.md:69) and set by a test (manifold-app generator_provider.rs:27); `MANIFOLD_ODF_DEBUG` is consumed by `tools/audio_analysis/eval/odf_attribution.py`. Both are live diagnostics today.
- Verdict: grade C, action ask (patterns sweep), lines 0.

### engine-runtime-038

Tool-path and plugin-override env reads, listed for the patterns sweep: `FFPROBE_PATH` (manifold-playback audio_sync.rs:113), `FFMPEG_PATH` (audio_sync.rs:121, percussion_backend.rs:155, manifold-media audio_muxer.rs:110), `MANIFOLD_PYTHON_PATH` (percussion_backend.rs:113), `MANIFOLD_{NAME}_PLUGIN` built with `format!` (manifold-native ffi/mod.rs:13-14, names DepthEstimator, BlobDetector), `HOME` (percussion_backend.rs:493, manifold-recording config.rs:29; standard, not a knob).
- `rg -n "env::var(_os)?\(" "$REPO/crates/manifold-playback/src" "$REPO/crates/manifold-media/src" "$REPO/crates/manifold-native/src" "$REPO/crates/manifold-recording/src" "$REPO/crates/manifold-audio/src" "$REPO/crates/manifold-compositor/src" "$REPO/crates/manifold-spectral/src" "$REPO/crates/manifold-profiler/src" "$REPO/crates/manifold-led/src" --glob "*.rs"`: 13 line(s): crates/manifold-native/src/ffi/mod.rs:14:    if let Ok(path) = std::env::var(&env_key) { | crates/manifold-compositor/src/layer_compositor.rs:1874:        if !stale.is_empty() && std::env::var("MANIFOLD_LOG_REBUILD_REASON").is_ok() { | crates/manifold-media/src/audio_muxer.rs:110:        if let Ok(p) = std::env::var("FFMPEG_PATH") { | crates/manifold-recording/src/config.rs:29:        let home = std::env::var("HOME").unwrap_or_default(); | crates/manifold-audio/src/analysis.rs:1681:            if std::env::var_os("MANIFOLD_ODF_DEBUG").is_some() { | crates/manifold-playback/src/percussion_backend.rs:113:        if let Ok(p) = std::env::var("MANIFOLD_PYTHON_PATH") | crates/manifold-playback/src/percussion_backend.rs:155:        if let Ok(p) = std::env::var("FFMPEG_PATH") | crates/manifold-playback/src/percussion_backend.rs:459:    std::env::var(key) | crates/manifold-playback/src/percussion_backend.rs:467:    std::env::var(key) | crates/manifold-playback/src/percussion_backend.rs:474:    match std::env::var(key) { | crates/manifold-playback/src/percussion_backend.rs:493:    let base = std::env::var("HOME") | crates/manifold-playback/src/audio_sync.rs:113:    if let Ok(env_path) = std::env::var("FFPROBE_PATH") | crates/manifold-playback/src/audio_sync.rs:121:    if let Ok(ffmpeg_env) = std::env::var("FFMPEG_PATH")
- That command is the full list of environment reads in the nine crates (build-time `env!("CARGO_MANIFEST_DIR")` reads are separate and not knobs). None is a candidate for removal on evidence available to a static sweep; ffmpeg / ffprobe / python paths are how Peter points the app at his tool installs.
- Verdict: grade C, action ask (patterns sweep), lines 0.

### engine-runtime-039

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F warp_interleaved <repo>`: 12 hit(s); definition at crates/manifold-playback/src/audio_warp.rs:44; other code hits: crates/manifold-playback/src/audio_warp.rs:110 crates/manifold-playback/src/audio_warp.rs:111 crates/manifold-playback/src/audio_warp.rs:112 crates/manifold-playback/src/audio_warp.rs:113 crates/manifold-playback/src/audio_warp.rs:114 crates/manifold-playback/src/audio_warp.rs:122; mentions only in .beads/issues.jsonl, AUDIO_LAYER_DESIGN.md (deleted).
- `rg -l -w -F warp_interleaved /tmp/simplification_census/corpus/projects`: 0 file(s).
- `bd show BUG-ycmj2`: 37 line(s): ○ BUG-ycmj2 · Integrate Signalsmith for live audio warp and matching export   [● P2 · OPEN] | Owner: Peter Kiemann · Type: task | Created: 2026-10-08 · Updated: 2026-10-09 |  | DESCRIPTION |  | ...
- `audio_warp.rs` (161 lines) with `build.rs`, `native/signalsmith_stretch.cpp` and the vendored Signalsmith headers. `warp_interleaved` has only its own unit tests as callers (7 uses inside audio_warp.rs). `BUG-ycmj2` (Integrate Signalsmith for live audio warp and matching export): "Deferred explicitly by Peter on 2026-10-09", and says to reuse this wrapper and the vendored library. `BUG-i9ozs (Audio clip tempo edits: waveform EOF remapping)` is the related audio clip tempo bead.
- Verdict: grade C, action ask (keep: Peter deferred it, not abandoned it). Listed so the census does not rediscover it. `~161` Rust lines, plus native and vendor files.

### engine-runtime-040

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F subscribe_keyed <repo>`: 9 hit(s); definition at crates/manifold-playback/src/osc_receiver.rs:373; other code hits: crates/manifold-playback/src/osc_sync.rs:200 crates/manifold-playback/src/osc_sync.rs:201 crates/manifold-playback/src/osc_receiver.rs:351 crates/manifold-playback/src/osc_receiver.rs:357; mentions only in docs/CORE_ENGINE_FINDINGS.md, docs/CORE_ENGINE_MAP.md.
- `rg -l -w -F subscribe_keyed /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F unsubscribe_keyed <repo>`: 6 hit(s); definition at crates/manifold-playback/src/osc_receiver.rs:383; other code hits: crates/manifold-playback/src/osc_sync.rs:201 crates/manifold-playback/src/osc_receiver.rs:351 crates/manifold-playback/src/osc_receiver.rs:357 crates/manifold-playback/src/osc_receiver.rs:372; mentions only in docs/CORE_ENGINE_FINDINGS.md.
- `rg -l -w -F unsubscribe_keyed /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n -w "subscribe_keyed|unsubscribe_keyed" "$REPO/crates" --glob "*.rs" | rg -v "^\S+:[0-9]+:\s*//"`: 7 line(s): crates/manifold-playback/src/osc_sync.rs:200:            // Plain `subscribe` (not `subscribe_keyed`) is used deliberately: | crates/manifold-playback/src/osc_sync.rs:201:            // subscribe_keyed's `unsubscribe_keyed` uses swap_remove, which | crates/manifold-playback/src/osc_receiver.rs:351:    /// should use subscribe_keyed / unsubscribe_keyed instead). | crates/manifold-playback/src/osc_receiver.rs:357:    /// key returned by subscribe_keyed() and use unsubscribe_keyed() for | ...
- Both methods are only named in comments (osc_receiver.rs:351, 357, 372; osc_sync.rs:200-201); the last command, which drops comment lines, finds only the two definitions. `OscSyncController::enable_osc` deliberately uses plain `subscribe` because `unsubscribe_keyed` uses `swap_remove`, which invalidates the other keys (osc_sync.rs:200-201; docs/CORE_ENGINE_MAP.md section 13 (Honest edges) item 13 calls the scheme broken by design). The pub screen counted the comment mentions. Spans 371-378 (8) and 380-389 (10). `~19`.
- Verdict: grade A, action delete, risk none.

### engine-runtime-041

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F request_clip_poster <repo>`: 3 hit(s); definition at crates/manifold-media/src/video_renderer.rs:197; other code hits: crates/manifold-media/src/video_renderer.rs:247; mentions only in docs/CLIP_THUMBNAILS_DESIGN.md.
- `rg -l -w -F request_clip_poster /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n -w request_clip_poster "$REPO" --hidden -g "!.git/" -g "!.beads/**" -g "!docs/archive/**"`: 3 line(s): docs/CLIP_THUMBNAILS_DESIGN.md:154:  decoded frame) via an ISOLATED async decode. `VideoRenderer::request_clip_poster` | crates/manifold-media/src/video_renderer.rs:197:    pub fn request_clip_poster(&mut self, clip_id: &str, video_clip_id: &str) -> bool { | crates/manifold-media/src/video_renderer.rs:247:    /// [`Self::request_clip_poster`] but walks the isolated decoder across
- `rg -n -w "request_clip_filmstrip|has_poster|poster_texture|evict_posters" "$REPO/crates" --glob "*.rs" | rg -v "video_renderer.rs"`: 4 line(s): crates/manifold-app/src/content_pipeline.rs:3110:                        if !vid_r.has_poster(cid_str) { | crates/manifold-app/src/content_pipeline.rs:3128:                            if vid_r.request_clip_filmstrip(cid_str, video_clip_id, &bar_times) { | crates/manifold-app/src/content_pipeline.rs:3151:                    vid_r.evict_posters(&self.clip_atlas_visible); | crates/manifold-app/src/content_pipeline.rs:3193:                                && let Some(t) = v.poster_texture(cid_str)
- `VideoRenderer::request_clip_poster` (video_renderer.rs:191-238, 48 lines with doc) submits a one-frame poster decode for a parked clip. Its only mentions are a doc comment on the filmstrip variant (video_renderer.rs:247) and docs/CLIP_THUMBNAILS_DESIGN.md line 154. The filmstrip path (`request_clip_filmstrip`) is the live one: content_pipeline calls it and the other poster accessors (third command). The pub screen read the doc-comment mention as a use.
- Cascade: the shared `poster_clips` map and `has_poster` stay (the filmstrip path uses them). The doc comment at :247 and the design doc line need a touch-up.
- Verdict: grade A, action delete, risk none (no caller). `~48`.

### engine-runtime-042

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F ExternalOutputType <repo>`: 5 hit(s); definition at crates/manifold-led/src/types.rs:30; other code hits: crates/manifold-led/src/types.rs:26 crates/manifold-led/src/types.rs:55 crates/manifold-led/src/types.rs:87; mentions only in docs/LED_STRIPS_DESIGN.md.
- `rg -l -w -F ExternalOutputType /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg -n -w "ExternalOutputType|output_type" "$REPO/crates" --glob "*.rs"`: 4 line(s): crates/manifold-led/src/types.rs:26:// ─── Enums (exact match: ExternalOutputType.cs) ─── | crates/manifold-led/src/types.rs:30:pub enum ExternalOutputType { | crates/manifold-led/src/types.rs:55:    pub output_type: ExternalOutputType, | crates/manifold-led/src/types.rs:87:            output_type: ExternalOutputType::ArtNet,
- `rg -n "deny_unknown_fields" "$REPO/crates/manifold-led/src"`: no output.
- `ExternalOutputType` (types.rs:28-33, 6 lines) has one variant, `ArtNet`, and one field, `LedSettings::output_type` (types.rs:55, `#[serde(default)]`, saved as `outputType`), whose only other use is the default constructor (types.rs:87). Nothing reads it. A saved project may carry the key (none of the readable corpus projects does), so deleting it needs the load to ignore the unknown key (serde ignores unknown fields unless `deny_unknown_fields` is set; the last command finds none in manifold-led). Census section 2 (Saved projects: a loading check, never a usage check) applies: this is a load-path point, not a usage point.
- Why this is not a plain delete: it is the placeholder for a second output type (sACN, other) on the LED rig. Peter's call.
- Verdict: grade C, action ask, risk prep (LED settings save/load). Category 3.9. `~9`.

## Module follow-through (coordinator trap: own impls count as uses, comments count as uses)

Four checks over all nine crates, in addition to the per-row checks above. Scratch scripts; nothing written to the repo.

1. Modules with no outside reference. For every `src/**/*.rs` module in the nine crates, search the whole `crates` tree for `stem::`, `::stem`, `use ... stem` and brace imports, dropping hits inside the module's own file and the `mod stem;` line. Zero outside references: `active_window.rs` (row engine-runtime-008) and `audio_warp.rs` (row engine-runtime-039), plus three that are not dead code: `audio/src/capture/process_tap.rs` and `unsupported_tap.rs` (platform-gated by `cfg`, reached through `capture/mod.rs`) and the `recording_soak` bin. Every other module has a real outside reference, including the ones with only one: fsr1 and pq_encoder (content_pipeline.rs:773, 758), percussion_planner and percussion_progress_parser (percussion_orchestrator.rs:48, 74), blit (artnet.rs:12), format_converter (session.rs:18), blob_detector (blob_ffi.rs:9), cdom and lowbank (measure/mod.rs:68-90).
2. Types used only by their own file. Every `struct`, `enum`, `trait`, `type` row of `raw/pub_refs/*.tsv` that is not USED, searched across the tree minus its own file (comment lines dropped): 24 in these crates. Dead in a cluster already recorded: `PlaybackNotifier` (003), `ActiveTimelineClipWindow` (008), `InsertCursorState` (009), `PercussionBpmDecision` (023), `FlowPixel` (032), `SessionComparison` and `PassDelta` (031). The rest are returned or stored by live functions in the same file and stay: `AbletonTrack`, `AbletonDevice`, `AbletonMacro` (built by the live session discovery, ableton_bridge.rs:1310-1501), `DecodedAudio`, `PreloadedAudioData`, `GestureState`, `ClipControlSourceView`, `TempoSource`, `ExportRestore`, `MidiImportResult`, `OscCallback`, `OscStampedCallback`, `PercussionImportResult`, `StatusColor`, `ProcessOutputLine`, `SyncResult`, `DisplayCaptureError`, `PreparedGeneratorResize`, `DisplayMappedFrame`, `AnalyzedHop`, `ProcessTapCapture`, `MuxError`, `DecoderError`, `VideoMetadata`, `ExportResult`, `ExportError`, `ProbeResult`, `ProbeReport`, the profiler summary structs, and `ExternalOutputType` (042).
3. Comment-excluded recount. All 440 CRATE-ONLY and USED rows of the nine crates re-counted with comment-only lines dropped (`//`, `///`, `//!`) and docs, beads and markdown excluded. Four rows drop to zero: `set_clock` (row 002), `subscribe_keyed` and `unsubscribe_keyed` (row 040), `request_clip_poster` (row 041). Rows left with one or two hits were read: each hit is a live caller, a test, or a caller already recorded as dead (`capture_project_bpm` and `capture_tempo_lane_snapshot` stay live through `append_tempo_change` and `end_session_if_active`; `clear_ownership` is inside `disable_sender`, row 011).
4. Corpus: every `0 file(s)` line above is a loading check only (census section 2): a hit would mean the deletion needs a load path. The one such case here is `oscSyncMode` in row 011, which keeps its saved value and enum.

## Rows reviewed and kept (not candidates)

Each TEST-ONLY row of the pub screen that is not a candidate, with the test callers and why it stays (census section 3.2 traps: stub-renderer accessors, test seams and inspection accessors are test infrastructure on purpose).

### kept: set_video_sync_interval

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F set_video_sync_interval <repo>`: 2 hit(s); definition at crates/manifold-playback/src/engine.rs:1123; other code hits: crates/manifold-playback/tests/sync_engine_integration.rs:82.
- `rg -l -w -F set_video_sync_interval /tmp/simplification_census/corpus/projects`: 0 file(s).
- One test caller (tests/sync_engine_integration.rs:82) shrinks the drift-sync cadence to 0.5 s for a fast test. The field is read by the live gate at engine.rs:1505. Test seam on live state: keep.

### kept: get_timeline_fallback_bpm

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F get_timeline_fallback_bpm <repo>`: 2 hit(s); definition at crates/manifold-playback/src/engine.rs:2728; other code hits: crates/manifold-playback/tests/engine_tick.rs:325.
- `rg -l -w -F get_timeline_fallback_bpm /tmp/simplification_census/corpus/projects`: 0 file(s).
- One assertion (tests/engine_tick.rs:325) reads the project BPM through it. Three lines; inlining it into the test would be equal. Not worth a row.

### kept: recently_started_time

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F recently_started_time <repo>`: 2 hit(s); definition at crates/manifold-playback/src/engine.rs:622; other code hits: crates/manifold-playback/tests/sync_engine_integration.rs:253.
- `rg -l -w -F recently_started_time /tmp/simplification_census/corpus/projects`: 0 file(s).
- One test (tests/sync_engine_integration.rs:253) asserts `start_clip` stamps the real engine clock into `recently_started_times`, a live gate for the compositor. Test of a real invariant: keep.

### kept: set_port

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F set_port <repo>`: 3 hit(s); definition at crates/manifold-playback/src/osc_receiver.rs:259; other code hits: crates/manifold-playback/tests/osc_timecode.rs:160 crates/manifold-playback/tests/osc_timecode.rs:230.
- `rg -l -w -F set_port /tmp/simplification_census/corpus/projects`: 0 file(s).
- tests/osc_timecode.rs:160 and :230 call `set_port(0)` to bind an ephemeral port. Test seam: keep.

### kept: start_count_for, stop_count_for, last_edge_flag_for

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F start_count_for <repo>`: 8 hit(s); definition at crates/manifold-playback/src/renderer.rs:134; other code hits: crates/manifold-playback/tests/frame_exact_transport.rs:715 crates/manifold-playback/tests/external_follow.rs:647 crates/manifold-playback/tests/engine_tick.rs:139 crates/manifold-playback/tests/engine_tick.rs:170.
- `rg -l -w -F start_count_for /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F stop_count_for <repo>`: 4 hit(s); definition at crates/manifold-playback/src/renderer.rs:142; other code hits: crates/manifold-playback/tests/engine_tick.rs:403 crates/manifold-playback/tests/engine_tick.rs:413 crates/manifold-playback/tests/external_follow.rs:647.
- `rg -l -w -F stop_count_for /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F last_edge_flag_for <repo>`: 3 hit(s); definition at crates/manifold-playback/src/renderer.rs:155; other code hits: crates/manifold-playback/tests/engine_tick.rs:144 crates/manifold-playback/tests/engine_tick.rs:172.
- `rg -l -w -F last_edge_flag_for /tmp/simplification_census/corpus/projects`: 0 file(s).
- Accessors of the stub renderer test double in renderer.rs, used by tests/engine_tick.rs, external_follow.rs and frame_exact_transport.rs (two to seven calls each). Test infrastructure: keep.

### kept: is_playing_layer, playing_count

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F is_playing_layer <repo>`: 15 hit(s); definition at crates/manifold-playback/src/session_state.rs:118; other code hits: crates/manifold-playback/src/session_state.rs:676 crates/manifold-playback/src/session_state.rs:688 crates/manifold-playback/src/session_state.rs:818 crates/manifold-playback/src/session_state.rs:857.
- `rg -l -w -F is_playing_layer /tmp/simplification_census/corpus/projects`: 0 file(s).
- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F playing_count <repo>`: 8 hit(s); definition at crates/manifold-playback/src/session_state.rs:126; other code hits: crates/manifold-playback/src/session_state.rs:836 crates/manifold-playback/src/session_state.rs:840 crates/manifold-playback/src/session_state.rs:910 crates/manifold-playback/tests/session_mode.rs:241.
- `rg -l -w -F playing_count /tmp/simplification_census/corpus/projects`: 0 file(s).
- Inspection accessors on the session runtime used by its own unit tests and by tests/session_mode.rs and tests/frame_exact_transport.rs. Keep.

### kept: drain_out

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F drain_out <repo>`: 33 hit(s); definition at crates/manifold-playback/src/transport_sync.rs:438; other code hits: crates/manifold-playback/tests/ableton_transport_sync.rs:256 crates/manifold-playback/tests/ableton_transport_sync.rs:296 crates/manifold-playback/tests/ableton_transport_sync.rs:494 crates/manifold-playback/tests/ableton_transport_sync.rs:533; mentions only in docs/ABLETON_TRANSPORT_SYNC_DESIGN.md.
- `rg -l -w -F drain_out /tmp/simplification_census/corpus/projects`: 0 file(s).
- The product loop uses `pop_out` (ableton_bridge.rs:2572); `drain_out` is a three-line convenience with 30-odd callers in transport_sync.rs's own tests and tests/ableton_transport_sync.rs. Keep.

### kept: i32_scalar

- `rg --hidden -g '!.git/' -g '!plugins/vendor/' -n -w -F i32_scalar <repo>`: 5 hit(s); definition at crates/manifold-audio/src/trained/container.rs:42; other code hits: crates/manifold-audio/src/trained/spec/tests.rs:92 crates/manifold-audio/src/trained/spec/tests.rs:93 crates/manifold-audio/src/trained/spec/tests.rs:169 crates/manifold-audio/src/trained/spec/tests.rs:189.
- `rg -l -w -F i32_scalar /tmp/simplification_census/corpus/projects`: 0 file(s).
- One of four constructors on `Array` (next to `f64s`, `i64s`, `text`); the four callers are in trained/spec/tests.rs. Test support in a data container: keep.

## Suppressions in the nine crates (step 2)

Found with `rg -n -B3 -A1 "allow\((dead_code|unused)|expect\((dead_code|unused)" crates/manifold-{playback,compositor,audio,spectral,media,recording,profiler,native,led}` over every `.rs` file. `raw/suppressions.txt` lists two of the three; the `cfg_attr` form below is not in it.

- `git -C "$REPO" blame -L 43,43 --date=short crates/manifold-media/src/export_session.rs`: 1 line(s): d754eb08ed (Peter Kiemann 2026-03-24 43)     #[allow(dead_code)]
- `git -C "$REPO" blame -L 60,60 --date=short crates/manifold-playback/src/midi_input.rs`: 1 line(s): 1901cb9c22 (Peter Kiemann 2026-09-09 60)     #[expect(dead_code, reason = "ownership-only field (Drop closes the port); un-suppress: never — remove only if MidiDevice s
- `git -C "$REPO" blame -L 13,13 --date=short crates/manifold-audio/src/directory/fallback.rs`: 1 line(s): 74eb12c0fa (Peter Kiemann 2026-06-17 13) #![cfg_attr(target_os = "macos", allow(dead_code))]
- `crates/manifold-media/src/export_session.rs:43` `#[allow(dead_code)]` on `ExportError::UnsupportedPlatform`: reason comment present, blame 2026-03-24, condition (a non-macOS encoder path) has not happened: candidate engine-runtime-035 (C).
- `crates/manifold-playback/src/midi_input.rs:60` `#[expect(dead_code, reason = "ownership-only field (Drop closes the port); un-suppress: never ...")]` on `MidiDevice::connection`: blame 2026-09-09. The field exists so the midir connection lives as long as the device; `expect` fails the build if the field ever becomes read, so it checks itself. Keep.
- `crates/manifold-audio/src/directory/fallback.rs:13` `#![cfg_attr(target_os = "macos", allow(dead_code))]`: blame 2026-06-17. The module is the cpal fallback used on every non-macOS platform ("until a native backend lands", file header lines 3-5), compiled and dead on macOS. Cross-platform is a product requirement (CLAUDE.md), so the condition has not passed. Keep.
- No other `allow(dead_code)`, `allow(unused...)` or `expect(dead_code)` in these crates. The `allow(clippy::too_many_arguments)` hits (playback, compositor, spectral) are not dead-code suppressions and are out of 3.3.

## Dependencies, features, flags (steps 3 and 4)

- `raw/machete.txt`: one row in these crates, `log` in manifold-spectral (engine-runtime-034). None for playback, compositor, audio, media, recording, profiler, native, led.
- `raw/features.tsv` rows with a count of 3 or less in these crates: only `manifold-playback project-lend` (count 3). It is live: declared at playback Cargo.toml:37, gates `PlaybackEngine::swap_project` (engine.rs:583), enabled by manifold-app's `ui-snapshot` feature (manifold-app Cargo.toml:101), and `swap_project` is called from manifold-app/src/ui_snapshot/script.rs:865 and :877. Keep. The other rows are above 3 and not listed: recording `recording-proofs` (15), compositor and spectral `gpu-proofs` (1033), compositor and playback `testkit` (1881), spectral `gpu` (4891) and `default`.
- `raw/const_bools.txt`: five entries, none in these crates (node-engine, foundation, ui).
- Environment variables: engine-runtime-036, 037 and 038. A one-valued bool that is not a const, `show_debug_logs`, is engine-runtime-024.
