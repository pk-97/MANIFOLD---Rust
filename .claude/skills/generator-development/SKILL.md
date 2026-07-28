---
name: generator-development
description: Complete pattern for adding generators — Plasma (compute), Lissajous (line-based), Galactic Rock (3D mesh) as templates. Invoke before any generator work alongside the mandatory docs/DECOMPOSING_GENERATORS.md read.
---
# Generator Development Guide

Three patterns: **compute** (GPU-driven, fills texture), **line-based** (CPU vertices, rasterized lines/dots), and **3D mesh** (compute + instanced render with depth/shadows).

## Registration (inventory-based, as of 2026-04-09)

Touch exactly **2 files** to add a new generator:

```
1. manifold-renderer/src/generators/my_gen.rs    (NEW FILE — impl + registration)
2. manifold-renderer/src/generators/mod.rs        pub mod my_gen;
```

All metadata and factory registration lives in the implementation file via `inventory::submit!`:

```rust
use manifold_core::GeneratorTypeId;
use manifold_core::generator_registration::{GeneratorMetadata, ParamSpec};
use crate::generators::registration::GeneratorFactory;

inventory::submit! {
    GeneratorMetadata {
        id: GeneratorTypeId::MY_GEN,  // or GeneratorTypeId::new("MyGen")
        display_name: "My Generator",
        is_line_based: false,
        available: true,
        osc_prefix: "myGen",
        legacy_discriminant: Some(26),  // None for brand-new generators
        params: &[
            ParamSpec::continuous("Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("Scale", 0.1, 4.0, 1.0, "F2", "scale"),
            ParamSpec::toggle("Snap", 0.0, 1.0, 0.0, "snap"),
            ParamSpec::whole_labels("Mode", 0.0, 3.0, 0.0,
                &["A", "B", "C", "D"], "mode"),
        ],
        string_params: &[],
    }
}

inventory::submit! {
    GeneratorFactory {
        id: GeneratorTypeId::MY_GEN,
        create: |device| Box::new(MyGenGenerator::new(device)),
    }
}
```

If other code needs to reference this generator's type ID, add a const to
`generator_type_id.rs`: `pub const MY_GEN: Self = Self(Cow::Borrowed("MyGen"));`
— but this is optional if nothing outside the generator file needs it.

**ParamSpec helpers** (const fn, all `'static`):
- `ParamSpec::continuous(name, min, max, default, fmt, osc)` — continuous slider
- `ParamSpec::toggle(name, min, max, default, osc)` — on/off
- `ParamSpec::whole(name, min, max, default, osc)` — integer
- `ParamSpec::whole_labels(name, min, max, default, &labels, osc)` — integer with labels

The `inventory` crate collects submissions at link time. Registries in `manifold-core`
(definition, type) and factories in `manifold-renderer` (registry.rs) all iterate
`inventory::iter` at startup — no manual wiring needed.

## Generator Trait

```rust
pub trait Generator: Send {
    fn generator_type(&self) -> &GeneratorTypeId;
    fn render(&mut self, gpu: &mut GpuEncoder, target: &GpuTexture, ctx: &GeneratorContext) -> f32;
    fn resize(&mut self, device: &GpuDevice, width: u32, height: u32);
    fn internal_resolution_scale(&self) -> f32 { 1.0 }   // Override for downsampling
    fn reset_state(&mut self, _device: &GpuDevice) {}     // Override for stateful sims
}
```

## GeneratorContext (available per frame)

```rust
ctx.time          // f64 — absolute time in seconds
ctx.beat          // f64 — current beat position
ctx.dt            // f32 — delta time in seconds
ctx.width         // u32 — render resolution (may be scaled)
ctx.height        // u32
ctx.output_width  // u32 — final output resolution
ctx.output_height // u32
ctx.aspect        // f32 — width/height
ctx.anim_progress // f32 — current animation progress (for edge animation)
ctx.trigger_count // u32 — increments on each clip trigger (NoteOn)
ctx.params        // [f32; MAX_GEN_PARAMS] — parameter values
ctx.param_count   // u32 — how many params are valid
```

## Pattern A: Compute Generator (Plasma template)

```rust
const SHADER: &str = include_str!("shaders/my_gen.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MyUniforms {
    time: f32,
    beat: f32,
    aspect: f32,
    speed: f32,
    scale: f32,
    mode: u32,
    _pad: [f32; 2],   // Pad to 16-byte boundary
}

pub struct MyGenGenerator {
    pipeline: GpuComputePipeline,
    // Or for multiple modes: pipelines: [GpuComputePipeline; N],
}

impl MyGenGenerator {
    pub fn new(device: &GpuDevice) -> Self {
        Self {
            pipeline: device.create_compute_pipeline(SHADER, "cs_main", "My Generator"),
        }
    }
}

impl Generator for MyGenGenerator {
    fn generator_type(&self) -> &GeneratorTypeId { &GeneratorTypeId::MY_GEN }

    fn render(&mut self, gpu: &mut GpuEncoder, target: &GpuTexture,
              ctx: &GeneratorContext) -> f32 {
        // Read params with safe indexing and defaults
        let speed = if ctx.param_count > 0 { ctx.params[0] } else { 1.0 };
        let scale = if ctx.param_count > 1 { ctx.params[1] } else { 1.0 };
        let mode = if ctx.param_count > 2 { ctx.params[2].round() as u32 } else { 0 };

        let uniforms = MyUniforms {
            time: ctx.time as f32,
            beat: ctx.beat as f32,
            aspect: ctx.aspect,
            speed, scale, mode,
            _pad: [0.0; 2],
        };

        gpu.native_enc.dispatch_compute(
            &self.pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Texture { binding: 1, texture: target },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "My Generator",
        );

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &GpuDevice, _width: u32, _height: u32) {
        // Only needed if generator allocates resolution-dependent resources
    }
}
```

**For multiple mode variants**, use function constants:
```rust
let pipelines = std::array::from_fn(|i| {
    device.create_specialized_compute_pipeline(
        SHADER, "cs_main",
        &[("u.mode", &format!("{}.0", i))],
        &format!("MyGen Mode {i}"),
    )
});
```

## Pattern B: Line-Based Generator (Lissajous template)

```rust
use crate::generators::line_pipeline::LinePipeline;
use crate::generators::generator_math::LineGeneratorHelper;

pub struct MyLineGen {
    line_pipeline: LinePipeline,
    helper: LineGeneratorHelper,
}

impl MyLineGen {
    pub fn new(device: &GpuDevice) -> Self {
        let line_pipeline = LinePipeline::new(device, "My Lines");
        let mut helper = LineGeneratorHelper::new(VERTEX_COUNT, EDGE_COUNT);

        // Define edge topology (which vertices connect)
        for i in 0..VERTEX_COUNT {
            helper.edge_a.push(i);
            helper.edge_b.push((i + 1) % VERTEX_COUNT);
        }

        Self { line_pipeline, helper }
    }
}

impl Generator for MyLineGen {
    fn render(&mut self, gpu: &mut GpuEncoder, target: &GpuTexture,
              ctx: &GeneratorContext) -> f32 {
        // Compute vertex positions (CPU-side)
        for i in 0..VERTEX_COUNT {
            let t = i as f32 / VERTEX_COUNT as f32;
            self.helper.projected_x[i] = /* x position */;
            self.helper.projected_y[i] = /* y position */;
            self.helper.projected_z[i] = /* depth (0..1) for edge animation */;
        }

        // Prepare line instances from vertices + edges
        let (positions, instances, num_edges, edge_thick, dot_thick) =
            self.helper.prepare_instances(
                ctx.output_height as f32,
                ctx.aspect,
                line_width,     // pixel width of lines
                show_verts,     // f32 toggle for vertex dots
                vert_size,      // dot radius
                animate,        // edge animation amount
                speed,          // animation speed
                window,         // animation window width
                scale,          // overall scale
                dot_scale,      // dot size multiplier
                ctx.dt,
            );

        // Rasterize lines + dots
        self.line_pipeline.draw(
            gpu, target, positions, instances, num_edges,
            edge_thick, dot_thick,
            ctx.beat as f32, "My Lines", ctx.width, ctx.height,
        );

        self.helper.anim_progress
    }

    fn resize(&mut self, device: &GpuDevice, width: u32, height: u32) {
        self.line_pipeline.resize(device, width, height);
    }
}
```

## CRITICAL: Naga Multi-Entry-Point Uniform Size Rule

**If a WGSL file has multiple `@compute` entry points, all `var<uniform>` bindings at the SAME binding index MUST have the SAME byte size across ALL entry points.** Naga generates a single Metal argument buffer layout per shader module — mismatched sizes produce broken Metal code (GPU corruption/hang with no compile error).

**Symptoms:** GPU hang, garbage output, or hard lock. No shader compilation error.

**Fix:** Either:
1. **Pad all uniforms at the same binding index to the same size** (how `fluid_scatter_3d.wgsl` handles it — all `@binding(2)` uniforms padded to 112 bytes)
2. **Split entry points into separate `.wgsl` files** (cleaner, avoids padding dance)

This applies to ANY binding index, not just uniforms — but uniforms are the most common case since textures/buffers at the same index with different types are also problematic.

**Example (broken):**
```wgsl
// Entry point A uses: @binding(2) var<uniform> params_a: SmallUniforms; // 16 bytes
// Entry point B uses: @binding(2) var<uniform> params_b: BigUniforms;   // 32 bytes
// → GPU corruption, no compile error
```

## Pattern C: 3D Mesh Generator (Galactic Rock template)

Uses `MeshPipeline` for instanced cube rendering with depth testing, and optionally shadow mapping.

**Infrastructure files:**
- `mesh_pipeline.rs` — `MeshPipeline`, `MeshInstance`, `MeshUniforms`, procedural cube (36 verts from vertex_index), depth stencil, two-point PBR lighting. Max 131,072 instances.
- `mesh_pipeline.wgsl` — vertex shader reads instances from storage buffer, applies Euler XYZ rotation, fragment does two-point Phong lighting.
- Camera utilities in `mesh_pipeline.rs`: `perspective_rh()`, `look_at_rh()`, `ortho_rh()`, `mat4_mul()`.

**Galactic Rock pattern (compute + shadow + render):**
1. Compute pass: particle simulation → writes `MeshInstance` array to shared buffer
2. Shadow pass(es): `draw_instanced_depth()` from light POV → Depth32Float shadow maps (2048×2048)
3. Main render: instanced draw sampling shadow maps via comparison sampler
4. Optional post-process: compute blur pass

**Nested Cubes pattern (multi-pass depth):**
1. Pass 1: solid fill with `depth_stencil_write` (Compare::Less, write: true), depth bias
2. Pass 2: wireframe edges with `depth_stencil_read` (Compare::LessEqual, write: false)

**Shadow map setup:**
```rust
shadow_map: GpuTexture (Depth32Float, 2048×2048, RENDER_TARGET | SHADER_READ)
shadow_sampler: comparison sampler (Compare::LessEqual)
light_vp: ortho_rh() from light position
```

**Instance buffer pattern:**
```rust
const MAX_INSTANCES: u64 = 131_072;
const INSTANCE_STRIDE: u64 = 32; // 2 × vec4

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MeshInstance {
    pos_scale: [f32; 4],  // xyz: position, w: scale
    rot_pad: [f32; 4],    // xyz: Euler rotation, w: pad
}

// CPU writes via shared storage buffer
let instances_buf = device.create_buffer_shared(MAX_INSTANCES * INSTANCE_STRIDE);
unsafe { self.instances_buf.write(0, bytemuck::cast_slice(&instances)); }
```

**Draw calls:**
```rust
// Standard depth draw
gpu.native_enc.draw_instanced_depth(pipeline, target, depth_tex, &depth_stencil, &bindings, vertex_count, instance_count, load_action, "label");

// Extended (wireframe, depth bias, primitive type)
gpu.native_enc.draw_instanced_depth_ex(pipeline, target, depth_tex, &depth_stencil, &bindings, vertex_count, instance_count, load_action, fill_mode, primitive_type, depth_bias, "label");
```

## Key Differences: Effects vs Generators

| Aspect | Effect | Generator |
|--------|--------|-----------|
| Registration | `EffectMetadata` + `EffectFactory` via `inventory::submit!` | `GeneratorMetadata` + `GeneratorFactory` via `inventory::submit!` |
| Params | `ParamSpec` (shared) — same helpers for both | `ParamSpec` (shared) — same helpers for both |
| Input | Source texture + target | Just target (generates from scratch) |
| Context | `EffectContext` + `EffectInstance` | `GeneratorContext` |
| Lifecycle | Singleton per type, per-owner state via `owner_key` | Per-clip instance, state on struct |
| State key | `ctx.owner_key` (i64) | Typically self-contained |
