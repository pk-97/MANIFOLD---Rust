# tooling sweep

- **Sweep:** tooling
- **Model:** Sonnet 5.5, medium effort
- **Main commit:** 37a9c180a (`raw/main_commit.txt`)
- **Scope:** `scripts/` (not `scripts/ui-flows/`), `scripts/dev.py` verbs, `.claude/hooks/`, `docs/` top level and `the deleted archive folder`, the repo root, `plugins/` except `plugins/vendor/`, `tv-led-mirror/`. Also `.claude/orchestration/` (tracked, found while following `gate_policy.py`).
- **Raw files used:** `raw/scripts.tsv`, `raw/docs.tsv`, `raw/doc_reads_30d.txt`, `raw/transcripts_30d.txt` (765 transcript files, 2.0 GB), `raw/main_commit.txt`.
- **Extra outputs** (all in `/tmp/simplification_census/tooling/`): `commands_30d.txt` (every Bash command from the 30-day transcripts, 250,782 lines, matched with escaped quotes handled, which the screen's `[^"]{0,300}` pattern misses), `script_runs.tsv` (per script: mentions, direct runs, verb runs), `verb_help.tsv`, `verb_noarg.tsv`, `hook_fires_by_hook_event.txt`, `hook_first_last.tsv`, `hook_effect.tsv`, `docs_zero_detail.txt`, `docs_stem_refs.tsv`, `archive_live_refs.tsv`, `archive_no_live_refs.tsv`, `standard_rg.txt`, `proof_log.txt`.
- **Denied commands:** two, both skipped. A reflex `cd /dev/null` on the first call (retried with absolute paths). One shell loop that used awk and also named `.claude/hooks` (denied by the config-write guard; rewritten with `cut` and no `.claude` path in the command). One non-denial hiccup: macOS has no `timeout`, so the first `--help` loop exited 127 for every verb; it was rerun without `timeout`.
- **Line counting rule:** the file's lines, its own tests and data files, and the `scripts/dev.py` / `scripts/TOOLS.md` lines that name it. `~` means the dev.py share is estimated. Binary files count as files.
- **Telemetry window:** `.claude/telemetry/hook-fires.jsonl` starts 2026-10-03, so hook counts cover 16 days, not 30.
- **Not covered:** see the end of this file.

Candidates: 31 (A 9, B 3, C 19). Lines removable by grade: A about 10,860 (plus one binary file), B about 340, C about 36,360 (16,800 of it is the whole archive folder, 8,430 is `tv-led-mirror/` with its lockfile).

Grades: A every search empty, B code searches empty with one named gap, C needs Peter. Every row has its section in `tooling.proof.md`. No row here is a hook that enforces a CLAUDE.md hard rule; those are listed in the hooks table below and are never delete candidates.

## Candidates

| id | location | category | proof | risk | grade | action | lines |
|---|---|---|---|---|---|---|---|
| tooling-001 | `audit_rename.py (deleted)` | 3.12 | one-shot rename table whose renames are all applied (phases 1 to 7c, 2026-05-17); 0 transcript commands in 30 days; repo-wide 7 hits: its own docstring, 2 dev.py lines, 1 doc line saying it "is not the right tool" | dev | A | delete script, its INTERNAL and COST_CLASSES entries in dev.py | ~2255 |
| tooling-002 | `rt_a2_term_cost.py (deleted)`, `rt_a3_term_cost.py`, `rt_a2_term_cost.json`, `rt_a3_term_cost.json` | 3.12 | the env var they set (`MANIFOLD_RT_NATIVE_TERMS`) is deleted from the renderer, so the probes measure nothing (script docstring: RETIRED PROBE); 0 runs in 30 days; refs are dev.py plus one comment in `crates/manifold-app/src/rt_capture.rs:426` | dev | A | delete; edit the one rt_capture.rs comment | ~1150 |
| tooling-003 | `.claude/hooks/cargo-check-on-stop.sh` | 3.12 | no registration names it (removed from settings in 07278f02a 2026-05-27); repo-wide search for the name is empty; hard-coded absolute path | dev | A | delete | 29 |
| tooling-004 | `libstdin.rlib` (repo root) | 3.12 | tracked rustc output from a stdin compile (committed in 3b8344890 2026-03-29); 0 references except the census doc | none | A | delete | 1 file |
| tooling-005 | `.claude/orchestration/crate-split/` (82 files, 8 plan dirs) and `is_inert_plan_path` in `scripts/gate_policy.py` with its 4 call sites and 1 test | 3.12 | replay data for `crate_move_replay.py`, which no longer exists; `RENDERER_CRATE_SPLIT_DESIGN.md` is SHIPPED and says the move plans are retired and live in git history; the only reader is the gate helper that treats the folder as inert | dev | A | delete the folder, then delete `is_inert_plan_path`, its calls in gate_readiness, gpu_scope, cpu_scope, and `test_crate_move_plans_are_inert_for_all_scope_planners` | ~4400 |
| tooling-006 | `scripts/clay_region_probe.py`, `scripts/points_pixel_probe.py`, verbs `clay-probe`, `points-probe` | 3.12 | one-shot gates for SCENE_RENDER_MODE P2/P3, which is SHIPPED; 0 transcript commands; refs are dev.py and each other; the design doc does not name them | dev | B | delete both and their 2 verbs | ~141 |
| tooling-007 | `scripts/gltf_def_capture.py`, verb `gltf-def-capture` | 3.12 | harness for the INV-R8 equivalence gate of RENDERER_RUNTIME_DECOMPOSITION_DESIGN, which is COMPLETE; 0 transcript commands; refs are that design and one landing note. Gap: the matching `render-import --dump-def` mode in `crates/manifold-app/src/bin/render_import.rs` is Rust and belongs to the app-ui sweep | dev | B | delete the script and verb; ask the app-ui sweep about the bin mode | ~119 |
| tooling-008 | `scripts/dev.py` verbs `narrow-band-ref`, `narrow-band-grid-ref` | 3.12 | both targets are import-only libraries (no `__main__`) without an executable bit, so the verbs crash (`PermissionError`) and could run nothing; the modules are used by their tests and cited by GPU_FLIP_NARROW_BAND_DESIGN | dev | A | drop the two verbs, add the two modules to INTERNAL | ~4 |
| tooling-009 | `scripts/hook_census.py`, `scripts/codex_regressions.py` (verbs `hook-census`, `codex-regressions`) | 3.12 | verbs crash with `PermissionError`: the scripts lack the executable bit. They work when called with `python3`: `trunk_health.py:432` runs hook_census nightly, `gate_readiness.py` and `codex_checks.py` import codex_regressions | dev | A | `chmod +x` both (0 lines); if nobody wants the codex-regressions CLI, drop the verb instead | 0 |
| tooling-010 | `scripts/launch_live_ui.py`, `live_ui.py`, `live_ui_generator_demo.py`, `live_ui_safety_demo.py`, `test_live_ui.py`, `test_live_ui_safety.py`, 4 verbs | 3.12 | Peter may run these by hand: client for the opt-in live UI socket (`crates/manifold-app/src/live_ui.rs`, 625 lines); UI_AUTOMATION_DESIGN is IN PROGRESS; 4 verbs crash (no executable bit); 0 transcript runs in 30 days | stage | C | ask: fix the exec bits and keep, or retire the client with the design's deferred phases | 530 |
| tooling-011 | `scripts/blob_v2_native_bench.py`, verb `blob-bench` | 3.12 | measures the BlobDetector V2 C ABI; 0 transcript runs; verb crashes (no executable bit); named as `native_abi_reference` in the JSON that `crates/manifold-app/src/blob_v2_performance.rs:577` writes; BLOB_TRACKING_V2_DESIGN owes allocation measurement (BUG-7bi8 (measure Blob V2 allocation events), open) | dev | C | ask | ~565 |
| tooling-012 | `scripts/rt_toggle_matrix.py`, `scripts/rt_quality_matrix.py`, `scripts/rt_quality_baseline.json`, `scripts/pick_probe_regions.py` and their 3 verbs | 3.12 | RT harnesses with 0 runs in 30 days, still valid (`rt-capture` exists in manifold-app); RT_QUALITY_SETTINGS_DESIGN is IN PROGRESS and names the quality oracle; UI_RELIABILITY_AUDIT cites the toggle matrix (4 of its beads are open or in progress); pick_probe_regions is only called from rt_quality_matrix | dev | C | ask: keep as the RT oracle or retire after the RT quality work lands | 2211 |
| tooling-013 | `land_wave.py (deleted)`, `test_land_wave.py (deleted)`, verb `land-wave` | 3.12 | second landing ceremony next to `land_branch.py`; AGENT_ROUTING and GIT_TREE_DISCIPLINE call it a fallback for a pile-up; the only 30-day invocations are `--help`; no real landing | dev | C | ask: merge `--batch` into `land_branch.py` or delete | 341 |
| tooling-014 | `depth_relight_sweep.py (deleted)`, verb `depth-relight` | 3.12 | 0 transcript commands; DEPTH_RELIGHT_DESIGN names it for P6 acceptance; the census already carries Peter's call on depth relight (delete or turn back on) | prep | C | ask with the depth relight decision | ~151 |
| tooling-015 | `scripts/install-abletonosc-patch.sh`, `scripts/uninstall-abletonosc-patch.sh`, verb `ableton-patch` | 3.12 | edits Peter's installed AbletonOSC script; 0 transcript runs; only `assets/abletonosc-patches/README.md` and dev.py name it | prep | C | ask: Peter runs this by hand | ~128 |
| tooling-016 | `.claude/hooks/conversation-recall-hook.py`, `index-conversations.py`, `search-conversations.py` | 3.12 | registered on every prompt: 5,190 fires in 16 days, mean 78 ms, max 7.4 s, but only 107 injections (first prompt of a session); the memory rule for "we discussed this" is a raw-transcript grep, not the digests; 1,345 digests, 112 refreshed in 30 days. Not a CLAUDE.md hard-rule hook | dev | C | ask: keep the recall injection or retire hook, indexer and searcher | 901 |
| tooling-017 | `the deleted archive folder` 15 files with no live reference | 3.12 | each stem searched repo-wide, memory dir, beads, `.claude/commands`: no hit in crates, scripts, tests, tools, assets, hooks, live docs; 6 are mentioned only in closed beads or other archive docs, 9 nowhere; archive README says "kept for historical record" | dev | A | delete the 15 files, regenerate `docs/README.md` | 2847 |
| tooling-018 | `the deleted archive folder` 4 files named by open beads: `SCENE_MODIFIER_MATH_VIEW_DESIGN.md` (BUG-fgfk (scene modifier Math View sweeps)), `FFT_CAPACITANCE_PRESSURE_FINDINGS.md` (BUG-wsim (FFT pressure split research)), `LIQUID_SURFACE_STAGE_DESIGN.md` (BUG-3zkk3 (mesh relaxation on packed positions) and BUG-twnl (one Liquid Surface node)), `TRIGGER_LANES_DESIGN.md` (BUG-hkbdp (test suite review after the crate split), child 1) | 3.12 | no live doc or code reference, but an open bead names each | dev | C | ask: delete when the beads close, or fold the bead notes first | 1016 |
| tooling-019 | `HARNESS_SEAM_EXTRACTION_SPEC.md (deleted)` | 3.12 | BUILD SPEC for `render_main_ui_passes`, which is built (`crates/manifold-app/src/ui_frame.rs`; `draw_immediate_passes` survives only in comments); 0 reads in 30 days; repo-wide search for the name: 1 hit (the generated index); no bead, no memory pointer | dev | A | delete | 177 |
| tooling-020 | `docs/rt_dn_i_sweep_results.md` | 3.12 | a dated measurement snapshot (2026-08-08), lower-case name outside the doc naming scheme; 0 reads; named only by closed bead BUG-iadf (RT denoise 1:1 mode cost) and the generated index | dev | B | delete (the bead keeps the summary) or move under `docs/notes/` | 80 |
| tooling-021 | 13 docs for features never built: `AUDIO_ENGINE_DIRECTION`, `SHOW_PREPARATION_DESIGN`, `COMMERCIALIZATION_DESIGN`, `BUSINESS_PLAN`, `PROJECTION_MAPPING_DESIGN`, `PRO_DJ_LINK_DESIGN`, `DJ_PERFORMANCE_DESIGN`, `RT_STAGE4_MOTION_DENOISE_DESIGN`, `MAPPING_CARDS`, `MAPPING_GRAMMAR_DESIGN`, `ML_NODES_DESIGN`, `SCENE_SPLAT_MODIFIERS_DESIGN`, `COMPETITIVE_STEAL_PASS` | 3.12 | status lines say approved, proposed, direction or draft, nothing built; 0 reads in 30 days; at most 5 inbound references each, mostly from each other | dev | C | ask: which of these are still on the roadmap; the rest go | 2104 |
| tooling-022 | `docs/CORE_ENGINE_F5_SCOPING.md`, `docs/DRAG_SURFACES_SURVEY.md` | 3.12 | a scoping input "not a design doc" and a read-only drag audit; 0 reads; inbound only from CORE_ENGINE_FINDINGS, a landing note and BUG_BACKLOG; no open bead | dev | C | ask | 148 |
| tooling-023 | the other 42 files in `the deleted archive folder` | 3.12 | each has at least one live reference (code comment, live doc or memory pointer); the folder is 62 files, 20,667 lines of history git already holds; deleting means editing those references | dev | C | ask: policy call, keep the archive or let git hold it | 16799 |
| tooling-024 | `.claude/orchestration/crate-split-seams.md`, `.claude/orchestration/parked.md` | 3.12 | seams findings file (2,557 lines) is cited twice by RENDERER_CRATE_SPLIT_DESIGN as "site detail"; parked.md (76 lines) holds god-file Wave 1 parked items, pointed to only by a memory file. `decisions.md` stays: `temporal_reset.rs` and AGENT_ROUTING cite it | dev | C | ask | 2633 |
| tooling-025 | `tv-led-mirror/` (17 files) | 3.12 | standalone app (screen capture to ArtNet and Hue LEDs), not a workspace member, last commit 2026-05-13, 0 references from the rest of the repo, 4,923 lines plus a 3,509-line lockfile. Peter may run it by hand | none | C | ask | 8432 |
| tooling-026 | `plugins/manifold-analyzer-cli/` | 3.12 | headless WAV to CSV spectrum CLI from 2026-04-20; only the plugins workspace member list names it; the DSP crate it calls stays | dev | C | ask | ~110 |
| tooling-027 | `MANIFOLD.app/` (Info.plist, launch.sh) | 3.12 | double-click launcher for `target/release/manifold`; 0 references anywhere; Peter may use it | none | C | ask | 23 |
| tooling-028 | `_typos.toml` | 3.12 | config for the `typos` tool (installed at /opt/homebrew/bin/typos); nothing in scripts, hooks, gate or docs runs it | none | C | ask: Peter may run typos by hand | 56 |
| tooling-029 | `opencode.json` (untracked, 2026-10-12) | 3.12 | config for the opencode CLI (installed); not tracked, no reference; the 2026-10-14 rule decisions say no other providers | none | C | ask | 83 |
| tooling-030 | `rust-analyzer.toml` | 3.12 | memory caps for rust-analyzer; CLAUDE.md says no rust-analyzer in agent sessions and the LSP skill is retired; Peter's editor (`.vscode/`) may still use it | none | C | ask | 21 |
| tooling-031 | `scripts/blender/` (`fbx2glb.py`, `make_hostile_rig.py`) | 3.12 | Blender-side helpers named only by IMPORT_ANYTHING_WAVE_DESIGN and BUG_BACKLOG; no verb, no gate; run inside Blender by hand | prep | C | ask | 108 |

## Verbs that crash

`scripts/dev.py VERB --help` run for every script-backed verb. Of 70 verbs: 27 print usage, 9 crash with `PermissionError` (the script has no executable bit), 14 have no argument parser (6 were started with no arguments and run; 8 not run because running them writes files, runs flows or launches things), 11 point at `tools/audio_analysis/lab/` (executable; left to the tools-lab sweep) and 9 are `cargo run` verbs (not run: they build).

| verb | script | rc | cause | row |
|---|---|---|---|---|
| `hook-census` | `scripts/hook_census.py` | 1 | not executable | tooling-009 |
| `codex-regressions` | `scripts/codex_regressions.py` | 1 | not executable | tooling-009 |
| `narrow-band-ref` | `scripts/narrow_band_reference.py` | 1 | not executable, and the module has no `__main__` | tooling-008 |
| `narrow-band-grid-ref` | `scripts/narrow_band_grid_reference.py` | 1 | same | tooling-008 |
| `blob-bench` | `scripts/blob_v2_native_bench.py` | 1 | not executable | tooling-011 |
| `live-ui-launch` | `scripts/launch_live_ui.py` | 1 | not executable | tooling-010 |
| `live-ui` | `scripts/live_ui.py` | 1 | not executable | tooling-010 |
| `live-ui-generator-demo` | `scripts/live_ui_generator_demo.py` | 1 | not executable | tooling-010 |
| `live-ui-safety-demo` | `scripts/live_ui_safety_demo.py` | 1 | not executable | tooling-010 |

## Hooks

Fifteen hook files are registered in `.claude/settings.json`, all through `hook_telemetry.py`. Every one fired in the window. No registered hook has zero fires. 13 other hook names in the telemetry (closes-trailer-guard, worker-seat-charge, context-ceiling-guard, delegation-nudge, probe-loop-guard, spinning-guard, lsp-nudge, cc-fleet-tier-guard, landing-report-guard, dead-code-suppression-hook, subagent-stop-gate, lane-health-guard, agent-tier-spawn-guard) have no file and no registration left: they were deleted on 2026-10-03 and 2026-10-14, and their fires stop on those days. Only `docs/GATE_RUNTIME_DESIGN.md` and archive docs still mention them, as history.

| hook | fires (16 days) | with effect | note |
|---|---|---|---|
| `preToolUseBash.py` | 54,431 | 429 deny, 68 ask, 3 context | hard-rule hook (no `cd`, commit pathspec, land guard); never a delete candidate |
| `context-nudge.py` | 62,293 | 365 context | path-triggered invariants named in CLAUDE.md; never a delete candidate |
| `test-summary-guard.py` | 53,163 | 303 context | |
| `worktree-guard.py` | 7,866 | 5 deny | enforces the one-writer and slot-ring rules; never a delete candidate |
| `memory-history-guard.py` | 7,866 | 11 deny | enforces CLAUDE.md "Memory is rules, not history"; never a delete candidate |
| `bare-id-guard.py` | 7,866 | 48 deny | enforces CLAUDE.md "IDs carry names"; never a delete candidate |
| `ignored-test-guard.py` | 7,866 | 0 | enforces CLAUDE.md "No `#[ignore]` on tests"; rarely fires because nobody adds ignored tests, which is the point; never a delete candidate |
| `agent-worktree-isolation-guard.py` | 523 | 0 | enforces CLAUDE.md "Agent worktrees come from the slot ring only"; never a delete candidate |
| `agent-launch-guard.py` | 523 | 30 deny | |
| `response-style-hook.py` | 5,185 | 5,185 injections (15-line file) | CLAUDE.md Voice rule |
| `conversation-recall-hook.py` | 5,190 | 107 injections | tooling-016 |
| `local-time.py` | 272 | 272 injections | session start clock |
| `session-end-worktree-scrub.py` | 89 | 0 (silent by design, mean 201 ms) | trims idle slot caches at session end; disk hygiene, not a hard rule |
| `wrap-up-stop-guard.py` | 326 | 20 block | enforces the always-end-with-the-summary-box rule |
| `permission-request-guard.py` | 151 | 6 deny | PERMISSION_BOUNDARY |

Unregistered files in `.claude/hooks/`: `cargo-check-on-stop.sh` (tooling-003); `design_status.py` and `design_status_check.py` (called by the landing gate and CLAUDE.md, keep); `hook_telemetry.py` (the wrapper); `index-conversations.py` and `search-conversations.py` (called by the recall hook, tooling-016); baseline text files read by the guards; `system-prompt/minimal.md` (used by Peter's `claude-m` alias in `~/.zshrc`); `context-nudges/` (all 7 snippets are named in `table.json`). Every `test_*.py` there tests a live hook.

## Scripts that are libraries other scripts import

`gate_cancellation`, `gate_policy`, `gate_readiness`, `gate_workspace`, `gate_passes`, `cpu_scope`, `gpu_scope`, `diff_scope`, `codex_checks`, `codex_regressions`, `storage_budget`, `gpu_queue`, `run_ui_flows`, `landing_gate`, `gpu_proofs_gate`, `gen_docs_index`, `watch_land` and `landing_metrics` (both imported by `status.py`), `live_ui` (imported by the two live UI demos), `narrow_band_reference` (imported by `narrow_band_grid_reference` and the tests). `native_source_identity.rs` is a build-time include of `manifold-core/build.rs` and `manifold-physics/build.rs`. None of these is a candidate on its own.

## Checked and kept

- `scripts/claude-pane.sh`: no repo reference, but the user command `~/.claude/commands/claude-pane.md` runs it.
- `scripts/fix_bare_ids.py`: 0 runs, but a dry run today lists pending fixes in live docs, so the tool still has work.
- `scripts/kick_label_extract.py`, `repair_audio_fixture.py`: fixture provenance, documented in `tests/fixtures/audio_labels/README.md`; `sync_rig_report.py`: built 2026-10-14 for the rig log in manifold-playback.
- `scripts/mgpcg_reference.py`, `lentine_reference.py`: called from Rust GPU FLIP tests (`gpu_flip_lentine.rs`); `rt_region_probe.py`: cited by `rt_t38_multibounce.rs`.
- `scripts/bridge_probe_gate.py`, `feature_matrix.py`, `rt_noise_gate.py`, `stale_beads.py`, `fleet_health.py`: run by the nightly `trunk_health.py` or the landing gate.
- `scripts/rt_dynamic_acceptance.py` (1,238 lines) and its test: runner for the in-progress SCENE_MODIFIER_RT campaign.
- `scripts/gate_runner.py` and its selftest: the verdict trail `preToolUseBash.py` checks before a merge.
- `scripts/fixtures/gate-p1.json` (156 KB): fixture for `test_gate_readiness.py`.
- `.claude/orchestration/decisions.md`: cited by `crates/manifold-node-engine/src/exec/temporal_reset.rs` and AGENT_ROUTING.
- The 41 low-read docs: 24 kept (shipped contract, in progress, open bead, unsolved, cited by live docs or code) and 17 sit in rows 019 to 022; per-doc reasons are in the proof file.

## Seen, not judged

- `docs/evidence/` is 15 MB in 107 files (mostly screenshots); `docs/landings/` is 126 files, 9,049 lines of history that CLAUDE.md says takes no new files.
- Untracked or ignored disk leftovers in the repo root: `chain-stats.log` (1.3 MB, May), `diag.log`, `fluid-probe-out/`, `.hpss_scratch/`, `.codex/`, `.pytest_cache/`. They cost disk only.
- `.claude/scripts/token_report.py` is a gitignored 284-line copy of `scripts/token_report.py` (274 lines); the tracked one is the live one.
- `crates/manifold-app/src/bin/render_import.rs` `--dump-def` mode (about 40 lines, plus a `Serialize` derive on `ImportReport`) only serves `gltf_def_capture.py`; for the app-ui sweep.
- `~/.zshrc` alias `k3m` still starts the retired `cc-fleet` stack; outside the repo.
- `scripts/test_blob_detector.py` (216 lines, not executable, no verb) tests the legacy V1 BlobDetector C ABI; BLOB_TRACKING_V2_DESIGN says V1 is preserved.

## Not covered

- `tools/` (tools-lab sweep) and the 11 `lab-*` and `detector-release` verbs: not run.
- The 9 `cargo run` verbs: `--help` would build, which the standing rules forbid, so they are not checked.
- `scripts/ui-flows/`, `plugins/vendor/`, `.claude/skills/`, `.beads/`, the memory directory beyond pointer searches, and every Rust crate (the Rust halves of the rows above go to the code sweeps).
- Top-level docs with reads or at least 3 inbound references (about 200 of 252): only `design_status.py --lifecycle-check` covers them, and it passes.
- Telemetry before 2026-10-03 and transcripts older than the 30-day window.
- The 8 verbs whose `--help` would write or launch (`docs-index`, `glb-status`, `flows`, `flows-batch-proof`, `depth-relight`, `claude-pane`, `fetch-gltf`, `ableton-patch`) were not started; their executable bits are set.
