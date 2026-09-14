# Motion Mosh and Data Mosh — retained image corruption

**Status:** IMPLEMENTED · 2026-09-14 · Codex. P1/P2 complete; P3 bounded renderer proofs verified. Unobserved application surfaces are listed in section 9.
**Prerequisites:** native Farneback plugin for Motion Mosh.
**Execution contract:** AGENTS.md and DESIGN_DOC_STANDARD.md sections 5–6.

Movement pulls old imagery through the current picture; Data Mosh instead holds and displaces square fragments. Peter: “DO NOT bake timing or sync or cycles into the effect, the params and drivers are used to control speed and timing of param automation.” Clip-trigger events may change the visual, following Plasma and FluidSim3D. Neither preset reads a clock. References supplied on 2026-09-14 favour recognisable detail, selective square breakup, horizontal fragments and quiet dark regions.

## 1. Audit — verified 2026-09-14

Snapshot at dfb5c0514ec3a0639c9bd2d6051717e326764a44; extend existing infrastructure.

| Piece | Source | Finding |
|---|---|---|
| Native optical flow | `crates/manifold-renderer/src/node_graph/primitives/optical_flow_estimate.rs` | Existing Farneback worker, RGBA = x/confidence/y/valid. Allocates per update, lacks reset, no busy guard, asynchronous arrival varies by execution speed. |
| GPU readback | `crates/manifold-renderer/src/gpu_readback.rs` | Existing previous-frame completion contract; allocates GPU and CPU buffers per submission. Extend with reusable storage. |
| Feedback | `crates/manifold-renderer/src/node_graph/primitives/temporal.rs` | Existing state-store loop and late-capture ping-pong. Seed available on allocation; resets currently clear to zero. |
| Flow warp | `crates/manifold-renderer/src/node_graph/primitives/uv_displace_by_flow.rs` | Existing signed R/B flow consumer; positive weight gathers backward current-to-previous vectors. |
| Block fields | `crates/manifold-renderer/src/node_graph/primitives/block_displace_field.rs` | Existing offset and aligned hash. Wire its time input from an explicit Pattern control to suppress clock fallback. |
| Composition | `assets/effect-presets/Glitch.json`, `StylizedFeedback.json` under manifold-renderer | Existing remap, channel mixer, vector length, smoothstep and masked blend cover reconstruction. No active SmearMosh preset despite older documentation. |
| Clip responses | `assets/generator-presets/Plasma.json`, `FluidSim3D.json`, `assets/effect-presets/Strobe.json` | Trigger count, baseline, gate, never-repeat index, scalar switches. Reuse; no embedded envelope timing. |
| State ownership | `src/preset_runtime/core.rs`, `src/layer_compositor.rs` under manifold-renderer | Content-owned chain/state-store; idle/seek/project-load clear hooks; topology may harvest compatible node state. Verify actual bypass behavior rather than relying on stale lifecycle prose. |

The vocabulary sweep found downsample (changes dimensions and averages) but no full-canvas block-centre sampler. This is the sole new GPU atom, used on Motion Mosh's flow and Data Mosh's retained image. No codec, detector, organic-mask, mask ownership or UI work.

## 2. Decisions

**D1.** Ship two new JSON graph presets. Existing project IDs, defaults and graphs remain compatible. No new serialized project fields; preset metadata uses the current schema and existing parameter/driver UI.

**D2.** Retain OpenCV/Farneback. Add optional `fixed_lag=false` to Optical Flow; Motion Mosh opts in at a small analysis resolution. Readback at frame N, submit at N+1, consume at N+2 before submitting another request. Fixed-lag consumption waits for its one worker request if necessary. This makes playback and export scheduling identical without an export flag or an effect clock. Cost: a slow analysis can stall the content thread at its deadline; P3 measures it. Rejected: replacing the flow library without comparative evidence; async-only export whose output depends on host speed.

**D3.** Reuse feedback, with opt-in `seed_on_reset=false` extended to true in the new colour-history loops. Seed is the fresh source. Generation-tag flow requests and reject stale responses across clear/resize. Keep one inference in flight and recycle Rust pixel, flow, upload and readback buffers. Colour loops opt into `copy_capture=true`: the existing swap rotates a producer texture after rendering, so a producer also presented as the effect output needs copy capture to retain the current frame. Internal mask feedback keeps zero-copy capture. Both new Feedback options default false for existing graphs. OpenCV's own scratch allocation is outside the Rust allocation contract and is measured as analysis cost.

**D4.** Recovery is a continuous 0–1 control: 1 writes the clean source into the retained loop; drivers can pulse it at any musical event. Clip recovery uses a single-frame pulse emitted by Trigger Gate. No timed decay, oscillator, BPM, beat subdivision or automatic recovery interval is embedded.

**D5.** Clip Trigger defaults enabled. Trigger Visual selects Recover, Reverse/Reblock, or Kick. Motion Reverse alternates flow direction using the existing never-repeat trigger index. Data Reblock changes the corruption layout. Kick briefly increases displacement, leaving the displaced result in history. Off absorbs events without replay. Manual controls remain active, including while trigger responses are enabled.

**D6.** Reconstruction uses convex blends, not additive RGB feedback. Persistence is bounded 0–1; 1 intentionally freezes selected data blocks. Alpha is blended with RGB. Displacement clamps at the image boundary. Resource counts depend on graph size and resolution, never elapsed time or trigger count.

## 3. Graphs and controls

Motion Mosh: source → optical flow → block sample. Signed motion feeds UV displacement of feedback; a channel conversion → vector length → threshold produces a movement mask. A second feedback loop retains that mask with bounded attenuation so trails survive the end of motion. Masked blend combines fresh source and dragged history, then recovery blends to fresh source and captures the result.

Controls: Drag (default 2), Persistence (.985), Block Size (16 pixels), Motion Threshold (.75 reference pixels), Recover (0), Clip Trigger (on), Trigger Visual (Recover). Drag is a gain on measured displacement, not a speed. The retained movement mask decays per rendered update using Persistence; it is not a timed envelope or cycle.

Data Mosh: block-sampled feedback → remap by block displacement; aligned block hash → threshold selects missed updates; masked blend restores unselected regions from current source; recovery writes a clean frame into history.

Controls: Corruption (.65), Persistence (.985), Block Size (16 pixels), Displacement (.3), Pattern (0), Recover (0), Clip Trigger (on), Trigger Visual (Recover). Pattern is a static hash coordinate, automatable externally. There is no autonomous random update clock.

Generic Block Sample: `in: Texture2D`, optional scalar `block_size`, `out: Texture2D`, full-canvas dimensions. Samples at integer block centres, clamps edges, size 1 is identity. Codegen body with Gather access; fusable with compatible neighbours. Trigger Gate adds `pulse: ScalarF32`; existing count output unchanged. Optional numeric shadow ports are added only where these graphs need existing numeric parameters to accept wires.

## 4. Retained-state contract

| Event | New presets |
|---|---|
| First frame / reset / resize | Fresh colour seed; no uninitialised or pre-reset image. Flow invalid until two valid analysis samples in current generation. |
| Effect bypass / layer idle | Drop or clear retained state through existing chain lifecycle; re-entry fresh. Verify harvesting does not resurrect bypassed history. |
| Seek / load / export warmup reset | Invalidate colour/mask history, previous analysis pairing and in-flight response generation. |
| Contiguous clip change | A continuing layer/group/master effect instance retains history and applies its enabled clip response. Recover gives a clean boundary; Reblock/Reverse/Kick intentionally carry imagery. A different per-clip effect instance starts fresh through the existing topology rules. |
| Clip gap | Existing idle clear causes fresh re-entry. |
| Export | Fixed frame lag and sequential state evolution; reset warmup residue; same frame sequence yields same output independent of waits. |
| Recover held at 1 | Clean source each frame and clean retained history; release starts from current imagery. |

## 5. Invariants and enforcement

- Preset structural tests reject clock wires, unknown ports, broken bindings and accidental changes to existing IDs.
- GPU Block Sample formula tests cover identity, block centres and alpha; fusion proof compares generated fused/unfused output.
- Trigger tests cover pulse width, first baseline, disabled backlog and repeated counts.
- Flow tests cover generation/dimension pairing, bounded flight schedule, buffer reuse, channel packing and failure warmup.
- Multi-frame graph proofs cover recovery, held recovery, persistent history, reset/re-entry and finite bounded RGBA. Round-trip both preset definitions before modulating in the harness.
- Bounded render comparison produces clean/processed imagery and cost numbers. A compile alone is not behavioral evidence.

## 6. Phasing

**P1 — temporal prerequisites.** Entry: verified base and audit above. Read back temporal, flow, readback and trigger implementations plus primitive/freeze guidance. Deliver the two backward-compatible extensions, reusable native flow and Block Sample with focused tests. Lead reviews every default path. Checks: diff-selected renderer clippy/tests and required GPU proofs. Gesture: trigger recovery after retained input changes. No independent timer, library replacement, new locks or unrelated infrastructure. Demo: graph readback proofs and P3 artifacts.

**P2 — both editable presets.** Entry: P1 ports exist. Deliver MotionMosh.json and DataMosh.json, metadata, bindings, reconstruction and trigger graphs, round-trip/structure tests. Read Plasma, FluidSim3D and StylizedFeedback wiring first. Validate both through the same graph compiler used by graph_tool; compare actual fused and canonical runtime output. Gesture: modulate corruption/drag, change trigger visual, retrigger, hold Recover then release. No mask ownership/UI changes. Demo: P3 renders.

**P3 — verification and landing.** Entry: compiled presets and focused tests. One bounded render comparison over translating detailed imagery (including the supplied portrait) plus clean recovery/reset/trigger cases; one correction/verification if evidence requires it. Capture frame CPU/GPU cost and wait cost at declared resolution/frame count, no soak. Check frame values, static-source behavior and fused/unfused parity. Run scripts/gpu_proofs_gate.py and scripts/land_branch.py (which invokes landing_gate.py) after exact-path stage commits. Preserve failed evidence and track gaps in beads; release landed slot or review and retire unfinished slot. Do not claim app/visual/perf verification from compilation.

## 7. Decided — do not reopen

1. Native Metal through manifold-gpu; existing OpenCV analysis.
2. Editable graphs, bounded retained resources, parameter drivers own timing.
3. Clip-trigger visuals are event responses, not autonomous cycles.
4. Preserve existing projects/effects and mask infrastructure boundaries.

## 8. Deferred

Actual codec damage, blob detection V2, organic masks, tracking overlays and inset windows are outside this work. Revisit only on explicit user scope. Broader changes to all asynchronous analysis consumers require separate evidence; this work fixes the existing flow seam needed by Motion Mosh.

## 9. Verification evidence — 2026-09-14

The scoped Metal GPU proof gate passes both complete graphs after project-instance serialization and normal manifest reconciliation. It covers held/manual/clip recovery, alternate clip visuals against disabled triggers, zero persistence, bypass re-entry, reset replay including in-flight native analysis, finite bounded pixels, and fused/canonical parity. Block Sample formula/alpha and Trigger Gate pulse tests pass. Feedback reset is observed through a pinned downstream output so late capture cannot invalidate the observation.

Observed 24-frame, 512×512 portrait renders preserve distinct styles: directional ghosting in Motion and selective square reconstruction in Data. The provided reference image remains external to the repository. Optional `MOSH_PROBE_IMAGE` and `MOSH_PROBE_DIR` on the GPU proof generate PNG/GIF evidence. No overlay/inset design was added.

At 512×512 over 16 steady synthetic-motion frames, canonical Motion measured CPU median **6.268 ms**, max **6.667 ms**, and GPU median **0.422 ms**, max **0.448 ms**. Data measured CPU **0.033/0.042 ms** and GPU **0.188/0.191 ms** (median/max). CPU includes the fixed-lag native-analysis wait; GPU readback used by the test to inspect images is excluded. These are bounded development-build measurements on this machine, not 1080p/4K frame-budget claims. The paired fusion proof also prints canonical/fused costs at the same resolution.

The native bundle worked with an explicit path but Cargo's crate working directory hid it from the existing loader. Debug builds now also search the build workspace's plugin directory; packaged release discovery is unchanged. The existing renderer GPU-test lint failures were repaired mechanically without changing test assertions.

Full application UI interaction, complete exported movies and high-resolution frame budgets remain outside the observed proof; lifecycle and export claims above refer to the shared sequential renderer/reset path.
