# Phase 1 — manifold-gpu Extensions

Copy everything below the line into a new Claude Code chat to execute Phase 1.

See `NATIVE_METAL_UI_MIGRATION.md` for full migration context.

---

Read `CLAUDE.md` and `docs/NATIVE_METAL_UI_MIGRATION.md` before starting.

You are extending the `manifold-gpu` crate to support UI-thread rendering. This is Phase 1 of a larger migration from wgpu to native Metal. The crate currently serves the content thread only. You are adding NEW methods and types — do NOT modify any existing methods or types. The content thread must continue working identically.

Read every file you need to modify BEFORE making changes. Read the metal crate source at `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/metal-0.33.0/src/` to verify Metal API names before using them.

Work without breaks. Complete all 8 tasks below. Build with `cargo clippy --workspace -- -D warnings` and `cargo test --workspace` after all changes. Fix any issues. Commit and push when done.

### Context

**Crate location:** `/Users/peterkiemann/MANIFOLD - Rust/crates/manifold-gpu/`

**Architecture:** manifold-gpu wraps the native `metal` crate (v0.33) directly. Zero wgpu. It compiles WGSL→MSL via naga→SPIR-V→spirv-opt→SPIRV-Cross→MSL. Resource bindings use a `SlotMap` that maps WGSL `@binding(N)` to Metal argument indices.

**Key existing types:**
- `GpuDevice` — `metal::Device` + `metal::CommandQueue` + pipeline caching + binary archive
- `GpuEncoder` — retained `MTLCommandBuffer` with auto-managed encoder state machine
- `GpuTexture` — `metal::Texture` wrapper with width/height/depth/format
- `GpuBuffer` — `metal::Buffer` wrapper with size + optional mapped pointer
- `GpuRenderPipeline` — `metal::RenderPipelineState` + `SlotMap` + label
- `GpuSampler` — `metal::SamplerState` wrapper

**Key existing methods on GpuEncoder:**
- `draw_fullscreen(pipeline, target, bindings, clear, store, label)` — fullscreen triangle, fragment-only bindings
- `draw_instanced(pipeline, target, bindings, vertex_count, instance_count, load_action, label)` — vertex+fragment bindings, no vertex buffer
- `clear_texture(texture, r, g, b, a)` — render pass clear
- `commit(self)` — end encoder, commit command buffer

**Key existing methods on GpuDevice:**
- `create_render_pipeline(wgsl, vs, fs, color_format, blend, label)` — no vertex descriptor
- `create_buffer(size, usage)` — private storage
- `create_buffer_shared(size)` — shared storage with mapped pointer
- `create_texture(desc)`, `create_sampler(desc)`, `create_encoder(label)`

**Existing types in types.rs:** `GpuTextureFormat`, `GpuTextureDimension`, `GpuTextureUsage`, `GpuTextureDesc`, `GpuBufferUsage`, `GpuStorageMode`, `GpuFilterMode`, `GpuAddressMode`, `GpuSamplerDesc`, `GpuLoadAction`, `GpuBlendFactor`, `GpuBlendOp`, `GpuBlendState`, `GpuBinding`

**Re-exports in `metal/mod.rs`:** `GpuDevice`, `GpuTexture`, `GpuBuffer`, `GpuSampler`, `GpuComputePipeline`, `GpuRenderPipeline`, `GpuEvent`, `GpuHeap`, `TexturePool`, `GpuEncoder`

### Task 1: Vertex Layout Types (types.rs)

Add these types to `crates/manifold-gpu/src/types.rs`:

```rust
/// Vertex attribute format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuVertexFormat {
    Float32,
    Float32x2,
    Float32x3,
    Float32x4,
    Uint32,
    Uint8x4,
}

/// A single vertex attribute in a vertex buffer layout.
#[derive(Clone, Copy, Debug)]
pub struct GpuVertexAttribute {
    /// Vertex format (e.g. Float32x2 for position).
    pub format: GpuVertexFormat,
    /// Byte offset within the vertex struct.
    pub offset: u32,
    /// Shader location (matches WGSL @location(N)).
    pub shader_location: u32,
}

/// Vertex buffer layout — describes the memory layout of vertices.
#[derive(Clone, Debug)]
pub struct GpuVertexLayout {
    /// Stride in bytes between consecutive vertices.
    pub stride: u32,
    /// Vertex attributes in this buffer.
    pub attributes: Vec<GpuVertexAttribute>,
}
```

### Task 2: Vertex Format Conversion (format.rs)

Add a conversion function to `crates/manifold-gpu/src/metal/format.rs`:

```rust
pub fn to_mtl_vertex_format(fmt: GpuVertexFormat) -> metal::MTLVertexFormat {
    match fmt {
        GpuVertexFormat::Float32 => metal::MTLVertexFormat::Float,
        GpuVertexFormat::Float32x2 => metal::MTLVertexFormat::Float2,
        GpuVertexFormat::Float32x3 => metal::MTLVertexFormat::Float3,
        GpuVertexFormat::Float32x4 => metal::MTLVertexFormat::Float4,
        GpuVertexFormat::Uint32 => metal::MTLVertexFormat::UInt,
        GpuVertexFormat::Uint8x4 => metal::MTLVertexFormat::UChar4,
    }
}
```

Check that the `metal` crate v0.33 exposes these `MTLVertexFormat` variants. Read the metal crate source at `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/metal-0.33.0/src/` to verify the exact enum names. If they differ, use the correct names.

### Task 3: create_render_pipeline_with_vertex_layout (device.rs)

Add a new method to `GpuDevice` in `crates/manifold-gpu/src/metal/device.rs`. This is a variant of the existing `create_render_pipeline()` that additionally configures an `MTLVertexDescriptor` from a `GpuVertexLayout`.

The method must:
1. Accept `vertex_layout: &GpuVertexLayout` as an additional parameter
2. Compile WGSL→MSL identically to the existing method (reuse the same code path)
3. Build an `MTLVertexDescriptor` with:
   - One vertex buffer at index 30 (buffer index 30 is a safe slot — Metal has 31 buffer slots (0-30), and SPIRV-Cross assigns bindings from 0 upward, so 30 avoids collision with uniform/storage buffer bindings). The exact index to use: read the existing SPIRV-Cross MSL output to see what buffer indices are used by bindings, then pick a high unused index. Index 30 is standard practice.
   - Per-attribute: format, offset, buffer_index=30
   - Layout stride from the GpuVertexLayout
4. Set the vertex descriptor on the `RenderPipelineDescriptor` before creating the pipeline state
5. Use the same pipeline caching + binary archive logic as the existing method. Use a different hash (incorporate vertex layout stride into the hash to differentiate from vertex-less pipelines with the same shader).

**Important:** The existing `create_render_pipeline()` must NOT be modified. The new method is a separate method that content-thread code never calls.

**Metal vertex descriptor API** (verify against metal crate v0.33):
```rust
let vtx_desc = metal::VertexDescriptor::new();
let attr = vtx_desc.attributes().object_at(location).unwrap();
attr.set_format(to_mtl_vertex_format(attribute.format));
attr.set_offset(attribute.offset as u64);
attr.set_buffer_index(VERTEX_BUFFER_INDEX);  // 30
let layout = vtx_desc.layouts().object_at(VERTEX_BUFFER_INDEX).unwrap();
layout.set_stride(vertex_layout.stride as u64);
layout.set_step_function(metal::MTLVertexStepFunction::PerVertex);
layout.set_step_rate(1);
desc.set_vertex_descriptor(Some(&vtx_desc));
```

Read the metal crate source to verify `VertexDescriptor`, `VertexAttributeDescriptor`, `VertexBufferLayoutDescriptor` APIs and method names.

Signature:
```rust
pub fn create_render_pipeline_with_vertex_layout(
    &self,
    wgsl_source: &str,
    vs_entry: &str,
    fs_entry: &str,
    color_format: GpuTextureFormat,
    blend: Option<GpuBlendState>,
    vertex_layout: &GpuVertexLayout,
    label: &str,
) -> GpuRenderPipeline
```

### Task 4: draw_indexed() on GpuEncoder (encoder.rs)

Add a new method to `GpuEncoder` in `crates/manifold-gpu/src/metal/encoder.rs`. Follow the same pattern as `draw_instanced()` but with vertex buffer + index buffer + indexed draw.

The method must:
1. End any active encoder (`self.end_current()`)
2. Create a render pass descriptor with the target texture as color attachment
3. Set load action from parameter, store action = Store
4. Clear color = (0, 0, 0, 0) when clearing
5. Create render command encoder
6. Set render pipeline state
7. Set viewport if provided, otherwise full texture dimensions
8. Bind vertex buffer at index 30 (same as the vertex descriptor buffer index)
9. Set all bindings on BOTH vertex and fragment stages (same pattern as `draw_instanced()`)
10. Call `draw_indexed_primitives` with TriangleList, the index count, index type UInt32, and the index buffer
11. End encoding

**Metal draw_indexed API:**
```rust
enc.draw_indexed_primitives(
    metal::MTLPrimitiveType::Triangle,
    index_count as u64,
    metal::MTLIndexType::UInt32,
    &index_buffer.raw,
    0,  // index buffer offset
);
```

Verify this method exists on `RenderCommandEncoderRef` in the metal crate v0.33.

Signature:
```rust
#[allow(clippy::too_many_arguments)]
pub fn draw_indexed(
    &mut self,
    pipeline: &GpuRenderPipeline,
    target: &GpuTexture,
    bindings: &[GpuBinding],
    vertex_buffer: &GpuBuffer,
    index_buffer: &GpuBuffer,
    index_count: u32,
    viewport: Option<(f32, f32, f32, f32)>,
    load_action: crate::GpuLoadAction,
    label: &str,
)
```

### Task 5: GpuSurface — CAMetalLayer Wrapper (NEW FILE)

Create a new file `crates/manifold-gpu/src/metal/surface.rs`.

This wraps a CAMetalLayer for presenting rendered content to a window. Platform-specific (macOS only, which is fine — the entire metal/ directory is macOS-only).

**Dependencies needed in Cargo.toml:**
- `core-graphics-types = "0.2"` (for CGSize)
- `raw-window-handle = "0.6"` (for window handle extraction)

Add these to `[target.'cfg(target_os = "macos")'.dependencies]` in the manifold-gpu Cargo.toml.

**Types:**

```rust
/// A presentable surface backed by a CAMetalLayer.
pub struct GpuSurface {
    /// Retained CAMetalLayer pointer.
    layer_ptr: *mut std::ffi::c_void,
    pub width: u32,
    pub height: u32,
    pub format: GpuTextureFormat,
}

/// A drawable acquired from a GpuSurface.
/// Must be presented before being dropped (or the drawable is wasted).
pub struct GpuDrawable {
    /// Raw CAMetalDrawable pointer (retained).
    drawable_ptr: *mut std::ffi::c_void,
}
```

Both need `unsafe impl Send` (CAMetalLayer and CAMetalDrawable are thread-safe for these operations).

**GpuSurface methods:**

`GpuDevice::create_surface(window, width, height, format, vsync) -> GpuSurface`:
1. Extract NSView from window via `raw-window-handle` (`HasWindowHandle` → `RawWindowHandle::AppKit`)
2. Create `metal::MetalLayer::new()`
3. Configure: `set_device(self.raw_device())`, `set_pixel_format(to_mtl_pixel_format(format))`, `set_framebuffer_only(true)`, `set_display_sync_enabled(vsync)`, `set_maximum_drawable_count(3)`, `set_drawable_size(CGSize)`, `set_contents_scale(1.0)`
4. Set on NSView: `[ns_view setLayer:layer]`, `[ns_view setWantsLayer:YES]`
5. Retain the layer (explicit `objc_retain` — the local MetalLayer will drop its reference)
6. Return GpuSurface

`surface.resize(width, height)`:
- Update `self.width`, `self.height`
- Call `layer.set_drawable_size(CGSize { width, height })`

`surface.next_drawable() -> Option<GpuDrawable>`:
- Call `layer.next_drawable()` → returns `Option<&MetalDrawableRef>`
- If Some, retain the drawable, wrap in GpuDrawable
- If None, return None

`surface.configure_edr()`:
- Call `setColorspace:kCGColorSpaceExtendedLinearSRGB` via objc msg_send
- Call `setWantsExtendedDynamicRangeContent:YES` via objc msg_send
- Requires CGColorSpace FFI externs (same pattern as `output_presenter.rs`)

`surface.set_contents_gravity_resize_aspect()`:
- Set `contentsGravity = kCAGravityResizeAspect` via objc msg_send

`surface.set_background_color(r, g, b, a)`:
- Create CGColor via `CGColorCreateGenericRGB` and set via `setBackgroundColor:`

**GpuDrawable methods:**

`drawable.texture() -> *const metal::TextureRef`:
- Call `drawable.texture()` on the raw CAMetalDrawable — returns the MTLTexture backing the drawable
- Return as raw pointer (the caller uses it as a render target in a render pass)

`drawable.present(self)`:
- Call `drawable.present()` — schedules the drawable for display at the next vsync
- Consumes self

**Drop impls:**
- `GpuSurface::drop` — `objc_release` the layer_ptr
- `GpuDrawable::drop` — `objc_release` the drawable_ptr (if not presented, drawable is returned to pool)

**GpuEncoder integration:**

Add a method to GpuEncoder:
```rust
pub fn present_drawable(&mut self, drawable: &GpuDrawable)
```
This calls `self.cmd_buf().present_drawable(drawable)` — schedules the present as part of the command buffer commit. The drawable is presented when the command buffer completes. This is preferred over `drawable.present()` because it coordinates with the command buffer's GPU work.

Actually, the metal crate's `present_drawable` takes a `&MetalDrawableRef`. You'll need to cast the drawable_ptr to `&MetalDrawableRef`. Check the metal crate for the exact method signature on `CommandBufferRef`.

**Wire it up:**
- Add `pub mod surface;` to `metal/mod.rs`
- Add `pub use surface::{GpuSurface, GpuDrawable};` to `metal/mod.rs`

### Task 6: Smoke Test

Create a test in `crates/manifold-gpu/src/metal/surface.rs` (or a separate test file) that exercises the vertex layout + draw_indexed path:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_indexed_draw_to_texture() {
        // 1. Create GpuDevice
        let device = GpuDevice::new();

        // 2. Create a small render target (4x4, Rgba8Unorm)
        let target = device.create_texture(&GpuTextureDesc {
            width: 4,
            height: 4,
            depth: 1,
            format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
            label: "test target",
        });

        // 3. Define vertex layout: position (Float32x2) + color (Float32x4)
        let layout = GpuVertexLayout {
            stride: 24,
            attributes: vec![
                GpuVertexAttribute {
                    format: GpuVertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                },
                GpuVertexAttribute {
                    format: GpuVertexFormat::Float32x4,
                    offset: 8,
                    shader_location: 1,
                },
            ],
        };

        // 4. Create pipeline with vertex layout
        let wgsl = r#"
            struct VertexInput {
                @location(0) position: vec2<f32>,
                @location(1) color: vec4<f32>,
            };
            struct VertexOutput {
                @builtin(position) position: vec4<f32>,
                @location(0) color: vec4<f32>,
            };
            @vertex
            fn vs_main(in: VertexInput) -> VertexOutput {
                var out: VertexOutput;
                out.position = vec4<f32>(in.position, 0.0, 1.0);
                out.color = in.color;
                return out;
            }
            @fragment
            fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
                return in.color;
            }
        "#;

        let pipeline = device.create_render_pipeline_with_vertex_layout(
            wgsl, "vs_main", "fs_main",
            GpuTextureFormat::Rgba8Unorm,
            None,
            &layout,
            "test pipeline",
        );

        // 5. Create vertex buffer (fullscreen quad as 2 triangles)
        // Positions: (-1,-1), (1,-1), (1,1), (-1,1)
        // Color: red (1,0,0,1) for all vertices
        #[repr(C)]
        struct Vertex {
            pos: [f32; 2],
            color: [f32; 4],
        }
        let vertices = [
            Vertex { pos: [-1.0, -1.0], color: [1.0, 0.0, 0.0, 1.0] },
            Vertex { pos: [ 1.0, -1.0], color: [1.0, 0.0, 0.0, 1.0] },
            Vertex { pos: [ 1.0,  1.0], color: [1.0, 0.0, 0.0, 1.0] },
            Vertex { pos: [-1.0,  1.0], color: [1.0, 0.0, 0.0, 1.0] },
        ];
        let vertex_data = unsafe {
            std::slice::from_raw_parts(
                vertices.as_ptr() as *const u8,
                std::mem::size_of_val(&vertices),
            )
        };
        let vertex_buffer = device.create_buffer_shared(vertex_data.len() as u64);
        vertex_buffer.write(0, vertex_data);

        // 6. Create index buffer (two triangles: 0,1,2 + 0,2,3)
        let indices: [u32; 6] = [0, 1, 2, 0, 2, 3];
        let index_data = unsafe {
            std::slice::from_raw_parts(
                indices.as_ptr() as *const u8,
                std::mem::size_of_val(&indices),
            )
        };
        let index_buffer = device.create_buffer_shared(index_data.len() as u64);
        index_buffer.write(0, index_data);

        // 7. Draw
        let mut encoder = device.create_encoder("test draw");
        encoder.draw_indexed(
            &pipeline,
            &target,
            &[],  // no bindings needed
            &vertex_buffer,
            &index_buffer,
            6,
            None,
            GpuLoadAction::Clear,
            "test indexed draw",
        );
        encoder.commit();

        // If we get here without panicking, the pipeline creation
        // and draw_indexed encoding worked correctly.
        // (GPU readback verification would require waiting for completion
        // and reading pixels — beyond the scope of this smoke test.)
    }
}
```

### Task 7: Update Re-exports

In `crates/manifold-gpu/src/metal/mod.rs`, add:
- `pub mod surface;` alongside the other module declarations
- `pub use surface::{GpuSurface, GpuDrawable};` in the re-export block

In `crates/manifold-gpu/src/lib.rs`, update the doc comment to remove the line "The UI thread stays on wgpu directly — this crate is only for the content thread." since that's no longer true.

### Task 8: Update Cargo.toml

Add to `crates/manifold-gpu/Cargo.toml` under `[target.'cfg(target_os = "macos")'.dependencies]`:

```toml
core-graphics-types = "0.2"
raw-window-handle = "0.6"
```

### Verification

After all changes:
1. `cargo clippy --workspace -- -D warnings` — must pass clean
2. `cargo test --workspace` — must pass (including the new smoke test)
3. No existing tests broken
4. No existing public API signatures changed

### File Summary

| File | Action |
|------|--------|
| `crates/manifold-gpu/Cargo.toml` | Add core-graphics-types, raw-window-handle deps |
| `crates/manifold-gpu/src/lib.rs` | Update doc comment |
| `crates/manifold-gpu/src/types.rs` | Add GpuVertexFormat, GpuVertexAttribute, GpuVertexLayout |
| `crates/manifold-gpu/src/metal/format.rs` | Add to_mtl_vertex_format() |
| `crates/manifold-gpu/src/metal/device.rs` | Add create_render_pipeline_with_vertex_layout() |
| `crates/manifold-gpu/src/metal/encoder.rs` | Add draw_indexed() |
| `crates/manifold-gpu/src/metal/surface.rs` | NEW — GpuSurface, GpuDrawable, full CAMetalLayer wrapper |
| `crates/manifold-gpu/src/metal/mod.rs` | Add surface module + re-exports |

### Critical Rules

- Do NOT modify any existing method signatures or types
- Do NOT break the content thread's usage of manifold-gpu
- Follow the existing code style: `snake_case`, `#[allow(clippy::too_many_arguments)]` where needed
- Use `unsafe extern "C" { fn objc_retain(...); fn objc_release(...); }` from `super::` (already declared in mod.rs) — do NOT re-declare
- The `#[macro_use] extern crate objc;` is at crate root — `msg_send!`, `sel!`, `sel_impl!` are available everywhere
- Metal crate is v0.33 (`metal = "0.33"`)
- Edition 2024 — use `unsafe { unsafe { ... } }` pattern for unsafe fn bodies if needed
- Read the metal crate source at `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/metal-0.33.0/src/` to verify API names before using them
