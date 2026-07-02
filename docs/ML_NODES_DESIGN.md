# ML Nodes — Perception Runtime, Node Roster, Point Arrays

Status: **APPROVED** (Peter, 2026-07-02). Designed on Fable; implementation is Sonnet work, phased in §12.
Companions: NODE_CATALOG.md (roster lands there), CHANNEL_TYPE_SYSTEM.md (point arrays ride §5-§6 machinery), MULTI_DISPLAY_DESIGN.md §12 (auto-calibration shares this runtime).
Prerequisites: none for the Vision/CoreML tier; the ONNX tier needs VULKAN_BACKEND_DESIGN shipped. Sequencing: `docs/DESIGN_BUILD_ORDER.md` wave 3.
Execution contract: read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting any phase. Conformance-hardened: run the §8.3 pre-flight before each phase — node ids here predate the vocab-audit apply; check the migration table for any node this design references.

## 1. Goal

ML nodes let a neural net *understand* a frame and hand the result to the graph: depth maps, person masks, motion fields, skeletons, "name it, get a mask" segmentation. Point a camera at the performer and their body drives the show — mask, silhouette, wrists, face. On stage this is the difference between camera-as-gimmick and camera-as-instrument.

They are ordinary atoms. Frame in (any `Texture2D` — camera, clip, generator, the whole composition), result out (texture or point array). No special subsystem visible to the user.

Non-goals, permanent: no generic "load any ONNX file" node (tensor-shape/port-typing mess; curated single-purpose atoms only, same doctrine as curated-primitives-over-raw-WGSL). No audio ML on the graph (audio stays on the perform surface). No fused monoliths — each node is one inference task.

## 2. Inventory (what exists, 2026-07)

- **Three ML nodes ship today**: `node.depth_estimate_midas`, `node.person_segment`, `node.optical_flow_estimate`. All follow the same proven pattern: background worker thread, node reads the latest completed result, `Update Interval (frames)` + `Analysis Max Dim` params, async GPU→CPU readback, staging-buffer upload back to a texture. **The async contract is built and battle-tested; this design keeps it unchanged.**
- **The brains are toy-tier**: `assets/plugins/DepthEstimator` is OpenCV DNN pinned to `DNN_TARGET_CPU`, running MiDaS-small-256 (2020). Only `midas_small.onnx` actually ships. Person segmentation uses selfie-seg-class models built for video calls. Optical flow is classical Farneback — CPU math, not ML, and fine as-is.
- **`node.one_euro_filter`** exists for scalar smoothing.
- **Array wires are Channels-typed structs** (`KnownItem` + `ChannelSpec` slices, std430, GPU storage buffers with runtime length via `arrayLength`). Pose output needs zero new port machinery.
- **No camera or stream input exists anywhere.** manifold-media decodes files only.

## 3. Runtime architecture

### 3.1 Task-level traits, not tensor-level

The abstraction is the *task*, not the model format. `manifold-native` grows an `inference` module:

```rust
pub trait DepthEstimator: Send {
    fn infer(&mut self, frame: &InferenceFrame) -> DepthResult;
}
pub trait PersonMatter: Send { /* frame -> alpha matte */ }
pub trait PoseEstimator: Send { /* frame -> Vec<PersonPose> */ }
pub trait HandTracker: Send { /* frame -> Vec<HandPose> */ }
pub trait FaceLandmarker: Send { /* frame -> Vec<FaceMesh> */ }
pub trait PromptSegmenter: Send { /* (frame, text prompt) -> mask */ }
```

A platform factory picks the implementation. Nodes and workers speak traits; they never know CoreML from ONNX. This is the same shape as `CaptureBackend` in manifold-audio.

### 3.2 Apple backend (primary)

- **Vision framework built-ins** cover pose (`VNDetectHumanBodyPoseRequest`, multi-person), hands (`VNDetectHumanHandPoseRequest`, 21 points), face landmarks (`VNDetectFaceLandmarksRequest`), and person segmentation / instance masks (`VNGeneratePersonSegmentationRequest`, `VNGeneratePersonInstanceMaskRequest`). Zero model files shipped, ANE-accelerated, Apple maintains the models. Verify exact request availability against the deployment target at implementation.
- **CoreML** runs the tasks Vision doesn't ship: monocular depth, promptable segmentation. Models converted offline at dev time (coremltools → `.mlpackage`), compiled to `.mlmodelc` on first load (seconds — always at load/setup, never mid-show).
- **Zero-copy input**: Vision/CoreML take `CVPixelBuffer`. We have IOSurface infrastructure — GPU downsample → IOSurface-backed pixel buffer → inference, no CPU readback on the Apple path. (The existing readback path stays for the generic backend.)
- **Inference costs zero GPU time.** ANE is a separate unit; the 4.5–5.5 ms/frame render budget is untouched. This is the main reason CoreML wins on Apple.
- Bindings via `objc2` family crates, in-crate under `cfg(target_os = "macos")`. The C++ plugin-bundle pattern is not extended to new work.

### 3.3 Generic backend (later, gated)

ONNX Runtime (`ort` crate) behind a cargo feature, one impl per task trait, open-licensed models (§5 table). Built when Linux/Windows (Vulkan port, queue #5) becomes real — not before. Numeric differences vs the Apple path are accepted (Peter, 2026-07-02); cross-backend tests are tolerance-based, never bit-exact.

### 3.4 Workers and the model cache

- Per-node worker thread (today's pattern), owning a boxed task-trait object.
- **`ModelCache: AHashMap<ModelId, Arc<LoadedModel>>`** — two nodes using the same model share one loaded instance (CoreML prediction is thread-safe). Matters at typical project scale (53 layers).
- Legacy OpenCV plugin: depth and segmentation paths retire when P1/P3 land. Farneback flow keeps the plugin alive until a flow replacement is worth doing (it isn't yet).

## 4. Sources

- **`node.camera`** — new source atom. AVCaptureSession → CVPixelBuffer → IOSurface → Metal texture, zero-copy. Device-selection param; Continuity Camera (iPhone as camera) comes free with AVFoundation. Camera permission handled at app level (Info.plist).
- ML nodes consume **any** `Texture2D`. Camera is just one source — running depth estimation on your own generative output is legitimate and encouraged.
- Deferred sources, same slot later: NDI in, Syphon in, ScreenCaptureKit (capture any app/display — the video analog of the audio output-tap).

## 5. Node roster and model picks

License rule: **Apache-2.0 / MIT / BSD ship; GPL / AGPL / CC-BY-NC are banned** (commercial product). Every pick below carries its license; verify all licenses again at implementation — the model landscape moves.

| Node | Status | Apple path | Generic path (later) | License notes |
|---|---|---|---|---|
| `node.depth_estimate` | upgrade existing | Depth Anything V2 **ViT-S** via CoreML | same, ONNX | DA V2 Small = Apache-2.0. **Base/Large are CC-BY-NC — banned.** |
| `node.person_segment` | upgrade existing | Vision person segmentation / instance masks (built-in) | BiRefNet-class matting, MIT | **RVM is GPL — banned.** Verify candidate license at pick time. |
| `node.optical_flow_estimate` | keep | Farneback (existing plugin) | same | No change. ML flow (NeuFlow-class) only if a real need appears. |
| `node.pose` | new | Vision body pose (built-in) + our tracker | RTMPose-class, Apache-2.0 | **Ultralytics YOLO-pose is AGPL — banned.** |
| `node.hand_pose` | new | Vision hand pose (built-in) | MediaPipe hand model, Apache-2.0 | |
| `node.face_landmarks` | new | Vision face landmarks (built-in) | MediaPipe face landmarker, Apache-2.0 | |
| `node.segment_anything` | new | SAM 2-class small + open-vocab grounder, CoreML | same, ONNX | SAM 2 = Apache-2.0; grounder (Grounding-DINO/OWLv2-class) = Apache-2.0. |

`node.segment_anything` — "name it, get a mask": text param (`Prompt`), text embedded once per prompt change (worker-side, cached), mask output every inference tick. v1 output = single combined mask; per-instance mask selection deferred. "Explode the chair" = chair mask → existing shatter/particle graphs. Person mask remains its own cheaper node (built-in, faster, zero prompt).

## 6. Point arrays (pose / hands / face)

The genuinely new output shape: points, not pixels. Rides the existing Array Channels system.

### 6.1 Item struct

```rust
#[repr(C)]
pub struct Keypoint2D {
    pub x: f32,          // source-frame UV [0,1]
    pub y: f32,
    pub confidence: f32, // 0 = absent/occluded
    pub person: u32,     // stable tracking ID
    pub joint: u32,      // joint index, per-task namespace
}
// KnownItem SPECS: x: F32, y: F32, confidence: F32, person: U32, joint: U32
```

One struct for all three tasks; `joint` namespaces differ (body ~17–19, hand 21 per hand, face ~470). Buffer holds only live keypoints — `arrayLength` gives the count. Companion `Scalar` output: `person_count` (or hand/face count).

### 6.2 Identity and tracking

- The **worker owns ID association**, independent of backend (greedy IoU/ByteTrack-style matching, ~100 lines, MIT-licensed prior art). Vision doesn't provide stable IDs; ours are consistent across both backends.
- IDs are **monotonic u32, never reused within a session** (same doctrine as typed IDs). Person walks off camera → their keypoints stop appearing; walk back within a short re-association window → same ID; otherwise a new one.
- This **supersedes the parked blob tracker** at a higher semantic level (Peter unparked person-tracking-in-evolved-form, 2026-07-02). Blob detection itself stays parked.

### 6.3 Smoothing and convenience

- Worker-side **One-Euro filter per (person, joint)**, exposed as a `Smoothing` param on the node. Identity-aware smoothing has to live where identity lives. The standalone `node.one_euro_filter` stays for scalar wires.
- **`node.skeleton_edges`** — pure transform atom mapping a pose array → `EdgePair`-style array for the existing line/mesh renderers. Draw the skeleton with what's already there; no bespoke skeleton renderer.

### 6.4 Coordinate space

Keypoints are in source-frame UV. Mapping camera space → stage space (multi-display) is calibration's job (MULTI_DISPLAY_DESIGN.md §12), not the pose node's.

## 7. Export determinism

Bug that exists today: workers run at wall-clock rate, so exported output depends on export speed — non-deterministic. Decision:

- **Offline render mode: inference runs synchronously**, on the exported-frame count cadence (`Update Interval` is already frame-based). Slow but exact, reproducible.
- **Live mode: latest-result**, unchanged.

The mode flag comes from the playback engine; workers expose both paths. Applies to all ML nodes including the three shipping ones — this fix lands with the runtime (P1).

## 8. Reset and staleness

Hard cut / seek / chain reset, but the worker's latest result is from the previous scene — stale content bleeds for a few frames. Decision: **generation counter**. Reset bumps the node's generation; in-flight results are tagged and discarded on arrival if their generation is stale. Wired into the existing state-reset walk (both chain-state caches).

## 9. Packaging and distribution

- **Vision built-ins: zero files.** Pose, hands, face, person-seg ship nothing on Apple.
- **Bundled**: depth (DA V2 ViT-S fp16 CoreML, ~50 MB) — small enough to live in the app.
- **Downloaded on demand**: the promptable-segmentation pack (SAM 2 small + grounder, ~300 MB class). Download and CoreML-compile happen in **setup contexts only — never mid-show**. Cached under Application Support; sha256-pinned manifest.
- ONNX artifacts are produced only when the generic backend becomes real.

## 10. Performance

- Apple path: inference on ANE = zero GPU contention; input handoff zero-copy via IOSurface. Content-thread cost per inference tick ≈ one GPU downsample + result upload (today's staging pattern).
- Thermal: 4+ concurrent models at high Hz will throttle eventually; `Update Interval` is the user's lever, defaults chosen per node (depth every 2–3 frames is invisible; pose wants every frame).
- Same-input dedup (5 layers each running depth on the same camera = 5× inference) — **deferred optimization**: result cache keyed (model, source texture `DataVersion`). Note it, don't build it yet.
- MetalFX upscaling (render at fraction, ML-upscale to native — attacks the open 4K-margin problem) is **adjacent infra, not a graph node**; tracked as its own perf item, not in this design's phases.

## 11. Testing

- **Mock backends** per task trait (deterministic fake outputs) — graph-level tests need no weights, no ANE, run in CI.
- Real-model smoke tests behind `#[ignore]`.
- Depth-node migration (OpenCV MiDaS → CoreML DA V2): tolerance/visual comparison, not bit parity — different model, better output is the point. Headless-PNG verification for the visual check.
- No GPU parity harness for inference outputs (non-deterministic across backends by design).

## 12. Phasing

- **P1 — runtime**: task traits + Vision/CoreML backends + ModelCache + generation reset + export-sync contract. Migrate `depth_estimate_midas` → DA V2 CoreML and `person_segment` → Vision as proof. Retire OpenCV DNN paths (flow stays).
- **P2 — camera**: `node.camera`, zero-copy, device param, permissions.
- **P3 — pose**: `Keypoint2D` item + tracker + `node.pose` + worker-side One-Euro + `node.skeleton_edges`.
- **P4 — hands + face**: same contract, new joint namespaces.
- **P5 — promptable segmentation**: SAM 2 + grounder, prompt param, download pack.
- **P6 — generic backend**: ONNX Runtime impls. Gated on cross-platform being strategic (queue #5); do not start speculatively.

Full workspace sweep gates P1 (runtime infrastructure). P2–P5 are per-node scope.

## 13. Decided (don't reopen)

1. Task-level backend traits; CoreML/Vision on Apple, ONNX Runtime elsewhere; per-platform differences accepted.
2. Vision built-ins wherever they exist — zero shipped weights for pose/hands/face/person-seg on Apple.
3. Async contract unchanged: worker at own Hz, node reads latest, never blocks the tick.
4. Export mode = synchronous inference, frame-count cadence. Live = latest-result.
5. Reset = generation counter discarding stale in-flight results.
6. Keypoints = `Keypoint2D` Channels-typed array (§6.1), UV space, monotonic never-reused person IDs, worker-owned tracking.
7. License gate: Apache/MIT/BSD ship; GPL/AGPL/CC-BY-NC banned. Audit at every model pick.
8. Curated nodes only — generic ONNX-loader node rejected permanently.
9. Person tracking via pose IDs supersedes the parked blob tracker; blob detection itself stays parked.
10. Camera is a source node; ML nodes accept any Texture2D.
11. Model downloads/compiles in setup contexts only, never mid-show.
12. Optical flow stays Farneback until an ML replacement earns its place.

## 14. Deferred / rejected

- **Real-time diffusion img2img** — approved direction, needs its own design pass (model management, prompt-as-param, seed/latent interpolation, thermal budget). Separate doc; the async contract here is its foundation.
- MetalFX upscaling — separate perf item (§10).
- NDI / Syphon / ScreenCaptureKit sources — same source-node slot, later.
- Same-input inference dedup — deferred optimization (§10).
- Per-instance mask outputs on `node.segment_anything` — v1 is single mask.
- Generic ONNX loader node — **rejected**, not deferred.
- Audio ML (stem separation etc.) — off the graph, per standing doctrine; different design if ever.
