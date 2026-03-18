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

### [D-11] FluidSim3D density volume: R32Float instead of R16Float
- **Unity does:** `RenderTextureFormat.RHalf` (R16Float) for 3D density volumes
- **Rust does:** `R32Float` for 3D density volumes
- **Why:** Metal does not support `STORAGE_BINDING` on `R16Float` textures. The density volume requires both storage writes (compute resolve) and texture reads (blur, gradient). R32Float supports both. All reads use `textureLoad` (not `textureSample`), so R32Float's lack of filtering on Metal is not an issue. 2x memory per volume but higher precision — no visual degradation.
- **Files affected:** `manifold-renderer/src/generators/fluid_simulation_3d.rs`, `shaders/fluid_scatter_3d.wgsl`, `shaders/fluid_blur_3d.wgsl`

### [D-12] Texture formats: Rgba16Float instead of R32Float/Rg32Float for storage+sampling textures
- **Unity does:** `RFloat` (R32Float) for density, `RGFloat` (Rg32Float) for vector fields — these textures are both written by compute and sampled with bilinear filtering
- **Rust does:** `Rgba16Float` for all textures that need both `STORAGE_BINDING` and filtered `textureSample`
- **Why:** On Metal via wgpu, `R32Float` and `Rg32Float` are NOT filterable (`textureSample` requires `Float { filterable: true }`). Unity's Metal backend handles this internally. `Rgba16Float` is the only format that supports both storage writes and filtered sampling on Metal. Half precision (16-bit) is sufficient for display intermediates and density fields; the extra channels are unused but harmless.
- **Applies to:** FluidSim3D 2D display density, FluidSimulation density/vector field, Mycelium trail textures
- **Files affected:** `fluid_simulation_3d.rs` (DENSITY_FORMAT), `fluid_simulation.rs` (DENSITY_FORMAT, VECTOR_FORMAT), `mycelium.rs` (TRAIL_FORMAT), `shaders/fluid_scatter_3d.wgsl`

### [D-13] FluidSim3D simulate shader: textureLoad instead of textureSampleLevel for density
- **Unity does:** `_DensityTex.SampleLevel(sampler_linear_clamp, pos, 0)` — bilinear filtered sample
- **Rust does:** `textureLoad(t_density, coord, 0)` — nearest-neighbor load
- **Why:** The 3D density volume uses `R32Float` (D-11), which is not filterable on Metal. Since the density is already 3D Gaussian-blurred before the simulate shader reads it, nearest-neighbor sampling is visually equivalent to bilinear — the blur removes the high-frequency content that filtering would smooth.
- **Files affected:** `shaders/fluid_simulate_3d.wgsl`

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

---

## Add new divergences above this line.
## If you're tempted to add one — first ask: can I match Unity instead?
