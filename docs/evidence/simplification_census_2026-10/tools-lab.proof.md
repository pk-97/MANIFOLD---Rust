# tools-lab proof

Every command behind a row in `tools-lab.md`, with a one-line result. `R` is the repo root, `SP` is the session scratchpad, `T` is `tools/audio_analysis`. Commands marked (rerun) were repeated on 2026-10-19 after the first pass; the rest come from the first pass and use the scripts in the appendices. Helper script sources are appendices A to D; appendix E is the per-file inventory the table was built from. Nothing here ran a Python tool, a build, a test or the GPU.

Beads named once here (bare below): BUG-yn979 (Simplification pass); BUG-9ngk8 (Trained audio detectors) with BUG-9ngk8.1 (Kick detector), BUG-9ngk8.2 (Snare + clap detector) and BUG-9ngk8.2.1 (Snare detector: fast breakbeats and rolls); BUG-9ixtf (Kick goal follow-ups: percussive residual, pitch-settling trajectory, loose-truth training); BUG-qy8q7 (Trained kick detector misses drop kicks after a high-passed build-up roll); BUG-1v1z9 (Kick model: try one net over the spectrum plus the 69 measurements); BUG-ymy4z (Bass and synth detector families: labels and precision before any model); BUG-d8l (Grow classifier training data (1-2 more shows)); BUG-dcoov (Variant matcher re-judge: trust audit and landing of feat/detector-catalog-fix); BUG-8dy68 (Detector data: CC0/CC BY music and samples for kick and snare training); BUG-woji (MTL4FX availability probe checks protocol name as class — MTL4 path never activates); BUG-7rngq (Midnight kick labels with no kick-stem attack: kicks or not?); BUG-qy2 (shipping-license-audit); BUG-dlr (beat-this-frame-hop-exceeds-5ms-alignment-target); BUG-t32 (beat-this-short-clip-bpm-convergence); BUG-9ra (harmonix-matched-audio-carries-multi-second-timing-offset); BUG-1gm (madmom-onset-detector-not-gain-or-timestretch-invariant-on-real-mixes); BUG-j8v (analyzer-false-fires-on-sustained-pads).

## Method, shared by all rows

1. Inventory (appendix A, `inventory.sh`): `git ls-files 'tools/*.py'` gives 389 tracked Python files under `tools/`. Per file: `wc -l`; last commit date from one `git log --name-only` pass (`lastcommit.tsv`); importers by `rg -l --glob '*.py' -e '^\s*(import|from)\b.*\bSTEM\b' tools scripts`; `rg -c -F /BASE scripts/dev.py`; command counts from the 30-day transcripts (`all_commands_u.txt`, 16 MB of de-duplicated shell commands from every `~/.claude/projects/*MANIFOLD---Rust*` jsonl, parent and subagent). Result: `inventory.tsv`, 389 rows, 10 fields each.
2. Strict import graph (appendix B, `graph.sh`): a regex per file for qualified imports (`from tools.audio_analysis.eval.x import`, `import ...`), `from eval import a, STEM`, relative `from .stem import` and bare `from stem import`. Result: `edges.tsv`, 1,344 edges once the dynamic ones below are added (`edges2.tsv`).
3. Dynamic edges: `rg -n 'import_module\(' tools --glob '*.py'` (list L5). Each computed name was expanded by hand into the files it can reach and added to `edges2.tsv`.
4. Closure (appendix C): awk BFS from the roots `lab/*.py`, `catalog/*.py` and `kick_container.py` over `edges2.tsv`. Result: 127 files; 79 in `eval/` (37 imported directly by lab or catalog, 42 only through other eval files). Everything else is outside the closure and gets a tag from `tag.sh` (appendix D): date and name cluster.
5. Why the closure is the right boundary: `lab/run.py` `module_hashes()` hashes the bytes of each loaded `tools/` module into the run id, and `imports()` preloads the closure; so a file outside the closure cannot change what a lab run does or what its id is.

### tools-lab-001

Claim: 93 files, 10,780 lines, nothing live reaches them.

- `wc -l` over list L1 -> 10,780 (rerun); 69 modules/runners/scripts (8,990) plus 24 tests (1,790).
- `rg -l -w -e '(<93 stems>)' "$R" --glob '!target/**' --glob '!docs/evidence/**' --glob '!**/__pycache__/**' --glob '!.git/**'` (rerun) -> 80 files, all inside the batch or `eval/scoreboard/`, plus one: `tests/fixtures/audio_labels/evening_validation_passages_2026-10-09.json`.
- That fixture hit: `rg -n review_kick_evening_passages` on it -> line 23, a `"path"` provenance field naming a script in an old worktree; the fixture's only reader is `kick_night_extra_passages.py` (line 22), which is in the batch. Data; stays.
- Same search with `--hidden` over `.claude scripts crates docs` (rerun) -> 0 files. Over `.beads/issues.jsonl` -> 1 hit, `kick_night_label_source_audit`, in the closed bead BUG-7rngq.
- Scoreboard records: `k9_outside_mentions.txt` -> 19 file-to-record pairs in 12 `eval/scoreboard/*.json` records, all dated 2026-10-09. Results, not code; stay.
- Importers from outside the batch: `awk` over `edges2.tsv` for edges whose target is in L1 and whose source is not -> 0. Edges inside the batch -> 79.
- Computed-name imports that could reach the batch (L5): `{family}_labels` (7 sites) resolves to `bass_labels`, `kick_labels`, `snare_labels`, `synth_labels` (`jq -r '.family'` over `lab/experiments/*.json` and `lab/plans/*.json` gives only bass, kick, snare, synth; rerun), none in L1; `kick_stable_{method}` -> `kick_stable_rms/quadrature`, not in L1; `kick_{variant}_experiment` -> `kick_tonal_experiment`, `kick_pcen_experiment`, not in L1; `eval.kick_dsp_experiments` -> not in L1.
- `rg -c -F "/$base" scripts/dev.py` for each of the 93 -> 0 each.
- `rg -l -w -e '(<stems>)' tools/audio_analysis/lab` -> 0 (no plan, config, test or module of the lab names one).
- `git log --format=%ad --date=short -- <file>` for each -> 2026-10-09 for every file (24 commits). Last transcript command naming one -> 2026-10-11.
- Tests: `test_targets.tsv` lists each dated-2026-10-09 test with the modules it imports, split live or dead. 9 tests import only batch modules; 15 more import at least one batch module (some also import a module that only 002 frees) -> all 24 go with the batch. 29 further tests import only live-or-bridge modules; they are not in this row (20 in 002, 9 stay).
- Docs: AUDIO_EVAL_HARNESS_GUIDE.md section 0 (Trained detectors — what holds) files the H-series under git history already; `rg -l -w -e '(<stems>)' docs` (rerun) -> 0.

Grade A: all searches above empty or limited to the named data files.

### tools-lab-002

Claim: cutting the sibling imports of 12 bridge modules frees 24 modules and 20 tests.

- Bridge modules: L2 list 1 (12 files). They are the modules the label chain (`kick_goal_eval`, `kick_goal_labels`, `detector_*`) imports for a few helpers; `rg -n '^\s*(from|import)' <chain file>` shows each edge.
- `closure_hubcut.txt`: the closure walk repeated with the 12 bridge modules' own outgoing edges removed -> 103 files instead of 127; the 24 that drop out are `freed_hub.txt` (L2 list 2). `freed_tests2.txt` (L2 freed tests) is the 20 tests whose only targets are in those 24.
- Lines (rerun): `wc -l` over the 24 modules -> 2,862; over the 20 tests -> 1,427; sum 4,289.
- What stays with the chain after the cut: L2 lists 3 and 4 (18 modules, 9 tests).
- Why B: the cut means editing files the run id hashes (`kick_goal_eval`, `kick_goal_labels`, the bridge modules) and `code_id()` hashes function source per file path, so any function that moves changes its cache key. Named gap: I did not diff which `code_id` closures include which bridge function. The guide forbids edits to `kick_fusion_features.py` and `kick_fusion_bandwise.py`; neither is a bridge module.

### tools-lab-003

- `edges2.tsv` edges into the 30 `KG` files from outside `KG` -> 1 (`note_view.py` imports `kick_goal_view.py`; `note_view` is itself a kept one-off). From the closure -> 0.
- `wc -l` over KG (list L3) -> 2,956.
- Beads: `jq -r 'select((.description//"")+(.notes//"")+(.design//"") | test("run_kick_goal|kick_goal_")) | .id' .beads/issues.jsonl` -> BUG-9ixtf (`run_kick_goal_frontier/preds/selfsim2`), BUG-1v1z9 (`run_kick_goal_fast`), BUG-qy8q7 and BUG-9ngk8.1 (the loop in prose), all open.
- Transcripts: last command naming a member -> 2026-10-13.
- Docs: AUDIO_EVAL_HARNESS_GUIDE.md section 0 (Trained detectors — what holds) still calls them "the research tools".
- Grade C: the open beads decide it, and only Peter knows whether that work moves to the lab.

### tools-lab-004

- Members: 14 `CLS` files in `tags.tsv` (3,752 lines; names in appendix E) plus `train/` (5 .py and `sources.toml`, 1,576 lines): 5,328.
- `rg -l 'exam_classifier|probe_classifier|bakeoff_b1|fit_stage1_profiles|sweep_p4|heldout_acceptance_p4|egmd_drum_truth|slakh_drum_truth' crates scripts tools/audio_analysis/lab tools/audio_analysis/catalog` -> 0.
- AUDIO_EVENT_CLASSIFIER_DESIGN.md status block -> verdict SHORT OF BAR; P4 and P5 blocked on Peter's call whether to expand training data or park. BUG-d8l is the open data bead. Last command naming a member: 2026-10-11.

### tools-lab-005

- Members: 28 `ACC-HARNESS` (4,311), 8 `ACC-FETCH` (1,573), 3 `ACC-CAUSAL` (780): 6,664. All last committed before 2026-08-01 (July).
- Importers from the closure -> 0 (rerun). Edges into the 39 group files in `edges2.tsv`: 84 in all, 38 from the classifier arc and 46 from the group itself (rerun with awk over `edges2.tsv`).
- AUDIO_ANALYSIS_ACCURACY_DESIGN.md status IN PROGRESS (P1 to P4 shipped). Beads naming members: open BUG-dlr, BUG-t32, BUG-1gm, BUG-j8v, deferred BUG-9ra.
- `causal_compare.py`, `causal_sweep.py` and `odf_attribution.py` drive Rust examples (`crates/manifold-audio/examples/causal_events.rs`, `detector_fires.rs`); `fd` confirms both examples exist.

### tools-lab-006

- `rg -n 'percussion_json_pipeline' crates scripts .claude --hidden` (rerun) -> `crates/manifold-playback/src/percussion_backend.rs:17`, `SCRIPT_FILE_NAME`. `content_commands.rs` reaches it through `detect_clip` (first pass).
- Rust-side `manifold_audio` hits are the Rust crate of that name, not the Python package; the Python package is reached only through the script.
- Lines: 21 .py in `manifold_audio/` 5,273, `percussion_json_pipeline.py` 10, `lameenc.py` 37, `stage_runtime_mac.sh` 316, `requirements.runtime.mac.txt` 35 -> 5,671 (the tracked `decoder_alignment.json` is data, left out).
- BUG-qy2 says madmom and ADTOF models cannot ship commercially; AUDIO_EVENT_CLASSIFIER_DESIGN.md quotes Peter wanting this pipeline gone. Product code: stage risk, grade C.

### tools-lab-007

- `rg -c -e 'Assets/|Tools/AudioAnalysis|IL2CPP|Unity' T/README.md` (rerun) -> 17.
- `fd PercussionPipelineBuildPostprocessor "$R" --hidden --no-ignore` (rerun) -> nothing. `ls "$R/Assets"` (rerun) -> `abletonosc-patches models mri-data plugins private-preview`; no `Scripts/`.
- `git log -1 --format=%ad --date=short -- T/README.md` (rerun) -> 2026-03-20.

### tools-lab-008

- `git ls-files tools/rt_prototype` -> 19 files; outside `compare/`: 14 files, 2,735 lines (rerun). `rg -n rt_prototype Cargo.toml` -> no hit, so it is not a workspace member.
- `rg -n 'rt_prototype' crates scripts` (rerun) -> `raytrace.rs:4-5` (ports `accel.rs` and `rt_trace.metal`), `shadow_rays.msl:1568`, `manifold-gpu/Cargo.toml:87`, comments in `rt_p1_shadow.rs` and `rt_bug17r3_lightless_gi.rs`, `scripts/rt_region_probe.py:4`, and `rt_t38_multibounce.rs:62-64` with two `include_str!` lines.
- Those two are `compare/RtBleed.json` and `compare/RtAmbientOnly.json`. A delete of the crate without moving `compare/` breaks that GPU proof's build.
- RAYTRACING_DESIGN.md (IN PROGRESS) still says lanes run the crate. No real run in 30 days.

### tools-lab-009

- `rg -l mtl4fx_repro "$R" --glob '!target/**' --hidden` (rerun) -> the README itself and the census doc. 0 transcript commands. 235 lines.
- The README is the repro that backs the hard-off recorded in BUG-woji.

### tools-lab-010

- `rg -l 'tools/segmentation|segmentation/' "$R" --hidden` outside the folder (rerun) -> the census doc only. 0 transcript commands in 30 days. `git ls-files` -> 8 files, 954 lines. Local `.venv/` and `masks/` are ignored and untouched.

### tools-lab-011

- `rg -l 'nifti_to_mrivol|mrivol_to_tiff' "$R" --hidden` (rerun) -> `tools/mrivol_to_tiff.py` itself. 0 commands. `mri.rs` defines the `.mrivol` magic and header, so the converters are the format's producers. 281 lines.

### tools-lab-012

- `rg -n 'detector_subset' "$R" --hidden` (rerun) -> the file itself.
- `rg -n 'caught, ' tools --glob '*.py'` (rerun) -> `detector_eval.py:244` still logs `cutoff {th}: m/n caught, f false fires`, inside `held_out` and only when `log` is set; the regex in `detector_subset.py:10` matches it.
- `rg -n 'held_out\(' tools --glob '*.py'` (rerun) -> `lab/score.py:102` and `lab/test_lab.py:105,129`, all `log=None`. No caller produces a log of that shape. The runner that did (`snare_net.py`) went in a83bda6a3 (Detector Lab P4, 2026-10-11).
- Grade B: the named gap is that a future caller could pass `log=print`.

### tools-lab-013

- `rg -n 'snare_what_fires|snare_full_s0' "$R"` (rerun) -> the script itself and one line in BUG-ymy4z. The default input `snare_full_s0.npz` is the retired runner's name; the lab saves `s<seed>.npz` (`lab/run.py:399`, `:412`, `:481`). Grade B: the script needs a rewrite to read lab saves, which is the bead's own ask.

### tools-lab-014

- `rg -l night-BRIEF "$R" --hidden` (rerun) -> nothing but the census files. `wc -l` -> 191. The two JSON plans in `lab/plans/` stay.

### tools-lab-015

- Names: list L4 (21 configs, 1,239 lines, rerun). `rg -l -w -F NAME tools docs crates scripts .claude .beads --hidden` excluding the config itself (rerun) -> 0 files for each of the 21. `lab/experiments/` holds 56 configs; 35 stay.
- A config is run by passing its path on the command line, so a name no file mentions can still be used by hand. Hence ask.

### tools-lab-016

- `rg -n -w attack_env "$R" --hidden` (rerun) -> `catalog/variants.py:107`, the definition.
- `rg -n -w smooth tools --glob '*.py'` filtered to catalog and variants (rerun) -> the definition at `:102` and the one call at `:109` inside `attack_env`. `rises` is also called at `:211`, so it stays. 8 lines with blanks.

### tools-lab-017

- `rg -n -w 'download_candidates|plan_downloads' "$R" --hidden` (rerun) -> definitions at `variants.py:297` and `:317`, three calls in `catalog/test_tree.py:234-240`, and BUG-dcoov. The bead asks whether `download_candidates` needs its next bounded round; BUG-8dy68 is the data campaign they serve. The regexes sit just above at `variants.py:292-294`. `rg -n -w 'FINALISH|NOT_BOUNCE|WIP_MASTER' tools --glob '*.py'` (rerun) -> `FINALISH` and `WIP_MASTER` are used only at `variants.py:309` and `:311` inside `download_candidates`; `NOT_BOUNCE` is also imported by `lab/review_project.py:489` (live), so it stays.

### tools-lab-018

- `rg -l local_corpus_inventory "$R" --hidden` (rerun) -> `.beads/issues.jsonl` only (and this sweep). 267 lines. `catalog/catalog.py` was committed 2026-10-12 and scans the full music tree.

### tools-lab-019

- `rg -l -w 'snare_listen|snare_samples|snare_timing' "$R" --hidden` (rerun) -> the files themselves. 45 + 87 + 37 = 169 lines. `lab/audit.py` and `lab/offsets.py` own snare label truth and export-offset proof.

## Lists

### L1 The 93 files of tools-lab-001 (all under `tools/audio_analysis/eval/`)

Modules (20): kick_anchored_score, kick_attack_background, kick_bass_continuity, kick_candidate_augmented_score, kick_causal_calibration, kick_compact_shape_score, kick_controlled_mixtures, kick_covariance_score, kick_dsp_controls, kick_episode_trial, kick_event_window_score, kick_failure_casebook, kick_fast_features, kick_fusion_error_audit, kick_hard_negative_weights, kick_mixture_fit, kick_spectral_shape, kick_stem_augmented_score, kick_streaming_reference, kick_timbre_features.

kick_night (15): kick_night_{anchor_probe, extra_passages, glide_component, kernel_support, label_source_audit, lowshare_mix, lowsupport_probe, oracle_frontier, oracle_labels, relabel, render, replay, stack, stem_offset, validate}.

Runners (24): run_kick_{anchored_trial, attack_background_trial, boosted_trial, candidate_augmented_trial, causal_calibration_trial, compact_shape_trial, complex_audit, covariance_trial, coverage_blend_trial, event_window_trial, extended_coverage_trial, hard_negative_trial, heldout, interaction_trial, kernel_blend_trial, kernel_trial, mixture_trial, spectral_shape_trial, stem_augmented_trial, threeway_blend_trial, timbre_trial, training_coverage_trial, upper_cue, zero_core_trial}.

Night runners (5): run_kick_night_h19 to run_kick_night_h23.

Scripts (5): prepare_kick_expanded_review, render_kick_comparison, review_kick_evening_passages, verify_kick_controls, verify_kick_streaming.

Tests (24): top level (15) test_kick_{anchored_score, attack_background, candidate_augmented_score, causal_calibration, compact_shape_score, controlled_mixtures, covariance_score, event_window_score, hard_negative_weights, kernel_blend, spectral_shape, stem_augmented_score, streaming_reference, threeway_blend, zero_core_trial}; `tests/` (9) test_kick_{bass_continuity, episode_trial, failure_casebook, fusion_error_audit, mixture_fit, timbre_features}, test_prepare_kick_expanded_review, test_run_kick_complex_audit, test_run_kick_timbre_trial.

Per-file lines: appendix E, tag `K9-batch`.

### L2 tools-lab-002 detail

List 1, the 12 bridge modules: run_kick_fusion_trial, run_kick_dsp_experiments, master_kick_comparison, live_kick_baseline, kick_attack_rejection, note_clean, kick_night_common, kick_night_dump, kick_night_diagnose, kick_night_stem_audit, kick_night_snap_labels, kick_night_drum_onset_shape.

List 2, freed modules (24): kick_boosted_score, kick_coverage_blend, kick_dsp_experiments, kick_excess_balance_trial, kick_fusion_calibration, kick_fusion_fine_calibration, kick_hybrid_experiment, kick_interaction_score, kick_kernel_blend, kick_kernel_score, kick_night_dump, kick_pcen_experiment, kick_shape_features, kick_stable_quadrature, kick_stable_rms, kick_subspace_score, kick_sustain_audit, kick_threeway_blend, kick_training_coverage, kick_trajectory_features, run_kick_fusion_calibration, run_kick_shape_trial, run_kick_trajectory_trial, verify_kick_evening_passages.

Freed tests (20): test_kick_boosted_score, test_kick_coverage_blend, test_kick_evening_passages, test_kick_interaction_score, test_kick_kernel_score, test_kick_training_coverage (top level); `tests/` test_kick_{dsp_experiments, excess_balance_trial, fusion_calibration, fusion_fine_calibration, hybrid_experiment, pcen_experiment, shape_features, stable_quadrature, stable_rms, subspace_score, sustain_audit, trajectory_features}, test_run_kick_shape_trial, test_run_kick_trajectory_trial.

List 3, modules that stay with the label chain (18): kick_goal_{project_kicks, recall_labels, rolls, stereo, trigger_labels, wip_labels}, kick_night_{common, diagnose, drum_onset_shape, snap_labels, stem_audit}, kick_tonal_experiment, live_kick_baseline, master_kick_comparison, note_clean, run_kick_dsp_experiments, run_kick_fusion_trial, run_kick_goal_tail_component.

List 4, tests that stay (9): `tests/` test_kick_{attack_rejection, complex_onset, fusion_bandwise, fusion_features, tonal_experiment, upper_cue}, test_live_kick_baseline, test_master_kick_comparison, test_run_kick_fusion_trial.

### L3 The 30 kick-goal loop files of tools-lab-003 (all under `tools/audio_analysis/eval/`)

Libraries (8): kick_goal_{buildup, featsets, render_audit, rules, songnorm, synth, view, wip_2425}. Runners (20): run_kick_goal_{coldstart, count_check, data, deadline, fast, frontier, frontier_saved, learning, more_prep, nn, preds, rescore, rolls, rules, selfsim, selfsim2, shape, show, songnorm, templates}. Others (2): kick_night_scoreboard, show_kick_labels. The kick_goal_* modules the label chain imports (`kick_goal_project_kicks`, `_recall_labels`, `_rolls`, `_stereo`, `_trigger_labels`, `_wip_labels`) are not in this list; they stay (L2 list 3).

### L5 Dynamic import sites (`rg -n 'import_module\(' tools --glob '*.py'`, rerun)

- `{family}_labels` through `tools.audio_analysis.eval.`: `note_view.py:23`, `family_cands.py:34`, `lab/components.py:37`, `lab/audit.py:74-75`, `note_generated.py:21`, `note_clean.py:92`, `note_false.py:39`.
- `kick_tonal_experiment.py:209` -> `eval.kick_dsp_experiments`.
- `run_kick_dsp_experiments.py:75` -> `eval.kick_{variant}_experiment`.
- `kick_hybrid_experiment.py:54` -> `eval.kick_stable_{method}`.
- `lab/cache.py:88-93` -> resolves names from function source for `code_id()`; not a graph edge.

### L4 The 21 unreferenced experiment configs (`lab/experiments/*.json`)

bass_generated, kick_gen50_gated_all, kick_gen50_gated_t32, kick_shipped_recipe, kick_v2_all, kick_v2_final_e24, kick_v2_gain, snare_gen50_gated, snare_gen50_gated_all, snare_gen50_hybrid, snare_gen50_hybrid_low16_nostage, snare_gen50_hybrid_low16_t16, snare_gen50_hybrid_low16_t32, snare_gen50_hybrid_low16_t8, snare_gen_share20, snare_gen_share35, snare_v2_all, snare_v2_gain_probe, snare_v2_mid_probe, synth_base, synth_generated. Lines per file range 34 to 83; total 1,239.


## Appendix A inventory.sh (per-file lines, last commit, importers, dev.py, transcript runs)

```sh
#!/bin/zsh
# Inventory of tracked tools/ python files: lines, last commit, importers, dev.py, runs in 30d, other refs.
R="/Users/peterkiemann/MANIFOLD - Rust"
SP="/private/tmp/claude-501/-Users-peterkiemann-MANIFOLD---Rust/09f52141-8f51-4bf9-9fa8-8ef4a534e26b/scratchpad"
OUT="$SP/inventory.tsv"
print -r -- "path	lines	last	n_imp	devpy	runs_base	runs_dirbase	runs_mod	otherrefs	importers" > "$OUT"
while read -r f; do
  base="${f:t}"
  stem="${base%.py}"
  dir="${f:h:t}"
  lines=$(wc -l < "$R/$f" | tr -d ' ')
  last=$(rg -F -m1 "$f" "$SP/lastcommit.tsv" | cut -f1)
  if [[ "$stem" == "__init__" ]]; then
    print -r -- "$f	$lines	$last	-	-	-	-	-	-	-" >> "$OUT"; continue
  fi
  # importers: other .py files under tools/ and scripts/ with an import/from line naming the stem
  imp=$(rg -l --glob '*.py' -e "^\s*(import|from)\b.*\b${stem}\b" "$R/tools" "$R/scripts" 2>/dev/null | rg -v -F "/$f" | sed "s|$R/||" | tr '\n' ',' )
  nimp=$(print -r -- "$imp" | tr ',' '\n' | rg -c . || echo 0)
  dv=$(rg -c -F "/$base" "$R/scripts/dev.py" 2>/dev/null || echo 0)
  rb=$(rg -c -F "$base" "$SP/all_commands_u.txt" 2>/dev/null || echo 0)
  rdb=$(rg -c -F "$dir/$base" "$SP/all_commands_u.txt" 2>/dev/null || echo 0)
  rm=$(rg -c -e "\b(eval|lab|catalog|train|manifold_audio)\.${stem}\b" "$SP/all_commands_u.txt" 2>/dev/null || echo 0)
  other=$(rg -l -F "$base" "$R" -g '!tools/**' -g '!docs/evidence/**' -g '!target/**' -g '!plugins/vendor/**' -g '!.git/**' -g '!.beads/**' -g '!tools/audio_analysis/catalog/catalog.sqlite' --hidden 2>/dev/null | wc -l | tr -d ' ')
  print -r -- "$f	$lines	$last	$nimp	$dv	$rb	$rdb	$rm	$other	${imp//tools\/audio_analysis\//}" >> "$OUT"
done < "$SP/pyfiles.txt"
print "done"
```

## Appendix B graph.sh (strict import edges, importer then dependency)

```sh
#!/bin/zsh
# Import edges "importer<TAB>dependency" for tools/audio_analysis python files (strict patterns).
R="/Users/peterkiemann/MANIFOLD - Rust"
SP="/private/tmp/claude-501/-Users-peterkiemann-MANIFOLD---Rust/09f52141-8f51-4bf9-9fa8-8ef4a534e26b/scratchpad"
OUT="$SP/edges.tsv"
: > "$OUT"
PFX='((tools\.)?(audio_analysis\.)?'
while read -r f; do
  base="${f:t}"; stem="${base%.py}"
  [[ "$stem" == "__init__" ]] && continue
  rel="${f#tools/audio_analysis/}"
  sub="${rel%%/*}"         # eval | lab | catalog | train | manifold_audio | file
  [[ "$rel" == "$sub" ]] && sub=""
  # fetch/ and export/ and tests/ one level deeper
  dotted="${rel%.py}"; dotted="${dotted//\//.}"   # eval.fetch.egmd
  last="${dotted##*.}"
  # strict qualified import: from [tools.audio_analysis.]eval.fetch.egmd import / import ...eval.x
  pat1="^\\s*(from|import)\\s+(tools\\.)?(audio_analysis\\.)?${dotted//./\\.}(\\s|\$|,)"
  # from [tools.audio_analysis.]eval[.fetch] import a, b, STEM
  parent="${dotted%.*}"
  [[ "$parent" == "$dotted" ]] && parent=""
  srcs=()
  rg -l --glob '*.py' -e "$pat1" "$R/tools" "$R/scripts" 2>/dev/null | sed "s|$R/||" | while read -r s; do print -r -- "$s	$f"; done >> "$OUT"
  if [[ -n "$parent" ]]; then
    pat2="^\\s*from\\s+(tools\\.)?(audio_analysis\\.)?${parent//./\\.}\\s+import\\s+.*\\b${stem}\\b"
    rg -l --glob '*.py' -e "$pat2" "$R/tools" "$R/scripts" 2>/dev/null | sed "s|$R/||" | while read -r s; do print -r -- "$s	$f"; done >> "$OUT"
    # relative: from .stem import / from . import stem  (same package)
    pat3="^\\s*from\\s+\\.${stem}\\s+import|^\\s*from\\s+\\.\\s+import\\s+.*\\b${stem}\\b"
    rg -l --glob '*.py' -e "$pat3" "$R/$(dirname $f)" 2>/dev/null | sed "s|$R/||" | while read -r s; do print -r -- "$s	$f"; done >> "$OUT"
    # bare: from stem import / import stem  (script dir on path)
    pat4="^\\s*(from|import)\\s+${stem}(\\s|\$|,)"
    rg -l --glob '*.py' -e "$pat4" "$R/$(dirname $f)" 2>/dev/null | sed "s|$R/||" | while read -r s; do print -r -- "$s	$f"; done >> "$OUT"
  fi
done < "$SP/pyfiles.txt"
sort -u "$OUT" | rg -v -P '^(\S+)\t\1$' > "$OUT.u"; mv "$OUT.u" "$OUT"
wc -l "$OUT"
```

## Appendix C bfs.sh (closure from the roots; reproduces 127 files, 79 in eval/)

Roots are the 48 files `lab/*.py`, `catalog/*.py` and `kick_container.py`. Run as `bfs.sh roots.txt edges2.tsv`.

```sh
#!/bin/zsh
# usage: bfs.sh roots.txt edges.tsv  -> files reachable from the roots, sorted. edges.tsv is "importer<TAB>dependency".
awk -F'\t' 'FNR==NR{root[++nr]=$1; next}
  {a[$1, ++c[$1]]=$2}
  END{for(i=1;i<=nr;i++) if(!(root[i] in s)){s[root[i]]=1; q[++t]=root[i]}
      for(h=1;h<=t;h++){f=q[h]; for(j=1;j<=c[f];j++){d=a[f,j]; if(!(d in s)){s[d]=1; q[++t]=d}}}
      for(i=1;i<=t;i++) print q[i]}' "$1" "$2" | sort
```

## Appendix D tag.sh (cluster tag for every eval file outside the closure)

```sh
#!/bin/zsh
# Tag every non-closure eval python file with a cluster; print per-file TSV: tag path lines date runs
SP="/private/tmp/claude-501/-Users-peterkiemann-MANIFOLD---Rust/09f52141-8f51-4bf9-9fa8-8ef4a534e26b/scratchpad"
P=tools/audio_analysis/eval/
while IFS=$'\t' read -r fpath lines date nimp rb rdb rm oth; do
  rel="${fpath#$P}"; base="${rel:t}"; stem="${base%.py}"
  is_test=0; [[ "$base" == test_* ]] && is_test=1
  live_test=0
  if (( is_test )); then
    live=$(rg -F "$rel	" "$SP/test_targets.tsv" | cut -f2 | head -1)
    [[ "$live" != "LIVE:" && -n "$live" ]] && live_test=1
  fi
  tag=""
  case "$rel" in
    fetch/*) tag=ACC-FETCH;;
  esac
  if [[ -z "$tag" ]]; then
    case "$stem" in
      causal_compare|causal_sweep|odf_attribution) tag=ACC-CAUSAL;;
      test_bundles) tag=LIVE-TEST;;
      exam_classifier|probe_classifier|bakeoff_b1|fit_stage1_profiles|heldout_acceptance_p4|sweep_p4|egmd_drum_truth|slakh_drum_truth|test_train_dataset|test_train_license_allowlist|test_stage1_dsp_detection|test_sweep_p4|test_dense_window|test_precision_postprocessing) tag=CLS;;
    esac
  fi
  if [[ -z "$tag" ]]; then
    if [[ "$date" < "2026-08-01" ]]; then tag=ACC-HARNESS
    elif [[ "$date" == "2026-10-09" ]]; then
      if (( is_test )); then (( live_test )) && tag=K9-TEST-LIVE || tag=K9-TEST-DEAD
      else tag=K9
      fi
    else
      case "$stem" in
        kick_goal_*|run_kick_goal_*|show_kick_labels|kick_night_scoreboard) tag=KG;;
        test_als_extract|test_detector_channels|test_detector_eval|test_kick_goal_fast_counts) tag=LIVE-TEST;;
        *) tag=FAM;;
      esac
    fi
  fi
  print -r -- "$tag	$rel	$lines	$date	$rb	$rdb"
done < "$SP/noncl.tsv" | sort > "$SP/tags.tsv"
awk -F'\t' '{c[$1]++; l[$1]+=$3} END{for(k in c) printf "%-14s %4d files %6d lines\n", k, c[k], l[k]}' "$SP/tags.tsv" | sort
```

## Appendix E per-file inventory (389 files)

Columns: tag, lines, last commit date, `i` importers (strict graph), `d` dev.py mentions, `r` transcript commands naming the base name and the dir/base name over 30 days, path under `tools/audio_analysis/` (loose files under `tools/` keep their path). Tags: LIVE-direct, LIVE-indirect, LIVE-pkg (closure); LAB, CATALOG, LAB-support; K9-batch (tools-lab-001); K9-test-live (tests of modules that 002 frees or that stay); KG (003); CLS and CLS-train (004); ACC-HARNESS, ACC-FETCH, ACC-CAUSAL (005); BACKEND (006); SEGMENTATION (010); MRI (011); LIVE-TEST (tests of live modules); FAM (the one-off group; 012, 013, 018, 019 and the kept ones).

```
ACC-CAUSAL 302 2026-07-18 i=1 d=0 r=0/0 eval/causal_compare.py
ACC-CAUSAL 228 2026-07-18 i=0 d=0 r=0/0 eval/causal_sweep.py
ACC-CAUSAL 250 2026-07-18 i=0 d=0 r=0/0 eval/odf_attribution.py
ACC-FETCH 93 2026-07-18 i=0 d=0 r=0/0 eval/fetch/babyslakh.py
ACC-FETCH 380 2026-07-18 i=0 d=0 r=0/0 eval/fetch/egmd.py
ACC-FETCH 83 2026-07-18 i=0 d=0 r=0/0 eval/fetch/harmonix.py
ACC-FETCH 184 2026-07-18 i=0 d=0 r=0/0 eval/fetch/harmonix_audio.py
ACC-FETCH 166 2026-07-18 i=1 d=0 r=0/0 eval/fetch/maestro.py
ACC-FETCH 95 2026-07-18 i=0 d=0 r=0/0 eval/fetch/musdb18.py
ACC-FETCH 379 2026-07-18 i=2 d=0 r=0/0 eval/fetch/self_render.py
ACC-FETCH 193 2026-07-18 i=1 d=0 r=0/0 eval/fetch/slakh2100.py
ACC-HARNESS 206 2026-07-18 i=5 d=0 r=0/0 eval/baseline_scoreboard_p3.py
ACC-HARNESS 263 2026-07-17 i=8 d=0 r=2/1 eval/beat_scoring.py
ACC-HARNESS 345 2026-07-17 i=1 d=0 r=1/1 eval/beat_tracker_alignment.py
ACC-HARNESS 270 2026-07-17 i=4 d=0 r=4/0 eval/calibration.py
ACC-HARNESS 261 2026-07-17 i=2 d=0 r=0/0 eval/click_track.py
ACC-HARNESS 582 2026-07-18 i=1 d=0 r=0/0 eval/full_pack_baseline.py
ACC-HARNESS 230 2026-07-17 i=6 d=0 r=2/0 eval/liveshow_extract.py
ACC-HARNESS 284 2026-07-17 i=2 d=0 r=0/0 eval/metamorphic.py
ACC-HARNESS 306 2026-07-17 i=13 d=0 r=35/0 eval/metrics.py
ACC-HARNESS 54 2026-07-17 i=2 d=0 r=0/0 eval/midi_synth.py
ACC-HARNESS 72 2026-07-17 i=1 d=0 r=0/0 eval/noise_floor.py
ACC-HARNESS 36 2026-07-18 i=26 d=0 r=6/1 eval/paths.py
ACC-HARNESS 128 2026-07-17 i=0 d=0 r=0/0 eval/render_beat_clicks.py
ACC-HARNESS 254 2026-07-18 i=7 d=1 r=126/0 eval/run.py
ACC-HARNESS 70 2026-07-17 i=1 d=0 r=3/0 eval/sweep.py
ACC-HARNESS 50 2026-07-17 i=0 d=0 r=1/1 eval/tests/test_beat_scoring.py
ACC-HARNESS 88 2026-07-17 i=0 d=0 r=0/0 eval/tests/test_beat_tracker_alignment.py
ACC-HARNESS 82 2026-07-17 i=0 d=0 r=0/0 eval/tests/test_calibration.py
ACC-HARNESS 52 2026-07-17 i=0 d=0 r=0/0 eval/tests/test_click_track.py
ACC-HARNESS 59 2026-07-17 i=0 d=0 r=0/0 eval/tests/test_fixtures_toml.py
ACC-HARNESS 55 2026-07-17 i=0 d=0 r=0/0 eval/tests/test_full_pack_baseline.py
ACC-HARNESS 107 2026-07-17 i=0 d=0 r=0/0 eval/tests/test_liveshow_extract.py
ACC-HARNESS 111 2026-07-17 i=0 d=0 r=0/0 eval/tests/test_metamorphic.py
ACC-HARNESS 111 2026-07-17 i=0 d=0 r=0/0 eval/tests/test_metrics.py
ACC-HARNESS 53 2026-07-17 i=0 d=0 r=0/0 eval/tests/test_midi_synth.py
ACC-HARNESS 88 2026-07-17 i=0 d=0 r=0/0 eval/tests/test_p3_fetch_scripts.py
ACC-HARNESS 55 2026-07-17 i=0 d=0 r=0/0 eval/tests/test_self_render.py
ACC-HARNESS 39 2026-07-17 i=0 d=0 r=0/0 eval/tests/test_sweep_guard.py
BACKEND 37 2026-03-20 i=0 d=0 r=0/0 lameenc.py
BACKEND 14 2026-03-20 i=0 d=- r=-/- manifold_audio/__init__.py
BACKEND 5 2026-03-20 i=0 d=0 r=0/0 manifold_audio/__main__.py
BACKEND 207 2026-07-18 i=8 d=0 r=1/1 manifold_audio/adtof_detection.py
BACKEND 559 2026-07-18 i=6 d=0 r=0/0 manifold_audio/analyzer.py
BACKEND 197 2026-07-17 i=17 d=0 r=0/0 manifold_audio/audio_io.py
BACKEND 259 2026-03-20 i=5 d=0 r=0/0 manifold_audio/basic_pitch_detection.py
BACKEND 88 2026-07-17 i=4 d=0 r=1/1 manifold_audio/beat_tracking.py
BACKEND 683 2026-07-17 i=4 d=0 r=2/2 manifold_audio/bpm.py
BACKEND 790 2026-07-17 i=2 d=0 r=0/0 manifold_audio/cli.py
BACKEND 34 2026-03-20 i=1 d=0 r=0/0 manifold_audio/conflict_resolution.py
BACKEND 299 2026-03-20 i=4 d=0 r=0/0 manifold_audio/external_tools.py
BACKEND 112 2026-03-20 i=1 d=0 r=0/0 manifold_audio/gestures.py
BACKEND 63 2026-03-20 i=5 d=0 r=0/0 manifold_audio/math_utils.py
BACKEND 72 2026-07-18 i=5 d=0 r=0/0 manifold_audio/mel_patch.py
BACKEND 256 2026-07-17 i=13 d=0 r=24/0 manifold_audio/models.py
BACKEND 104 2026-03-20 i=5 d=0 r=0/0 manifold_audio/onset_detection.py
BACKEND 111 2026-03-20 i=1 d=0 r=0/0 manifold_audio/peak_detection.py
BACKEND 594 2026-07-18 i=7 d=0 r=0/0 manifold_audio/precision_postprocessing.py
BACKEND 64 2026-03-20 i=1 d=0 r=0/0 manifold_audio/profiles.py
BACKEND 127 2026-07-18 i=4 d=0 r=1/0 manifold_audio/spectral.py
BACKEND 635 2026-07-18 i=5 d=0 r=0/0 manifold_audio/stage1_dsp_detection.py
BACKEND 10 2026-03-20 i=0 d=0 r=0/0 percussion_json_pipeline.py
CATALOG 0 2026-10-12 i=0 d=- r=-/- catalog/__init__.py
CATALOG 227 2026-10-12 i=3 d=0 r=3/2 catalog/als_scan.py
CATALOG 1090 2026-10-12 i=8 d=0 r=119/90 catalog/catalog.py
CATALOG 328 2026-10-12 i=2 d=0 r=17/11 catalog/comps.py
CATALOG 138 2026-10-12 i=2 d=0 r=8/3 catalog/names.py
CATALOG 145 2026-10-12 i=0 d=0 r=8/6 catalog/test_catalog.py
CATALOG 384 2026-10-12 i=0 d=0 r=15/6 catalog/test_tree.py
CATALOG 232 2026-10-12 i=3 d=0 r=1386/5 catalog/tree.py
CATALOG 209 2026-10-12 i=3 d=0 r=8/6 catalog/usable.py
CATALOG 473 2026-10-12 i=4 d=0 r=34/16 catalog/variants.py
CLS 401 2026-07-18 i=0 d=0 r=0/0 eval/bakeoff_b1.py
CLS 88 2026-07-18 i=4 d=0 r=0/0 eval/egmd_drum_truth.py
CLS 469 2026-07-18 i=1 d=0 r=0/0 eval/exam_classifier.py
CLS 243 2026-07-18 i=0 d=0 r=0/0 eval/fit_stage1_profiles.py
CLS 202 2026-07-18 i=0 d=0 r=0/0 eval/heldout_acceptance_p4.py
CLS 111 2026-07-18 i=0 d=0 r=0/0 eval/probe_classifier.py
CLS 121 2026-07-18 i=1 d=0 r=0/0 eval/slakh_drum_truth.py
CLS 890 2026-07-18 i=7 d=0 r=0/0 eval/sweep_p4.py
CLS 63 2026-07-18 i=0 d=0 r=0/0 eval/tests/test_dense_window.py
CLS 416 2026-07-18 i=0 d=0 r=0/0 eval/tests/test_precision_postprocessing.py
CLS 315 2026-07-18 i=0 d=0 r=0/0 eval/tests/test_stage1_dsp_detection.py
CLS 178 2026-07-18 i=0 d=0 r=0/0 eval/tests/test_sweep_p4.py
CLS 177 2026-07-18 i=0 d=0 r=0/0 eval/tests/test_train_dataset.py
CLS 78 2026-07-18 i=0 d=0 r=0/0 eval/tests/test_train_license_allowlist.py
CLS-train 6 2026-07-18 i=0 d=- r=-/- train/__init__.py
CLS-train 556 2026-07-18 i=3 d=0 r=0/0 train/dataset.py
CLS-train 213 2026-07-18 i=0 d=0 r=3/0 train/export.py
CLS-train 170 2026-07-18 i=4 d=0 r=1/0 train/model.py
CLS-train 539 2026-07-18 i=1 d=0 r=1/1 train/train.py
FAM 65 2026-10-13 i=0 d=0 r=4/2 eval/bass_survey.py
FAM 18 2026-10-11 i=0 d=0 r=1/0 eval/detector_subset.py
FAM 78 2026-10-11 i=0 d=0 r=7/0 eval/family_cands.py
FAM 110 2026-10-11 i=0 d=0 r=9/5 eval/kick_buildup_sim.py
FAM 63 2026-10-11 i=0 d=0 r=4/3 eval/kick_generated.py
FAM 267 2026-10-10 i=0 d=0 r=1/0 eval/local_corpus_inventory.py
FAM 68 2026-10-11 i=0 d=0 r=2/1 eval/note_false.py
FAM 45 2026-10-11 i=0 d=0 r=2/2 eval/note_generated.py
FAM 88 2026-10-11 i=0 d=0 r=1/0 eval/note_view.py
FAM 70 2026-10-11 i=0 d=0 r=6/1 eval/project_folder_survey.py
FAM 45 2026-10-11 i=0 d=0 r=1/0 eval/snare_listen.py
FAM 86 2026-10-11 i=0 d=0 r=3/2 eval/snare_roll_sim.py
FAM 87 2026-10-11 i=0 d=0 r=2/0 eval/snare_samples.py
FAM 37 2026-10-11 i=0 d=0 r=1/0 eval/snare_timing.py
FAM 70 2026-10-11 i=0 d=0 r=5/0 eval/snare_what_fires.py
K9-batch 88 2026-10-09 i=3 d=0 r=0/0 eval/kick_anchored_score.py
K9-batch 226 2026-10-09 i=3 d=0 r=2/1 eval/kick_attack_background.py
K9-batch 247 2026-10-09 i=1 d=0 r=1/0 eval/kick_bass_continuity.py
K9-batch 248 2026-10-09 i=2 d=0 r=0/0 eval/kick_candidate_augmented_score.py
K9-batch 108 2026-10-09 i=2 d=0 r=1/0 eval/kick_causal_calibration.py
K9-batch 55 2026-10-09 i=2 d=0 r=0/0 eval/kick_compact_shape_score.py
K9-batch 355 2026-10-09 i=2 d=0 r=0/0 eval/kick_controlled_mixtures.py
K9-batch 89 2026-10-09 i=2 d=0 r=0/0 eval/kick_covariance_score.py
K9-batch 75 2026-10-09 i=0 d=0 r=0/0 eval/kick_dsp_controls.py
K9-batch 97 2026-10-09 i=1 d=0 r=0/0 eval/kick_episode_trial.py
K9-batch 122 2026-10-09 i=2 d=0 r=0/0 eval/kick_event_window_score.py
K9-batch 186 2026-10-09 i=1 d=0 r=0/0 eval/kick_failure_casebook.py
K9-batch 32 2026-10-09 i=1 d=0 r=2/0 eval/kick_fast_features.py
K9-batch 281 2026-10-09 i=1 d=0 r=0/0 eval/kick_fusion_error_audit.py
K9-batch 162 2026-10-09 i=5 d=0 r=0/0 eval/kick_hard_negative_weights.py
K9-batch 192 2026-10-09 i=2 d=0 r=0/0 eval/kick_mixture_fit.py
K9-batch 53 2026-10-09 i=0 d=0 r=3/0 eval/kick_night_anchor_probe.py
K9-batch 60 2026-10-09 i=0 d=0 r=2/0 eval/kick_night_extra_passages.py
K9-batch 99 2026-10-09 i=1 d=0 r=3/0 eval/kick_night_glide_component.py
K9-batch 102 2026-10-09 i=1 d=0 r=2/0 eval/kick_night_kernel_support.py
K9-batch 84 2026-10-09 i=0 d=0 r=1/0 eval/kick_night_label_source_audit.py
K9-batch 116 2026-10-09 i=0 d=0 r=3/3 eval/kick_night_lowshare_mix.py
K9-batch 50 2026-10-09 i=0 d=0 r=1/0 eval/kick_night_lowsupport_probe.py
K9-batch 230 2026-10-09 i=1 d=0 r=7/7 eval/kick_night_oracle_frontier.py
K9-batch 106 2026-10-09 i=1 d=0 r=2/2 eval/kick_night_oracle_labels.py
K9-batch 58 2026-10-09 i=1 d=0 r=3/0 eval/kick_night_relabel.py
K9-batch 68 2026-10-09 i=0 d=0 r=1/0 eval/kick_night_render.py
K9-batch 40 2026-10-09 i=0 d=0 r=2/0 eval/kick_night_replay.py
K9-batch 81 2026-10-09 i=3 d=0 r=0/0 eval/kick_night_stack.py
K9-batch 53 2026-10-09 i=0 d=0 r=2/0 eval/kick_night_stem_offset.py
K9-batch 88 2026-10-09 i=0 d=0 r=2/0 eval/kick_night_validate.py
K9-batch 192 2026-10-09 i=2 d=0 r=0/0 eval/kick_spectral_shape.py
K9-batch 179 2026-10-09 i=2 d=0 r=0/0 eval/kick_stem_augmented_score.py
K9-batch 155 2026-10-09 i=4 d=0 r=0/0 eval/kick_streaming_reference.py
K9-batch 256 2026-10-09 i=2 d=0 r=0/0 eval/kick_timbre_features.py
K9-batch 236 2026-10-09 i=1 d=0 r=0/0 eval/prepare_kick_expanded_review.py
K9-batch 71 2026-10-09 i=0 d=0 r=2/1 eval/render_kick_comparison.py
K9-batch 167 2026-10-09 i=0 d=0 r=0/0 eval/review_kick_evening_passages.py
K9-batch 124 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_anchored_trial.py
K9-batch 84 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_attack_background_trial.py
K9-batch 192 2026-10-09 i=11 d=0 r=0/0 eval/run_kick_boosted_trial.py
K9-batch 100 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_candidate_augmented_trial.py
K9-batch 89 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_causal_calibration_trial.py
K9-batch 79 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_compact_shape_trial.py
K9-batch 191 2026-10-09 i=1 d=0 r=0/0 eval/run_kick_complex_audit.py
K9-batch 107 2026-10-09 i=1 d=0 r=0/0 eval/run_kick_covariance_trial.py
K9-batch 127 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_coverage_blend_trial.py
K9-batch 98 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_event_window_trial.py
K9-batch 113 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_extended_coverage_trial.py
K9-batch 159 2026-10-09 i=1 d=0 r=0/0 eval/run_kick_hard_negative_trial.py
K9-batch 96 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_heldout.py
K9-batch 88 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_interaction_trial.py
K9-batch 107 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_kernel_blend_trial.py
K9-batch 80 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_kernel_trial.py
K9-batch 190 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_mixture_trial.py
K9-batch 85 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_night_h19.py
K9-batch 109 2026-10-09 i=0 d=0 r=1/0 eval/run_kick_night_h20.py
K9-batch 109 2026-10-09 i=0 d=0 r=4/0 eval/run_kick_night_h21.py
K9-batch 126 2026-10-09 i=4 d=0 r=6/2 eval/run_kick_night_h22.py
K9-batch 164 2026-10-09 i=0 d=0 r=3/0 eval/run_kick_night_h23.py
K9-batch 84 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_spectral_shape_trial.py
K9-batch 88 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_stem_augmented_trial.py
K9-batch 110 2026-10-09 i=0 d=0 r=1/0 eval/run_kick_threeway_blend_trial.py
K9-batch 202 2026-10-09 i=1 d=0 r=0/0 eval/run_kick_timbre_trial.py
K9-batch 208 2026-10-09 i=5 d=0 r=0/0 eval/run_kick_training_coverage_trial.py
K9-batch 170 2026-10-09 i=0 d=0 r=0/0 eval/run_kick_upper_cue.py
K9-batch 210 2026-10-09 i=1 d=0 r=0/0 eval/run_kick_zero_core_trial.py
K9-batch 99 2026-10-09 i=0 d=0 r=0/0 eval/test_kick_anchored_score.py
K9-batch 117 2026-10-09 i=0 d=0 r=0/0 eval/test_kick_attack_background.py
K9-batch 100 2026-10-09 i=0 d=0 r=0/0 eval/test_kick_candidate_augmented_score.py
K9-batch 61 2026-10-09 i=0 d=0 r=0/0 eval/test_kick_causal_calibration.py
K9-batch 77 2026-10-09 i=0 d=0 r=0/0 eval/test_kick_compact_shape_score.py
K9-batch 61 2026-10-09 i=0 d=0 r=0/0 eval/test_kick_controlled_mixtures.py
K9-batch 79 2026-10-09 i=0 d=0 r=0/0 eval/test_kick_covariance_score.py
K9-batch 72 2026-10-09 i=0 d=0 r=0/0 eval/test_kick_event_window_score.py
K9-batch 154 2026-10-09 i=1 d=0 r=0/0 eval/test_kick_hard_negative_weights.py
K9-batch 99 2026-10-09 i=1 d=0 r=0/0 eval/test_kick_kernel_blend.py
K9-batch 115 2026-10-09 i=0 d=0 r=0/0 eval/test_kick_spectral_shape.py
K9-batch 103 2026-10-09 i=0 d=0 r=0/0 eval/test_kick_stem_augmented_score.py
K9-batch 71 2026-10-09 i=3 d=0 r=0/0 eval/test_kick_streaming_reference.py
K9-batch 59 2026-10-09 i=0 d=0 r=0/0 eval/test_kick_threeway_blend.py
K9-batch 106 2026-10-09 i=0 d=0 r=0/0 eval/test_kick_zero_core_trial.py
K9-batch 44 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_bass_continuity.py
K9-batch 31 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_episode_trial.py
K9-batch 23 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_failure_casebook.py
K9-batch 62 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_fusion_error_audit.py
K9-batch 88 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_mixture_fit.py
K9-batch 99 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_timbre_features.py
K9-batch 30 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_prepare_kick_expanded_review.py
K9-batch 24 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_run_kick_complex_audit.py
K9-batch 16 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_run_kick_timbre_trial.py
K9-batch 69 2026-10-09 i=0 d=0 r=0/0 eval/verify_kick_controls.py
K9-batch 125 2026-10-09 i=0 d=0 r=0/0 eval/verify_kick_streaming.py
K9-test-live 135 2026-10-09 i=2 d=0 r=0/0 eval/test_kick_boosted_score.py
K9-test-live 90 2026-10-09 i=1 d=0 r=0/0 eval/test_kick_coverage_blend.py
K9-test-live 43 2026-10-09 i=0 d=0 r=0/0 eval/test_kick_evening_passages.py
K9-test-live 65 2026-10-09 i=1 d=0 r=0/0 eval/test_kick_interaction_score.py
K9-test-live 53 2026-10-09 i=0 d=0 r=0/0 eval/test_kick_kernel_score.py
K9-test-live 98 2026-10-09 i=0 d=0 r=0/0 eval/test_kick_training_coverage.py
K9-test-live 66 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_attack_rejection.py
K9-test-live 106 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_complex_onset.py
K9-test-live 34 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_dsp_experiments.py
K9-test-live 39 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_excess_balance_trial.py
K9-test-live 113 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_fusion_bandwise.py
K9-test-live 95 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_fusion_calibration.py
K9-test-live 57 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_fusion_features.py
K9-test-live 92 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_fusion_fine_calibration.py
K9-test-live 37 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_hybrid_experiment.py
K9-test-live 69 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_pcen_experiment.py
K9-test-live 59 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_shape_features.py
K9-test-live 64 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_stable_quadrature.py
K9-test-live 43 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_stable_rms.py
K9-test-live 153 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_subspace_score.py
K9-test-live 72 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_sustain_audit.py
K9-test-live 79 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_tonal_experiment.py
K9-test-live 62 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_trajectory_features.py
K9-test-live 90 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_kick_upper_cue.py
K9-test-live 53 2026-10-09 i=0 d=0 r=2/2 eval/tests/test_live_kick_baseline.py
K9-test-live 30 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_master_kick_comparison.py
K9-test-live 74 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_run_kick_fusion_trial.py
K9-test-live 82 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_run_kick_shape_trial.py
K9-test-live 42 2026-10-09 i=0 d=0 r=0/0 eval/tests/test_run_kick_trajectory_trial.py
KG 168 2026-10-11 i=0 d=0 r=2/1 eval/kick_goal_buildup.py
KG 97 2026-10-10 i=9 d=0 r=22/5 eval/kick_goal_featsets.py
KG 75 2026-10-10 i=0 d=0 r=2/1 eval/kick_goal_render_audit.py
KG 38 2026-10-10 i=1 d=0 r=4/0 eval/kick_goal_rules.py
KG 45 2026-10-10 i=3 d=0 r=7/0 eval/kick_goal_songnorm.py
KG 178 2026-10-10 i=1 d=0 r=7/0 eval/kick_goal_synth.py
KG 201 2026-10-10 i=2 d=0 r=11/9 eval/kick_goal_view.py
KG 50 2026-10-11 i=0 d=0 r=2/0 eval/kick_goal_wip_2425.py
KG 104 2026-10-10 i=0 d=0 r=5/3 eval/kick_night_scoreboard.py
KG 110 2026-10-10 i=0 d=0 r=2/1 eval/run_kick_goal_coldstart.py
KG 105 2026-10-10 i=0 d=0 r=3/1 eval/run_kick_goal_count_check.py
KG 68 2026-10-11 i=15 d=0 r=17/2 eval/run_kick_goal_data.py
KG 77 2026-10-10 i=0 d=0 r=6/3 eval/run_kick_goal_deadline.py
KG 271 2026-10-11 i=0 d=0 r=33/16 eval/run_kick_goal_fast.py
KG 117 2026-10-10 i=3 d=0 r=20/9 eval/run_kick_goal_frontier.py
KG 45 2026-10-10 i=0 d=0 r=4/2 eval/run_kick_goal_frontier_saved.py
KG 62 2026-10-10 i=0 d=0 r=4/0 eval/run_kick_goal_learning.py
KG 40 2026-10-11 i=0 d=0 r=6/0 eval/run_kick_goal_more_prep.py
KG 132 2026-10-10 i=0 d=0 r=21/1 eval/run_kick_goal_nn.py
KG 64 2026-10-10 i=0 d=0 r=7/0 eval/run_kick_goal_preds.py
KG 90 2026-10-10 i=0 d=0 r=4/4 eval/run_kick_goal_rescore.py
KG 64 2026-10-10 i=0 d=0 r=4/3 eval/run_kick_goal_rolls.py
KG 88 2026-10-10 i=0 d=0 r=3/2 eval/run_kick_goal_rules.py
KG 103 2026-10-10 i=4 d=0 r=13/4 eval/run_kick_goal_selfsim.py
KG 121 2026-10-10 i=0 d=0 r=27/12 eval/run_kick_goal_selfsim2.py
KG 70 2026-10-10 i=0 d=0 r=3/2 eval/run_kick_goal_shape.py
KG 108 2026-10-10 i=0 d=0 r=3/0 eval/run_kick_goal_show.py
KG 46 2026-10-10 i=0 d=0 r=6/4 eval/run_kick_goal_songnorm.py
KG 78 2026-10-10 i=0 d=0 r=4/1 eval/run_kick_goal_templates.py
KG 141 2026-10-10 i=0 d=0 r=10/0 eval/show_kick_labels.py
LAB 1 2026-10-11 i=0 d=- r=-/- lab/__init__.py
LAB 134 2026-10-11 i=1 d=1 r=29/9 lab/audit.py
LAB 176 2026-10-12 i=6 d=0 r=20/12 lab/cache.py
LAB 189 2026-10-12 i=2 d=1 r=79/17 lab/compare.py
LAB 255 2026-10-12 i=11 d=0 r=27/14 lab/components.py
LAB 182 2026-10-12 i=9 d=0 r=35/15 lab/config.py
LAB 243 2026-10-12 i=2 d=0 r=7/2 lab/contract.py
LAB 2 2026-10-11 i=0 d=- r=-/- lab/export/__init__.py
LAB 81 2026-10-11 i=1 d=0 r=0/0 lab/export/finder_kick.py
LAB 38 2026-10-11 i=1 d=0 r=0/0 lab/export/finder_snare.py
LAB 36 2026-10-11 i=1 d=0 r=2/0 lab/export/frames.py
LAB 20 2026-10-12 i=1 d=0 r=1/1 lab/export/kick2.py
LAB 165 2026-10-12 i=2 d=0 r=3/2 lab/export/measures.py
LAB 67 2026-10-11 i=1 d=0 r=31/3 lab/export/net.py
LAB 18 2026-10-12 i=1 d=0 r=1/1 lab/export/perc.py
LAB 46 2026-10-11 i=1 d=0 r=24/3 lab/export/stage.py
LAB 56 2026-10-11 i=2 d=0 r=2/2 lab/export/trees.py
LAB 122 2026-10-12 i=7 d=0 r=20/8 lab/kick_verdict.py
LAB 170 2026-10-12 i=7 d=0 r=24/15 lab/models.py
LAB 368 2026-10-13 i=4 d=1 r=54/42 lab/offsets.py
LAB 279 2026-10-12 i=3 d=1 r=36/19 lab/overnight.py
LAB 337 2026-10-14 i=1 d=1 r=20/15 lab/preflight.py
LAB 25 2026-10-11 i=15 d=0 r=12/10 lab/recipe.py
LAB 368 2026-10-12 i=14 d=1 r=64/42 lab/registry.py
LAB 730 2026-10-12 i=7 d=1 r=132/32 lab/release.py
LAB 370 2026-10-12 i=6 d=0 r=20/15 lab/remix.py
LAB 1427 2026-10-13 i=1 d=1 r=45/35 lab/review_project.py
LAB 597 2026-10-13 i=6 d=1 r=126/60 lab/run.py
LAB 270 2026-10-12 i=5 d=1 r=41/15 lab/score.py
LAB 138 2026-10-12 i=4 d=0 r=24/5 lab/stage.py
LAB 94 2026-10-12 i=3 d=0 r=24/6 lab/templates.py
LAB 110 2026-10-12 i=0 d=0 r=2/1 lab/test_coincident.py
LAB 1883 2026-10-13 i=0 d=1 r=138/101 lab/test_lab.py
LAB 257 2026-10-12 i=0 d=0 r=10/5 lab/test_overnight.py
LAB 231 2026-10-14 i=0 d=0 r=3/3 lab/test_preflight.py
LAB 99 2026-10-12 i=0 d=0 r=2/2 lab/test_remix.py
LAB 584 2026-10-13 i=0 d=0 r=6/6 lab/test_review_project.py
LAB-support 52 2026-10-11 i=0 d=0 r=8/7 kick_container.py
LIVE-TEST 152 2026-10-10 i=0 d=0 r=10/10 eval/tests/test_als_extract.py
LIVE-TEST 92 2026-07-17 i=0 d=0 r=0/0 eval/tests/test_bundles.py
LIVE-TEST 40 2026-10-11 i=0 d=0 r=0/0 eval/tests/test_detector_channels.py
LIVE-TEST 64 2026-10-11 i=0 d=0 r=2/2 eval/tests/test_detector_eval.py
LIVE-TEST 67 2026-10-10 i=0 d=0 r=5/5 eval/tests/test_kick_goal_fast_counts.py
LIVE-direct 313 2026-10-11 i=20 d=0 r=48/16 eval/als_extract.py
LIVE-direct 19 2026-10-11 i=3 d=0 r=6/0 eval/bass_labels.py
LIVE-direct 182 2026-07-17 i=5 d=0 r=1/0 eval/bundles.py
LIVE-direct 65 2026-10-11 i=9 d=0 r=8/4 eval/detector_cands.py
LIVE-direct 98 2026-10-11 i=7 d=0 r=6/3 eval/detector_channels.py
LIVE-direct 148 2026-10-11 i=3 d=0 r=16/11 eval/detector_compare.py
LIVE-direct 268 2026-10-11 i=21 d=0 r=40/23 eval/detector_eval.py
LIVE-direct 632 2026-10-13 i=18 d=0 r=93/55 eval/detector_labels.py
LIVE-direct 50 2026-10-11 i=5 d=0 r=17/7 eval/detector_net.py
LIVE-direct 32 2026-10-11 i=13 d=0 r=11/4 eval/detector_songs.py
LIVE-direct 198 2026-10-13 i=5 d=0 r=20/14 eval/detector_wip.py
LIVE-direct 141 2026-10-11 i=4 d=0 r=10/3 eval/generated_sets.py
LIVE-direct 355 2026-10-09 i=69 d=0 r=13/4 eval/kick_attack_rejection.py
LIVE-direct 126 2026-10-09 i=4 d=0 r=3/3 eval/kick_complex_onset.py
LIVE-direct 84 2026-10-10 i=31 d=0 r=17/2 eval/kick_fusion_bandwise.py
LIVE-direct 162 2026-10-10 i=14 d=0 r=19/0 eval/kick_fusion_features.py
LIVE-direct 90 2026-10-09 i=5 d=0 r=1/0 eval/kick_glide_features.py
LIVE-direct 585 2026-10-11 i=56 d=0 r=100/53 eval/kick_goal_eval.py
LIVE-direct 307 2026-10-10 i=23 d=0 r=47/19 eval/kick_goal_labels.py
LIVE-direct 47 2026-10-10 i=6 d=0 r=7/1 eval/kick_goal_lowbank.py
LIVE-direct 86 2026-10-10 i=15 d=0 r=17/9 eval/kick_goal_melodic.py
LIVE-direct 311 2026-10-12 i=17 d=0 r=80/39 eval/kick_goal_nn.py
LIVE-direct 48 2026-10-10 i=5 d=0 r=13/2 eval/kick_goal_profile.py
LIVE-direct 41 2026-10-10 i=3 d=0 r=3/0 eval/kick_goal_residual.py
LIVE-direct 68 2026-10-10 i=10 d=0 r=23/5 eval/kick_goal_selfsim.py
LIVE-direct 50 2026-10-10 i=3 d=0 r=2/1 eval/kick_goal_tail_features.py
LIVE-direct 73 2026-10-10 i=10 d=0 r=16/5 eval/kick_goal_templates.py
LIVE-direct 459 2026-10-11 i=4 d=0 r=9/2 eval/kick_goal_wip_sections.py
LIVE-direct 191 2026-10-13 i=5 d=0 r=37/15 eval/kick_labels.py
LIVE-direct 105 2026-10-09 i=6 d=0 r=2/0 eval/kick_upper_cue.py
LIVE-direct 248 2026-10-11 i=6 d=0 r=21/4 eval/note_labels.py
LIVE-direct 85 2026-10-12 i=3 d=0 r=7/4 eval/note_pitch.py
LIVE-direct 31 2026-10-11 i=4 d=0 r=5/1 eval/part_classes.py
LIVE-direct 51 2026-10-11 i=6 d=0 r=8/4 eval/snare_cands.py
LIVE-direct 57 2026-10-11 i=1 d=0 r=6/1 eval/snare_generated.py
LIVE-direct 77 2026-10-13 i=8 d=0 r=65/29 eval/snare_labels.py
LIVE-direct 20 2026-10-11 i=2 d=0 r=4/0 eval/synth_labels.py
LIVE-indirect 168 2026-10-09 i=29 d=0 r=0/0 eval/kick_boosted_score.py
LIVE-indirect 100 2026-10-09 i=4 d=0 r=0/0 eval/kick_coverage_blend.py
LIVE-indirect 74 2026-10-09 i=12 d=0 r=6/0 eval/kick_dsp_experiments.py
LIVE-indirect 142 2026-10-09 i=5 d=0 r=0/0 eval/kick_excess_balance_trial.py
LIVE-indirect 97 2026-10-09 i=18 d=0 r=2/0 eval/kick_fusion_calibration.py
LIVE-indirect 154 2026-10-09 i=4 d=0 r=1/0 eval/kick_fusion_fine_calibration.py
LIVE-indirect 99 2026-10-10 i=2 d=0 r=8/6 eval/kick_goal_project_kicks.py
LIVE-indirect 100 2026-10-10 i=1 d=0 r=3/2 eval/kick_goal_recall_labels.py
LIVE-indirect 58 2026-10-10 i=4 d=0 r=6/1 eval/kick_goal_rolls.py
LIVE-indirect 44 2026-10-10 i=1 d=0 r=1/1 eval/kick_goal_stereo.py
LIVE-indirect 164 2026-10-10 i=5 d=0 r=14/4 eval/kick_goal_trigger_labels.py
LIVE-indirect 80 2026-10-11 i=3 d=0 r=7/1 eval/kick_goal_wip_labels.py
LIVE-indirect 158 2026-10-09 i=8 d=0 r=0/0 eval/kick_hybrid_experiment.py
LIVE-indirect 107 2026-10-09 i=9 d=0 r=0/0 eval/kick_interaction_score.py
LIVE-indirect 114 2026-10-09 i=4 d=0 r=2/0 eval/kick_kernel_blend.py
LIVE-indirect 179 2026-10-09 i=7 d=0 r=1/0 eval/kick_kernel_score.py
LIVE-indirect 170 2026-10-09 i=29 d=0 r=7/1 eval/kick_night_common.py
LIVE-indirect 173 2026-10-09 i=10 d=0 r=4/0 eval/kick_night_diagnose.py
LIVE-indirect 69 2026-10-09 i=3 d=0 r=3/1 eval/kick_night_drum_onset_shape.py
LIVE-indirect 135 2026-10-10 i=3 d=0 r=5/0 eval/kick_night_dump.py
LIVE-indirect 153 2026-10-09 i=3 d=0 r=4/1 eval/kick_night_snap_labels.py
LIVE-indirect 108 2026-10-09 i=7 d=0 r=3/1 eval/kick_night_stem_audit.py
LIVE-indirect 108 2026-10-09 i=4 d=0 r=0/0 eval/kick_pcen_experiment.py
LIVE-indirect 58 2026-10-09 i=4 d=0 r=0/0 eval/kick_shape_features.py
LIVE-indirect 69 2026-10-09 i=2 d=0 r=0/0 eval/kick_stable_quadrature.py
LIVE-indirect 55 2026-10-09 i=2 d=0 r=0/0 eval/kick_stable_rms.py
LIVE-indirect 116 2026-10-09 i=18 d=0 r=0/0 eval/kick_subspace_score.py
LIVE-indirect 168 2026-10-09 i=5 d=0 r=0/0 eval/kick_sustain_audit.py
LIVE-indirect 64 2026-10-09 i=5 d=0 r=1/0 eval/kick_threeway_blend.py
LIVE-indirect 240 2026-10-09 i=8 d=0 r=2/2 eval/kick_tonal_experiment.py
LIVE-indirect 129 2026-10-09 i=8 d=0 r=0/0 eval/kick_training_coverage.py
LIVE-indirect 84 2026-10-09 i=26 d=0 r=2/0 eval/kick_trajectory_features.py
LIVE-indirect 192 2026-10-09 i=19 d=0 r=6/0 eval/live_kick_baseline.py
LIVE-indirect 95 2026-10-09 i=5 d=0 r=2/0 eval/master_kick_comparison.py
LIVE-indirect 143 2026-10-11 i=1 d=0 r=1/0 eval/note_clean.py
LIVE-indirect 121 2026-10-09 i=47 d=0 r=6/0 eval/run_kick_dsp_experiments.py
LIVE-indirect 156 2026-10-09 i=18 d=0 r=0/0 eval/run_kick_fusion_calibration.py
LIVE-indirect 231 2026-10-09 i=38 d=0 r=5/2 eval/run_kick_fusion_trial.py
LIVE-indirect 69 2026-10-10 i=11 d=0 r=9/3 eval/run_kick_goal_tail_component.py
LIVE-indirect 143 2026-10-09 i=21 d=0 r=0/0 eval/run_kick_shape_trial.py
LIVE-indirect 149 2026-10-09 i=22 d=0 r=1/0 eval/run_kick_trajectory_trial.py
LIVE-indirect 135 2026-10-09 i=4 d=0 r=1/0 eval/verify_kick_evening_passages.py
LIVE-pkg 13 2026-07-17 i=0 d=- r=-/- eval/__init__.py
LIVE-pkg 4 2026-07-17 i=0 d=- r=-/- eval/fetch/__init__.py
LIVE-pkg 0 2026-07-17 i=0 d=- r=-/- eval/tests/__init__.py
MRI 157 2026-03-23 i=0 d=0 r=0/0 tools/mrivol_to_tiff.py
MRI 124 2026-03-21 i=0 d=0 r=0/0 tools/nifti_to_mrivol.py
SEGMENTATION 218 2026-06-22 i=1 d=0 r=0/0 tools/segmentation/effects.py
SEGMENTATION 72 2026-06-22 i=1 d=0 r=0/0 tools/segmentation/matte.py
SEGMENTATION 126 2026-06-22 i=0 d=0 r=2/0 tools/segmentation/render.py
SEGMENTATION 387 2026-06-22 i=0 d=0 r=0/0 tools/segmentation/segment.py
```
