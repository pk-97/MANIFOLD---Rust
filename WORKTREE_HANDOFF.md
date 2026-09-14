# Effect masks: unfinished, not landed

Resume task: BUG-walb. Performance follow-up: BUG-puh3. Known RT proof failure:
BUG-jgsf. Original branch `codex/effect-masks`, base
`adfb11de21b463cf43e9a6f4df6575520c256c11`, implementation tip `67a245d80`.

Peter authorized masks, source-only/contour follow-on, and oscilloscope plus
spectrogram using existing atoms. Keep answers concise and usage bounded.
Only the first mask slice is implemented. Nothing from this branch landed.

## Implemented

- EffectGroup.mask_effect_id references an ordinary preset card. Existing group
  dry input branches into coverage; existing MaskedMix closes spatial wet/dry.
- Add Mask creates or uses a Cmd+G group. Shape, incoming-image and named-layer
  sources; stable IDs, undo/redo, serialization, duplication and structural guards.
- Content-thread command dispatch and visible masked-group membership in the rack.
- Five hidden mask presets, generated catalog, fusion sections and thumbnails.
- Layer Source reaches effect execution. Owned pixel snapshots prevent reused
  render targets changing previous-frame reads; publication follows master FX,
  retains grouped children, and removes stale sources. Persistent storage costs
  one texture and copy per rendered layer. Masks conservatively disable occlusion
  render-skip; narrowing dependencies is BUG-puh3.

## Validation and exact blockers

Focused core mask tests (3), editing tests (5), mask GPU tests, moving Infrared
render, target-reuse and grouped-child publication regressions passed. The UI
Add Mask / group header / undo flow passed. Final landing gate passed clippy,
UI flows, design/docs checks, deny, ignored-test guard and tooling checks.

Landing remains blocked:

1. `manifold-app::godfile_regrowth::no_register_listed_file_regrows`:
   preset_runtime/core.rs is 2229 lines, ceiling 2200; build.rs is 762, ceiling
   750. Extract cohesive mask/group helpers under the existing renderer runtime
   decomposition contract. Do not raise ceilings or remove useful documentation.
   Nextest stopped early; thousands of remaining tests did not run.
2. `rt_bugmajv_kernel_toggle::rt_kernel_toggle_sequence_preserves_raster_base`
   failed in both full GPU runs. Existing BUG-jgsf records this failure under GPU
   contention. Every other GPU proof passed in the final run; no golden drift.
   Do not change its golden or tolerance speculatively. Reassess evidence before
   any named-red decision.

Stopped at the repository's two-attempt budget. Full app behavior is unverified;
this archive is source preservation, never an app landing.

## Resume

Acquire a fresh worktree from this archive, review the runtime decomposition doc,
extract the two overgrown helpers, run focused checks and the required landing
gate. Use `env RUSTC_WRAPPER=`: sccache cannot invoke rustc in this environment.
Metal checks need native device access; sandboxed runs report No Metal device.
The desktop hook resolves command cwd to main; use absolute `git -C` paths and
main-root `.codex/hooks/guard.py` for any necessary exact-command exceptions.

After the mask landing, source-only visibility and contour remain to implement.
Image/layer masks still need transform and feather controls; their layer string
can currently be edited through the graph. Reuse node.transform, paired
node.gaussian_blur and node.edge_detect. Source-only must exclude screen/LED
presentation and occlusion while retaining rendering; do not repurpose mute.
Audio data ownership and reusable waveform/spectrum atoms still require an audit.
No audio implementation started. See docs/EFFECT_MASKS_DESIGN.md.

Evidence in `archive-evidence/` contains the final gate transcript and synthetic
mask render/UI capture. Earlier commits retain all source and generated assets.
