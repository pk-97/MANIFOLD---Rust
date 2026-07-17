# Bugfix Wave 2026-07-17 — landing report

Directive: `docs/BUGFIX_WAVE_2026_07_17_DESIGN.md` (6 mechanical Sonnet lanes, orchestrated
overnight). All 6 lanes landed on `main`, in 5 batches, each gated with the full workspace
sweep (`cargo clippy --workspace -- -D warnings`, `cargo nextest run --workspace`,
`cargo deny check bans`, `python3 .claude/hooks/bug_status.py`) before push.

## Outcomes by lane

**Lane 1 — dock scroll input (BUG-199, VD-029).** `primary_mouse_wheel` (`window_input.rs`)
now routes wheel events over the Scene Setup and Audio Setup docks through the generic
`UIEvent::Scroll` pipeline, matching the existing dropdown-open branch; both panels consume
it via their existing `handle_scroll`. Real app and the headless harness now share one path.
`scene-setup-add-fog-drag.json` green again; new `audio-dock-scroll.json` proves the audio
dock. VD-029 closed. One disclosed deviation: extended the `audio_sends_scene()` test fixture
with 24 extra rows so the audio dock actually overflows its viewport (fixture-only, no
mechanism change). Merged `7ae42498`.

**Lane 2 — scene/automation removal commands (BUG-193, BUG-184).** New
`RemoveSceneObjectCommand`/`RemoveSceneLightCommand` (`commands/graph.rs`), whole-level
snapshot/restore undo mirroring the existing Add commands, wired to a per-row "✕" in the
Scene Setup panel. Right-click on an automation lane opens a "Clear Automation"/"Remove Lane"
context menu dispatching the existing `ClearLaneCommand`/`RemoveLaneCommand`. Merged `1d217113`.

**Escalation surfaced during Lane 2's landing (not a lane defect):** merging origin/main
into the lane branch collided with a brand-new, Peter-approved design doc
(`docs/SCENE_OBJECT_AND_PANEL_V2_DESIGN.md`, written the same day by a concurrent session)
whose own P3 phase independently planned to build the same Remove-object affordance against
a new `scene_object`/`Object`-wire model. Consulted the reserved Fable advisor per the
session's standing instructions (judgment call, not mechanical): decision was to land Lane
2's fix as-is — it's real, tested, and closes a gap Peter has hit directly — and note in
BUG-193's backlog entry that V2's P3 becomes a *port* of these commands to the new model
(reusing their inverse-pair execute+undo tests), not a rebuild from scratch. No further
action needed from this wave; flagged for whoever executes V2 P3.

**Lane 3 — mesh stats in the graph def (BUG-194, BUG-195, folds in BUG-196).**
`source_vertex_count`/`source_bbox_radius` declared as import-time-provenance params on
`node.gltf_mesh_source`/`node.gltf_skinned_mesh_source`, seeded at import/merge time.
`SceneVm::from_def` sums them (plus a closed-form table for `node.cube_mesh`/`node.grid_mesh`)
into a new header vertex-count field, degrading to "≥ N" rather than fabricating an exact
count when a contribution is unresolved. `merge_import_into_graph`'s BUG-195 scale-sanity now
prefers the stored bbox radius over the orbit-camera-distance proxy. BUG-196: all 8
`manual_is_multiple_of` sites (drifted up from 6 originally logged) rewritten to
`.is_multiple_of()`. UI wiring for the new header field is a named follow-up, out of this
lane's enumerated scope. Merged `d96199f3` (+ a backlog-reflow fixup commit).

**Lane 4 — ui-automation harness honesty (BUG-198, BUG-192).** The headless script `Runner`
now owns a real `UndoRedoManager`, recording every dispatched `ContentCommand::Execute`/
`ExecuteBatch`; the `Key` arm intercepts Cmd+Z/Cmd+Shift+Z and dispatches real undo/redo.
Any other modifier-bearing `Key` with no seam now fails loudly instead of silently returning
"ok" — closing the silent-no-op class BUG-198 named. `under_text_matches` (`automation.rs`)
rewritten to climb outward one enclosing level at a time, stopping at the nearest same-parent
sibling carrying any text — fixes both the zero-match failure BUG-192 named AND an
undocumented cross-match bug found while confirming the root cause test-first (two rows
nesting under one shared outer container, e.g. `layer_header.rs`'s real shape). Verified
independently at landing: reran `scene-setup-fog-undo-removes-fog.json` (new, 14/14 green —
Cmd+Z genuinely removes the Density row, Cmd+Shift+Z restores it) and
`scene-setup-modifier-stack.json` (`undo_count` genuinely walks 9→8→7→6 across 4× Cmd+Z).
Merged `7ddf9af8`.

**Lane 5 — fusion coverage floor (BUG-183).** Not a partition regression: commit `a065dec4`
unbundled CinematicScene out of the bundled fused-preset set, dropping the count 33→32 while
regions/atoms ratcheted up. Floors updated to 32/56/240 (measured 32/56/243) in
`fusion_coverage_baseline`, comment rewritten citing `a065dec4`. Landed first (solo) to free
a worktree slot when the pool was briefly full. Merged `d3c3968b` (+ a backlog-reflow fixup).

**Lane 6 — diagnosis only (BUG-190, BUG-191).** BUG-190 (BrainStem ~20ms CPU-encode wall):
root-caused via direct instrumentation to `gltf_skeleton_pose`'s O(n) linear scan per joint
per frame across 59 skinned objects (~6.9ms/call, run twice per frame) — the
redundant-reparse suspect measured negligible and was ruled out. No fix shipped (exceeds the
lane's small-fix bar); written up with a concrete per-instance joint-range cache fix shape
for the next wave. BUG-191 (perf-soak `--start` seek spike): `spawn_from_image`'s four
hand-written compute pipelines had zero prewarm coverage, costing 66.4ms on the post-seek
frame — shipped `SeedParticlesFromTexture::prewarm_pipelines`, wired into
`GeneratorRegistry::prewarm_all` mirroring the existing `ScatterOnMesh` pattern, gated with a
new GPU cache-hit test. Remaining ~53ms attributed to project-specific `wgsl_compute`
instances and `blob_tracker` — written up as a scoped next-wave item, not re-derived from
scratch. Merged `f13a4d2c`.

## Housekeeping found and fixed at landing time (not part of any lane's diff)

Each of Lane 3's and Lane 5's own bug entries initially landed with a `Status: FIXED` line
but stayed filed under `## Open` instead of `## Fixed` — caught by
`python3 .claude/hooks/bug_status.py`'s housekeeper warning at merge time, not by the lane
agents themselves. Fixed via a follow-up commit on each lane branch before final merge
(`docs(bug-backlog): move BUG-183 into Fixed section`, `... move BUG-194/195/196 into Fixed
section`). A large, pre-existing, unrelated drift (~30 ancient bugs, BUG-001 through BUG-119,
archived in `BUG_BACKLOG_CLOSED.md` without a `## Fixed` pointer here) surfaced on every merge
this session — out of scope for this wave, not touched.

## Gates

Every batch ran the full landing sweep clean: `cargo clippy --workspace -- -D warnings`,
`cargo nextest run --workspace` (3559/3559 passing at the final landing, up from 3544 at the
start), `cargo deny check bans` (`bans ok`), and `bug_status.py` (clean for every bug this
wave touched). Lane-specific GPU-featured gates (`--features gpu-proofs`) were rerun for
Lanes 3, 5, and 6 at their own landing since they touched GPU-tested code.

## Escalations / new bugs filed

- The SCENE_OBJECT_AND_PANEL_V2_DESIGN.md P3 collision above (resolved, not blocking).
- BUG-190's per-instance joint-range cache fix (Lane 6, written up, not shipped this wave).
- BUG-191's remaining ~53ms wgsl_compute/blob_tracker attribution (Lane 6, written up, not
  shipped this wave).

No new `BUG-2xx` ids were needed — next free id remains `BUG-204` per the directive.

## Not in this wave (per the directive's "Decided — do not reopen" / exclusions)

BUG-187 (stays with mesh-quantization xfail), BUG-170 (real vendored-crate work, queued),
BUG-182 (blocked on Peter's .exr files), BUG-185 (golden re-baseline, orchestrator PNG-review
call), BUG-186/BUG-188 (glTF loader polish, next conformance session), BUG-197 (already FIXED
before this wave).
