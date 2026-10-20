# tooling: proof

Commands run against `/Users/peterkiemann/MANIFOLD - Rust` at main 37a9c180a. `R` is that path. `C` is `/tmp/simplification_census/tooling/commands_30d.txt`, built once with:

```
tr '\n' '\0' < /tmp/simplification_census/raw/transcripts_30d.txt | xargs -0 rg -o --no-filename '"command":"(?:[^"\\]|\\.)*"' > C
```

It holds 250,782 Bash command strings (44 MB) from 765 transcript files. The same session appears more than once (main transcript, subagent copies), so counts are mentions, not distinct runs; only zero versus non-zero is used as evidence. A direct run is a command where the name follows a command boundary:

```
rg -c "(\"command\":\"|&& |; |\| |\(|python3? (-I )?|bash |sh |nohup |time |\./|-- )[^ ]*scripts/NAME" C
```

`STD NAME` below is `rg --hidden -g '!.git/' -g '!plugins/vendor/' -g '!target/' -g '!*.jsonl' -n -F NAME R`, with the generated indexes (`docs/README.md`, `scripts/TOOLS.md`) and `docs/evidence/` hits counted apart (output in `/tmp/simplification_census/tooling/standard_rg.txt`).

## Shared evidence

### Scripts: runs in 30 days

- `rg -F` and the direct-run regex per script, all 64 non-test scripts, written to `script_runs.tsv` (name, mentions, direct runs, verb, verb runs). 32 of 64 have zero direct runs: audit_rename, blob_v2_native_bench, bridge_probe_gate, clay_region_probe, depth_relight_sweep, diff_scope, fix_bare_ids, gate_cancellation, gate_workspace, gen_glb_conformance_status, gltf_def_capture, kick_label_extract, launch_live_ui, live_ui, live_ui_generator_demo, live_ui_safety_demo, narrow_band_grid_reference, narrow_band_reference, pick_probe_regions, points_pixel_probe, repair_audio_fixture, rt_a2_term_cost, rt_a3_term_cost, rt_dynamic_acceptance, rt_quality_matrix, rt_region_probe, rt_toggle_matrix, sync_rig_report, claude-pane.sh, fetch-gltf-conformance.sh, install-abletonosc-patch.sh, uninstall-abletonosc-patch.sh. Three of the 32 (diff_scope, gate_cancellation, gate_workspace) are imported libraries.
- 9 test files have zero runs too: test_blob_detector, test_codex_regressions, test_live_ui, test_live_ui_safety, test_narrow_band_grid_reference, test_narrow_band_reference, test_repair_audio_fixture, test_rt_dynamic_acceptance, test_watch_land.
- Verb route: `rg -o '"command":"[^"]{0,300}dev\.py [a-z][a-z-]*'` over the 30-day transcripts (the brief's command, 10 hits): only `dev.py lab-review` (8, tools-lab) and `dev.py status` (2, both inside a `bd close` message, not a run). `rg -c -F "dev.py VERB" C` for each verb: 0 for every verb except `status` (6 mentions, all in text). Agents call the scripts directly; nobody runs verbs through `dev.py`.
- Imports between scripts: `rg -n "^\s*(import|from) (<all script stems>)\b" R/scripts` (listed in tooling.md, "Scripts that are libraries").

### Verbs

- `R/scripts/dev.py VERB --help` for 61 script-backed verbs (output in `verb_help.tsv`). 27 rc=0 (usage printed). 9 rc=1 with `PermissionError: [Errno 13] Permission denied` on the script: blob-bench, narrow-band-ref, narrow-band-grid-ref, live-ui-launch, live-ui, live-ui-generator-demo, live-ui-safety-demo, hook-census, codex-regressions. `ls -l R/scripts` confirms each of the 9 scripts is `-rw-r--r--` or `-rw-rw-r--`.
- 14 verbs were not given `--help` because their script has no argument parser and the arguments would be taken as data: land-wave, gpu-scope, flows, flows-batch-proof, docs-index (writes `docs/README.md`), glb-status (writes a doc), claude-pane (launches tmux), pick-probe-regions, depth-relight (writes `/tmp/depth-relight-sweep/`), status, stale-beads, fix-bare-ids, fetch-gltf (downloads), ableton-patch (edits Ableton files). Read-only ones were started with no arguments (`verb_noarg.tsv`): status prints the lane screen, stale-beads prints the stale list, gpu-scope prints a plan, land-wave prints its usage, pick-probe-regions prints "Usage:", fix-bare-ids prints "would fix" lines (dry run). All six start. All 14 scripts have the executable bit.
- 9 `cargo run` verbs not run (build). 11 `tools/audio_analysis/lab/*` verbs not run; `[ -x ]` on all 11 targets: all executable.
- `rg -o '^    "([A-Za-z0-9_.-]+\.(py|sh))":' R/scripts/dev.py` gives 75 `COST_CLASSES` keys; every key resolves to a file in `scripts/` or `tools/`. No stale cost entry.

### Hooks

- `jq '.hooks' R/.claude/settings.json` (saved in `settings_hooks.json`): 15 hook files registered across SessionStart, Stop, SessionEnd, UserPromptSubmit, PreToolUse (Bash, Edit|Write|MultiEdit, Agent), PostToolUse (wrap_up, Bash), PermissionRequest, PermissionDenied. `jq '.hooks // "none"' R/.claude/settings.local.json` returns "none". User-level hooks (`~/.claude/settings.json`) are two `afplay` pings and `token-guard.py`; out of scope.
- `jq -r '[.hook,.event] | @tsv' R/.claude/telemetry/hook-fires.jsonl | sort | uniq -c` (251,659 lines; first fire 2026-10-03T03:18:47, last 2026-10-19T09:39:02). Per-hook first and last fire time: `hook_first_last.tsv`. Per-hook count, non-zero exits, outputs, decisions: `jq -s 'group_by(.hook)[] | ...'` into `hook_effect.tsv`. Mean and max milliseconds: `jq -s 'group_by(.hook) | map({h, n, avg, max})'`.
- 13 hook names in the telemetry are not registered and have no file: `fd -H -t f "^NAME" R` is empty for each. `git log --diff-filter=D --name-only -- '.claude/hooks/*.py'` shows them deleted in 0626cffaf (2026-10-03, "Retire the friction hooks, the LSP skill, and the proxy-era tooling") and 8f7968da3 (2026-10-14, closes-trailer-guard). Their remaining mentions: `docs/GATE_RUNTIME_DESIGN.md` (history sentence), `docs/archive/*`.
- Unregistered files: `rg -F NAME` for cargo-check-on-stop.sh (0 hits), design_status_check.py (landing_gate.py, GIT_TREE_DISCIPLINE.md, test_gate_passes.py), design_status.py (landing-protocol nudge, DESIGN_DOC_STANDARD.md), index-conversations.py and search-conversations.py (conversation-recall-hook.py), local-time.py and session-end-worktree-scrub.py and test-summary-guard.py (settings.json). `jq -r '.. | strings' context-nudges/table.json | rg '\.md$'` lists all 7 snippet files. `rg -n 'minimal\.md' ~/.zshrc` shows the `claude-m` alias that uses `system-prompt/minimal.md`.

### Docs

- `R/.claude/hooks/design_status.py --lifecycle-check` printed `lifecycle: OK`, rc=0.
- `awk -F'\t' '$1==0 && $2<=2' raw/docs.tsv` gives 41 docs (`docs_zero_reads.tsv`). For each: status line (`rg -m1 -i '^\*\*(status|state)'`), line count, first and last commit date, inbound files (`docs_zero_detail.txt`), then references by stem across repo, `.rs` files, beads and the memory directory (`docs_stem_refs.tsv`), then the status of every bead that names the stem (`jq -r --arg s STEM 'select((tostring)|contains($s)) | "\(.id)=\(.status)"' R/.beads/issues.jsonl`).
- Triage of the 41 (kept = not a candidate):

| doc | lines | disposition | why |
|---|---|---|---|
| AUDIO_ENGINE_DIRECTION | 104 | tooling-021 | DIRECTION memo, post-release, 0 inbound |
| AUDIO_FLOOR_UNIFICATION | 173 | kept | a memory pointer cites it |
| AUTHORING_GRAPHS | 195 | kept | living manual for preset JSON; 0 inbound, listed in the generated index |
| CLIP_BOUNDARY_FRAME_DESIGN | 100 | kept | SHIPPED; one `.rs` file cites it |
| COMPETITIVE_STEAL_PASS | 120 | tooling-021 | CLOSED decisions record, memory pointer |
| HARNESS_SEAM_EXTRACTION_SPEC | 177 | tooling-019 | built |
| SHOW_PREPARATION_DESIGN | 163 | tooling-021 | PROPOSED, not approved |
| UI_PRESENT_CONTENT_GPU_CONTENTION | 102 | kept | "diagnosed, not fixed, leave it"; memory pointer |
| AUTOMATION_AUTHORING_AUDIT | 70 | kept | open bead BUG-i70 (VD-001 automation lanes L4 pass) |
| COMMERCIALIZATION_DESIGN | 81 | tooling-021 | approved, not built |
| COMPILE_CONTRACT_DESIGN | 303 | kept | SHIPPED contract, 17 stem references, 13 `.rs` files |
| CORROSION_MEMORY_AUDIT | 727 | kept | open bead BUG-dl16 (Corrosion V9 GPU stalls after warmup) |
| DRAG_SURFACES_SURVEY | 65 | tooling-022 | read-only audit, BUG_BACKLOG only |
| FOOTER_OVERPAINT_INVESTIGATION | 273 | kept | UNSOLVED; UI_CLIP_AND_Z_OWNERSHIP_DESIGN cites it |
| GPU_FLIP_NARROW_BAND_DESIGN | 212 | kept | IN PROGRESS |
| MAPPING_CARDS | 458 | tooling-021 | draft corpus, every card unjudged |
| MULTI_SELECTION_UX_PLAN | 112 | kept | phases shipped, still in the design-status dead-reference baseline |
| PROJECTION_MAPPING_DESIGN | 250 | tooling-021 | approved, not built |
| PRO_DJ_LINK_DESIGN | 145 | tooling-021 | approved, not built |
| RT_STAGE4_MOTION_DENOISE_DESIGN | 157 | tooling-021 | approved, not built |
| RT_TIER3_SCOPING | 191 | kept | open bead BUG-t1p5 (raytrace.rs godfile audit) |
| SCENE_ECHO_DYNAMICS_DESIGN | 99 | kept | IN PROGRESS |
| SCENE_MESH_ASSEMBLY_DESIGN | 153 | kept | IN PROGRESS |
| SCENE_PANEL_VS_INSPECTOR_CARDS | 63 | kept | cited by CINEMATIC_SCENE_TAIL_DESIGN |
| STRUCTURAL_AUDIT_VERDICTS | 177 | kept | 6 stem references including DESIGN_DOC_STANDARD |
| UI_RELIABILITY_AUDIT_2026_09_06 | 302 | kept | 7 beads name it, 4 not closed: BUG-xl2w (modifier card membership), BUG-b1qr (canvas mapping opener), BUG-wl6g (Scene Loop card audit), BUG-gwjo (input event path) |
| AUDIO_OBJECT_INGEST_DESIGN | 171 | kept | approved direction, 3 references, memory pointer |
| BLOB_TRACKING_V2_DESIGN | 294 | kept | IMPLEMENTED with an open bead (tooling-011) |
| BUSINESS_PLAN | 150 | tooling-021 | agreed direction, nothing built |
| CLIP_THUMBNAILS_DESIGN | 443 | kept | base system shipped, 2 live references |
| CORE_ENGINE_F5_SCOPING | 83 | tooling-022 | scoping input consumed by CORE_ENGINE_FINDINGS |
| DESIRED_STATE_RECONCILIATION_DESIGN | 363 | kept | SHIPPED, cited contract of CORE_ENGINE_MAP |
| DJ_PERFORMANCE_DESIGN | 141 | tooling-021 | approved, not built |
| GRAPH_FREEZE_COMPILER_DESIGN | 245 | kept | v1 landed; FREEZE_COMPILER_MAP is authority; one `.rs` file cites it |
| INDEXED_MESH_RENDERING_DESIGN | 129 | kept | CLOSED negative result ("do not revive"), 2 live references |
| MAPPING_GRAMMAR_DESIGN | 54 | tooling-021 | direction only |
| ML_NODES_DESIGN | 190 | tooling-021 | approved, not built |
| SCENE_DECIMATE_DESIGN | 425 | kept | PROPOSED with open bead BUG-mztz (revise Decimate architecture after scene-modifier RT) |
| SCENE_MATHEMATICAL_FIELDS_DESIGN | 129 | kept | IN PROGRESS |
| SCENE_SPLAT_MODIFIERS_DESIGN | 91 | tooling-021 | PROPOSED, not implemented |
| rt_dn_i_sweep_results | 80 | tooling-020 | measurement snapshot |

- Archive: for every `docs/archive/*` file, `rg -l --hidden ... -F STEM R` excluding `docs/archive/**`, `docs/landings/**`, `docs/evidence/**`, `.beads/**`, `docs/README.md` (live references; `archive_live_refs.tsv`) and a separate count of history references in archive and landings. 20 of 61 files have no live reference (`archive_no_live_refs.tsv`, 4,182 lines). 8 of the 20 are named in `.beads/issues.jsonl` or memory; bead statuses (`jq` as above): SCENE_MODIFIER_MATH_VIEW_DESIGN open, BOKEH_DOF_ROUND2_DESIGN closed, FFT_CAPACITANCE_PRESSURE_FINDINGS open, GPU_FLIP_TICK_SPLIT_DESIGN closed, PROVIDER_OPERATIONS closed, LIQUID_SURFACE_STAGE_DESIGN open, TRIGGER_LANES_DESIGN open (a child bead); PRESET_FORK_INVENTORY is not in beads but cited by `memory/project_graph_system.md`.

## Candidate sections

### tooling-001

Claim: `audit_rename.py (deleted)` (2,251 lines) is a one-shot rename tool whose table is spent.

- `STD audit_rename` -> 7 hits: 4 in its own docstring, 2 in `scripts/dev.py` (INTERNAL set line 201, COST_CLASSES line 213), 1 in `docs/PRIMITIVE_LIBRARY_DESIGN.md:820`, which says the script "is not the right tool" for project-file migration.
- `rg -c -F audit_rename C` -> 0 (no transcript command in 30 days, run or mention).
- `git log --format='%h %ad %s' -- audit_rename.py (deleted)` -> renames applied in 9 commits on 2026-05-17 (phases 1 to 7c); later commits only change paths and permissions.
- Script header: apply mode reports `[skip-applied]` for every done rename and then runs `cargo test ... -- --ignored` and the whole lib test set, which would also run against today's tree.
- Crates, hooks, tests, assets: no hit (the `STD` search covers them).

### tooling-002

Claim: `rt_a2_term_cost.py`, `rt_a3_term_cost.py` and their JSON outputs measure nothing.

- `rt_a2_term_cost.py` docstring: "RETIRED PROBE (RT_QUALITY_SETTINGS_DESIGN.md D8): MANIFOLD_RT_NATIVE_TERMS is deleted from the renderer ... The env assignment in run_capture() is inert." `rt_a3_term_cost.py` is the A3a split-dispatch variant of the same probe.
- `rg -n 'RT_NATIVE_TERMS' R/crates` -> 1 hit, a comment in `crates/manifold-nodes-scene/src/node_graph/primitives/render_scene.rs:3665` saying the variable "went out".
- `STD rt_a2_term_cost` -> 8 hits: dev.py (INTERNAL line 201, COST_CLASSES line 270), the script itself (3), `docs/RT_QUALITY_SETTINGS_DESIGN.md:123` (a negative-`rg` gate line), `docs/SIMPLIFICATION_CENSUS.md:344`, `crates/manifold-app/src/rt_capture.rs:426` (a comment). `STD rt_a3_term_cost` -> 6 hits of the same kinds. The JSON files are named only by their scripts.
- `rg -c -F rt_a2_term_cost C` and `rt_a3_term_cost` -> 0 each. The census doc line 344 records the same: mentioned in 38 sessions, run in none.
- `wc -l`: 257 + 282 (scripts), 134 + 474 (JSON).

### tooling-003

Claim: `.claude/hooks/cargo-check-on-stop.sh` (29 lines) is not wired to anything.

- `jq '.hooks' R/.claude/settings.json` -> no entry names it; `settings.local.json` has no hooks.
- `git -C R log -S'cargo-check-on-stop' -- .claude/settings.json` -> added 4253a6319 (2026-05-17), removed 07278f02a (2026-05-27, "config: remove cargo-check-on-stop hook").
- `STD cargo-check-on-stop` -> 0 hits. `rg -c -F cargo-check-on-stop C` -> 4 mentions, all this sweep's own earlier reads.
- Script body: `cd "/Users/peterkiemann/MANIFOLD - Rust"` then `cargo check` on every stop; it would also break the "no cd" and "no foreground build" rules if wired.

### tooling-004

Claim: `libstdin.rlib` is an accidental commit.

- `ls -la R/libstdin.rlib` -> 5.3 KB; `git log -- libstdin.rlib` -> one commit, 3b8344890 (2026-03-29, a core/playback unit-type refactor).
- `STD libstdin.rlib` -> 1 hit, `docs/SIMPLIFICATION_CENSUS.md:210`. Not in `.gitignore`.

### tooling-005

Claim: `.claude/orchestration/crate-split/` is replay data for a deleted tool.

- `git ls-files .claude/orchestration/crate-split | wc -l` -> 82; `xargs cat | wc -l` -> 4,353 lines. Dirs: p1, p1w, p2a, p2b, p2c, p3a, p3b, p3c (plans, rewrites, manifests, template crates).
- `ls R/crate_move_replay.py (deleted) R/scripts/move_identity_check.py` -> both "No such file". p1's README: "Reviewed input to crate_move_replay.py (deleted)".
- `git log -3 -- .claude/orchestration/crate-split` -> newest e4a5aa0b8 (2026-10-10) "Retire T2 move scaffolding: crate_move_replay, move_identity_check, test_census, t2 move plans".
- `rg -n 'plans|retired' docs/RENDERER_CRATE_SPLIT_DESIGN.md` -> status line 3 "SHIPPED ... Owed: none"; line 167 "crate_move_replay (retired; in git history)"; line 248 "The move plans live in git history (move plans retired)".
- `STD is_inert_plan_path` -> 8 hits: the definition (`scripts/gate_policy.py:6`), import and use in `gate_readiness.py` (15, 303), `gpu_scope.py` (42, 117, 465), `cpu_scope.py` (16, 137). That is 4 call sites. The one test is `test_crate_move_plans_are_inert_for_all_scope_planners` (`scripts/test_gate_readiness.py:364` to about 398), which also asserts the folder `crate-split-other` is not inert.
- `STD crate_move_replay` -> `.git-blame-ignore-revs` comments, `scripts/fixtures/gate-p1.json`, `docs/RENDERER_CRATE_SPLIT_DESIGN.md` (history). None reads the plan files.
- Side effect worth saying: every `rg` of the repo hits the 82 template files (crate names, Cargo.toml, lib.rs copies).
- Gap for the deleting lane: `scripts/fixtures/gate-p1.json` (156 KB) is a separate P1 fixture used by `test_gate_readiness.py`; it stays unless that test is reshaped.

### tooling-006

Claim: the clay and points probes were one-shot gates for a shipped design.

- `scripts/clay_region_probe.py` (80 lines) and `scripts/points_pixel_probe.py` (55 lines): docstrings name SCENE_RENDER_MODE_DESIGN P2 and P3 gates.
- `docs/SCENE_RENDER_MODE_DESIGN.md` status line 3: "SHIPPED ... Card rows + gates are the cited contract". `rg -n 'clay-probe|points-probe|clay_region_probe|points_pixel_probe' docs/SCENE_RENDER_MODE_DESIGN.md R/crates` -> 0 hits (the contract does not name them).
- `STD clay_region_probe` -> 4 hits (dev.py x2, the points probe's docstring, the generated index). `STD points_pixel_probe` -> 3 hits (dev.py x2, generated index). `rg -c` on C -> 0 each.
- Gap: the graphs the probes read (`graph-tool render` clay-probe and points-probe graphs) were not searched for; if a flow still uses them the probes could matter.

### tooling-007

Claim: `gltf_def_capture.py` served a completed refactor gate.

- Docstring: captures dumps "for the P3-D INV-R8 equivalence gate" of RENDERER_RUNTIME_DECOMPOSITION_DESIGN; status line 3 "COMPLETE — all phases landed 2026-07-22".
- `STD gltf_def_capture` -> 7 hits: the design (2), `docs/landings/2026-07-22-wave3-batch2.md:10` ("INV-R8 harness (permanent)"), dev.py x2, the script's own usage, the generated index. `rg -c` on C -> 0.
- The batch note calls it "permanent". Reuse case: any later importer refactor could run the same capture-first diff. Gap: nothing in a gate or test depends on it.
- `rg -n 'dump-def|dump_def' R/crates -g '*.rs'` -> `crates/manifold-app/src/bin/render_import.rs:398-424` (the bin mode this script drives). Not judged here.

### tooling-008

Claim: two verbs point at libraries.

- `dev.py narrow-band-ref --help` and `narrow-band-grid-ref --help` -> rc=1 `PermissionError`.
- `rg -c '__main__' scripts/narrow_band_reference.py scripts/narrow_band_grid_reference.py` -> no match in either file (rg prints counts only for files with a match, and printed none). The files end in function definitions.
- Importers: `rg -n 'narrow_band' R/scripts/test_narrow_band_*.py` -> both tests import them. `docs/GPU_FLIP_NARROW_BAND_DESIGN.md:166-167` calls them "the canonical CPU references". Rust test `gpu_flip_narrow_band_tests.rs:290` ports `narrow_band_grid_reference.Field.sample`.
- `scripts/test_dev.py` requires every script to be a verb or in INTERNAL (the dev.py docstring), so the fix is moving two names into INTERNAL.

### tooling-009

Claim: two verbs crash on a file mode, the scripts work.

- `dev.py hook-census --help`, `dev.py codex-regressions --help` -> rc=1 `PermissionError`.
- `rg -n 'hook_census' scripts/trunk_health.py` -> line 432 runs `["python3", "scripts/hook_census.py", "--days", "7"]` in the nightly job. `rg -n 'codex_regressions' scripts` -> imports in `gate_readiness.py:355`, `codex_checks.py:75,128`.
- The census doc line 207 already records the hook-census crash. The root cause is one: 13 non-test files in `scripts/` lack the executable bit; 4 are libraries (`gate_*`), the rest are in this list.

### tooling-010

Claim (ask): the live UI client scripts are unused by agents but may be Peter's.

- `wc -l`: 98 + 132 + 117 + 106 + 31 + 46 = 530. `rg -c` on C: launch_live_ui 4 mentions, 0 direct runs; the others 0.
- `STD live_ui` hits `docs/UI_AUTOMATION_DESIGN.md` (IN PROGRESS: P1 to P4 shipped, broader flows deferred), `docs/UI_RELIABILITY_AUDIT_2026_09_06.md`, `crates/manifold-app/src/live_ui.rs` (625 lines), `main.rs`, `app.rs`. The Rust server is live; these four scripts are its only clients.
- `dev.py live-ui-launch|live-ui|live-ui-generator-demo|live-ui-safety-demo --help` -> rc=1 `PermissionError` each.

### tooling-011

Claim (ask): `blob_v2_native_bench.py` has no runner but is named in app output.

- `rg -c` on C -> 2 mentions, 0 direct runs; `dev.py blob-bench --help` -> rc=1 `PermissionError`.
- `rg -n 'blob_v2_native_bench' R/crates` -> `crates/manifold-app/src/blob_v2_performance.rs:577` writes `"native_abi_reference": {"script": "scripts/blob_v2_native_bench.py", ...}` into its report.
- `docs/BLOB_TRACKING_V2_DESIGN.md` status: IMPLEMENTED, owed allocation measurement (BUG-7bi8 (measure Blob V2 transient allocation events), open).

### tooling-012

Claim (ask): the RT toggle and quality harnesses have no recent runs.

- `rg -c` on C -> rt_toggle_matrix 6 mentions, rt_quality_matrix 4, pick_probe_regions 0; direct runs 0 for all three.
- `rg -n 'rt-capture|"rt-capture"' R/crates/manifold-app/src/rt_capture.rs` -> the subcommand both harnesses drive exists (live-flip support at line 385).
- `scripts/rt_quality_matrix.py:421` calls `scripts/pick_probe_regions.py`. `rt_quality_baseline.json` (623 lines) is the baseline for the quality matrix.
- Designs: RT_QUALITY_SETTINGS_DESIGN status IN PROGRESS; UI_RELIABILITY_AUDIT line 265 cites `rt_toggle_matrix.py` cells.
- `rt_dynamic_acceptance.py` (1,238 lines) is not in this row: it is the runner for SCENE_MODIFIER_RT (IN PROGRESS) and is kept.

### tooling-013

Claim (ask): `land_wave.py` duplicates the landing ceremony.

- `rg -o` on C for any `land_wave.py` command boundary -> only `land_wave.py (deleted) --help` (2 mentions). No landing.
- Docs: `docs/AGENT_ROUTING.md:37` "a fallback for a pile-up of reviewed branches"; `.claude/GIT_TREE_DISCIPLINE.md:76` mentions `--batch`. CLAUDE.md names only `scripts/land_branch.py`.
- Header: written 2026-09-05 after a worktree-pinning bug; batch mode added 2026-10-01 (854f54888).
- `wc -l`: 215 + 126 (test).

### tooling-014

Claim (ask): `depth_relight_sweep.py` waits on Peter's depth relight call.

- `rg -c` on C -> 0. `STD depth_relight_sweep` -> dev.py x2 and `DEPTH_RELIGHT_DESIGN.md (deleted):60,67` (P6 acceptance command). The design status is APPROVED, P1 to P5b on main; the census doc line 356 lists depth relight "delete or turn back on" as owed to Peter.
- `wc -l` 148; with dev.py lines ~151.

### tooling-015

Claim (ask): the AbletonOSC patch scripts run against Peter's Ableton install.

- `rg -c` on C -> 0. References: `assets/abletonosc-patches/README.md`, each other, dev.py (`ableton-patch`). Both scripts default to a path under `$HOME/Library/CloudStorage/Dropbox/Music Production/Ableton/User Library/Remote Scripts/AbletonOSC/` (not opened).
- `wc -l` 102 + 24.

### tooling-016

Claim (ask): the recall hook runs on every prompt for 107 injections.

- `jq -s 'map(select(.hook=="conversation-recall-hook.py")) | [length, mean ms, max ms, mean out bytes of the non-zero ones]'` -> 5190 fires, mean 78 ms, max 7,453 ms, mean injection 6,487 bytes; `hook_effect.tsv`: 107 fires with output.
- Registered under UserPromptSubmit in settings.json together with response-style-hook.py. `wc -l`: 142 + 513 + 246 = 901.
- Digests: 1,345 files in `~/.claude/projects/-Users-peterkiemann-MANIFOLD---Rust/digests/`, 112 changed in 30 days (`fd -e md . DIR --changed-within 30d | wc -l`): the indexer is live.
- Memory: `reference_conversation_recall.md` documents it; `feedback_trust_peter_memory_search_transcripts.md` sends "we discussed this" questions to a raw-transcript grep.
- Not a CLAUDE.md hard-rule hook (the hard rules name other hooks).

### tooling-017

Claim: 15 archived docs have no live reference.

Files and lines: EFFECT_GENERATOR_CARD_UNIFICATION 111, BOKEH_DOF_ROUND2_DESIGN 113, INFRARED_BLACKOUT_HANDOFF_2026_06_12 136, AUDIT_NEXT_STEPS 152, GPU_FLIP_TICK_SPLIT_DESIGN 162, PROVIDER_OPERATIONS 187, FUSION_COVERAGE_AUDIT_2026_06_11 200, GRAPH_COMPILER_HARVEST_PLAN 261, PRIMITIVE_LIBRARY_COMPLETION_PLAN 261, PARAM_STORAGE_REDESIGN_BRIEF 278, FLEET_STACK_HANDOVER 375, MESH_UV_AND_RENDERER_TEXTURE_PLAN 393, AGENT_ROUTING_PROVIDER_LANES 40, PRESET_FORK_EXECUTION 79, AUDIO_SPECTROGRAM_AND_ANALYSIS_PLAN 99. Sum 2,847.

- Live-reference search per stem: `rg -l --hidden -g '!.git/' -g '!target/' -g '!*.jsonl' -g '!docs/README.md' -g '!docs/archive/**' -g '!docs/landings/**' -g '!docs/evidence/**' -g '!.beads/**' -F STEM R` -> empty for all 15.
- Code-side check: `rg -l -F STEM R/crates R/scripts R/tests R/tools R/assets R/CLAUDE.md` -> empty for all 15.
- Memory, commands and beads: `rg -l -F STEM <memory dir> ~/.claude/commands R/.beads/issues.jsonl` -> only BOKEH_DOF_ROUND2_DESIGN (closed bead BUG-4myu (BOKEH-ROUND2 tent dilation)), GPU_FLIP_TICK_SPLIT_DESIGN (closed bead BUG-q2s2i (even frames on GPU FLIP)), PROVIDER_OPERATIONS (closed bead BUG-bo5l (glm-4.7 fallback classifier)). Doc mentions inside the archive only: EFFECT_GENERATOR_CARD_UNIFICATION (PRESET_UNIFICATION_PLAN), FUSION_COVERAGE_AUDIT (GRAPH_COMPILER_HARVEST_PLAN), PARAM_STORAGE_REDESIGN_BRIEF (PARAM_STORAGE_DESIGN), PROVIDER_OPERATIONS (FLEET_STACK_HANDOVER, AGENT_ROUTING_PROVIDER_LANES).
- `README.md (deleted)`: "Kept for historical record — not active reference."
- Follow-up for the deleting lane: `scripts/gen_docs_index.py` regeneration; check `crates/manifold-core/tests/fixtures/docs_cited_paths_baseline.txt` and `.claude/hooks/design_status_dead_refs_baseline.txt` for these names.

### tooling-018

Claim (ask): four archived docs have open beads.

- `jq -r --arg s STEM 'select((tostring)|contains($s)) | "\(.id)=\(.status)"' R/.beads/issues.jsonl`: SCENE_MODIFIER_MATH_VIEW_DESIGN -> BUG-fgfk (Extend scene modifier Math View with parameter sweeps and equation graphs) open; FFT_CAPACITANCE_PRESSURE_FINDINGS -> BUG-wsim (Research: FFT pressure split for particle liquids) open; LIQUID_SURFACE_STAGE_DESIGN -> BUG-3zkk3 (Mesh relaxation on packed positions inside smooth_surface_mesh) open and BUG-twnl (Replace the Liquid Surface group with one node across the four water presets) open; TRIGGER_LANES_DESIGN -> BUG-tqtel (Trigger lanes: unify control timing and delivery) closed and BUG-hkbdp (Test suite review and reduction after the crate split) open, child 1.
- Same live-reference searches as tooling-017: empty. Lines 105 + 115 + 244 + 552 = 1,016.

### tooling-019

Claim: `HARNESS_SEAM_EXTRACTION_SPEC.md (deleted)` is a build sheet for built work.

- Spec: "BUILD SPEC ... folds the main-window immediate-pass assembly into shared code ... Deletes draw_immediate_passes".
- `rg -n -w 'render_main_ui_passes|draw_immediate_passes' R/crates` -> `ui_snapshot/script.rs:3143` calls `crate::ui_frame::render_main_ui_passes(...)`; `draw_immediate_passes` appears only in comments (`script.rs:3092`). The seam exists and the old function is gone.
- `STD HARNESS_SEAM_EXTRACTION_SPEC` -> 1 hit (generated index). `docs_stem_refs.tsv`: stem refs 0, `.rs` 0, beads 0, memory 0. Reads in 30 days: 0.

### tooling-020

Claim: `docs/rt_dn_i_sweep_results.md` is a measurement snapshot.

- Header: "Measured 2026-08-08. Tree: lane/rt-operating-sweep @ 8a331ef7". 80 lines, no status line, lower-case name.
- `STD rt_dn_i_sweep_results` -> 3 hits: the generated index and 2 in `docs/evidence/` (other sweeps). Beads: BUG-iadf (RT denoise 1:1 mode: +34ms unexplained vs +2ms upscaled), closed. Gap: the numbers may be the only record of the sweep; the bead may not carry them.

### tooling-021

Claim (ask): thirteen docs describe features nothing builds.

- Status lines (`docs_zero_detail.txt`): AUDIO_ENGINE_DIRECTION "DIRECTION ... post-release"; SHOW_PREPARATION_DESIGN "PROPOSED ... not built or approved"; COMMERCIALIZATION_DESIGN, PROJECTION_MAPPING_DESIGN, PRO_DJ_LINK_DESIGN, DJ_PERFORMANCE_DESIGN, RT_STAGE4_MOTION_DENOISE_DESIGN "APPROVED design, not built"; BUSINESS_PLAN "agreed direction"; MAPPING_CARDS "DRAFT CORPUS ... every card draft-unjudged"; MAPPING_GRAMMAR_DESIGN "DIRECTION captured ... not built"; ML_NODES_DESIGN "APPROVED"; SCENE_SPLAT_MODIFIERS_DESIGN "PROPOSED ... not implemented"; COMPETITIVE_STEAL_PASS "CLOSED (decisions pinned)".
- All 0 reads in 30 days. Inbound references (`docs_stem_refs.tsv`): 0 to 5, mostly among the group (DJ_PERFORMANCE_DESIGN, PRO_DJ_LINK_DESIGN, AUDIO_ENGINE_DIRECTION cite each other; COMMERCIALIZATION_DESIGN and BUSINESS_PLAN cite each other). Memory pointers: MAPPING_CARDS, MAPPING_GRAMMAR_DESIGN, ML_NODES_DESIGN, PRO_DJ_LINK_DESIGN and COMPETITIVE_STEAL_PASS have one each.
- Lines 104 + 163 + 81 + 150 + 250 + 145 + 141 + 157 + 458 + 54 + 190 + 91 + 120 = 2,104.

### tooling-022

- CORE_ENGINE_F5_SCOPING: "scoping input for a Fable design pass. Not a design doc"; inbound `docs/CORE_ENGINE_FINDINGS.md` and `docs/landings/2026-07-10-core-engine-findings.md`. DRAG_SURFACES_SURVEY: "Read-only audit of every drag gesture"; inbound `docs/BUG_BACKLOG.md`. Beads: none for either. Lines 83 + 65 = 148.

### tooling-023

Claim (ask): the remaining archive.

- From `archive_live_refs.tsv` and the counts above: 62 files, 20,667 lines; minus tooling-017 (2,847), tooling-018 (1,016), README (5) leaves 16,799 in 42 files (PRESET_FORK_INVENTORY, 319 lines, has a memory pointer and no repo reference; the other 41 each have at least one live reference; the most cited, PRESET_LIBRARY_DESIGN, has 41 referencing files).
- Archive README promises restore with `git mv`. Git history holds all of it.

### tooling-024

- `.claude/orchestration/crate-split-seams.md` 2,557 lines: `docs/RENDERER_CRATE_SPLIT_DESIGN.md:136,190` cite it ("site detail"; P0 deliverable). Design status SHIPPED, "kept as the crate layering contract".
- `.claude/orchestration/parked.md` 76 lines: referenced only by `memory/project_godfile_decomposition_campaign.md`. `decisions.md` (140 lines): `crates/manifold-node-engine/src/exec/temporal_reset.rs:4` and `docs/AGENT_ROUTING.md:104` cite it, so it stays.

### tooling-025

- `git ls-files tv-led-mirror | wc -l` 17; `wc -l` total 8,432 (Cargo.lock 3,509; hue.rs 1,674; main.rs 883; gui.rs 864). Last commit 2026-05-13; 17 commits between 2026-05-09 and 2026-05-13.
- `rg -n 'tv-led|members' R/Cargo.toml` -> not a workspace member. `rg -l -i 'tv-led-mirror|TVLEDMirror' R/docs R/CLAUDE.md memory` -> only `docs/VULKAN_BACKEND_DESIGN.md` and the census files. Nothing in scripts, hooks or crates calls it.

### tooling-026

- `plugins/manifold-analyzer-cli/src/main.rs` 106 lines, added 2026-04-20; `rg -n 'analyzer-cli' R` -> `plugins/Cargo.toml:7` (member list) and itself only.

### tooling-027

- `MANIFOLD.app/Contents/MacOS/launch.sh` (3 lines: `cd` to the repo, `exec ./target/release/manifold`) and `Info.plist` (20). `rg -n 'MANIFOLD\.app' R` -> one hit in an archive doc (unrelated). Added 2026-04-13, last 2026-06-17.

### tooling-028

- `_typos.toml` 56 lines (2026-05-17). `which typos` -> /opt/homebrew/bin/typos. `rg -n -i '\btypos\b' R` outside the file -> two code comments about a different meaning of the word. Nothing runs it.

### tooling-029

- `opencode.json` 83 lines, `git status --short opencode.json` -> `??` (untracked), mtime 2026-10-12. `which opencode` -> /opt/homebrew/bin/opencode. Only archive docs mention opencode (PROVIDER_OPERATIONS, FLEET_STACK_HANDOVER).

### tooling-030

- `rust-analyzer.toml` 21 lines (2026-07-21). CLAUDE.md Tooling: "No rust-analyzer in agent sessions". `lsp-nudge.py` and the LSP skill were deleted 2026-10-03.

### tooling-031

- `scripts/blender/fbx2glb.py` 34, `make_hostile_rig.py` 74. `rg -l -F NAME R/scripts R/crates R/docs` plus the hooks folder -> `docs/IMPORT_ANYTHING_WAVE_DESIGN.md`, `docs/BUG_BACKLOG.md`. Not in `dev.py`.

## Commands that were denied or failed

- `cd /dev/null` (first call): hook denied; retried with absolute paths.
- One loop using `awk` and the path `.claude/hooks`: denied by the config-write guard; reran with `cut` and no `.claude` path.
- `timeout 20 dev.py ...` loop: `timeout: command not found` (macOS); reran without it.
- A `rg -r` rewrite on the transcript counts: the harness warned that `-r` rewrites output; the counts were re-taken with `sort | uniq -c` on the raw lines.
- Two evidence-file writes were refused by the bare-ID guard until each bead ID carried its name; rewritten.
