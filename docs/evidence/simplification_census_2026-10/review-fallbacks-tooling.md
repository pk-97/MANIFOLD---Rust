# Review: fallbacks and tooling

- **Reviewer:** Opus (claude-opus-5-5), read-only on code, light review (no random sample).
- **Main:** 090efbfa5. `git diff 37a9c180a HEAD -- crates scripts .claude` is empty, so every screen still holds.
- **Files reviewed:** `fallbacks.md`, `fallbacks.proof.md`, `tooling.md`, `tooling.proof.md`. Only grade cells in `fallbacks.md` were changed; nothing in `tooling.md`.
- **Scratch helpers** (session scratchpad, not in the repo): `overlaps.jq`, `delete_counts.sh`, `window_counts.sh`, `history_windows.sh`, `history_windowed.sh`.

## The finding that changes grades: old shapes live on in archive history

The fallbacks sweep's window test dates each file by its last save. A V2 archive also keeps up to 50 autosaves plus every manual save as `history/<hash>.json.gz`, and File → Revert to Snapshot loads them through the full migration ladder (`crates/manifold-io/src/loader.rs:107-116` calls `load_project_from_json_with` on the snapshot JSON). Deleting a migration breaks reverting to any snapshot saved before it, and the corpus dumps only `project.json`, never the history.

I read the history of the 88 local archives with a manifest in `~/Downloads` and `tests/fixtures` (5,746 snapshot entries; Dropbox not touched) and probed every snapshot dated 2026-05-08 to 07-18:

| row | shape | snapshots that carry it |
|---|---|---|
| fallbacks-002 | `"WireframeDepthGraph"` | GraphTestsV4 29, graphTests 26 |
| fallbacks-003 | generator saved as effect kind | GraphTestsV4 8, audioTesting 30 (partial jq pass, a floor) |
| fallbacks-004 | feature kind `brightness`/`liveliness` | audioTesting 70 |
| fallbacks-005 | send `source: {layer}` | audioTesting 135 (one checked by hand: 1.9.0, `[{"layer":"dd229493"}]`) |
| fallbacks-006 | glTF anim tables | TripMachine 5 |
| fallbacks-018 | `audioTrigger` | NewAudioTesting 8 |
| fallbacks-009 | `userParamBindings` | feedbackTests 87, nodeTests 2, graphTests 1 |

Every hit is in a Downloads project, mostly test projects. The online-only show archives were not opened, and their history can hold any of these shapes whatever their last-save date. So the "0 files carry it" premise behind the six B deletes is wrong.

Method trap for census section 3.8 (Silent fallbacks, legacy paths, migrations and their tests): a migration is live while any archive's history holds its input, not only the archives' current `project.json`.

## Grade changes (`fallbacks.md` only)

| row | was | now | why |
|---|---|---|---|
| fallbacks-002, 003, 004, 005, 006, 018 | B | C | History snapshots in local archives carry each shape (table above). Deleting breaks Revert to Snapshot on those entries. Whether old snapshots must keep reverting is Peter's call. |
| fallbacks-024 | C | B | Rejecting an edit whose target is gone is an implementation call, not a stage or look call; `fire_param.rs` already does it. |
| fallbacks-036 | C | B | Reporting and dropping 2 orphan bindings in one test project is an implementation call. |
| fallbacks-042 | C | B | Counting dropped param ids into `load_report` is reporting only. |
| fallbacks-049 | C | B | The defaults differ by design (PARAM_STEP_ACTIONS decision D3: Fire and Step/Random rows use Transient, gate and continuous rows use Both). The rule is written out in 9 places; merging them into one owner is a refactor. No disagreement exists today; the row's "can disagree" is a drift risk only. |

Knock-on: the `fallbacks.md` Lines paragraph is now stale. Grade B migration lines drop from about 1,530 to about 530 (fallbacks-001, 007, 008). The other 996 lines move to C.

Wording errors that don't change a grade:
- fallbacks-005's proof column says no corpus project has any send `source`. 19 do, all in the current `{layers: [...]}` shape at 1.10.0 or later. No corpus file has a send below 1.10, so the corpus conclusion stood until history was counted.
- tooling-002's row says its references are dev.py and one Rust comment. `docs/RT_QUALITY_SETTINGS_DESIGN.md:123` also names both scripts as deliberately kept inert warnings. The deleting lane edits that line too. A stands, because a doc mention is a mention.
- tooling-005 doesn't cite the open bead that already owns it: child 6.5 of BUG-hkbdp (Test suite review and reduction after the crate split), "Water Cargo hygiene and retirement of the T2 move scaffolding", names `.claude/orchestration/crate-split/` plans and templates for retirement. Per census section 4 (The candidate record), the delete rides on BUG-hkbdp.6.5 with that bead's fix shape. It is not a second finding.
- tooling-003 and tooling-013 are already in tonight's draft `docs/evidence/overnight_2026-10-19/PLAN.md` step 6 (process-layer deletions).

## Delete-migration corpus counts, re-run

Whole-corpus counts (182 projects), a superset of the sweep's below-rung counts:

| row | re-run | matches sweep |
|---|---|---|
| fallbacks-002 | `"WireframeDepthGraph"` 0 projects, 0 assets; online-only in window 0 | yes |
| fallbacks-003 | genParams with `effectType` and no `generatorType` 0; NONE type with graph id 0; online-only 0 | yes |
| fallbacks-004 | `"kind": "brightness"/"liveliness"` 0; online-only 0 | yes |
| fallbacks-005 | legacy `{layer}` or `"capture"` source 0; no sends below 1.10 at all; online-only 0 | yes (wording above) |
| fallbacks-006 | all 10 table keys 0 in projects and assets; online-only 0 | yes |
| fallbacks-007 | bloomAmount 45, feedbackAmount 53, strobeMode 43, masterEffectOrder 44, genRotSpeedXY 49, param0 40; above 1.0.0: 0 each. No `deny_unknown_fields` in core or io | yes |
| fallbacks-008 | `"durationMode"` in 80 projects; Deserialize ignores the value (`types.rs:911-915`), so all already load as NoteOff | yes (sweep counted 63 plain-JSON sources) |
| fallbacks-018 | `"audioTrigger"` 0 projects, 0 assets; online-only dated July 0 | yes |
| fallbacks-009 (C, for reference) | `"userParamBindings"` 0; online-only in window 1 (V6 VERTICAL) | yes |

Online-only dates came from `stat` metadata, 67 files, 2026-02-13 to 09-28. No file was opened.

## Bugs, not slop

Each was confirmed by reading the code path unless marked. None was reproduced at runtime.

1. **Opening an old show deletes clips.** `crates/manifold-io/src/loader.rs:492` calls `repair_overlapping_clips` (498-540). It removes the shorter clip of every overlapping pair, and the earlier one when both are the same length. Re-run of the overlapping-pair counts: WAYPOINTS 297, PATTERN 573, STAGNATE 983, STAGNATE V3 3,232, WAYPOINTS VERT 229. That is 38 corpus files, all 1.0.0 (32) or 1.1.0 (6). On stage: an old show opens with hundreds of clips gone, the toast shows only a count, and the first save makes it permanent. How to repair overlaps is Peter's call: trim the earlier clip to the next start, or delete as now.
2. **Export throws away every command except Cancel.** `crates/manifold-app/src/content_export.rs:440-447` drains the channel and keeps only CancelExport. On stage or in prep: quit during an export and the Shutdown is eaten. The window then blocks on the content-thread join (`app.rs:2078-2082`) until the export ends, and may hang after it, because cloned senders can keep the channel open (not reproduced). Any edit or seek sent during the export is lost. The warmup loop re-queues Shutdown for exactly this reason (`content_commands.rs:338-342`).
3. **A save can wipe the snapshot history.** `crates/manifold-io/src/archive.rs:228-237`: `read_manifest` turns any read or parse error into None. Line 97 `unwrap_or_default()` then starts an empty history, and the copy at 153-154 keeps only live hashes. In prep: one save over a show file whose manifest is damaged silently empties File → Revert to Snapshot.
4. **Region delete lands on layer 0.** `crates/manifold-editing/src/service.rs:357-359` takes the region's layer range from `start_layer_id`/`end_layer_id` only, with `unwrap_or((0, 0))`. `crates/manifold-core/src/selection.rs:110-125` collapses a missing edge onto the other. `crates/manifold-ui/src/ui_state.rs:1204-1216` prunes only `selected_layer_ids`. In prep: delete a region's top and bottom layers, then Delete, Split, Duplicate or Copy, and it hits layer 0's clips in that time range. Delete one edge only and it hits one layer instead of the range.
5. **Opening a project reorders queued commands.** `crates/manifold-app/src/content_commands.rs:345-349` pops one queued command per warmed layer and re-sends it to the back. On stage: on every open, SeekTo, ResizeContent and SetProject (queued behind LoadProject) come out rotated by the layer count, and two edits sent while a show opens can run in the wrong order.
6. **Edits on a stale target record dead undo steps.** `Command::was_applied` defaults to true (`crates/manifold-editing/src/command.rs:49-51`). `execute_with_validation` (`undo.rs:37-58`) then pushes the step and clears redo. Sample read: the blend mode and opacity commands (`commands/settings.rs:616-670`) do nothing on a missing layer and still count as applied. Of 199 command impls under `commands/`, 54 override `was_applied`. The sweep's "about 101 with a bail-out" is approximate but the right size. In prep: the edit does nothing, the next Cmd+Z undoes nothing, and redo history is gone.
7. **Corrupt enum values load silently as defaults.** `crates/manifold-core/src/types.rs:190-215` maps an unknown LayerType number, string or JSON type to Video. The same catch-all sits in 9 more deserializers in `types.rs` (72, 265, 322, 378, 424, 556, 616, 846, 1013) and in `percussion_analysis.rs:41`. `preset_type_id.rs:313, 331` turn a non-integer number into discriminant 0 (Transform for effects). On stage: a damaged or hand-edited file opens with a layer turned into Video or an effect turned into Transform, and the next save makes it permanent. The old reason is gone: `loader.rs:183-195` refuses newer files before deserialize.
8. **Pasted video renders black** (fallbacks-033). Paste goes through `process_dropped_files` (`input_host.rs:1509-1540`). Its video branch (`project_io.rs:1063-1100`) adds the library entry and any new layer to the UI copy only; only the AddClip command reaches content. A Finder drop routes video through `import_video_files` instead (`app.rs:2275-2277`). In prep: Cmd+V a video from Finder and the clip plays black.
9. **Unreadable audio becomes a 0-beat clip** (fallbacks-034). `project_io.rs:1426-1438` returns zero seconds on a decode error. In prep: a bad file drops as an empty clip, and Replace Audio saves duration 0.
10. **Dead fader on a generator card** (fallbacks-029). `crates/manifold-node-engine/src/runtime/build.rs:455-470` drops a binding whose node is gone without a chain error; the effect path records one. On stage: the card shows a fader that moves nothing. The sweep counted 11 such bindings in 3 projects; not re-run.
11. **Add Graph Node on a missing target** (fallbacks-027). `commands/graph/node_edit.rs:135-139` only prints, so a dead undo step is recorded.
12. **Undo that can't find its graph silently skips** (fallbacks-025). 33 `let _ = with_existing_target_graph_mut` sites, count re-run. The step still moves to redo, so history and project disagree.
13. **Helper claims, not re-read by me:** fallbacks-026 (a binding edit writes the private graph, then bails; the layer stops tracking its preset), 030 and 031 (owned by BUG-38rup (Effect chains: effects depend on neighbours, silent fallbacks kill params)), 037 (scene-modifier manifest and graph drift after a failed reconcile), 038 (a malformed physical object splits without physics), 047 (preview can show the wrong node for a stale scope).

Slop, not bugs: fallbacks-001, 032, 035, 039 to 046, 049, 050.

## Dispositions: every B and C row

`bead` means the lead logs it: a decision bead for Peter, or the existing bead named. For migrations, "convert" means opening and saving the file once in a current build. A history snapshot inside an archive is not converted by that.

### fallbacks (45 rows: fix 24, bead 17, delete 4)

| id | disposition | the fact that decides it |
|---|---|---|
| fallbacks-001 | fix | Every ladder gate reads the version taken once at `migrate.rs:17`. The 21 middle stamps are dead writes, so no file is affected. |
| fallbacks-002 | bead | No project file, but 55 history snapshots in two Downloads test archives carry it. Deleting costs reverting to those (and to any in the unopened show archives). |
| fallbacks-003 | bead | No project file; at least 38 snapshots in GraphTestsV4 and audioTesting carry it. Same cost as 002. |
| fallbacks-004 | bead | No project file; 70 snapshots in audioTesting. Same cost. |
| fallbacks-005 | bead | No project file; 135 snapshots in audioTesting. Same cost. |
| fallbacks-006 | bead | No project file; 5 snapshots in TripMachine. Same cost. The pass also runs at every graph build. |
| fallbacks-007 | delete | The 41 flat fields sit only in 1.0.0 files and nothing reads them but the audit fingerprint. Without `deny_unknown_fields`, old files and snapshots load the same. Peter loses nothing. |
| fallbacks-008 | delete | Deserialize already ignores the value, so all 80 files that store it load as NoteOff today. Nothing changes. |
| fallbacks-018 | bead | No project file; 8 snapshots in NewAudioTesting. Same cost as 002. |
| fallbacks-020 | fix | Bug 2. |
| fallbacks-021 | fix | Bug 3. |
| fallbacks-022 | fix | Bug 4. |
| fallbacks-023 | fix | Bug 5. |
| fallbacks-025 | fix | Bug 12. Reject and drop the step rather than `expect`: the non-undo writers (percussion replan, recording, MIDI commit) were never audited, and a panic on stage is worse than a skip. |
| fallbacks-028 | fix | Bug 7. |
| fallbacks-029 | fix | Bug 10. The effect path already records this case as a chain error. |
| fallbacks-031 | bead | BUG-38rup owns it (open, P1). |
| fallbacks-032 | fix | `Option<Project>` is None only after `shutdown()`, which only a test calls. About 200 lines of None arms go. |
| fallbacks-033 | fix | Bug 8. |
| fallbacks-034 | fix | Bug 9. |
| fallbacks-035 | delete | 0 corpus graphs and 0 of 113 bundled presets lack metadata. Check the GLB import's from-scratch graph (`scene.rs:2767`) first. |
| fallbacks-037 | fix | `preset/modifier.rs` already reconciles before commit; the 48 refresh callers should do the same. |
| fallbacks-038 | fix | `.ok()` merges "not physical" with "malformed", so a broken physical object loses physics on split. |
| fallbacks-039 | fix | A splice that fails returns None, and the missing-output case logs nothing. |
| fallbacks-040 | fix | The second flatten's `.ok()` skips the binding-capture pass and its lints with no error. |
| fallbacks-041 | fix | 0 of 138 saved exposures are off-surface, so a strict error breaks no saved file. Gap: dynamic-surface nodes. |
| fallbacks-043 | fix | A damaged show file reports a UTF-8 error instead of the ZIP error. |
| fallbacks-044 | fix | Load repairs overlaps, so an overlap after an edit is a write-path bug: make it a `debug_assert!`. |
| fallbacks-047 | delete | Its strict twin `resolve_preview_target_with_scope` exists, and all 2,939 saved groups carry a nodeId. |
| fallbacks-050 | fix | Stored clamp 0.01..1, render clamp 0.25..1, and the UI offers only 1, 0.75 and 0.5: one clamp at the edit. |
| fallbacks-009 | bead | No project file. The online-only Liveschool V6 VERTICAL (05-13) is in the window, plus 90 local snapshots. Converting means downloading V6 VERTICAL once. Goes with 013. |
| fallbacks-010 | bead | 1 test project, plus 3 online-only files in the window (Corrosion V6, Corrosion No Thistles, Head Split). Converting those three frees about 1,000 lines. BUG-cavux (Math View migration journey tests red on the scene camera notice) owns the red tests. |
| fallbacks-011 | bead | No project file; 5 online-only artwork files in the window. Converting them frees 171 lines. |
| fallbacks-012 | bead | No project file; 3 online-only June album-art files. Converting frees 118 lines. |
| fallbacks-013 | bead | The same V6 VERTICAL file as 009; 55 lines. |
| fallbacks-014 | bead | All 53 fork ids are already named; 3 online-only files in the window; 79 lines. |
| fallbacks-015 | bead | 1 test project plus 48 online-only 3D projects, the Corrosion and Right Where I Need You videos among them. Converting 48 files frees about 1,150 lines. |
| fallbacks-016 | bead | 3 Downloads test projects plus 37 online-only files; about 860 lines. Running it twice (at load and at every build) can be fixed now regardless. |
| fallbacks-017 | fix | Corrosion V5 (saved 09-11) still holds `node.channel_mix`, renamed 07-03, because embedded presets are never rewritten. Baking every graph at load is the only way the 114-entry table can ever shrink. The convert-once that follows is Peter's call. |
| fallbacks-024 | fix | Bug 6. `fire_param.rs` is the model. |
| fallbacks-030 | bead | BUG-38rup owns it. |
| fallbacks-036 | fix | 2 orphan bindings in one test project. Report and drop them; keep identity for Composite. |
| fallbacks-042 | fix | About 5 lines to count the drops into `load_report`. |
| fallbacks-048 | bead | Every constructor passes `video_clip_id: None`. Is video-clip percussion placement a feature? Only Peter knows. |
| fallbacks-049 | fix | Same rule written out in 9 places; one owner function. |

### tooling (22 rows: bead 8, keep 7, delete 5, fix 2)

| id | disposition | the fact that decides it |
|---|---|---|
| tooling-006 | delete | SCENE_RENDER_MODE is SHIPPED and its contract doesn't name either probe; 0 runs in 30 days. Gap: the probe graphs weren't searched. |
| tooling-007 | delete | The design it served is COMPLETE and no gate or test uses it; 0 runs. Delete it together with the `render_import --dump-def` mode in one lane. |
| tooling-020 | delete | A dated measurement; the bead it served, BUG-iadf (RT denoise 1:1 mode cost), is closed; git keeps the table. |
| tooling-010 | fix | `chmod +x` the four scripts. The live-UI server (`live_ui.rs`) is live, UI_AUTOMATION_DESIGN is IN PROGRESS, and these are its only clients. |
| tooling-011 | fix | `chmod +x` and keep: the app's own Blob V2 report names it (`blob_v2_performance.rs:577`), and BUG-7bi8 (measure Blob V2 allocation events) is open. |
| tooling-012 | keep | RT_QUALITY_SETTINGS_DESIGN is IN PROGRESS and names the quality oracle; UI_RELIABILITY_AUDIT cites the toggle matrix with open beads. |
| tooling-013 | delete | CLAUDE.md names `land_branch.py` as the landing path, and `land_wave.py` has no lock and no land-pass record. 0 real landings in 30 days; tonight's draft plan step 6 already lists it. Edit the AGENT_ROUTING and GIT_TREE_DISCIPLINE mentions in the same lane. |
| tooling-014 | bead | Rides on Peter's depth relight call (delete it or turn it back on). |
| tooling-015 | keep | It installs Peter's AbletonOSC patch by hand; an install tool shows no agent runs by nature. |
| tooling-016 | bead | Fires on every prompt, 78 ms mean and 7.4 s worst, for 107 injections in 5,190 fires. Memory already sends "we discussed this" to a raw-transcript grep. Not a hard-rule hook. |
| tooling-018 | keep | An open bead names each file; delete each as its bead closes. |
| tooling-021 | bead | 13 designs that were never built, 0 reads in 30 days. Only Peter knows the roadmap. |
| tooling-022 | keep | CORE_ENGINE_F5_SCOPING is the input to F5, which the OPEN CORE_ENGINE_FINDINGS queue defers to a design pass. The DRAG_SURFACES_SURVEY half (65 lines, inbound only from BUG_BACKLOG) can go. |
| tooling-023 | bead | Each of the 42 files has a live reference. Keeping an archive folder or letting git hold it is a policy call. |
| tooling-024 | keep | The SHIPPED crate-split design cites `crate-split-seams.md` twice as its site detail; `parked.md` is the godfile campaign's parked list, pointed to by memory. |
| tooling-025 | bead | A standalone LED mirror outside the workspace, last commit 05-13. Only Peter knows if the rig still uses it. Ask in one bead with 026 to 028: "which hand-run tools do you still use". |
| tooling-026 | bead | Only the plugins member list names it. Same bead. |
| tooling-027 | bead | A double-click launcher with 0 references. Same bead. |
| tooling-028 | bead | `typos` is installed and nothing runs it. Same bead. |
| tooling-029 | delete | The no-other-providers decision covers it. It is untracked, so git can't restore it: Peter's yes first. |
| tooling-030 | keep | CLAUDE.md bans rust-analyzer only in agent sessions; Peter's editor may read it, and it is 21 lines. |
| tooling-031 | keep | The APPROVED import-anything design names both scripts. They run inside Blender, so no run log can show use. |

### tooling grade A rows, re-checked

`rg --hidden` for each name over `.claude`, `scripts`, `docs`, `crates`, `tests`, `tools`, `.config`, `CLAUDE.md`, `Cargo.toml`, plus the project and user `settings.json` and `settings.local.json`; `git log -1` on each file. No A row is a hook that enforces a CLAUDE.md hard rule.

| id | references found | last commit | holds |
|---|---|---|---|
| tooling-001 | own file, dev.py, `PRIMITIVE_LIBRARY_DESIGN.md:820` ("not the right tool") | a11afb344 2026-10-09, a mechanical path update | A |
| tooling-002 | dev.py, own files, `RT_QUALITY_SETTINGS_DESIGN.md:123`, census doc, `rt_capture.rs:426` comment; `MANIFOLD_RT_NATIVE_TERMS` only in a "went out" comment | 285e4ffb5 2026-10-01, the GPU queue sweep | A, edit the design line too |
| tooling-003 | only tonight's draft PLAN.md (which deletes it); in no settings file | 047219c7b 2026-03-19 | A |
| tooling-004 | census doc only | 3b8344890 2026-03-29 | A |
| tooling-005 | `gate_policy.py` and its 3 importers, `test_gate_readiness.py`, the gate-p1 fixture, BUG-hkbdp.6.5 | e4a5aa0b8 2026-10-10 | A, under BUG-hkbdp.6.5 |
| tooling-008 | no `__main__`, mode 644; imported by tests; ported by a Rust test | a0d1bd473 / 8a614f614 2026-10-03 | A |
| tooling-009 | mode 644 on both; `trunk_health.py` and `hook_telemetry.py` call hook_census; codex_regressions imported by 2 gate scripts | e808084a4 2026-10-03 / 8bd3c24c1 2026-09-10 | A (chmod) |
| tooling-017 | all 15 stems: no live reference in repo, memory or commands; no open bead | n/a | A |
| tooling-019 | generated index only. Its last edit, 92c4f58e6 2026-10-13, was a one-cell fix from an unrelated lane: the doc costs upkeep | 92c4f58e6 | A |

## What I did not check

- The online-only Dropbox archives and the 22 Dropbox-local ones: neither their `project.json` nor their history. Every migration verdict still turns on them.
- History snapshots outside the 05-08 to 07-18 range, and fallbacks-003 beyond a partial pass I stopped for time, so its count is a floor. What each old shape does on a revert without its migration (dropped and reported, or a failed load) was not checked shape by shape.
- Corpus counts for keep rows and for the C migrations other than 009 and the window dates.
- Rows I did not re-read in code: fallbacks-026, 030, 031, 035 to 048 (helper claims as recorded), and the A rows 045 and 046.
- Runtime reproduction of anything. Bugs 2, 4, 5 and 8 are the first to reproduce.
- Tooling B and C rows beyond the facts named above. No transcript re-count and no verb re-run.
