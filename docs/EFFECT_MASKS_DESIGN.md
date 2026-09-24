# Effect masks — spatial wet/dry for effect groups

**Status:** IN PROGRESS · 2026-09-24 · Codex. Masks, Modifier Groups and final-coverage preview implemented; snapshot publish narrowed to read layer sources. Oscilloscope and Spectrogram generators implemented; source-only routing, contour and audio-to-mask routing deferred.

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
- Cmd+G reaches `input_host.rs::handle_effect_group`; rack-group headers expose
  membership and the group modifier picker.

## 2. Decisions

D1. A mask is an ordinary `PresetInstance` in the existing effect list. Add
   `EffectGroup.mask_effect_id: Option<EffectId>` (camelCase, absent by default).
   That member's graph produces coverage on a branch from the group's dry input;
   it does not replace the colour image. All other members remain serial effects.
   This reuses card addressing, graph editing and modulation without a second
   parameter or preset ownership system.
D2. Cmd+G wraps one or several selected effects in a **Modifier Group** using
   the existing EffectGroup model. Without a modifier, effects run normally.
   The header's **Add Modifier** picker offers Mask — Circle, Rectangle, Gradient,
   Image, Layer, Blob, Blob Colour and Blob Motion; the ordinary mask card holds
   its controls inside the group. The three Blob masks share the V2 region
   detector/tracker, preserve connected-component pixels and holes, and expose
   source-specific brightness, colour or motion controls.
   The picker anchors below its button. Layer sources include their timeline row
   number so duplicate names remain distinguishable.
   A bordered container surrounds each group, with a distinct header and inset
   member cards. The header shows the effect count, or `Mask → N effects` when
   masked, to make the modifier scope explicit. Ungrouped effects sit outside it.
   One mask is supported per group. Once masked, the header replaces Add Modifier
   with **Preview Mask**. It opens the existing graph editor on the final coverage
   producer, with preview normalization off: black selects nothing, white selects
   fully, and grey selects partially. The ordinary mask card remains the tuning
   surface; the master monitor still shows the live result. This previews coverage
   before the group wet/dry multiplier; disabled masks/groups are bypassed in the
   live result. Previewing does not edit the project. The request uses the stable
   mask EffectId and waits for its own snapshot before focusing the coverage node.
   The picker captures the group ID; membership resolves on the content thread.
   Existing generic Group/Masked Group labels display as Modifier Group; custom
   names and serialized group data remain intact. Cmd+Shift+G ungroups as before.
   The mask card lives within its group.
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
- Blob mask coverage uses observed categorical labels; a retained unobserved
  track never paints a box. Validity is applied after inversion, so a failed or
  reset detector cannot turn an inverted mask fully on. `blob_v2_mask_pixels`,
  `blob_v2_group_mask_ring_and_dry_input` and `blob_v2_invalid_inverted_mask_is_zero`
  cover these paths. `inspector-blob-mask.json` covers picker and undo/redo routing.
- Cross-layer reads use owned snapshots, published after master effects. Grouped
  children remain addressable. GPU tests cover target reuse and stale sources.
- Snapshot storage is reused, and narrow dependency tracking is landed: only
  layers read as a layer source since the last publish are snapshotted — one
  texture and one copy per REFERENCED source per frame, not per rendered layer.
  Unreferenced layers serve the transparent-black fallback. Projects containing
  masks still conservatively disable occlusion render-skip so potential
  sidechain sources advance; render-set narrowing remains open, while
  presentation still skips occluded pixels.
- `inspector-add-mask.json` covers the card context menu and undo after card exit
  animation; `inspector-modifier-group.json` covers the group picker and undo/redo.
  Input-host tests cover grouping one or several selected effects. `group_mask_circle_moves_over_infrared_without_rebuild`
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
3. Separate Oscilloscope and Spectrogram generators (implemented). Each card exposes
   an Audio Send dropdown through the existing string-parameter rows. An explicit
   selection stores the send's stable ID in its graph node; reordering sends does
   not retarget it. `First send` is an explicit default mode. Deleted sources show
   as missing and emit zero data. Source edits use `SetGraphNodeParamCommand` and
   normal undo/redo. These are generator layers, with no input-image blend or Amount control.
   Spectrogram keeps its 512×256 analysis texture and samples into the canvas at
   the display transform. Audio-to-mask routing is outside this slice.

   The content thread already mixes capture and audio-layer taps into one
   `StreamingSendAnalyzer` per send. A per-hop callback copies its existing raw,
   floored, untilted spectrum into bounded reusable histories; Audio Setup keeps
   its existing scope drain. Raw mono feeds a bounded waveform ring. Only sends
   read by enabled graph sources are added to the existing analysis consumers.
   No second analyzer, audio worker, shared lock or per-frame history allocation.

   Data-only histories live in core. The live runtime and offline export driver
   own separate registries. Rendering borrows the registry through `GpuEncoder`
   for one frame; no cached raw pointer or new synchronization. Export tap
   discovery includes visualizer consumers before audio mixdown.
   PlaybackEngine exposes a runtime transport epoch for explicit seek, play,
   pause, stop and project replacement. Visual histories reset on that epoch
   or a source-routing change, without guessing from elapsed time. Continuous
   external-clock nudges and effect knob edits preserve history.

   `node.audio_waveform` supplies an array to Range, Array Math, Combine XY and
   Draw Lines. `node.audio_spectrum` supplies raw magnitude history as a texture;
   decibel conversion, contrast, color mapping and canvas transforms stay separate
   graph operations. Source nodes are non-pure I/O boundaries. Any new numeric
   GPU atom uses the shared standalone/fusion code generator.

   Gesture: route a playing audio layer to a send, add either generator, change its
   source and waveform window or spectral history while it plays, then export.
   Focused tests cover ring bounds, waveform sampling, spectrum chronology,
   missing sources, analyzer/scope coexistence and offline feeding. GPU proofs
   check both graphs against synthetic audio and inspect their rendered output;
   UI routing checks cover source identity and undo. Required landing checks apply.

Workers receive prepared bounded briefs, return edits/checks, and never land.
The lead reviews and uses `land_branch.py`, then releases the slot. At most two
workers overlap independent files; no repeated passed checks or broad sweeps.

## 6. Decided — do not reopen

All three source families; reusable atoms and graphs; group mask scope; existing
card/modulation ownership; delayed cross-layer routing; result masking by default.

## 7. Deferred

Automatic injection-mask UI, nested mask groups and using audio visualizers to
drive group masks are deferred until requested.
Standalone audio visualization is phase 3; it does not enable audio-to-mask routing.
