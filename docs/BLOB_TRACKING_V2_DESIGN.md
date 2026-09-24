# Blob Tracking V2 and Blob Mask

<!-- index: Implementation contract for a new shape-preserving blob tracker and effect-group masks, retaining the original Blob Track. -->

**Status:** IMPLEMENTED · 2026-09-23 · Codex. Six new presets, native V2 ABI, shared detector/tracker, mask/colour/motion graphs and menu entries are present; V2 shipped without changing the original Blob Track. Focused GPU, UI and 1080p app performance proofs passed. On an M4 Max, the default 600-frame V2 app-tick p95 added 0.67 ms over legacy; detector worker p95 was 0.234 ms with one-frame readback age and two-frame capture-to-output age. Process malloc-zone retained bytes and blocks are reported; transient allocation events and detector-only retained bytes remain unmeasured under BUG-7bi8. Follow-up refinement under BUG-sh4i: the 2026-09-24 V1 correction removes global histogram equalization after a held-out liquid clip exposed merged regions and detection dropouts. Modifier Group headers now offer Preview Mask, opening the existing graph monitor on final coverage with normalization off. V2 persistent selection and temporal mask refinement remain open.
**Prerequisites:** existing effect-group masks, native BlobDetector bundle, and optical-flow primitive; verified against the implementation.
**Execution contract:** Sol at Extra High owns one continuous overnight run through P1–P4, using native Luna lanes. Phases are bounded implementation/review/landing checkpoints, not reasons to end the task or await another prompt. Peter's end-to-end instruction overrides the earlier one-phase-per-session handoff and the design standard's fresh-session default. Read `DESIGN_DOC_STANDARD.md` sections 5–6 and current `AGENTS.md`; preserve their engineering and landing gates.

Build a new Blob Track V2 effect and an organic Blob Mask modifier from the same region detector. Preserve the original Blob Track: Peter explicitly said, “it must be available please that was wrong.” This is a new effect, not a replacement or project migration. Peter's mask request was “a blob tracking mask modifier that you can feed effects into.” In the existing product this means adding a mask to an effect group, then putting effects inside that group.

The constraints are frame-time cost, reusable buffers, predictable temporal state, and composable graphs. OpenCV stays. Rust owns scheduling and tracking; a new API in the existing native bundle will perform connected-component detection. This document is an implementation contract, not evidence of measured speed or improved footage. Motion Mosh and Data Mosh belong to their separate workstream.

Companions: [effect masks](EFFECT_MASKS_DESIGN.md) owns group routing and editing; [decomposition](DECOMPOSING_GENERATORS.md) and [adding primitives](ADDING_PRIMITIVES.md) own graph composition; [mosh](MOSH_EFFECTS_DESIGN.md) provides the shipped asynchronous-analysis precedent.

## 1. Audit — verified 2026-09-23

Snapshot: `9b419c32587f4370d1f2f216850d274900603273`. Paths below are repository-relative. Extend these seams; do not redesign them.

| Piece | Source anchor | State and consequence |
|---|---|---|
| Original preset | `crates/manifold-renderer/assets/effect-presets/BlobTracking.json` | Exists, available. Detection/filtering/tracking and HUD are already separate graph groups. Preserve its ID, parameters, availability and appearance. |
| Native detector | `assets/plugins/BlobDetector/BlobDetectorPlugin.cpp` (`BlobDetector_Process`) | Exists: equalization, blur, Canny, morphology, external contours, bounding boxes. Shape information is discarded. |
| Native loading | `crates/manifold-native/src/ffi/blob_ffi.rs` (`FfiBlobDetector`); `ffi/mod.rs` (`resolve_bundle_path`) | Exists. Preserve original symbols and loader lifetime policy; add a versioned API in the same bundle. |
| Analysis node | `crates/manifold-renderer/src/node_graph/primitives/blob_detect_ffi.rs` (`BlobDetectFfi`) | Exists, eight detections, downsample/readback/worker; allocating response and previous-image paths. Reuse architecture, not those allocations. |
| Tracking | same primitives directory, `track_persist.rs` (`assign_global`, `TrackPersist`) | Exists: best-first distance matching, grace counted on graph runs, compacted slots. Cannot carry reliable explicit identities through disappearances. |
| Smoothing/filtering | `one_euro_filter.rs` (`OneEuroFilter`); `array_filter_detections.rs` (`ArrayFilterDetections`) | Existing four-float box ABI. Do not feed an extended record into these nodes. |
| Worker/readback | `crates/manifold-renderer/src/background_worker.rs` (`BackgroundWorker`); `gpu_readback.rs` (`ReadbackRequest::try_read_into`) | Reusable infrastructure exists. Enforce one request in flight and return owned storage on every result path. |
| Temporal analysis | primitives `optical_flow_estimate.rs` (`FlowRequest`, `FlowResponse`, `flow_schedule`) | Exists: generation tags, recycled buffers, fixed-lag response consumption, clear-state and warmup handling. Model the new analysis lifecycle on this. |
| Foreground sources | `MaskImage.json`; primitives `chroma_key.rs`, `smoothstep_texture.rs`; `MotionMosh.json` | Brightness, colour proximity, and flow-magnitude graphs already exist. Recompose them. `node.threshold` preserves colour; it is not a binary mask generator. |
| Mask plumbing | `crates/manifold-core/src/effects/group.rs` (`EffectGroup::mask_effect_id`); `crates/manifold-renderer/src/preset_runtime/groups.rs` (`close_mix_group`) | Exists. Mask reads group dry input; red controls wet coverage. No new project schema. |
| Mask UI/editing | `crates/manifold-app/src/ui_root/dropdowns.rs` (`mask_menu_items`); `crates/manifold-editing/src/commands/effect_groups.rs` (`AddGroupMaskCommand`) | Exists. Add entries through this menu and its existing undoable action. |
| Mask composition/proofs | `MaskCircle.json`, `MaskImage.json`; `crates/manifold-renderer/src/preset_runtime/tests/group_mask.rs` | Existing invert/amount convention and dry-input, wet/dry, reload tests. Extend these tests. |
| Native distribution | `assets/plugins/BlobDetector/build.sh` | Builds and embeds OpenCV dependencies in `assets/plugins/BlobDetector.bundle`; rebuilds the bundle directory. Run only in the implementation slot. |

The primitive filenames above without a full directory are under `crates/manifold-renderer/src/node_graph/primitives/`; preset filenames are under `crates/manifold-renderer/assets/effect-presets/`.

## 2. Decisions

**D1 — Preserve legacy; add V2.** No edits to legacy detection behaviour, graph wiring, parameter IDs or C ABI. Rejected: silently upgrading `BlobTracking`, because saved performances depend on its current look.

The subsequent BUG-sh4i refinement authorizes a targeted V1 regression correction: preserve source contrast before Canny instead of histogram-equalizing it. Equalization amplifies low-contrast background texture, joins distinct regions, and leaves a frame-spanning contour that the existing filter rejects. Threshold/Sensitivity, the near-flat-frame gate, tracker, graph and ABI remain unchanged; existing projects can produce different detections on textured footage. This correction does not replace V1 with the V2 detector. `scripts/test_blob_detector.py` covers two strong regions on weak background texture and the near-flat-frame gate. V2's native region ABI retains its existing tests.

**D2 — Detect filled regions, retain a label image.** Feed a foreground mask to OpenCV connected components, retaining each selected component’s foreground labels while holes remain background. Use `connectedComponentsWithStats` with 8-connectivity, `CV_32S`, and `CCL_SAUF`. OpenCV documents the input and label/statistics contract in its [shape API](https://docs.opencv.org/4.10.0/d3/dc0/group__imgproc__shape.html). Rejected: another Canny-plus-box pipeline; it still loses the shapes needed by Blob Mask. Touching regions remain one region; no claimed object recognition.

**D3 — Keep OpenCV behind the existing plugin.** No `imageproc`, Rust OpenCV binding crate, new CV dependency, or hand-written connected-component implementation. Existing dependency packaging is the reuse target. Rust tracking arithmetic is small bounded application logic, not a replacement CV library.

**D4 — Separate preparation, detection, tracking and rendering.** The detector consumes a generic mask. The tracker consumes regions. HUD and mask rendering consume their outputs. Rejected by name: a `blob_tracking_v2` mega-node that selects sources, detects, tracks, draws and composites effects. It would duplicate group masks and hide reusable operations.

**D5 — Separate source presets.** Ship brightness, colour and motion variants using the same core. This avoids a source-mode switch with inactive controls and avoids computing unused optical flow. No automatic timing, clip cycles or modulation baked into these presets.

**D6 — Shape fidelity first; explicit limits on temporal claims.** Smooth the tracked boxes and estimate velocity. Mask coverage uses the latest observed component pixels; it does not warp stale silhouettes to predicted boxes. Brief disappearance retains identity, not a fabricated foreground. Feathering is spatial smoothing. Rejected: pretending box interpolation solves temporal silhouette reconstruction.

**D7 — Use the existing fixed-lag analysis schedule.** Deterministic response deadlines are preferable to output depending on thread speed. Port the optical-flow schedule and its tests, not its detector. A slow native call can still delay the deadline; measure that cost. No global analysis cache, additional thread pool, shared lock or unbounded queued work.

Architecture comparison: extending the legacy node has lower initial implementation cost but changes saved looks and still requires a second shape path. The chosen shared region pipeline adds native exports, three principal primitives and three small image operations, preserves legacy serialization, and supports both HUD and masks. It adds a label texture/readback and bounded tracking work. It does not solve touching-object separation or semantic recognition. These are costs and limits, not claims that V2 is universally faster.

## 3. Product contract and graphs

| Preset ID | Display name | Where it appears |
|---|---|---|
| `BlobTrackingV2` | Blob Track V2 | Effect picker, available true |
| `BlobTrackingV2Colour` | Blob Track V2 — Colour | Effect picker, available true |
| `BlobTrackingV2Motion` | Blob Track V2 — Motion | Effect picker, available true |
| `MaskBlob` | Blob Mask | Add Mask — Blob |
| `MaskBlobColour` | Blob Mask — Colour | Add Mask — Blob Colour |
| `MaskBlobMotion` | Blob Mask — Motion | Add Mask — Blob Motion |

Mask presets use `available:false` like `MaskImage`: reachable through the modifier menu, intentionally absent from the general effect picker. This is not permission to hide `BlobTracking`. No rename or alias migration.

Common analysis graph: source preparation → `node.resize_limit` → Gaussian denoise → `node.detect_regions` → `node.track_regions`. Detector labels also feed `node.region_mask` in mask presets. The resize node caps the longest dimension at 320, never upscales, preserves aspect, and rounds the other dimension to at least one pixel. Detector consumes those dimensions without another resize. Default update interval is 2 rendered frames; authored range 1–8. Fixed-lag policy is internal, not another performer toggle.

Source preparation:

* Brightness: `MaskImage`'s Rec.709 channel mix. Detector foreground threshold is the exposed **Threshold**, default 0.5, range 0–1.
* Colour: `node.rgb_distance` → existing `node.smoothstep` with edges Tolerance ±0.01 → `node.invert`. Default target red, Tolerance 0.3 (range 0–1); detector threshold fixed 0.5. Expose scalar Target Red/Green/Blue (0–1) and Tolerance. This reproduces the existing chroma-key RGB proximity math, not HSV isolation. The existing chroma-key Vec3 target cannot be exposed through scalar outer bindings (`crates/manifold-core/src/effects/bindings.rs`, `ParamConvert`); do not invent component binding syntax or redesign bindings.
* Motion: reuse the `MotionMosh` flow R/B extraction and vector-length graph, gated by flow validity. Flow uses fixed lag, analysis dimension 192, interval 1. Expose **Motion Threshold**, default 0.005 UV displacement per flow sample, range 0–0.1; normalize magnitude by `max(threshold,0.00001)` and detect at 1.0. Wire flow `cut_score > 0.28` to reset tracking and discard pre-cut detector work. Motion regions mean coherent moving pixels, not complete moving-object silhouettes. This variant has two analysis stages and greater latency; report it separately.

Expose **Denoise** 0–3 analysis pixels, default 1, using existing separable Gaussian graph passes with radius_mode=Dynamic, axes Horizontal/Vertical, address_mode=Clamp. Resize before denoise so the unit is stable. Zero is exact bypass. Source values are finite-clamped; threshold comparison is inclusive, with 1.0 still selecting exact-white pixels.

Common detector controls: **Min Area** 0–0.25, default 0.001; **Max Area** 0.01–1, default 0.8; **Max Blobs** integer 1–32, default 8. Area means foreground pixel count / analysis-image pixel count, not box area. For min > max use an empty interval, not silent swapping. Filter before choosing largest components. Advanced graph parameters add pixel-aspect-corrected min/max aspect, defaults 0.05 and 20; they need not clutter the outer card.

V2 reuses the existing HUD subgraph from `BlobTracking.json`, fed by the new tracker's compatible `boxes` output. Retain its visual controls/bindings. Replace the old detector/filter/tracker/One Euro group, not the draw primitives. Expose **Smoothing** seconds, 0–0.5, default 0.06, and **Retention** seconds, 0–1, default 0.15. These are physical filter durations, not project playback timing. No baked beat animation.

Mask graph: observed labels + tracked membership → `node.region_mask` → signed expansion → Gaussian feather → invert → amount → validity gate → final output. Expose **Selection** All/Largest (default All), **Expand** −32…32 analysis pixels (default 0), **Feather** 0…8 analysis pixels (default 1), **Invert** false, **Amount** 0…1 default 1. Perform expansion/feather at analysis resolution and upscale coverage linearly at the end. Labels themselves must never be linearly sampled.

Mask output is `(coverage,coverage,coverage,1)`. Empty successful detection is a valid empty mask; Invert then deliberately selects everything. Unavailable/error/reset state has valid=0, applied **after** inversion, so it cannot flood the group. Group wet/dry remains the existing `mix(dry,wet,clamp(mask.r*wetDry,0,1))`. The mask reads group input, never the processed wet branch. No new source-layer selector in this version.

## 4. Native and graph seams

All types here are new unless explicitly named existing. No old signature changes. Native CPU records live in new `crates/manifold-native/src/region_detector.rs`; renderer wire/storage types live in new `crates/manifold-renderer/src/node_graph/primitives/region_types.rs`.

```rust
pub const MAX_REGIONS: usize = 32;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Region {
    pub label: u32,
    pub x: f32, pub y: f32, pub width: f32, pub height: f32,
    pub area: f32, pub cx: f32, pub cy: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RegionOptions {
    pub threshold: f32, pub min_area: f32, pub max_area: f32,
    pub min_aspect: f32, pub max_aspect: f32, pub max_regions: u32,
}

pub enum RegionError { InvalidInput, NativeFailure }
pub trait RegionDetector: Send {
    fn process(&mut self, rgba: &[u8], width: u32, height: u32,
        options: RegionOptions, labels: &mut [u8],
        regions: &mut [Region; MAX_REGIONS]) -> Result<usize, RegionError>;
}
```

RGBA is tightly packed RGBA8; detector reads normalized red. `labels` is tightly packed one byte per pixel; 0 background, selected labels 1…count. Native regions use top-left normalized XY, Y down; box edges cover pixel extents, centroid uses pixel centres, area is normalized count. Sort accepted components by descending pixel area, then top, left, original component label; relabel in that order. All rejected pixels become 0. Reuse cv::Mat storage and application-owned ranking/remap scratch. Do not allocate one contour vector per component.

Add matching C structs `BlobRegionV2`, `BlobRegionOptionsV2` and exports to the same plugin:

```c
void* BlobDetectorV2_Create(void);
void BlobDetectorV2_Destroy(void* handle);
int32_t BlobDetectorV2_Process(void* handle,
    const uint8_t* rgba, size_t rgba_len, uint32_t width, uint32_t height,
    const BlobRegionOptionsV2* options,
    uint8_t* labels, size_t labels_len,
    BlobRegionV2* regions, size_t regions_capacity);
```

Return 0…32 count, −1 invalid input, −2 native failure. Catch exceptions at the C boundary. Validate pointers, dimensions, checked products, capacities and finite options in both languages; require exact image lengths, each dimension in 1–1024, and capacity ≥32. Clear outputs on failure. Wrapper `FfiRegionDetector::new() -> Result<Self, String>` lives in new `ffi/region_ffi.rs`, reuses bundle resolution and its non-unloading library policy. Do not make legacy loading require V2 symbols. Assert C/Rust size and offsets: Region 32 bytes; options 24 bytes. New symbols missing in an old bundle produce an explicit diagnostic, not legacy fallback.

Three new primitives, registered through the existing primitive registry:

| Primitive/file | Inputs | Outputs |
|---|---|---|
| `node.detect_regions`, `detect_regions.rs` | `mask: Texture2D`; optional scalar `reset` | `labels: Texture2D`; `regions: Channels[LABEL:U32,X:F32,Y:F32,WIDTH:F32,HEIGHT:F32,AREA:F32,CX:F32,CY:F32]`; `updated`, `sample_dt`, `valid`: ScalarF32 |
| `node.track_regions`, `track_regions.rs` | same `regions` channels; required scalar `updated`, `sample_dt`, `valid`; optional scalars `reset`, `frame_aspect` | `tracks` channels described below; legacy-layout `boxes: Channels[X:F32,Y:F32,WIDTH:F32,HEIGHT:F32]` |
| `node.region_mask`, `region_mask.rs` | `labels: Texture2D`; `tracks` channels | `out: Texture2D` |

`sample_dt` is elapsed source-capture time in seconds, computed with the project's `Seconds` type internally, never elapsed worker time. `updated=1` for exactly one graph execution per consumed sample; otherwise 0. When updated=0, retain the previously published validity, labels and regions; worker latency is not an invalid sample. A failed consumed sample publishes updated=1, valid=0 and empty outputs. Explicit lifecycle reset clears outputs/validity immediately even without a consumed sample. `valid=1` only after a successful current-generation sample, including an empty result. All outputs from one sample are published together. CPU consumers must see CPU-written region arrays in the same execution; do not queue a GPU write then read its mapped buffer before completion.

`labels` uses an analysis-sized Rgba8Unorm texture, RGB=label/255, A=1. Nearest/texel loads recover `round(r*255)` exactly. The renderer maintains a reusable RGBA expansion buffer for uploading the byte labels. This is categorical data, not a colour-managed image.

Track ABI is 64 bytes per slot, in order: `ID:U32,LABEL:U32,OBSERVED:U32,AGE:F32,X:F32,Y:F32,WIDTH:F32,HEIGHT:F32,CX:F32,CY:F32,VX:F32,VY:F32,AREA:F32,PAD0:U32,PAD1:U32,PAD2:U32`. Padding is zero; AREA is the current measured normalized foreground area, available to mask selection. Define matching `#[repr(C)]` renderer struct and shader declaration; assert offsets. AGE is seconds since track birth; VX/VY normalized image units/second. Track IDs are runtime-local nonzero u32 values, not EffectIds, serialized IDs or user routing addresses. Slots are fixed at 32; unused ID=0. Overflow resets the local tracker before reusing IDs. `boxes` compacts only observed tracks, in slot order, and uses the legacy HUD's top-left convention.

Three small supporting operations complete the graph:

* `node.resize_limit` in `resize_limit.rs`: `in/out: Texture2D`, authored `max_dim: Int` default 320, range 64–1024. Bilinear sampling into Rgba16Float, dimensions as section 3. Use `downsample.rs` as the output_dims/output_canvas_scale and allocation precedent; its existing integer factors cannot express this cap. Dimensions propagate from labels through region_mask, morphology and feather, so those passes remain analysis-sized. Max dimension is a graph construction setting, not a live slider. Add a standalone GPU resampling/dimension proof.
* `node.rgb_distance` in `rgb_distance.rs`: `in/out: Texture2D`; scalar parameters and optional shadow ports `red`, `green`, `blue`, defaults 1/0/0, ranges 0–1. Output RGB=`length(input.rgb-target.rgb)`, A=1, using the exact Euclidean operation from chroma_key. This one-operation primitive makes colour targets scalar-bindable without changing the existing chroma-key ABI. Pointwise fusion is supported and must have standalone/fused GPU parity.

The remaining primitive is `node.mask_extrema` in `mask_extrema.rs`: texture `in`, optional scalar `radius`, texture `out`; fixed authored `axis` X/Y. Radius is rounded/clamped to signed integer −32…32; positive=max/dilate, negative=min/erode, zero=identity. Use two instances X then Y for a square structuring element. Out-of-image samples are zero. It operates on coverage, never labels. This is a neighbourhood primitive, not a fusable pointwise body. Existing Gaussian/invert/amount nodes supply the remaining operations. Any added fusable body must pass the existing standalone-versus-fused GPU proof; do not claim fusion for neighbourhood or CPU operations.

## 5. Tracking, ownership and cost

Implement a bounded tracker with fixed arrays, modelled on `track_persist.rs`'s pair list; no tracking crate. Update state **only** on a fresh successful sample. valid=0/reset clears state immediately. On missing samples, retain output without ageing it again. On successful empty samples, advance age and misses by sample_dt. Expire unmatched tracks after Retention seconds; labels of unmatched tracks become 0 and OBSERVED=0 immediately.

For each existing track, predict centroid with its velocity for at most Retention seconds. Gate matches by aspect-corrected centroid distance ≤0.15 image heights and area ratio in [0.25,4]. Score valid pairs `0.65*(distance/0.15) + 0.35*(1-IoU)` using translated predicted boxes. Convert horizontal differences to image-height units with width/height; the tracker receives authored scalar `frame_aspect` through the existing texture-size graph, default 1.0 for stand-alone square fixtures. Sort by cost, then track ID, then detection label; accept best-first without reusing either endpoint. Keep IDs on matches, allocate new IDs for unmatched detections. Existing tracks that expire free slots before spawning; if all slots remain retained, drop unmatched detections until slots free. No promise to preserve identity through complete occlusion or indistinguishable crossings.

On a match, measured velocity is centroid displacement / sample_dt. Smooth velocity and box/centroid using `alpha = 1-exp(-sample_dt/max(smoothing_seconds,epsilon))`; smoothing zero copies measurements. New tracks initialize directly from their measurement with zero velocity. Guard zero/nonfinite dt; reset on backward time and gaps >1 second. Tracking area for matching and Largest selection is measured foreground area retained in CPU state and published as AREA, not smoothed box area. Pass exposed Smoothing and Retention to `smoothing_seconds` and `retention_seconds`; source/detector parameter IDs are the snake_case names in RegionOptions, and mask parameter IDs are `selection`, `expand`, `feather`, `invert`, `amount`. Keep previous raw measurements privately for velocity estimation; do not subtract a smoothed position from a raw measurement. Region-mask Largest chooses the largest **observed** region, ties by lowest track ID; All unions observed labels. It does not carry a largest-selection lock across frames.

Ownership: each detection-node instance owns its readback resources and `BackgroundWorker`; that worker exclusively owns one native handle. Each tracker instance owns its track array. No state serializes. UI sees the existing parameter snapshots and sends existing commands. Do not introduce `Arc<Mutex<_>>`, a per-project analysis manager, or shared detector state between presets.

Private worker packet contract: one owned reusable packet containing RGBA Vec, labels Vec, `[Region;32]`, options, dimensions, generation u64, sample serial u64 and capture timestamp `Seconds`; response returns the **entire packet** plus `Result<usize,RegionError>`. One in-flight request maximum. Recycling must work on success, empty result, failure and stale generation. Reserve at warmup/resolution change; retain high-water capacity. Use `ReadbackRequest::try_read_into`, not `try_read`, per-request `collect`, fresh frame clones or new response lists.

Mirror optical flow's `flow_schedule` ordering with tests: completed response consumed at the prescribed next-run deadline before submitting another request; readback and worker share no mutable allocation concurrently. Reset cancels/drains pending readback, increments generation, clears validity/labels/tracks, and rejects late replies while reclaiming their buffers. Resize, explicit reset, source replacement, seek/retrigger, disable/re-enable and preset removal must use the existing runtime lifecycle. Prewarm pipelines, analysis textures, buffers and native handle through existing warmup hooks; no first-use compilation during the acceptance gesture.

**Consequences, stated honestly:** bounded Rust buffers do not prove zero allocations inside OpenCV or std channel internals. Measure native allocation counts and peak memory separately. Do not call this allocation-free. If repeated native scratch allocation causes the deadline/latency gate to fail, report the evidence and stop that phase; changing the algorithm/backend is outside this contract. Fixed-lag waits may stall playback; motion adds optical-flow cost and additional lag. Masks retain the most recent observed pixels between analyses and can visibly lag fast edges. Smoothing adds box lag; it is adjustable.

## 6. Invariants and enforcement

The following are **new test names to implement**, not existing passing checks. Prefix all new tests `blob_v2_` so focused gates select them.

| Invariant | Required check |
|---|---|
| Original effect remains available/loadable with unchanged bindings | `blob_v2_legacy_preset_contract`; no diff to original preset or old C exports |
| Shape/holes, top-left coordinates and normalized area survive | `blob_v2_regions_ring_border_and_diagonal`; asymmetric off-centre fixtures, 8-connected diagonal, rejected component label=0 |
| Bounded ABI, deterministic selection, explicit error | `blob_v2_ffi_bounds_and_layout`, `blob_v2_top_k_filters_before_limit`, `blob_v2_missing_symbols_reported` |
| Stable IDs, fresh-sample aging and reusable slots | `blob_v2_tracking_identity_and_gaps`; two unequal blobs cross without overlap, one disappears/reappears within grace, detection order reverses; separate merge/split test asserts documented limits |
| Label image and tracks refer to the same sample | `blob_v2_sample_publication_is_atomic`; deliberately delayed worker and reused labels |
| Reset cannot resurrect old pixels | `blob_v2_lifecycle_discards_stale_samples`; reset/resize/backward seek/disable/re-enable with an in-flight response |
| Error cannot turn inverted mask fully on | `blob_v2_invalid_inverted_mask_is_zero`; distinguish successful empty result |
| Actual pixels, not boxes, drive group coverage | `blob_v2_group_mask_ring_and_dry_input`; ring hole stays dry, region wet, exterior dry, nested group and partial wet/dry |
| New primitives are composable and parameters connected | `blob_v2_preset_graphs_and_bindings`; validate all six graphs, each outer parameter's reachable target; metadata/menu contract |
| Buffer reuse is real | `blob_v2_packet_storage_reused`; pointer/capacity and in-flight-count assertions after warmup, failure, reset and size changes |
| GPU morphology, label decoding, feather and alpha agree with contract | `blob_v2_mask_pixels`; both axes, radius zero, positive/negative radius, border, labels 1 and 32, fp16 output tolerance |
| Save/load, editing and ownership stay intact | `blob_v2_mask_save_undo_redo`; normal EditingService path, params and group membership persist |

Numerical assertions are the image oracle. Save representative source/mask/HUD/composite PNGs for human review; do not make a model's visual judgement the correctness oracle. Track quality claims require those fixtures and an observed render, not compilation alone.

## 7. Phasing — four checkpoints in one overnight run

Before each phase: `git status --short`, re-read its source anchors, inspect existing beads for this work, acquire a verified application slot using `scripts/agent-worktree.py` per `.claude/GIT_TREE_DISCIPLINE.md`, and read `.codex/README.md`. A moved anchor requires a short conflict report, not implementation from memory. Read back the binding decisions in a few sentences. No duplicate planning report is required.

All phases use focused clippy/tests for changed crates and `scripts/codex_checks.py --base <verified-base>` for additional mapped checks. GPU phases run `python3 scripts/gpu_proofs_gate.py --filter blob_v2` plus required mapped proofs. These are test filters, not permission to skip mandatory landing checks. Finish through `scripts/land_branch.py` and release the slot. Do not run workspace-wide sweeps. Continue failed checks only with changed code or new evidence; report precise evidence when no justified next step remains.

### P1 — Native region seam

Entry/read-back: audit native plugin, wrapper, build script, original preset and D1–D3. Deliver new C exports, Rust trait/wrapper, deterministic fixed-capacity component selection, labels, errors, native fixtures and packaging proof. Register no incomplete user-facing preset.

Checks: `bash assets/plugins/BlobDetector/build.sh`; `cargo test -p manifold-native blob_v2_`; `cargo clippy -p manifold-native --all-targets -- -D warnings`. Implement first three applicable rows of section 6. The native tests must fail, not skip, when the rebuilt test bundle is absent. Add a relocated-bundle test using the existing plugin override: load from a temporary directory outside the repo, exercise old and new symbols, check `otool -L` dependencies and codesigning; no absolute Homebrew runtime paths. Preserve existing packaged notices. Report OpenCV version and allocation/latency measurements on empty, ring, noisy and crowded 320-pixel inputs.

Acceptance: L1; fixed input bytes yield exact expected labels/stats. A second independently constructed fixture catches hardcoding. Forbidden: legacy ABI changes, backend replacement, silent fallback, hand-rolled CV. This is a native foundation phase with no performer surface.

### P2 — Visible V2 brightness effect

Entry/read-back: P1 symbols/tests landed; inspect worker/readback, optical-flow lifecycle, registry, legacy graph end-to-end; bind D4/D6/D7. Deliver `resize_limit`, `detect_regions`, `track_regions`, wire types and brightness `BlobTrackingV2.json`, reusing the existing HUD. Include metadata/picker registration and any required thumbnail assets through existing tooling.

Checks: `cargo test -p manifold-renderer blob_v2_`; `cargo clippy -p manifold-renderer --all-targets -- -D warnings`; scoped GPU gate above. Implement tracking/publication/lifecycle/buffer/preset tests from section 6. New test `blob_v2_tracking_demo` renders a deterministic moving ring plus a smaller blob, disappearance and source reset; writes PNGs under `target/blob-v2-demo/` with IDs, measured sample age and tracked coordinates in a sidecar. Run via `cargo test -p manifold-renderer --features gpu-proofs blob_v2_tracking_demo -- --nocapture`.

Gesture/demo: select original Blob Track, then V2, sweep Threshold and Smoothing; both remain selectable, V2 changes regions/box response and surviving IDs stay stable. Extend the existing preset-picker UI-flow surface for this gesture; numerical tests establish tracking, the observed demo establishes visible output (L2). Forbidden: replacing legacy graph, new HUD renderer, treating repeated samples as misses, passing extended channels into old box filters.

### P3 — Blob Mask modifier

Entry/read-back: P2 landed; read effect-group mask compile path, `mask_menu_items`, `AddGroupMaskCommand`, existing group-mask tests and UI flow manifest. Deliver `region_mask`, `mask_extrema`, brightness `MaskBlob.json` and the Blob menu entry. Complete all mask, undo/redo and serialization checks in section 6. No core project-schema changes.

Checks: focused renderer/editing/app tests with `blob_v2_` prefix; focused clippy for those changed crates; scoped GPU gate. Add `blob_v2_mask_demo` (same command form as P2) writing raw mask and processed-group PNGs under `target/blob-v2-demo/`. Add `scripts/ui-flows/inspector-blob-mask.json` based on `inspector-add-mask.json` and register it for the existing `inspector` scene. Run `cargo xtask ui-snap inspector --script scripts/ui-flows/inspector-blob-mask.json`.

Gesture/demo: add Blob Mask to a group containing an obvious colour effect; sweep Expand through negative/zero/positive, Feather, Invert and group wet/dry, then undo/redo mask creation. Ring hole stays dry until the deliberately expanded mask closes it; controls affect coverage without rebuilding topology. L3 for add/undo interaction and L2 plus numeric pixel assertions for processed coverage. Forbidden: rectangle-only masks, bilinear label IDs, reading wet input, manual UI state writes.

### P4 — Source variants and measured release proof

Entry/read-back: P2/P3 landed; verify shipped chroma-key math, scalar binding limits and optical-flow node contracts and `MotionMosh.json` extraction wiring. Deliver `rgb_distance` and its standalone/fused proof, the four colour/motion preset variants and two corresponding mask menu entries, all binding/availability tests, motion cut reset and first-frame validity tests. Keep the shared detector/tracker identical across variants. No source-mode mega-node or vector-binding migration. Add the new helper to the primitive audit with its explicit scalar-binding reuse reason.

Checks: `cargo test -p manifold-renderer blob_v2_`; changed-crate clippy; scoped GPU gate; extend the P3 flow to select each mask variant. A new `blob_v2_source_variants_demo` generates matched-colour, wrong-colour, static, moving and hard-cut fixtures and records mask area, IDs, fresh-sample cadence and capture-to-output frame age. Motion first/reset frames must not flash full coverage. Commands follow P2's exact test form.

Performance acceptance: one bounded warm run of 600 frames after 60 warmup frames, 1080p output, analysis320/default8 blobs; a second case exercises authored max32 and 1024 analysis dimension as a stress case, reported separately. Record median/p95/max content-thread analysis work, worker duration, readback age, fixed-lag wait, allocation count and retained bytes; use `MANIFOLD_RENDER_TRACE=1`. Compare legacy and V2 on the same input/device/build. Default steady-state analysis contribution p95 must be ≤2ms and no content frame >20ms attributable to this change; failures are evidence to resolve before release, not numbers to relabel. Motion variant is reported separately including its existing flow cost. No claimed speedup without measurements. No automatic quality fallback when a limit fails.

Gesture/demo: choose Colour to isolate the coloured region, then Motion to isolate moving pixels; adjust relevant threshold live. Confirm all six presets resolve, every new mask menu item works and legacy stays available. Review one representative render per changed behaviour; do not repeat passed renders/checks without new evidence. Update this header and the existing effect-mask contract when implemented; record unresolved engineering/verification work in beads.

Delegation: Sol at Extra High is the orchestration agent and owns diagnosis, contracts, integration, review, validation and landing. Use native `gpt-5.6-luna` workers at high effort for bounded mechanical implementation after Sol fixes the shape. Keep two Luna lanes active when independent work exists; use a third only for a genuinely independent scope within the available slot limit. Parallelism is not a quota. Prepare briefs with `scripts/codex_prepare.py`; name exact file ownership, established findings, reuse targets, acceptance criteria and commands selected through `scripts/codex_checks.py`. Workers neither delegate nor land. Sol leaves owned lane files alone until return, reviews the edits, and integrates them. Do not parallelize dependent detector and mask work before the label/track ABI lands.

## 8. Decided — do not reopen

1. Original Blob Track remains available and compatible; V2 has new IDs.
2. OpenCV stays behind the current native bundle; Rust owns bounded tracking.
3. Connected-component labels preserve actual regions and holes.
4. Preparation, detection, tracking, mask rendering and group compositing remain separate.
5. Three source variants share one implementation; no inactive mode controls.
6. Identity retention does not invent missing silhouette pixels.
7. Reuse buffers and lifecycle infrastructure; measure native costs and fixed-lag stalls.
8. Existing mask menu, EditingService, parameter surface and project fields own the feature.

## 9. Deferred

| Excluded | Revival trigger |
|---|---|
| Motion Mosh/Data Mosh changes | Their separately owned workstream requests an explicit shared seam change. |
| Watershed, split touching objects, learned/person segmentation | Held-out footage proves component merging prevents the intended use and Peter commissions the added detector. |
| HSV/adaptive/background-subtraction sources | Brightness/colour/motion fixtures expose a documented selection failure needing another composable source. |
| Optical-flow warping of silhouettes, contour trails, distance-field rings | Basic mask ships and Peter requests that distinct temporal/visual behaviour. |
| Automatic hard-cut detector for brightness/colour | Demonstrated false cross-cut identities cannot be handled by existing lifecycle reset; requires a separate reusable cut source. |
| Per-ID UI selection, OSC blob streams, arbitrary-effect parameter mapping | A concrete consumer specifies ordering, lifetime and ownership. Current graph outputs preserve the data seam. |
| Cross-layer/source-only detection UI and visibility rules | The existing effect-mask workstream lands that contract; reuse it then. |
| Global detector deduplication or native-Rust replacement | Measurements establish a material bottleneck and a separately reviewed design beats the current pipeline. |

## 10. Overnight orchestration handoff

Peter's instruction: “I want Sol to run this overnight end to end so I can use it tomorrow morning. Sol should use Luna lanes and Sol will be set to Extra High. Sol acts as the orchestration agent.”

Execute P1 → P2 → P3 → P4 without asking permission between checkpoints. Keep the original task active across compaction; re-read this contract and actual landed state rather than restarting. A worker finishing is an integration checkpoint, not task completion. Do not stop after producing a plan, dispatching workers, compiling, landing P1, or leaving the feature solely in a worktree.

Sol may resolve routine implementation details, stale symbol locations and source-backed contract corrections within the approved product scope; update this document and the corresponding assertions when correcting it. Preserve the legacy effect, composability, ownership, buffer discipline and acceptance requirements. Do not add deferred features, change the backend, introduce shared locks, or weaken failed checks to finish overnight. Keep retries evidence-driven: return a failing lane's evidence to Sol, avoid speculative retries and continue any independent authorized work. A genuine blocker must be reported precisely, with verified work preserved under the slot lifecycle; never report an incomplete feature as ready.

Suggested lane allocation after the relevant interfaces are fixed:

| Checkpoint | Luna lane A | Luna lane B | Sol |
|---|---|---|---|
| P1 | Native component implementation | Rust wrapper and ABI/error fixtures | Define ABI; inspect native costs; packaging, review and landing |
| P2 | Bounded tracker and CPU fixtures | Resize helper and its GPU proof | Analysis lifecycle, shared registration, V2 graph integration and observed render |
| P3 | Mask raster/morphology under pinned ABI | Preset/menu/undo wiring | Group routing review, integration and mask acceptance |
| P4 | RGB-distance primitive and fusion proof | Colour/motion preset data after helper contract is fixed | Motion lifecycle, performance evidence and final app delivery |

Treat this table as ownership guidance, not permission to edit shared registration/test-module files concurrently. Sol owns shared files and merges the workers' required registrations. Assign fixture work within the lane owning the behaviour, and have Sol independently review the assertions. Workers use the same workstream slot with disjoint paths; they do not each acquire a slot or run simultaneous builds against the shared build lock.

Completion means all six new presets are reachable in their intended surfaces, the original Blob Track remains usable, native packaging is current, the required behavioural/GPU/UI and focused Rust checks pass, performance evidence is recorded, and verified app changes are committed, landed and pushed. Finish the repository's normal build/delivery path for the runnable app Peter will launch; verify its revision and provide the exact launch command/path. A worktree-only binary or an old installed app is not completion. Release landed slots and properly retire unfinished inactive slots if blocked.

Morning response: briefly state what is usable and where to find it, provide the exact launch command, link the observed demo artifacts and landed revision, and state any remaining limitation or failed gate plainly. Do not promise a completion time that has not been achieved.

Copyable start prompt:

> You are Sol, set to Extra High, acting as the orchestration agent. Read `docs/BLOB_TRACKING_V2_DESIGN.md` and current `AGENTS.md`. Implement the entire approved P1–P4 contract overnight, using native `gpt-5.6-luna` lanes at high effort for bounded work. You own design compliance, dependency ordering, integration, review, testing and landing. Continue automatically between phases and across compaction. Preserve original Blob Track and keep Motion/Data Mosh changes out of scope. Complete the required native packaging, GPU/UI/behavioural checks, performance measurements, app delivery and slot cleanup. Do not stop at a plan, a worker handoff or a partial implementation. Follow the failure budget and report genuine blockers accurately. Finish with the usable app's exact launch command, where to find the effects/masks, the landed revision and any remaining limitations.
