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

---

## Add new divergences above this line.
## If you're tempted to add one — first ask: can I match Unity instead?
