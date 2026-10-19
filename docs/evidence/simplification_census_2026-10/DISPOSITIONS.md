# Simplification census: dispositions

One line for every B and C row in the sweeps of BUG-yn979 (Simplification pass): 233 rows, with grades as the reviewers left them. The disposition comes from the review that covers the sweep. delete: remove it. keep: it stays. bead: the lead logs it, or the bead named already owns it. fix: a bug or a reshape, not a plain delete. tools-lab has no review, so its rows say unreviewed, or "delete (sweep, unreviewed)" where the sweep's own action decides it.

Parent beads named once: BUG-mmct0 (Playback pre-Fable code audit pilot), whose child .12 is the playback audit hygiene bead; BUG-hkbdp (Renderer crate split epic), whose .6 children are the water cleanup.

Conflicts for the lead:
- rescreen-011 says "delete, lands with water-009", but the water review keeps water-009.
- patterns-016: the patterns review says `SceneExposureSource` is a seam named in the crate-split design's do-not-reopen list. Tonight's plan, lane 6, deletes it. `NodeParamProvider` can go either way.
- engine-049: the engine review deferred to the patterns review, which says delete.

## engine-model (21 rows, review-engine-graph.md)

| id | grade | disposition | deciding fact |
|---|---|---|---|
| engine-014 | B | delete | 3 getters read only by 7 test asserts, which can read the fields |
| engine-019 | B | delete | `is_unknown` has one assert caller; the one nodes test can spell the id |
| engine-028 | B | delete | `with_forced_def_write` has no caller, so `force_def_write` is always false |
| engine-029 | C | bead | BUG-nwjg8 (Physics boundary P6a: rigid add, enable, split and duplicate) owns the add-with-physics path |
| engine-030 | B | delete | `generator_change::build_change` replaced the app caller; the command has no LED-specific logic |
| engine-032 | B | delete | the slip UI was removed on purpose; Swap and ClipEffects never had an app constructor |
| engine-033 | B | delete | the card's expose button builds `ToggleNodeParamExposeCommand`; repoint the app e2e test sites |
| engine-034 | B | delete | nothing removes an envelope in the product; the UI adds or toggles |
| engine-035 | B | delete | the layer header toggles analysis-only as a deliberate non-undo perform toggle |
| engine-036 | B | delete | selection region is UI state; nothing sends it as a command |
| engine-037 | C | keep | multi-display design in progress; its UI phases are unbuilt |
| engine-038 | C | keep | Session mode design in progress and names all 7 commands |
| engine-039 | C | split: delete Channels, keep Analysis | capture-fed sends are always stereo; send pitch analysis is the planned activation gate |
| engine-040 | C | bead | `lane.enabled` is read by the inspector and written only by this command: build the lane toggle or delete both (Peter's call) |
| engine-041 | C | fix | the core engine map promises a BPM rescale nothing performs; fix the doc and delete the command once Peter confirms clips keep their bar |
| engine-043 | B | delete | a 3-line wrapper; inline it in 3 history tests and fix PROJECT_IO_MAP.md |
| engine-045 | C | keep | multi-display venue file, same reason as engine-037 |
| engine-046 | C | bead | the relight call; it also decides graph-026 and patterns-024 |
| engine-047 | C | keep | documented diagnostic switch for the device gate |
| engine-048 | C | keep | UI_INVALIDATION_DESIGN.md names it as the live-run gate |
| engine-049 | C | delete | patterns review: `bench-timing` gates two wall-clock tests no gate runs |

## engine-runtime (19 rows, review-engine-graph.md)

| id | grade | disposition | deciding fact |
|---|---|---|---|
| engine-runtime-005 | B | fix | the log hooks are never installed, so four playback errors never print; use `log::error!` and `log::warn!` |
| engine-runtime-006 | B | bead | BUG-mmct0.12 item d owns it; MIDI launch quantization was fixed another way, so delete and keep both `else` arms |
| engine-runtime-007 | C | fix | live prewarm is a stub, so a cold pad can black-flash; no bead yet |
| engine-runtime-008 | C | bead | core engine finding F8 needs a measurement before wire or delete |
| engine-runtime-011 | B | delete | the Max for Live sender path is unused and its deletion was deferred, not refused |
| engine-runtime-014 | B | delete | `use_native_clock_note_events` only ever holds its initial value |
| engine-runtime-015 | B | delete | the per-layer `Layer.midi_device` filter is the live one |
| engine-runtime-021 | C | bead | BUG-mmct0.12 names the setter; random cue start stays only if wanted as a perform feature |
| engine-runtime-024 | B | fix | the flag goes, and the MIDI backlog warning becomes unconditional and rate-limited |
| engine-runtime-025 | B | delete | after engine-runtime-001 |
| engine-runtime-027 | B | delete | the unbuilt `FrameLease::deliver` can be written fresh |
| engine-runtime-030 | B | delete | its only caller is its own test |
| engine-runtime-031 | B | delete | no comparison CLI was ever built |
| engine-runtime-035 | C | keep | the suppression names its condition, a non-macOS encoder |
| engine-runtime-036 | C | keep | the Demucs settings are never built, so these env knobs are the only live Demucs config |
| engine-runtime-037 | C | keep | live diagnostics named in two design docs |
| engine-runtime-038 | C | keep | tool-path overrides, not knobs |
| engine-runtime-039 | C | keep | BUG-ycmj2 (Integrate Signalsmith for live audio warp and matching export) is deferred, not dropped |
| engine-runtime-042 | C | keep | LED_STRIPS_DESIGN.md plans sACN beside Art-Net; no saved file carries the key |

## graph (7 rows, review-engine-graph.md)

| id | grade | disposition | deciding fact |
|---|---|---|---|
| graph-020 | B | fix | written every blend pass, never read; delete it, but not with the blind deletes (stage path) |
| graph-021 | B | delete | 0 `PluginPrewarm` submissions and 0 `PostProcessEffect` impls since plugin effects became graphs |
| graph-022 | B | delete | its reader went with the CPU FLIP node; the one submission is read only by its own test |
| graph-024 | B | delete | remove both allows; keep one only where the compiler names a field |
| graph-025 | B | delete | rewrite the two shadow-math fixtures to `Light::sun` |
| graph-026 | C | bead | the relight call; 0 of 182 projects contain "relight" |
| graph-027 | C | keep | the patterns sweep judges the 16 knobs; no repo script sets any |

## rescreen (9 rows, review-engine-graph.md)

| id | grade | disposition | deciding fact |
|---|---|---|---|
| rescreen-001 | B | delete | 5 wrappers whose inner fns are what the product calls; repoint the tests |
| rescreen-002 | B | delete | the 3 staged evaluators move into `mod tests`; no product caller |
| rescreen-008 | B | delete | owner keys are id-derived, one per call site, so per-owner cleanup is never needed |
| rescreen-009 | B | delete | both lookups are test-only and named as superseded |
| rescreen-010 | C | bead | reclassify `node.watercolor` as a fused bundle, then delete the ledger (or delete with content-001) |
| rescreen-011 | B | delete | lands with water-009, which its own review keeps (conflict above) |
| rescreen-012 | C | keep | the physics boundary design lists `CompletionLedger` as boundary API |
| rescreen-013 | C | keep | the CPU reference the GPU timestep tests compare against |
| rescreen-014 | C | fix | the stiffness fit is stubbed, so the substep cap is silent; wire it as designed |

## water (13 rows, review-app-water-content.md)

| id | grade | disposition | deciding fact |
|---|---|---|---|
| water-008 | B | delete | test-only; the publish gate reads the stats on the GPU |
| water-009 | B | keep | a test seam: deleting it makes 8 tests build an evaluated frame, so test code grows |
| water-010 | B | delete | already ruled dead on BUG-hkbdp.6.5; about 100 lines across graph and water files, counted once |
| water-011 | C | delete | no caller since its example went; the boundary design lists creation API, not this knob (engine API, Peter's call) |
| water-012 | C | keep | the boundary design lists `add_triangle_mesh`; it is the only collider for concave ground |
| water-013 | C | delete | the only product user calls `with_capacity` |
| water-014 | C | delete | unreviewed checkpoint code, test-only, no doc or bead names it |
| water-015 | C | keep | BUG-hkbdp.6.1 keeps the reference engine; these setters configure its proofs |
| water-016 | C | bead | our own two-way coupling reference; decide with BUG-65r39 (manifold-fluids proof-only) whether it stays |
| water-017 | C | bead | same decision as water-016 |
| water-018 | C | bead | the app links 2,430 lines of probe C++ it never calls; BUG-65r39 stops the link |
| water-019 | C | bead | the oracle feature does not compile and no gate builds it: fix and gate it, or delete it |
| water-020 | C | delete | no script, doc, hook or bead names either example |

## app-ui (21 rows, review-app-water-content.md)

| id | grade | disposition | deciding fact |
|---|---|---|---|
| app-ui-005 | B | delete | written once on save-as, read by nothing; no save flash was ever drawn |
| app-ui-006 | B | delete | only the local texture pane is ever built; the Bridged path has no producer |
| app-ui-007 | B | delete | stored, never read, and the window design forbids branching on it |
| app-ui-008 | C | keep | the Session mode grid UI sends exactly these six commands |
| app-ui-010 | C | keep | the Vulkan design is approved; 45 lines of Phase 1 scaffold |
| app-ui-011 | C | bead | only a compile can say which of the 542 `unsafe` blocks are redundant |
| app-ui-012 | C | keep | the RT design has every BLAS retain its triangle descriptor |
| app-ui-013 | C | bead | delete the probe when BUG-l7t4 (audition-grid page fault) closes; armed, it leaks |
| app-ui-015 | B | delete | three setters with no call; BUG-ns5k (Benchmark legacy-path MetalFX denoised scaler) measures a cost they don't touch |
| app-ui-019 | B | delete | neither has a caller; edit the two design docs that list them |
| app-ui-024 | C | delete | the slider's own zone table already carries the three gestures; nothing calls `intent_for` |
| app-ui-043 | B | delete | resets are instant since BUG-061 (param right-click reset folded into SliderReset); the tick runs every frame for nothing |
| app-ui-049 | B | delete | no caller; edit the two design lines that list it |
| app-ui-050 | B | delete | no caller; the forbidden-pattern needle goes with it |
| app-ui-052 | B | delete | the timeline projection reads hide, pin and order; the four fns are test-only duplicates |
| app-ui-058 | B | delete | test-only; keep the `Icon::Layer*` variants, because atlas ids follow declaration order |
| app-ui-060 | C | delete | BUG-6in (effect-group headers, folding and collapse animation) is deferred with no design; git keeps it |
| app-ui-061 | C | delete | shadows are off by design since July; the switch keeps about 100 lines for a rejected look (in the report as Peter's call) |
| app-ui-062 | B | keep | gating 21 test accessors behind `cfg(test)` removes no lines |
| app-ui-063 | B | keep | same for 10 GPU accessors; three are named in the Vulkan design |
| app-ui-064 | C | bead | the env-variable list was input for the patterns sweep |

## content (28 rows, review-app-water-content.md)

| id | grade | disposition | deciding fact |
|---|---|---|---|
| content-001 | B | delete | Watercolor on stage is the atom-built preset; the wrapper is in no preset and no project |
| content-002 | B | delete | a shelf atom whose one wiring made pooling worse; never in a preset |
| content-003 | B | delete | `render_scene` instancing replaced it, and it drops material fields |
| content-004 | B | delete | GPU FLIP folded the solve into one node; both atoms postdate every online-only project |
| content-005 | B | delete | a Math View leftover, out of its preset the day it landed |
| content-006 | B | delete | four load-only aliases become four rename rows with seeded params |
| content-007 | C | keep | no port-rename migration exists; it rides the old-format call |
| content-008 | C | delete | created after every online-only project, its only use dropped the same day; amend OCEAN_SURFACE_DESIGN.md D7 |
| content-009 | C | keep | BUG-yqce (scene_mirror re-home after kind-framework removal) plans its home |
| content-010 | C | keep | the 3D geometry family being built; if ever deleted, `scripts/gate_policy.py:631-632` goes too |
| content-011 | C | delete | the Film Grain preset never used this atom: two grains, one shipped |
| content-012 | C | split: keep `node.peak`, delete `node.blob_overlay` | peak is live driver vocabulary; blob_overlay is a debug box drawer |
| content-013 | B | delete (move to fixtures) | three MPM test scenes; Add Fluid authors GPU FLIP only |
| content-014 | B | delete (move to fixtures) | its builder is already test-only; Dam Break's Physics View shows the same particles |
| content-015 | C | fix | drop "(GPU FLIP)" from two display names; the ids stay |
| content-016 | C | keep | hidden, with ids kept for old LED shows; Liveschool V5 is online-only |
| content-017 | C | keep | hidden for saved-project compatibility; two are repaired at load |
| content-018 | C | delete | the loader never reads this folder; close BUG-9wa4 (ReactionDiffusion runs 4 substeps per display frame) with it |
| content-019 | C | bead | three light types, Type goes; a restructure with a migration for 68 projects (Peter's call) |
| content-020 | C | bead | the water presets and Lantern author non-default values; needs a look check |
| content-021 | C | bead | the environment's sun should follow the scene's Sun light |
| content-022 | C | bead | one studio mode changes MetallicGlass's look (Peter's call) |
| content-023 | B | keep | the correctness oracle for four RT proofs, already hidden in the Scene tab |
| content-024 | C | bead | box Count sits on the card and does nothing without a copies body |
| content-025 | B | delete | Off and Removed shade the same; 0 projects store either |
| content-026 | C | fix | take the 178 import-only sampler fields off the UI, as `rigid_body` does; the importer still sets them |
| content-027 | C | keep | each control has a recent load migration and a design control contract |
| content-028 | C | bead | the fourteen match FLIP Fluids on purpose; cutting them from the UI is one table edit (Peter's call) |

## fallbacks (45 rows, review-fallbacks-tooling.md)

| id | grade | disposition | deciding fact |
|---|---|---|---|
| fallbacks-001 | B | fix | every ladder gate reads the version taken once; the middle stamps are dead writes |
| fallbacks-002 | C | bead | no project file, but 55 history snapshots in two Downloads test archives carry it |
| fallbacks-003 | C | bead | no project file; at least 38 history snapshots carry it |
| fallbacks-004 | C | bead | no project file; 70 history snapshots in audioTesting |
| fallbacks-005 | C | bead | no project file; 135 history snapshots in audioTesting |
| fallbacks-006 | C | bead | no project file; 5 snapshots in TripMachine; the pass also runs every graph build |
| fallbacks-007 | B | delete | 41 flat fields only in 1.0.0 files, read only by the audit fingerprint; old files load the same |
| fallbacks-008 | B | delete | Deserialize already ignores the value, so all 80 files load as they do today |
| fallbacks-009 | C | bead | online-only Liveschool V6 VERTICAL is in the window, plus 90 local snapshots |
| fallbacks-010 | C | bead | 3 online-only files in the window; BUG-cavux (Math View migration journey tests red on the scene camera notice) owns the red tests |
| fallbacks-011 | C | bead | 5 online-only files in the window; 171 lines |
| fallbacks-012 | C | bead | 3 online-only files in the window; patterns-033 would make it moot |
| fallbacks-013 | C | bead | the same V6 VERTICAL file as fallbacks-009 |
| fallbacks-014 | C | bead | all 53 fork ids already named; 3 online-only files in the window |
| fallbacks-015 | C | bead | 48 online-only 3D projects; converting them frees about 1,150 lines |
| fallbacks-016 | C | bead | 37 online-only files; the run-twice part can be fixed now |
| fallbacks-017 | C | fix | bake every graph migration at load so the 114-entry rename table can ever shrink |
| fallbacks-018 | C | bead | no project file; 8 snapshots in NewAudioTesting |
| fallbacks-020 | B | fix | export drains every command but Cancel, Shutdown included |
| fallbacks-021 | B | fix | an unreadable manifest starts an empty history and the save drops every snapshot |
| fallbacks-022 | B | fix | stale region edge ids send region edits to layer 0 |
| fallbacks-023 | B | fix | warmup reorders queued commands on every open |
| fallbacks-024 | B | fix | a command on a missing target still records an undo step; `fire_param.rs` is the model |
| fallbacks-025 | B | fix | 33 undo bodies skip silently; reject and drop the step rather than `expect` |
| fallbacks-028 | B | fix | unknown enum values load as defaults; the version guard already refuses newer files |
| fallbacks-029 | B | fix | a generator card keeps a fader for a missing node; the effect path records a chain error |
| fallbacks-030 | C | bead | BUG-38rup (Effect chains: effects depend on neighbours, silent fallbacks kill params) owns it |
| fallbacks-031 | B | bead | BUG-38rup owns it (open, P1) |
| fallbacks-032 | B | fix | `Option<Project>` is None only after `shutdown()`, which only a test calls |
| fallbacks-033 | B | fix | pasted video reaches the UI copy only and renders black |
| fallbacks-034 | B | fix | unreadable audio becomes a 0-beat clip |
| fallbacks-035 | B | delete | 0 corpus graphs and 0 bundled presets lack metadata; check the GLB import graph first |
| fallbacks-036 | B | fix | report and drop 2 orphan bindings; keep identity for Composite |
| fallbacks-037 | B | fix | `preset/modifier.rs` already reconciles before commit; the 48 callers should too |
| fallbacks-038 | B | fix | a malformed physical object loses physics on split |
| fallbacks-039 | B | fix | a failed splice returns None, and the missing-output case logs nothing |
| fallbacks-040 | B | fix | the second flatten's `.ok()` skips the binding-capture pass silently |
| fallbacks-041 | B | fix | 0 of 138 saved exposures are off-surface, so a strict error breaks no saved file |
| fallbacks-042 | B | fix | about 5 lines to count dropped param ids into `load_report` |
| fallbacks-043 | B | fix | a damaged show file reports a UTF-8 error instead of the ZIP error |
| fallbacks-044 | B | fix | load repairs overlaps, so an overlap after an edit is a write bug: `debug_assert!` |
| fallbacks-047 | B | delete | its strict twin exists; all 2,939 saved groups carry a nodeId |
| fallbacks-048 | C | bead | is video-clip percussion placement a feature? (Peter's call) |
| fallbacks-049 | B | fix | the same rule written in 9 places; one owner function |
| fallbacks-050 | B | fix | stored clamp 0.01 to 1, render clamp 0.25 to 1, and the UI offers only 1, 0.75 and 0.5 |

## tooling (22 rows, review-fallbacks-tooling.md)

| id | grade | disposition | deciding fact |
|---|---|---|---|
| tooling-006 | B | delete | the render-mode design is shipped and names neither probe; 0 runs in 30 days |
| tooling-007 | B | delete | the design it served is complete; delete with `render_import --dump-def` in one lane |
| tooling-010 | C | fix | `chmod +x` the four live-UI scripts; the live-UI server is live and these are its only clients |
| tooling-011 | C | fix | `chmod +x` and keep: the app's Blob V2 report names it; BUG-7bi8 (measure Blob V2 allocation events) is open |
| tooling-012 | C | keep | the RT quality design is in progress and names this oracle |
| tooling-013 | C | delete | a second landing path with no lock and no land-pass record; tonight's plan lane 10 deletes it |
| tooling-014 | C | bead | rides on the depth relight call |
| tooling-015 | C | keep | installs the AbletonOSC patch by hand; no agent run can show its use |
| tooling-016 | C | bead | fires on every prompt (78 ms mean, 7.4 s worst) for 107 injections in 5,190 fires |
| tooling-018 | C | keep | an open bead names each file; delete each as its bead closes |
| tooling-020 | B | delete | a dated measurement; BUG-iadf (RT denoise 1:1 mode cost) is closed; git keeps the table |
| tooling-021 | C | bead | 13 designs never built, 0 reads in 30 days; only Peter knows the roadmap |
| tooling-022 | C | keep | the F5 scoping input stays; the drag-surfaces survey half can go |
| tooling-023 | C | bead | each of the 42 files has a live reference; archive folder or git is a policy call |
| tooling-024 | C | keep | the crate-split design cites the seams file; `parked.md` is pointed to by memory |
| tooling-025 | C | bead | standalone LED mirror; one bead with 026 to 028: which hand-run tools are still used |
| tooling-026 | C | bead | same bead as tooling-025 |
| tooling-027 | C | bead | same bead as tooling-025 |
| tooling-028 | C | bead | same bead as tooling-025 |
| tooling-029 | C | delete | the no-other-providers decision covers it; untracked, so Peter's yes first |
| tooling-030 | C | keep | rust-analyzer is banned only in agent sessions; Peter's editor may read it |
| tooling-031 | C | keep | the import-anything design names both Blender scripts |

## patterns (32 rows, review-patterns.md)

| id | grade | disposition | deciding fact |
|---|---|---|---|
| patterns-004 | B | delete | 0 submissions; effects get aliases from JSON; the rename tool's target file is already gone |
| patterns-005 | C | bead | the inventory path fires only for 13 test submissions; moving tests to JSON is a test refactor |
| patterns-006 | B | delete | one impl overrides all 41 methods; the app depends on the compositor directly, so it is no seam |
| patterns-008 | B | delete | one unit-struct impl, held as a concrete field |
| patterns-009 | B | delete | 9 of 11 methods forward to same-named inherent methods; no crate outside core imports it |
| patterns-010 | B | delete | one product impl each; the second impls are test mocks |
| patterns-011 | B | delete | one FFI impl each; another platform ships another library behind the same loader |
| patterns-012 | B | delete | one impl, no mock |
| patterns-013 | C | keep | BUG-ns5k decides whether the legacy denoiser path, and this trait, survive |
| patterns-014 | B | delete | one compile-time submission, already pinned by the app contract test |
| patterns-015 | B | fix | seven copies of take-one-and-check; one generic helper in core |
| patterns-016 | C | bead | the crate-split design's do-not-reopen list names the scene-exposure seam; `NodeParamProvider` can go now (conflict above) |
| patterns-017 | C | keep | the same do-not-reopen list names these catalog seams and their provider ordering |
| patterns-018 | B | fix | seven copies of one walk; one `EffectGraphDef` method, without the false whole-document doc |
| patterns-019 | B | fix | core `find_node` already searches groups; replace the five copies |
| patterns-020 | B | fix | `ExecutionPlan` has no `output_resource`; 30 copies |
| patterns-021 | B | fix | `FrameTime` has no constructor; one testkit constructor replaces 73 copies |
| patterns-022 | C | bead | about 125 hand recursions; needs a walker on `EffectGraphDef` first |
| patterns-023 | B | fix | the same seven vector helpers twice inside one crate |
| patterns-024 | C | bead | the relight call, one decision with engine-046 and graph-026 |
| patterns-027 | B | delete | ablation for a closed bug, read every denoised frame |
| patterns-030 | B | fix | a per-frame env read in product code for one GPU proof leg; the proof sets a param instead |
| patterns-031 | B | delete | the knob only turns off a default-on skip; set by nothing, documented nowhere |
| patterns-032 | B | delete | never constructed since the port; every `Some` branch is unreachable |
| patterns-033 | B | delete | nothing can switch it on; take the clip-trigger rate arm and its dead `condition()` call too |
| patterns-034 | B | delete | built every projection, no production reader |
| patterns-035 | B | delete | built every projection with placeholder values, no production reader |
| patterns-036 | B | delete | only the variant and `hdri_file` are read |
| patterns-037 | B | delete | no reader once 034, 036 and app-ui-065 and 066 land |
| patterns-038 | B | delete | built, never read |
| patterns-040 | B | delete | a dead fn: the dock is its own column, so an unclaimed drag can't leak |
| patterns-041 | C | bead | the knob list: a knob named for a closed bug goes; one with no doc goes unless Peter uses it |

## tools-lab (16 rows, no review)

| id | grade | disposition | deciding fact |
|---|---|---|---|
| tools-lab-002 | B | delete (sweep, unreviewed) | cutting the bridge imports frees 24 modules and 20 tests; cache-keyed function bodies stay byte-identical |
| tools-lab-003 | C | unreviewed | open kick beads still name these runners |
| tools-lab-004 | C | unreviewed | the classifier design is blocked on park or expand the data |
| tools-lab-005 | C | unreviewed | the accuracy design is in progress and open beads name members |
| tools-lab-006 | C | unreviewed | live product code: clip detection runs it; counts as zero lines until Peter retires it |
| tools-lab-008 | C | unreviewed | ported crate; its `compare/` JSONs feed a GPU proof and move to fixtures first |
| tools-lab-009 | C | unreviewed | the evidence behind the MTL4 denoiser hard-off; keep until BUG-woji (MTL4FX availability probe checks protocol name as class) closes |
| tools-lab-010 | C | unreviewed | a side tool with 0 references and 0 runs |
| tools-lab-011 | C | unreviewed | the producers for the MriVolume generator |
| tools-lab-012 | B | delete (sweep, unreviewed) | parses a log line only a retired runner printed |
| tools-lab-013 | B | unreviewed | its default inputs come from a retired runner; BUG-ymy4z (Bass and synth detector families) wants this kind of inspection |
| tools-lab-014 | B | delete (sweep, unreviewed) | a brief for one finished night, 0 references |
| tools-lab-015 | B | unreviewed | 21 experiment configs named nowhere, but runnable by name |
| tools-lab-017 | B | unreviewed | test-only; BUG-dcoov (Variant matcher re-judge) asks whether a next download round is needed |
| tools-lab-018 | B | delete (sweep, unreviewed) | `catalog/catalog.py` now owns the inventory |
| tools-lab-019 | B | unreviewed | `lab/audit.py` and `lab/offsets.py` own the same jobs |
