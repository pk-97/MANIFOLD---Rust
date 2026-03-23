# MANIFOLD Rust Port — Known Divergences

This file documents EVERY case where the Rust port intentionally differs from Unity. If a divergence is not listed here, it is a bug.

Agents: Before adding a divergence, verify it is genuinely necessary. Most "Rust needs this" claims are wrong — match Unity first, diverge only when forced.

---

## Format: Each Entry

```
### [ID] Short description
- **Unity does:** What Unity's code does
- **Rust does:** What the Rust code does differently
- **Why:** The genuine technical reason this can't be a 1:1 translation
- **Approved by:** Peter / date
- **Files affected:** Rust files that contain this divergence
```

---

## Platform / Runtime Divergences

### [D-01] MonoBehaviour lifecycle → winit ApplicationHandler
- **Unity does:** `Awake()`, `Start()`, `Update()`, `LateUpdate()`, `OnDestroy()` lifecycle on MonoBehaviour
- **Rust does:** `new()` for init, `resumed()` / `window_event()` / `about_to_wait()` for winit event loop, `Drop` for cleanup
- **Why:** No Unity runtime. winit is the platform layer. The lifecycle maps cleanly: `Awake` → `new`, `Update` → `about_to_wait`, `OnDestroy` → `Drop`
- **Files affected:** `manifold-app/src/app.rs`

### [D-02] VideoPlayer → stub/future ffmpeg integration
- **Unity does:** `VideoPlayer` component with `prepareCompleted`, `targetTexture`, etc.
- **Rust does:** Stub `ClipRenderer` trait impl; video decode not yet implemented
- **Why:** No Unity VideoPlayer in standalone Rust. Will require ffmpeg or gstreamer integration.
- **Files affected:** `manifold-playback/src/renderer.rs`

### [D-03] RenderTexture → wgpu Texture + RenderTarget
- **Unity does:** `RenderTexture` with implicit format/usage management
- **Rust does:** `RenderTarget` struct wrapping `wgpu::Texture` + `TextureView` with explicit usage flags
- **Why:** wgpu requires explicit texture usage flags at creation time. The abstraction is structurally equivalent.
- **Files affected:** `manifold-renderer/src/render_target.rs`

### [D-04] Material + Shader → wgpu RenderPipeline + BindGroup
- **Unity does:** `Material.SetFloat("_Name", value)` with string-keyed uniforms
- **Rust does:** Typed uniform buffer structs written to GPU buffer, bound via BindGroup
- **Why:** wgpu has no string-keyed uniforms. Uniform data is a byte buffer. The VALUES and NAMES must still match Unity's — only the delivery mechanism changes.
- **Files affected:** All effect and generator `.rs` files in `manifold-renderer`

### [D-05] ComputeShader dispatch model
- **Unity does:** `ComputeShader.Dispatch(kernel, x, y, z)` with kernel index
- **Rust does:** `compute_pass.dispatch_workgroups(x, y, z)` with explicit pipeline binding
- **Why:** wgpu compute API is different. Dispatch dimensions and workgroup sizes must still match Unity.
- **Files affected:** All compute-based generators and effects

---

## Ownership / Borrow Divergences

### [D-06] Command execute/undo takes &mut Project
- **Unity does:** `ICommand.Execute()` and `Undo()` with no explicit parameter (commands hold references to project state)
- **Rust does:** `Command.execute(&mut self, project: &mut Project)` with explicit project parameter
- **Why:** Rust's borrow checker requires explicit ownership. Commands can't hold mutable references to the project they modify.
- **Files affected:** `manifold-editing/src/command.rs`, all command implementations

### [D-07] GeneratorRenderer.PreRender is a no-op; GPU rendering via downcast
- **Unity does:** `IClipRenderer.PreRender(time, beat, dt)` delegates to `RenderAll()` which does GPU work (Unity GPU API is globally accessible)
- **Rust does:** `ClipRenderer::pre_render()` is a no-op. App calls `GeneratorRenderer::render_all(queue, encoder, ...)` directly via `as_any_mut().downcast_mut()` to pass GPU context
- **Why:** The `ClipRenderer` trait lives in `manifold-playback` (no GPU deps). It cannot carry `wgpu::Queue` or `CommandEncoder` parameters. The GPU rendering must be called on the concrete type.
- **Files affected:** `manifold-renderer/src/generator_renderer.rs`, `manifold-app/src/app.rs`

### [D-08] EffectClipboard: instance-based vs static singleton
- **Unity does:** `EffectClipboard` is a static singleton class (lines 11-56)
- **Rust does:** `EffectClipboard` is an instance-based struct in `clipboard.rs`, requiring callers to pass a mutable reference
- **Why:** Rust has no global mutable singletons without `unsafe` or `Mutex`. Instance-based pattern is idiomatic and equivalent in behavior — the clipboard is owned by `EditingService`
- **Files affected:** `manifold-editing/src/clipboard.rs`

### [D-09] EditingService: stateless vs UIState-coupled
- **Unity does:** `EditingService` reads selection, cursor, layer state directly from `UIState` instance fields
- **Rust does:** `EditingService` is stateless — all methods take explicit parameters (selection IDs, region, etc.)
- **Why:** Rust's `EditingService` lives in `manifold-editing` which has no UI dependency. UIState lives in `manifold-ui`. The caller (app layer) bridges by passing explicit parameters. Behavior is equivalent — just the coupling point differs.
- **Files affected:** `manifold-editing/src/service.rs`

### [D-10] SyncArbiter: explicit authority parameter vs instance field
- **Unity does:** `SyncArbiter` stores `authority` and `target` as instance fields injected in constructor
- **Rust does:** All methods take `authority` and `target` as explicit parameters
- **Why:** Rust's borrow checker makes storing a `&mut dyn SyncArbiterTarget` as a field impractical (lifetime conflicts). Passing explicitly is functionally equivalent — same gating logic, same behavior.
- **Files affected:** `manifold-playback/src/sync.rs`

### [D-11] ~~FluidSim3D density volume: R32Float instead of R16Float~~ RESOLVED
- **Resolved:** Density volume now uses `Rgba16Float` — matches Unity RHalf precision (16-bit), supports both `STORAGE_BINDING` and filtered `textureSample` on Metal.

### [D-12] Texture formats: Rgba16Float instead of R32Float/Rg32Float for storage+sampling textures
- **Unity does:** `RFloat` (R32Float) for density, `RGFloat` (Rg32Float) for vector fields — these textures are both written by compute and sampled with bilinear filtering
- **Rust does:** `Rgba16Float` for all textures that need both `STORAGE_BINDING` and filtered `textureSample`
- **Why:** On Metal via wgpu, `R32Float` and `Rg32Float` are NOT filterable (`textureSample` requires `Float { filterable: true }`). Unity's Metal backend handles this internally. `Rgba16Float` is the only format that supports both storage writes and filtered sampling on Metal. Half precision (16-bit) is sufficient for display intermediates and density fields; the extra channels are unused but harmless.
- **Applies to:** FluidSim3D 2D display density, FluidSimulation density/vector field, Mycelium trail textures
- **Files affected:** `fluid_simulation_3d.rs` (DENSITY_FORMAT), `fluid_simulation.rs` (DENSITY_FORMAT, VECTOR_FORMAT), `mycelium.rs` (TRAIL_FORMAT), `shaders/fluid_scatter_3d.wgsl`

### [D-13] ~~FluidSim3D simulate shader: textureLoad for density~~ RESOLVED
- **Resolved:** Density volume is now Rgba16Float (filterable). Simulate shader uses `textureSampleLevel(t_density, s_field, pos, 0.0)` — matches Unity's `_DensityTex.SampleLevel(sampler_linear_clamp, pos, 0)` exactly.

### [D-14] Stateful effect owner key: i64 instead of int (i32)
- **Unity does:** `Dictionary<int, T>` for per-owner state maps in stateful effects
- **Rust does:** `HashMap<i64, T>` for per-owner state maps
- **Why:** Project-wide decision. Clip IDs use hash-based i64 keys. Using i64 consistently avoids narrowing conversions. Unity's `int` values fit within i64 with no data loss.
- **Files affected:** All stateful effects (Feedback, StylizedFeedback, Bloom, CRT, Halation), `EffectContext::owner_key`

### [D-15] InvertColors: standalone post-process effect vs compositor blend flag
- **Unity does:** InvertColors is a boolean flag (`GetParam(0) > 0.5`) on the compositor blend shader (`_InvertColors` uniform in `VideoCompositor.shader`). No separate render pass.
- **Rust does:** InvertColors is a standalone `PostProcessEffect` with its own shader pass and continuous `mix()` blending (0.0..1.0 intensity, not binary threshold).
- **Why:** Intentional improvement. Continuous blending provides smoother transitions and is more consistent with how all other effects work. The extra render pass cost is negligible.
- **Files affected:** `manifold-renderer/src/effects/invert_colors.rs`, `shaders/invert_colors.wgsl`

### [D-16] ReactionDiffusion state texture: Rgba16Float instead of Rgba32Float
- **Unity does:** `StatefulShaderGeneratorBase.StateTextureFormat` defaults to `ARGBFloat` (Rgba32Float) for the ping-pong simulation state textures
- **Rust does:** `STATE_FORMAT = Rgba16Float`
- **Why:** The simulation shader reads state via `textureSample` (bilinear-filtered Laplacian neighbor lookups). `Rgba32Float` is NOT filterable on Metal — wgpu crashes with "Texture binding expects Float { filterable: true }". Half precision (16-bit) is sufficient for the Gray-Scott RD simulation's 0–1 chemical concentrations. No visual difference.
- **Files affected:** `manifold-renderer/src/generators/reaction_diffusion.rs`

### [D-17] WireframeDepth native flow texture: Rgba16Float instead of Rgba32Float
- **Unity does:** `nativeFlowTexture` uses `RGBAFloat` (Rgba32Float) for CPU-uploaded optical flow data
- **Rust does:** `Rgba16Float` with f32→f16 conversion during `upload_native_flow_texture()`
- **Why:** The native flow texture is bound into the shared BGL where all 12 texture slots require `Float { filterable: true }` (passes use `textureSample`). `Rgba32Float` is NOT filterable on Metal. Optical flow vectors (typically ±0–50 pixel displacements) fit comfortably in f16 range. f32→f16 conversion is done per-upload (infrequent — every 2–4 frames). No visual difference.
- **Files affected:** `manifold-renderer/src/effects/wireframe_depth.rs`

## WireframeDepth Overhaul (Intentional Design Improvements)

### [D-18] WireframeDepth: removed heuristic depth mode — DNN always
- **Unity does:** User-selectable DepthSourceMode enum: Heuristic (GPU pass) or DNN (OpenVINO MiDaS)
- **Rust does:** DNN depth always. Removed `DepthSourceMode` enum, `PASS_HEURISTIC_DEPTH`, `estimate_depth_heuristic()`. If DNN backend unavailable, depth_tex stays black/previous frame (graceful degradation).
- **Why:** DNN depth is strictly superior. Heuristic was a fallback for systems without OpenVINO — not a useful creative choice. Removes 1 GPU pass and simplifies control flow.
- **Approved by:** Peter / 2026-03-19
- **Files affected:** `manifold-renderer/src/effects/wireframe_depth.rs`, `manifold-renderer/src/effects/shaders/fx_wireframe_depth.wgsl`, `manifold-core/src/types.rs`

### [D-19] WireframeDepth: removed persistence/history pass
- **Unity does:** `PASS_UPDATE_HISTORY` blends current wireframe with previous frame's wireframe for temporal line persistence. Controlled by "Persist" param.
- **Rust does:** Removed pass entirely. Composite reads wire mask directly. Removed `line_history_tex` from OwnerState, removed "Persist" param.
- **Why:** The temporal smoothing in the mesh stabilization pipeline already provides visual stability. The persistence pass added near-invisible ghosting at the cost of an extra GPU pass + render target.
- **Approved by:** Peter / 2026-03-19
- **Files affected:** `manifold-renderer/src/effects/wireframe_depth.rs`, `manifold-renderer/src/effects/shaders/fx_wireframe_depth.wgsl`, `manifold-core/src/types.rs`

### [D-20] WireframeDepth: replaced GPU semantic mask with DNN subject mask for edge follow
- **Unity does:** `PASS_SEMANTIC_MASK` runs a GPU heuristic (luminance + flow + depth + center bias) to estimate body/face/boundary regions. Result drives `PASS_MESH_FACE_WARP` for non-rigid mesh deformation.
- **Rust does:** Removed `PASS_SEMANTIC_MASK` entirely. Edge follow pass (`PASS_MESH_EDGE_FOLLOW`, renamed from `PASS_MESH_FACE_WARP`) reads DNN subject mask texture instead. Removed `semantic_tex` from OwnerState.
- **Why:** The GPU heuristic was a crude center-biased guess. The DNN subject segmentation (already running for the "Subject" param) provides actual object detection. Rewiring the edge follow pass to use it gives correct contour following for off-center subjects, multiple objects, and non-human content.
- **Approved by:** Peter / 2026-03-19
- **Files affected:** `manifold-renderer/src/effects/wireframe_depth.rs`, `manifold-renderer/src/effects/shaders/fx_wireframe_depth.wgsl`

### [D-21] WireframeDepth: "Face" param replaced by continuous "EdgeFollow"
- **Unity does:** Param 13 "Face" is a discrete toggle (0/1) that enables/disables the face warp pass.
- **Rust does:** Param 11 "EdgeFollow" is a continuous slider (0.0–1.0, default 0.5) controlling warp strength. Pass runs when strength > 0.01. Strength scales the temporal-smooth-derived warp range.
- **Why:** Continuous control is more useful than on/off. Users can dial in subtle edge conformity without full-strength warping.
- **Approved by:** Peter / 2026-03-19
- **Files affected:** `manifold-renderer/src/effects/wireframe_depth.rs`, `manifold-core/src/types.rs`

### [D-22] WireframeDepth: param registry reindexed (14 → 12 params)
- **Unity does:** 14 params: Amount, Density, Width, ZScale, Smooth, Persist, Depth, Subject, Blend, WireRes, MeshRate, CVFlow, Lock, Face
- **Rust does:** 12 params: Amount, Density, Width, ZScale, Smooth, Subject, Blend, WireRes, MeshRate, Flow, Lock, EdgeFollow
- **Why:** Removed Persist (D-19), Depth (D-18). Renamed CVFlow→Flow, Face→EdgeFollow (D-21). Reindexed all param reads.
- **Approved by:** Peter / 2026-03-19
- **Files affected:** `manifold-core/src/types.rs`, `manifold-renderer/src/effects/wireframe_depth.rs`

### [D-23] WireframeDepth: 3 parallel background workers instead of 1 sequential
- **Unity does:** Single native plugin call processes depth + flow + subject sequentially (~25-45ms total blocking main thread).
- **Rust does:** 3 independent `BackgroundWorker` instances (depth, flow, subject), each owning its own `FfiDepthEstimator`. All run in parallel on separate threads. Results polled independently.
- **Why:** Eliminates sequential FFI bottleneck. Wall-clock time drops from sum(all) to max(individual). Enabling subject segmentation no longer slows depth/flow updates.
- **Approved by:** Peter / 2026-03-19
- **Files affected:** `manifold-renderer/src/effects/wireframe_depth.rs`, `manifold-renderer/src/background_worker.rs`

### [D-24] WireframeDepth: shader reduced from 15 to 12 passes
- **Unity does:** 15 shader passes (0–14)
- **Rust does:** 12 shader passes (0–11). Removed: heuristic_depth (D-18), update_history (D-19), semantic_mask (D-20). Renamed mesh_face_warp → mesh_edge_follow.
- **Why:** Follows from D-18, D-19, D-20. Fewer GPU passes = less overhead.
- **Approved by:** Peter / 2026-03-19
- **Files affected:** `manifold-renderer/src/effects/wireframe_depth.rs`, `manifold-renderer/src/effects/shaders/fx_wireframe_depth.wgsl`

### [D-25] BlobTracking: higher readback resolution, per-frame detection, exposed Connect param
- **Unity does:** 320x180 readback at every 3 frames, `MATCH_RADIUS_SQ = 0.08`, connection threshold hardcoded at 0.35
- **Rust does:** 640x360 readback every frame, `MATCH_RADIUS_SQ = 0.08` (unchanged from Unity), connection threshold exposed as param 4 "Connect" (0.0–1.0, default 0.35)
- **Why:** Apple Silicon unified memory makes per-frame GPU readback essentially free. Higher resolution gives better blob boundary precision. Tighter match radius appropriate for per-frame detection. Exposable connection distance gives artistic control over web density.
- **Approved by:** Peter / 2026-03-19
- **Files affected:** `manifold-renderer/src/effects/blob_tracking.rs`, `manifold-core/src/effect_definition_registry.rs`

### [D-26] MidiInputController: Minis + native CLK plugin → midir unified path
- **Unity does:** Two MIDI input paths — Minis (Unity Input System callbacks) for normal operation, and a native CoreMIDI plugin (MidiClock.bundle) queue drain when CLK is the clock authority and the plugin's note-event API is available. `IsNativeClockNotePathActive()` gates which path is active each frame. `GetMinisFallbackTickInfo()` provides a polled absolute_tick when CLK is authority but the native note-event API is unavailable.
- **Rust does:** `midir` replaces both paths. midir IS the native CoreMIDI backend on macOS (ALSA on Linux, WinMM on Windows). All events arrive via a single `std::sync::mpsc` channel from midir callbacks. The `native_clock_path_active` flag is preserved for telemetry parity. The Minis fallback warning (logged once when CLK is authority but SupportsNoteEvents is false) is not ported — in midir, note events are always supported.
- **Why:** midir provides equivalent accuracy to Unity's native plugin. A single unified path eliminates the complexity of dual-path switching while matching the same visual and timing behavior. This is a genuine architectural simplification with no degradation.
- **Approved by:** Peter / task spec
- **Files affected:** `manifold-playback/src/midi_input.rs`

### [D-27] MidiInputController: device disconnect not individually tracked
- **Unity does:** `OnDeviceChange(device, InputDeviceChange.Removed)` → `UnregisterDevice(device)` removes a specific device by identity.
- **Rust does:** midir does not provide device disconnect callbacks in the same session. Ports are enumerated at `start()` and on `set_device_filter()`. Device disconnect means the midir connection silently stops delivering events (the callback simply doesn't fire). `unregister_all_devices()` is called only on `stop()`.
- **Why:** midir's API does not expose plug/unplug callbacks in the same way as Unity's InputSystem.onDeviceChange. The behavioral difference is cosmetic — the device name in the UI may show stale info until the next filter scan, but no incorrect events are generated.
- **Approved by:** Peter / task spec
- **Files affected:** `manifold-playback/src/midi_input.rs`

### [D-28] 11 effects intentionally excluded from Rust port
- **Unity does:** Has InfiniteZoom, Datamosh, SlitScan, Corruption (stateful), FluidDistortion (complex), GradientMap, Microscope, Surveillance, Redaction (simple) as post-process effects.
- **Rust does:** These effects are not ported and not available in the effect dropdown. Their enum discriminants are reserved but skipped (e.g. Microscope=35). Old projects with these effects deserialize as `EffectType::Unknown` and are stripped on load.
- **Why:** Intentionally excluded — not wanted in the Rust port. **DO NOT re-add these variants.** Discriminant gaps in `EffectType` are intentional.
- **Approved by:** Peter / 2026-03-21
- **Files affected:** `manifold-renderer/src/effects/`, `manifold-core/src/types.rs`, `manifold-core/src/effect_definition_registry.rs`

### [D-29] ComputeCompositor fast path excluded
- **Unity does:** `ComputeCompositor` batches up to 8 effect-free layers into a single compute pass for performance.
- **Rust does:** All layers go through the standard compositor path (per-layer blit with blend).
- **Why:** Too complex with minimal gain. The standard path is performant enough.
- **Approved by:** Peter / 2026-03-21
- **Files affected:** `manifold-renderer/src/compositor/`

### [D-30] MidiClockSyncController: MidiClock native CoreMIDI plugin → midir receiver
- **Unity does:** `MidiClock.cs` wraps a native CoreMIDI DLL (`MidiClock_Update()` P/Invoke) that maintains `PositionSixteenths` (SPP base) and `ClockTick` (0–5 sub-sixteenth) internally. The C# side polls once per frame via `UpdateState()` and reads the resulting fields.
- **Rust does:** `MidiClockReceiver` wraps a `midir::MidiInputConnection` callback. The callback receives raw MIDI bytes and reconstructs `position_sixteenths` and `clock_tick` inline: `0xF8` increments `clock_tick` (wraps at 6, increments `position_sixteenths`); `0xF2` SPP resets `position_sixteenths = (msb<<7)|lsb, clock_tick = 0`; `0xFA` Start resets both to 0; `0xFB`/`0xFC` Continue/Stop set `is_playing` without resetting position. State is snapshotted once per frame via `update_state()` using `Arc<Mutex<MidiClockState>>`.
- **Why:** midir IS the native CoreMIDI backend on macOS (D-26). Standard MIDI protocol — the byte-to-field reconstruction is deterministic and equivalent to what Unity's native plugin does internally. No P/Invoke required.
- **Approved by:** Peter / task spec
- **Files affected:** `manifold-playback/src/midi_clock_sync.rs`

### [D-31] EffectType/GeneratorType: integer enum → string-keyed newtype
- **Unity does:** `EffectType` and `GeneratorType` are C# enums with integer values. Serialized as integers in JSON (e.g. `"effectType": 12` for Bloom).
- **Rust does:** `EffectTypeId` and `GeneratorTypeId` are `Cow<'static, str>` newtypes with named constants. Serialized as strings (e.g. `"effectType": "Bloom"`). Deserialization accepts both legacy integers and strings for backward compatibility.
- **Why:** String-keyed IDs eliminate enum discriminant gaps when effects are added/removed, reduce the number of locations to update (8 → 3), enable future plugin/custom effect extensibility, and produce human-readable project files. The integer-to-string deserialization path ensures all existing projects load correctly.
- **Files affected:** `manifold-core/src/effect_type_id.rs`, `manifold-core/src/generator_type_id.rs`, `manifold-core/src/effect_type_registry.rs`, `manifold-core/src/generator_type_registry.rs`, all crates

### [D-32] Halation: separable Gaussian blur replaces 2D cross kernel
- **Unity does:** Halation uses two 13-tap 2D cross-pattern blur passes (ThresholdTintBlur + BlurWide) at half-resolution. The cross pattern has gaps between samples that create visible blocky artifacts when the half-res result is upscaled, especially on thin features like lines.
- **Rust does:** 4-pass separable Gaussian (ThresholdTint → BlurH → BlurV → Composite) with a 17-tap kernel per axis at half-resolution. Effective coverage: 17×17 = 289 unique positions vs Unity's 13-point cross. Same half-res buffers, similar GPU cost, dramatically smoother glow.
- **Why:** The 2D cross kernel produces visible staircase/blocky glow artifacts on thin features at high DPI. Separable Gaussian eliminates all sampling gaps while maintaining half-res performance.
- **Approved by:** Peter / 2026-03-23
- **Files affected:** `manifold-renderer/src/effects/halation.rs`, `manifold-renderer/src/effects/shaders/fx_halation.wgsl`

---

## Add new divergences above this line.
## If you're tempted to add one — first ask: can I match Unity instead?
