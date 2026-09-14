# Effect masks — spatial wet/dry for effect groups

**Status:** IN PROGRESS · 2026-09-14 · Codex. Masks first; audio visualizers follow after this landing.

Peter selected all three sources: shapes, the group's incoming image, and another
layer/generator. "This gives us some very cool sidechain options too."

## 1. Audit (2026-09-14)

- `effects/group.rs::EffectGroup` already owns bypass and wet/dry.
- `preset_runtime/build.rs::OpenGroup` captures the dry input;
  `close_mix_group` already closes the branch with a mix.
- `primitives/masked_mix.rs` blends using mask red times amount and supports fusion.
- `primitives/layer_source.rs` supplies layer output. Audit found retained render
  targets were mutable across frames; this slice adds owned pixel snapshots.
- `PresetInstance` already provides graph overrides, manifests, modulation and
  stable EffectId editing. Inspector controls use `param_surface.rs`.
- Cmd+G reaches `input_host.rs::handle_effect_group`; the current inspector lacks
  rack-group headers, so membership must become visible as part of this work.

## 2. Decisions

D1. A mask is an ordinary `PresetInstance` in the existing effect list. Add
   `EffectGroup.mask_effect_id: Option<EffectId>` (camelCase, absent by default).
   That member's graph produces coverage on a branch from the group's dry input;
   it does not replace the colour image. All other members remain serial effects.
   This reuses card addressing, graph editing and modulation without a second
   parameter or preset ownership system.
D2. The mask card lives within its group. Cmd+G supports one selected effect.
   Add-mask creates a group when required or attaches to the existing group.
   Removing the mask clears its reference; undo restores both. Ungroup removes
   the mask card and restores it on undo. Duplicating a layer remaps the reference.
   Group members remain contiguous when structurally edited; mask cards cannot
   become ordinary colour effects accidentally.
D3. Closing a masked group computes `mix(dry, wet, clamp(mask.r * wetDry, 0, 1))`.
   A disabled mask uses the ordinary group wet/dry. Continuous controls update
   existing bindings without rebuilding feedback state. Missing mask membership
   is an explicit invalid graph, never silently treated as another effect.
D4. Source presets use existing atoms: circle, rectangle, gradient, input image,
   and layer source. Image-derived sources explicitly extract brightness or alpha.
   Contour/softening/inversion are graph operations. The graph editor remains the
   custom routing surface. No new fused effect kernel or bespoke slider system.
D5. Cross-layer sources retain the existing one-frame delay. Source-only visibility
   must preserve rendering while excluding presentation; hide/mute semantics must
   not be repurposed. The picker lists available video/generator layers. Missing sources provide zero
   coverage following the existing Layer Source contract.
D6. Feedback masking defaults to masking its result. Injection masking remains a
   graph edit. No changes to Stylized Feedback's existing temporal contract.

## 3. Implementation boundaries

Core/editing owns the optional reference and undoable structural commands.
Renderer owns branch assembly and live bindings. UI projects group membership and
uses ordinary mask cards. Add/remove/reorder/group/ungroup dispatch through
`ContentCommand::ExecuteOnContent`, which applies through `EditingService`.
Effect-chain dispatch now supplies the existing `LayerSkinRegistry` to the graph,
matching generator dispatch; `LayerSource` caches its parsed ID until it changes.
No additional shared locks, threads, graph target kinds, or parameter identity maps.

## 4. Invariants and checks

- Old projects deserialize without masks; save/reload preserves mask identity and
  controls. Core serde and editing round-trip tests enforce this.
- Add/remove/ungroup/duplicate preserve membership and exact undo order. Editing
  tests and layer clone tests enforce this.
- Zero/full/partial coverage, group wet/dry and preserved alpha have GPU numerical
  proofs. Shape motion updates values without changing topology.
- Cross-layer reads use owned snapshots, published after master effects. Grouped
  children remain addressable. GPU tests cover target reuse and stale sources.
- Snapshot storage is reused; each rendered source costs one texture and one copy
  per frame. Projects containing masks conservatively disable occlusion render-skip
  so potential sidechain sources advance. Narrow dependency tracking is a future
  optimization; presentation still skips occluded pixels.
- `inspector-add-mask.json` covers the context menu, group header and undo after
  card exit animation. `group_mask_circle_moves_over_infrared_without_rebuild`
  checks moving shape coverage against the standalone Infrared output and emits
  a two-position render. Source-only routing remains phase 2.

## 5. Phases

1. Group mask vertical slice (implemented): model, commands, mask presets, branch assembly,
   visible membership and Add Mask. Gesture: move a soft circle across Infrared.
   Focused core/editing/renderer/UI tests and clippy; required GPU proofs and
   landing gate. Verify save/reload, then continue the gesture.
2. Sidechain source-only visibility and contour processing: reuse Layer Source and
   existing atoms. Gesture: use a separate playing layer to reveal the masked group
   without displaying the source itself. Resolve visibility against compositor
   code before authoring this slice; do not guess at hidden-layer scheduling.
3. Audio sources and separate visualizer presets: audit send ownership and bounded
   histories before committing the data seam. Existing analysis is authoritative;
   oscilloscope/spectrogram outputs feed the same group masks. No audio work starts
   before the mask landing. Its exact seam and checks are added at that point.

Workers receive prepared bounded briefs, return edits/checks, and never land.
The lead reviews and uses `land_branch.py`, then releases the slot. At most two
workers overlap independent files; no repeated passed checks or broad sweeps.

## 6. Decided — do not reopen

All three source families; reusable atoms and graphs; group mask scope; existing
card/modulation ownership; delayed cross-layer routing; result masking by default.

## 7. Deferred

Automatic injection-mask UI and nested mask groups are deferred until requested.
The audio phase is authorized follow-on work, not complete with the mask landing.
