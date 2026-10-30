//! `node.scatter_on_mesh` — scatter `Array<InstanceTransform>` across a
//! mesh's own surface, area-weighted so density is uniform regardless of
//! triangulation.
//!
//! Same 3-pass area/scan/place shape as `spawn_from_mesh.rs` — the
//! committed precedent (D8): per-triangle area (one thread per triangle),
//! a single-thread inclusive prefix-sum scan, then a barycentric place pass
//! that writes one `InstanceTransform` per active instance. The scan's
//! result never leaves the GPU (no same-frame GPU→CPU readback, DECOMPOSING
//! section 7's shared-buffer rule) — it feeds `place_main` directly, same as the
//! precedent.
//!
//! `max_capacity` is the allocation ceiling (`array_output_capacity`);
//! `count` is the live, port-shadowed instance count sweeping under it.
//! This is `spawn_from_mesh`'s two-param split — scatter originally
//! collapsed the two into `count` "for simplicity", which meant the
//! buffer was sized from whatever the count card happened to read at
//! graph-build time, silently capping the fader's live range (Scene 2,
//! 2026-07-11: slider to 48, 18 drawn). Presets without `max_capacity`
//! fall back to sizing from `count` (back-compat, Garden.json).
//!
//! Deterministic for a fixed `(seed, mesh)`: every random draw in the
//! shader is a `wang_hash` chain seeded from `(instance index, seed)`, no
//! true randomness anywhere.

use manifold_gpu::BufferUse;
use manifold_gpu::GpuBinding;
use std::borrow::Cow;

use manifold_node_engine::content_revision::ContentVersion;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::{InstanceTransform, MeshVertex};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::material::CoverRule;
use manifold_node_engine::scene::transform::{IDENTITY_MAT4, transform_point};

use super::render_scene::mat4_inverse;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct ScatterOnMeshUniforms {
    count: u32,
    seed: u32,
    vertex_count: u32,
    triangle_count: u32,
    scale_min: f32,
    scale_max: f32,
    align_to_normal: u32,
    // Mirrors shaders/scatter_on_mesh.wgsl's Params — place_main runs over
    // all `capacity` slots and parks [count, capacity) at zero scale, so a
    // lowered count fader removes instances instead of leaving a stale
    // drawn tail (render_scene draws buffer_size/32 unconditionally).
    capacity: u32,
    // 1 when `indices` lists the triangles, 0 when the vertices are a
    // triangle list.
    indexed: u32,
    // Reach (`near_spot`): the Terrain grid's vertices per side, the copy
    // window's cells per side, and the reach; all 0 over the whole surface.
    grid: u32,
    cells: u32,
    reach: f32,
    // Where copies grow: `CoverRule::packed`.
    rule_slope: [f32; 4],
    rule_height: [f32; 4],
    rule_breakup: [f32; 4],
    // The camera in the surface's own frame, for Reach.
    camera: [f32; 4],
}

/// Where copies grow, as a Material's Covers lie (`cover_rule.wgsl`): name,
/// label, default, low, high. The defaults hold everywhere.
type GrowParam = (&'static str, &'static str, f32, f32, f32);

const GROW_PARAMS: [GrowParam; 10] = [
    ("steeper_than", "Steeper Than (°)", 0.0, 0.0, 90.0),
    ("flatter_than", "Flatter Than (°)", 90.0, 0.0, 90.0),
    ("slope_softness", "Slope Softness (°)", 5.0, 0.0, 45.0),
    ("above", "Above (m)", -10_000.0, -10_000.0, 10_000.0),
    ("below", "Below (m)", 10_000.0, -10_000.0, 10_000.0),
    ("height_softness", "Height Softness (m)", 20.0, 0.0, 1000.0),
    ("breakup", "Breakup", 0.0, 0.0, 1.0),
    ("breakup_size", "Breakup Size (m)", 30.0, 0.1, 1000.0),
    ("water", "Water", 0.0, 0.0, 1.0),
    ("soil", "Soil", 0.0, 0.0, 1.0),
];

const fn grow_param(k: usize) -> ParamDef {
    let (name, label, default, low, high) = GROW_PARAMS[k];
    ParamDef {
        name: Cow::Borrowed(name),
        label,
        ty: ParamType::Float,
        default: ParamValue::Float(default),
        range: Some((low, high)),
        enum_values: &[],
    }
}

/// Where copies grow, from the params, each clamped to its range.
fn grow_rule(ctx: &EffectNodeContext<'_, '_>) -> CoverRule {
    let value = |k: usize| {
        let (name, _, default, low, high) = GROW_PARAMS[k];
        ctx.scalar_or_param(name, default).clamp(low, high)
    };
    CoverRule {
        amount: 1.0,
        steeper_than: value(0),
        flatter_than: value(1),
        slope_softness: value(2),
        above: value(3),
        below: value(4),
        height_softness: value(5),
        breakup: value(6),
        breakup_size: value(7),
        water: value(8),
        soil: value(9),
    }
}

/// What the area table was summed from. Positions moving over the same
/// triangles keep it, so each copy keeps its triangle and its spot on it and
/// rides the surface as it moves.
#[derive(Clone, Copy, PartialEq)]
struct Summed {
    triangles: u32,
    /// The mesh's triangle revision when it publishes one, else its
    /// content's.
    topology: Option<u64>,
    content: Option<ContentVersion>,
    indices: Option<ContentVersion>,
    rebuild_epoch: u64,
}

/// What the copies were last placed from.
#[derive(Clone, Copy, PartialEq)]
struct Placed {
    summed: Summed,
    vertices: ContentVersion,
    uniforms: ScatterOnMeshUniforms,
}

const SHADER_SRC: &str = concat!(
    include_str!("shaders/cover_rule.wgsl"),
    include_str!("shaders/scatter_on_mesh.wgsl"),
);

manifold_node_engine::primitive! {
    name: ScatterOnMesh,
    type_id: "node.scatter_on_mesh",
    purpose: "Scatter an Array<InstanceTransform> across a mesh's own surface (Array<MeshVertex>, as a triangle list or with the triangles in indices), area-weighted so instance density is uniform regardless of triangulation — the instance-producing sibling of node.spawn_from_mesh's surface mode, same 3-pass area/scan/place dispatch. Each instance gets a barycentric-sampled surface position, a uniform scale hashed into [scale_min, scale_max], and either a random upright yaw or (when align_to_normal is set) a rotation that additionally tilts the instance's local +Y onto the sampled triangle's flat face normal. Deterministic for a fixed (seed, mesh) — no true randomness. Pair with node.render_copies to draw the scattered instances: a field of scanned flowers on a terrain is terrain mesh -> node.scatter_on_mesh -> node.render_copies.",
    inputs: {
        vertices: Array(MeshVertex) required,
        // The triangles as vertex index triples, for an indexed mesh such as
        // a Terrain's grid. Unwired, the vertices are a triangle list.
        indices: Array(u32) optional,
        count: ScalarF32 optional,
        seed: ScalarF32 optional,
        scale_min: ScalarF32 optional,
        scale_max: ScalarF32 optional,
        // For Reach: the camera, and the surface's frame the copies sit in.
        camera: Camera optional,
        frame: Transform optional,
        // Optional execution gate: when wired, the scatter only RECOMPUTES
        // on this value's integer edges (+ the first frame) — same contract
        // as node.spawn_from_mesh's reset_trigger. Unwired -> recompute
        // every frame.
        reset_trigger: ScalarF32 optional,
    },
    outputs: {
        instances: Array(InstanceTransform),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("count"),
            label: "Count",
            ty: ParamType::Int,
            default: ParamValue::Float(256.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0), // 0 = size from count (back-compat)
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("seed"),
            label: "Seed",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scale_min"),
            label: "Scale Min",
            ty: ParamType::Float,
            default: ParamValue::Float(0.8),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scale_max"),
            label: "Scale Max",
            ty: ParamType::Float,
            default: ParamValue::Float(1.2),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("align_to_normal"),
            label: "Align To Normal",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("grid"),
            label: "Grid",
            ty: ParamType::Bool,
            default: ParamValue::Bool(false),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("reach"),
            label: "Reach (m)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 10_000.0)),
            enum_values: &[],
        },
        grow_param(0),
        grow_param(1),
        grow_param(2),
        grow_param(3),
        grow_param(4),
        grow_param(5),
        grow_param(6),
        grow_param(7),
        grow_param(8),
        grow_param(9),
    ],
    depth_rule: Terminal,
    composition_notes: "count/seed/scale_min/scale_max are port-shadows-param — wire an LFO or envelope into count to sweep flower density live, or into seed to re-roll the placement (a re-roll is a hard cut, not an animatable morph — the whole field re-samples). align_to_normal is NOT port-shadowed (a structural on/off choice, not a performance scalar): off gives every instance a random upright yaw (local +Y stays world-up); on additionally tilts +Y onto the sampled triangle's flat face normal, so instances lie flush against sloped or curved surfaces. `max_capacity` (static, never bind it to a card) declares the output's allocation ceiling; `count` sweeps live beneath it and slots beyond count park at zero scale. Set max_capacity to the count card's max so the fader's whole range is real; when max_capacity is 0/absent the buffer sizes from count at build time (legacy single-param behavior). Triangles are read as flat [v0,v1,v2] triples (standard triangle-list layout); a trailing partial triangle (vertex_count % 3 != 0) is ignored.",
    examples: ["Garden"],
    picker: { label: "Scatter On Mesh", category: Atom },
    summary: "Scatters copies of an object across a mesh's surface — a field of instances placed and sized randomly but deterministically, area-weighted so they don't clump on small triangles.",
    category: Geometry3D,
    role: Source,
    aliases: ["scatter on mesh", "surface scatter", "instance on surface", "field of instances", "populate mesh"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        // The macro-allocated `pipeline` field holds area_main; these hold
        // the other two entry points of the 3-pass dispatch.
        scan_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        place_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        // Per-triangle cumulative-area table, summed again only when the
        // triangles change (`Summed`).
        cumulative: Option<manifold_gpu::GpuBuffer> = None,
        cached_triangle_count: u32 = 0,
        summed: Option<Summed> = None,
        placed: Option<Placed> = None,
        // Bound in place of `indices` for a triangle list, which never
        // reads it.
        no_indices: Option<manifold_gpu::GpuBuffer> = None,
        // Last observed `reset_trigger` integer, for edge-gated recompute.
        last_reset_trigger: Option<i32> = None
    },
}

impl Primitive for ScatterOnMesh {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &manifold_node_engine::exec::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "instances" {
            return None;
        }
        // Ceiling from max_capacity when set (> 0); otherwise size from
        // count — the original single-param behavior, kept so presets
        // without max_capacity load identically.
        match params.get("max_capacity") {
            Some(ParamValue::Float(n)) if *n >= 1.0 => return Some(n.round() as u32),
            _ => {}
        }
        match params.get("count") {
            Some(ParamValue::Float(n)) => Some(n.round().max(0.0) as u32),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Execution gate (see the `reset_trigger` input): when wired,
        // recompute the scatter only on the trigger's integer edges (+ the
        // first frame). Cheap edge check before any allocation or dispatch.
        if let Some(ParamValue::Float(v)) = ctx.inputs.scalar("reset_trigger") {
            let current = v.round() as i32;
            let edge = self.last_reset_trigger != Some(current);
            self.last_reset_trigger = Some(current);
            if !edge {
                return;
            }
        }

        let count_runtime = ctx.scalar_or_param("count", 256.0).round().max(0.0) as u32;
        let seed = ctx.scalar_or_param("seed", 0.0).round() as u32;
        let scale_min = ctx.scalar_or_param("scale_min", 0.8);
        let scale_max = ctx.scalar_or_param("scale_max", 1.2);
        let align_to_normal = matches!(
            ctx.params.get("align_to_normal"),
            Some(ParamValue::Bool(true))
        );
        let [rule_slope, rule_height, rule_breakup] = grow_rule(ctx).packed();

        let Some(src) = ctx.inputs.array("vertices") else {
            return;
        };
        let indices = ctx.inputs.array("indices");
        let Some(out_buf) = ctx.outputs.array("instances") else {
            return;
        };

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let instance_size = std::mem::size_of::<InstanceTransform>() as u64;
        let vertex_count = (src.size / vertex_size) as u32;
        let capacity = (out_buf.size / instance_size) as u32;
        if capacity == 0 {
            return;
        }
        let count = count_runtime.min(capacity);
        // A Terrain's square grid places copies by map position.
        let grid = if matches!(ctx.params.get("grid"), Some(ParamValue::Bool(true))) {
            let grid = vertex_count.isqrt();
            if grid < 2 || grid * grid != vertex_count {
                ctx.error("Scatter On Mesh: Grid needs a square grid of vertices");
                return;
            }
            grid
        } else {
            0
        };
        // Reach: the copies fill cells round the camera on the grid, so they
        // need the camera in the grid's frame.
        let reach = ctx.scalar_or_param("reach", 0.0).max(0.0);
        let (cells, camera) = if reach > 0.0 {
            if grid == 0 {
                ctx.error("Scatter On Mesh: Reach needs a Terrain");
                return;
            }
            let Some(camera) = ctx.inputs.camera("camera") else {
                ctx.error("Scatter On Mesh: Reach needs the scene's camera");
                return;
            };
            let frame = ctx
                .inputs
                .transform("frame")
                .map_or(IDENTITY_MAT4, |frame| frame.world());
            let Some(inverse) = mat4_inverse(frame) else {
                ctx.error("Scatter On Mesh: the surface's frame is flat");
                return;
            };
            let [x, y, z] = transform_point(&inverse, camera.pos);
            (capacity.isqrt(), [x, y, z, 0.0])
        } else {
            (0, [0.0; 4])
        };
        let triangle_count = match indices {
            Some(indices) => (indices.size / 4 / 3) as u32,
            None => vertex_count / 3,
        };
        let uniforms = ScatterOnMeshUniforms {
            count,
            seed,
            vertex_count,
            triangle_count,
            scale_min,
            scale_max,
            align_to_normal: u32::from(align_to_normal),
            capacity,
            indexed: u32::from(indices.is_some()),
            grid,
            cells,
            reach,
            rule_slope,
            rule_height,
            rule_breakup,
            camera,
        };

        // The table is summed again only when the triangles change; with no
        // way to tell, every frame.
        let revision = ctx.inputs.mesh_revision("vertices");
        let content = ctx.inputs.content_version("vertices");
        let summed = Summed {
            triangles: triangle_count,
            topology: revision.map(|r| r.topology),
            content: if revision.is_some() { None } else { content },
            indices: ctx.inputs.content_version("indices"),
            rebuild_epoch: ctx.rebuild_epoch,
        };
        let knowable = (summed.topology.is_some() || summed.content.is_some())
            && (indices.is_none() || summed.indices.is_some());
        let summed = knowable.then_some(summed);
        let placed = summed.zip(content).map(|(summed, vertices)| Placed {
            summed,
            vertices,
            uniforms,
        });
        if placed.is_some() && self.placed == placed && ctx.outputs_retained() {
            ctx.mark_outputs_unchanged();
            return;
        }
        let resum = summed.is_none() || self.summed != summed;

        let gpu = ctx.gpu_encoder();

        // Lazy-compile all three entry points from the shared shader
        // source. `self.pipeline` (macro-provided) holds area_main.
        if self.pipeline.is_none() {
            self.pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER_SRC,
                "area_main",
                "node.scatter_on_mesh.area",
            ));
        }
        if self.scan_pipeline.is_none() {
            self.scan_pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER_SRC,
                "scan_main",
                "node.scatter_on_mesh.scan",
            ));
        }
        if self.place_pipeline.is_none() {
            self.place_pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER_SRC,
                "place_main",
                "node.scatter_on_mesh.place",
            ));
        }

        // Lazy-allocate / realloc the per-triangle cumulative-area table.
        let needs_alloc = self.cumulative.is_none() || self.cached_triangle_count != triangle_count;
        if needs_alloc {
            let bytes = u64::from(triangle_count.max(1)) * 4;
            self.cumulative = Some(gpu.device.create_buffer(bytes, BufferUse::Gpu));
            self.cached_triangle_count = triangle_count;
        }
        let cumulative = self.cumulative.as_ref().expect("cumulative just allocated");
        let indices = match indices {
            Some(indices) => indices,
            None => self
                .no_indices
                .get_or_insert_with(|| gpu.device.create_buffer(4, BufferUse::Gpu)),
        };

        let area_pipeline = self.pipeline.as_ref().expect("just inserted");
        let scan_pipeline = self.scan_pipeline.as_ref().expect("just inserted");
        let place_pipeline = self.place_pipeline.as_ref().expect("just inserted");

        let bindings = [
            GpuBinding::Buffer {
                binding: 0,
                buffer: out_buf,
                offset: 0,
            },
            GpuBinding::Bytes {
                binding: 1,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: src,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: cumulative,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: indices,
                offset: 0,
            },
        ];

        // area -> scan -> place, each pass reads what the previous wrote,
        // so a barrier between every stage (same shape as
        // spawn_from_mesh's surface mode).
        if grid == 0 && triangle_count > 0 && (resum || needs_alloc) {
            gpu.native_enc.dispatch_compute(
                area_pipeline,
                &bindings,
                [triangle_count.div_ceil(64), 1, 1],
                "node.scatter_on_mesh.area",
            );
            gpu.native_enc.compute_memory_barrier_buffers();

            gpu.native_enc.dispatch_compute(
                scan_pipeline,
                &bindings,
                [1, 1, 1],
                "node.scatter_on_mesh.scan",
            );
            gpu.native_enc.compute_memory_barrier_buffers();
        }
        // place_main guards internally on triangle_count == 0 (parks every
        // instance zeroed) so it always runs, even for a degenerate mesh.
        gpu.native_enc.dispatch_compute(
            place_pipeline,
            &bindings,
            [capacity.div_ceil(256), 1, 1],
            "node.scatter_on_mesh.place",
        );
        // A grid reads no area table, so a later place by area sums one.
        self.summed = if grid == 0 { summed } else { None };
        self.placed = placed;
    }
}

impl ScatterOnMesh {
    /// BUG-037: all three entry points (`area_main`/`scan_main`/`place_main`)
    /// come from one fixed, asset-independent shader source — same shape as
    /// `RenderScene::prewarm_pipelines`. Lazily compiled on first `run()`
    /// otherwise (real Metal compile, tens of ms), which is exactly the class
    /// of frame-0 stall BUG-037 tracks for a glTF scene layer that scatters
    /// instances across its mesh on the first rendered frame. The device's
    /// pipeline cache is keyed by shader hash and shared across every
    /// `ScatterOnMesh` instance, so warming here makes every later `run()`,
    /// on any layer, a cache hit. Called from `GeneratorRegistry::prewarm_all`
    /// at app startup.
    pub fn prewarm_pipelines(device: &manifold_gpu::GpuDevice) {
        device.create_compute_pipeline(SHADER_SRC, "area_main", "node.scatter_on_mesh.area");
        device.create_compute_pipeline(SHADER_SRC, "scan_main", "node.scatter_on_mesh.scan");
        device.create_compute_pipeline(SHADER_SRC, "place_main", "node.scatter_on_mesh.place");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_node_engine::exec::effect_node::EffectNode;

    /// `max_capacity` > 0 must win;
    /// 0/absent falls back to count (Garden.json back-compat).
    #[test]
    fn max_capacity_overrides_count_as_ceiling() {
        use manifold_node_engine::exec::effect_node::ParamValues;
        let prim = ScatterOnMesh::new();
        let node: &dyn EffectNode = &prim;
        let mut params: ParamValues = ParamValues::default();
        params.insert(Cow::Borrowed("count"), ParamValue::Float(18.0));
        params.insert(Cow::Borrowed("max_capacity"), ParamValue::Float(64.0));
        assert_eq!(
            node.array_output_capacity("instances", &params, &[]),
            Some(64)
        );
        params.insert(Cow::Borrowed("max_capacity"), ParamValue::Float(0.0));
        assert_eq!(
            node.array_output_capacity("instances", &params, &[]),
            Some(18),
            "max_capacity 0 must fall back to count sizing"
        );
    }

    #[test]
    fn array_output_capacity_reads_count_param() {
        use manifold_node_engine::exec::effect_node::ParamValues;
        let prim = ScatterOnMesh::new();
        let node: &dyn EffectNode = &prim;
        let mut params: ParamValues = ParamValues::default();
        params.insert(Cow::Borrowed("count"), ParamValue::Float(512.0));
        let cap = node.array_output_capacity("instances", &params, &[]);
        assert_eq!(cap, Some(512));
        // Wrong port name -> None (not this node's capacity to declare).
        assert_eq!(node.array_output_capacity("out", &params, &[]), None);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level tests. Multi-pass boundary primitive (like
    //! `spawn_from_mesh.rs`) — no freeze/fusion codegen exists to compare
    //! against, so these dispatch the hand-written WGSL entry points
    //! directly.
    use super::*;

    fn mk_vertex(pos: [f32; 3]) -> MeshVertex {
        MeshVertex {
            position: pos,
            _pad0: 0.0,
            normal: [0.0, 1.0, 0.0],
            _pad1: 0.0,
            uv: [0.0, 0.0],
            _pad2: [0.0, 0.0],
            tangent: [0.0; 4],
            color: [1.0; 4],
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_scatter(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        vertices: &[MeshVertex],
        count: u32,
        capacity: u32,
        seed: u32,
        scale_min: f32,
        scale_max: f32,
        align_to_normal: bool,
    ) -> Vec<InstanceTransform> {
        let vertex_count = vertices.len() as u32;
        let triangle_count = vertex_count / 3;
        // Grow everywhere.
        let [rule_slope, rule_height, rule_breakup] = CoverRule::default().packed();
        let uniforms = ScatterOnMeshUniforms {
            count,
            seed,
            vertex_count,
            triangle_count,
            scale_min,
            scale_max,
            align_to_normal: u32::from(align_to_normal),
            capacity,
            indexed: 0,
            grid: 0,
            cells: 0,
            reach: 0.0,
            rule_slope,
            rule_height,
            rule_breakup,
            camera: [0.0; 4],
        };
        dispatch(device, wgsl, vertices, &uniforms)
    }

    /// Run the place pass over `uniforms.capacity` slots, after the area
    /// table when the copies spread over the whole surface.
    fn dispatch(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        vertices: &[MeshVertex],
        uniforms: &ScatterOnMeshUniforms,
    ) -> Vec<InstanceTransform> {
        let capacity = uniforms.capacity;
        let triangle_count = uniforms.triangle_count;
        let vbuf = device.create_buffer(std::mem::size_of_val(vertices) as u64, BufferUse::Upload);
        unsafe {
            vbuf.write(0, bytemuck::cast_slice(vertices));
        }
        let ibuf = device.create_buffer(
            capacity as u64 * std::mem::size_of::<InstanceTransform>() as u64,
            BufferUse::Readback,
        );
        let cumulative =
            device.create_buffer(u64::from(triangle_count.max(1)) * 4, BufferUse::Upload);
        let no_indices = device.create_buffer(4, BufferUse::Upload);

        let bindings = [
            GpuBinding::Buffer {
                binding: 0,
                buffer: &ibuf,
                offset: 0,
            },
            GpuBinding::Bytes {
                binding: 1,
                data: bytemuck::bytes_of(uniforms),
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &vbuf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &cumulative,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: &no_indices,
                offset: 0,
            },
        ];

        let mut enc = device.create_encoder("scatter-on-mesh-test");
        if uniforms.grid == 0 && triangle_count > 0 {
            let area = device.create_compute_pipeline(wgsl, "area_main", "scatter-area-test");
            enc.dispatch_compute(
                &area,
                &bindings,
                [triangle_count.div_ceil(64), 1, 1],
                "area",
            );
            enc.compute_memory_barrier_buffers();
            let scan = device.create_compute_pipeline(wgsl, "scan_main", "scatter-scan-test");
            enc.dispatch_compute(&scan, &bindings, [1, 1, 1], "scan");
            enc.compute_memory_barrier_buffers();
        }
        let place = device.create_compute_pipeline(wgsl, "place_main", "scatter-place-test");
        enc.dispatch_compute(&place, &bindings, [capacity.div_ceil(256), 1, 1], "place");
        enc.commit_and_wait().expect("GPU work completes");

        let ptr = ibuf.mapped_ptr().expect("shared instance buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const InstanceTransform, capacity as usize) }
            .to_vec()
    }

    fn quad_mesh() -> Vec<MeshVertex> {
        // Two triangles forming a unit quad in the XZ plane (y=0), a
        // reasonable stand-in "terrain patch".
        let a = [0.0f32, 0.0, 0.0];
        let b = [4.0f32, 0.0, 0.0];
        let c = [4.0f32, 0.0, 4.0];
        let d = [0.0f32, 0.0, 4.0];
        vec![
            mk_vertex(a),
            mk_vertex(b),
            mk_vertex(c),
            mk_vertex(a),
            mk_vertex(c),
            mk_vertex(d),
        ]
    }

    #[test]
    fn same_seed_and_mesh_gives_identical_instance_buffer_across_two_runs() {
        let device = manifold_gpu::testkit::test_device();
        let wgsl = SHADER_SRC;
        let vertices = quad_mesh();
        let capacity = 128u32;

        let run1 = dispatch_scatter(
            &device, wgsl, &vertices, capacity, capacity, 42, 0.8, 1.2, false,
        );
        let run2 = dispatch_scatter(
            &device, wgsl, &vertices, capacity, capacity, 42, 0.8, 1.2, false,
        );

        for (i, (a, b)) in run1.iter().zip(run2.iter()).enumerate() {
            assert_eq!(
                a.pos_scale, b.pos_scale,
                "instance {i} pos_scale differs across identical runs"
            );
            assert_eq!(
                a.rot_pad, b.rot_pad,
                "instance {i} rot_pad differs across identical runs"
            );
        }
    }

    #[test]
    fn different_seed_gives_different_placement() {
        let device = manifold_gpu::testkit::test_device();
        let wgsl = SHADER_SRC;
        let vertices = quad_mesh();
        let capacity = 128u32;

        let run_a = dispatch_scatter(
            &device, wgsl, &vertices, capacity, capacity, 1, 0.8, 1.2, false,
        );
        let run_b = dispatch_scatter(
            &device, wgsl, &vertices, capacity, capacity, 2, 0.8, 1.2, false,
        );

        let any_differ = run_a
            .iter()
            .zip(run_b.iter())
            .any(|(a, b)| a.pos_scale != b.pos_scale || a.rot_pad != b.rot_pad);
        assert!(
            any_differ,
            "different seeds should not produce an identical placement"
        );
    }

    #[test]
    fn instances_land_on_the_mesh_surface_within_scale_bounds() {
        let device = manifold_gpu::testkit::test_device();
        let wgsl = SHADER_SRC;
        let vertices = quad_mesh();
        let capacity = 64u32;

        let instances = dispatch_scatter(
            &device, wgsl, &vertices, capacity, capacity, 7, 0.5, 1.5, false,
        );
        for (i, inst) in instances.iter().enumerate() {
            let [x, y, z, scale] = inst.pos_scale;
            assert!(
                y.abs() < 1e-4,
                "instance {i} y={y} should be on the y=0 quad plane"
            );
            assert!(
                (-1e-3..=4.0 + 1e-3).contains(&x),
                "instance {i} x={x} outside quad bounds"
            );
            assert!(
                (-1e-3..=4.0 + 1e-3).contains(&z),
                "instance {i} z={z} outside quad bounds"
            );
            assert!(
                (0.5 - 1e-4..=1.5 + 1e-4).contains(&scale),
                "instance {i} scale={scale} outside [0.5, 1.5]"
            );
            // rot_pad.y (yaw) should vary; rot_pad.x/z stay 0 when
            // align_to_normal is off (flat quad normal is world-up anyway,
            // but this exercises the "off" branch specifically).
            assert_eq!(
                inst.rot_pad[0], 0.0,
                "instance {i} pitch should be 0 with align_to_normal off"
            );
            assert_eq!(
                inst.rot_pad[2], 0.0,
                "instance {i} roll should be 0 with align_to_normal off"
            );
        }
    }

    #[test]
    fn align_to_normal_tilts_instances_on_a_sloped_triangle() {
        let device = manifold_gpu::testkit::test_device();
        let wgsl = SHADER_SRC;

        // A single triangle tilted 45 degrees off the XZ plane: its face
        // normal is NOT world-up, so align_to_normal must produce nonzero
        // pitch/roll to tilt the instance onto it.
        let v0 = [0.0f32, 0.0, 0.0];
        let v1 = [1.0f32, 1.0, 0.0];
        let v2 = [0.0f32, 1.0, 1.0];
        let vertices = vec![mk_vertex(v0), mk_vertex(v1), mk_vertex(v2)];
        let capacity = 32u32;

        let instances = dispatch_scatter(
            &device, wgsl, &vertices, capacity, capacity, 3, 1.0, 1.0, true,
        );

        // Face normal of this triangle.
        let e1 = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
        let e2 = [v2[0] - v0[0], v2[1] - v0[1], v2[2] - v0[2]];
        let raw = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let len = (raw[0] * raw[0] + raw[1] * raw[1] + raw[2] * raw[2]).sqrt();
        let mut n = [raw[0] / len, raw[1] / len, raw[2] / len];
        // The shader orients the face normal to the hemisphere of the
        // triangle's vertex normals (mk_vertex hardcodes +Y) — winding is
        // not authoritative. This triangle winds downward, so the aligned
        // side is the flipped one. Apply the same rule to the reference.
        if n[1] < 0.0 {
            n = [-n[0], -n[1], -n[2]];
        }

        // Reconstruct R = Rz(rz)*Ry(ry)*Rx(rx) and check R*(0,1,0) ≈ n for
        // every instance — the decomposed Euler triple must actually
        // reproduce the sampled triangle's normal.
        for (i, inst) in instances.iter().enumerate() {
            let [rx, ry, rz, _] = inst.rot_pad;
            let (cx, sx) = (rx.cos(), rx.sin());
            let (cy, sy) = (ry.cos(), ry.sin());
            let (cz, sz) = (rz.cos(), rz.sin());
            // R*(0,1,0) = column 1 of R = Rz(rz)*Ry(ry)*Rx(rx) (matches
            // render_instanced_3d_mesh.wgsl's euler_xyz composition),
            // verified numerically against a full matrix expansion in the
            // P4 worklog.
            let mapped_up = [-cz * sy * sx - sz * cx, -sz * sy * sx + cz * cx, cy * sx];
            for k in 0..3 {
                assert!(
                    (mapped_up[k] - n[k]).abs() < 1e-3,
                    "instance {i} axis {k}: decomposed rotation's up vector {mapped_up:?} != triangle normal {n:?}"
                );
            }
        }
    }

    /// place_main must park [count, capacity).
    #[test]
    fn slots_beyond_count_park_at_zero_scale() {
        let device = manifold_gpu::testkit::test_device();
        let wgsl = SHADER_SRC;
        let vertices = quad_mesh();
        let capacity = 64u32;

        // First fill every slot at full count, then re-run with count=10 —
        // the tail must be parked, not left stale.
        let _full = dispatch_scatter(
            &device, wgsl, &vertices, capacity, capacity, 5, 1.0, 1.0, false,
        );
        let low = dispatch_scatter(&device, wgsl, &vertices, 10, capacity, 5, 1.0, 1.0, false);

        for (i, inst) in low.iter().enumerate() {
            if i < 10 {
                assert!(
                    inst.pos_scale[3] > 0.0,
                    "instance {i} below count should be live"
                );
            } else {
                assert_eq!(
                    inst.pos_scale[3], 0.0,
                    "slot {i} at/beyond count must park at zero scale, got {:?}",
                    inst.pos_scale
                );
            }
        }
    }

    /// Flat ground
    /// with align ON must behave like align OFF: every instance finite and
    /// upright (R·(0,1,0) ≈ (0,1,0)) for EVERY yaw the hash produces.
    #[test]
    fn align_on_flat_ground_keeps_instances_upright_and_finite() {
        let device = manifold_gpu::testkit::test_device();
        let wgsl = SHADER_SRC;
        let vertices = quad_mesh();
        let capacity = 256u32;

        let instances = dispatch_scatter(
            &device, wgsl, &vertices, capacity, capacity, 11, 1.0, 1.0, true,
        );

        for (i, inst) in instances.iter().enumerate() {
            for (k, v) in inst.pos_scale.iter().chain(inst.rot_pad.iter()).enumerate() {
                assert!(v.is_finite(), "instance {i} field {k} is not finite: {v}");
            }
            let [rx, ry, rz, _] = inst.rot_pad;
            let (cx, sx) = (rx.cos(), rx.sin());
            let (cy, sy) = (ry.cos(), ry.sin());
            let (cz, sz) = (rz.cos(), rz.sin());
            let mapped_up = [-cz * sy * sx - sz * cx, -sz * sy * sx + cz * cx, cy * sx];
            assert!(
                (mapped_up[0].abs() < 1e-3)
                    && ((mapped_up[1] - 1.0).abs() < 1e-3)
                    && (mapped_up[2].abs() < 1e-3),
                "instance {i}: flat ground must keep instances upright, got up={mapped_up:?} from euler=({rx}, {ry}, {rz})"
            );
        }
    }

    /// Reach: copies sit on the Terrain grid's own triangles, and a camera
    /// moving one cell along x re-places only the column of slots whose
    /// cell left the window; every other copy keeps its slot and its spot.
    #[test]
    fn reach_copies_sit_on_the_grid_and_keep_their_slots_as_the_camera_moves() {
        let device = manifold_gpu::testkit::test_device();
        const RES: u32 = 17;
        let height = |x: f32, z: f32| 0.02 * x * z + 0.3 * x;
        let vertices: Vec<MeshVertex> = (0..RES * RES)
            .map(|i| {
                let x = (i % RES) as f32 * 2.0 - 16.0;
                let z = (i / RES) as f32 * 2.0 - 16.0;
                mk_vertex([x, height(x, z), z])
            })
            .collect();
        // The height on grid_indices.wgsl's split of each 2 m quad.
        let on_grid = |x: f32, z: f32| {
            let (gx, gz) = ((x + 16.0) / 2.0, (z + 16.0) / 2.0);
            let (cx, cz) = (gx.floor(), gz.floor());
            let (fx, fz) = (gx - cx, gz - cz);
            let at = |dx: f32, dz: f32| height((cx + dx) * 2.0 - 16.0, (cz + dz) * 2.0 - 16.0);
            if fx + fz <= 1.0 {
                at(0.0, 0.0) * (1.0 - fx - fz) + at(0.0, 1.0) * fz + at(1.0, 0.0) * fx
            } else {
                at(1.0, 1.0) * (fx + fz - 1.0)
                    + at(0.0, 1.0) * (1.0 - fx)
                    + at(1.0, 0.0) * (1.0 - fz)
            }
        };
        let [rule_slope, rule_height, rule_breakup] = CoverRule::default().packed();
        let place = |camera_x: f32| {
            let uniforms = ScatterOnMeshUniforms {
                count: 256,
                seed: 3,
                vertex_count: RES * RES,
                triangle_count: 0,
                scale_min: 1.0,
                scale_max: 1.0,
                align_to_normal: 0,
                capacity: 256,
                indexed: 0,
                grid: RES,
                cells: 16,
                reach: 8.0,
                rule_slope,
                rule_height,
                rule_breakup,
                camera: [camera_x, 0.5, -0.4, 0.0],
            };
            dispatch(&device, SHADER_SRC, &vertices, &uniforms)
        };
        // Cells are 1 m: the window runs from cell -8 to 7, then -7 to 8,
        // and cell -8's slots (column 8) take cell 8.
        let before = place(0.3);
        let after = place(1.3);
        let mut kept = 0;
        for (slot, (a, b)) in before.iter().zip(&after).enumerate() {
            for copy in [a, b] {
                let [x, y, z, size] = copy.pos_scale;
                if size > 0.0 {
                    let want = on_grid(x, z);
                    assert!((y - want).abs() < 1e-3, "slot {slot}: {y} against {want}");
                }
            }
            if a.pos_scale[3] > 0.0 && b.pos_scale[3] > 0.0 {
                let same = a.pos_scale[..3] == b.pos_scale[..3];
                assert_eq!(same, slot % 16 != 8, "slot {slot}");
                kept += usize::from(same);
            }
        }
        assert!(kept > 50, "{kept} copies kept their spot");
    }

    #[test]
    fn prewarm_pipelines_populates_the_shared_compute_cache() {
        // BUG-037. Order-independent by design (BUG-144's documented class,
        // same fix shape as the sibling render_scene/gltf_texture_source
        // prewarm tests and registry.rs's atom-codegen sweep test): `device`
        // is process-global across the whole `--features gpu-proofs --lib`
        // run, so another test's `GeneratorRegistry::prewarm_all` may have
        // already warmed these same three entry points before this test
        // runs. Asserting "cache hit after MY prewarm call" is correct
        // either way — the operationally meaningful fact (first live `run()`
        // is a cache hit, not a real compile) holds whether this call or an
        // earlier test's warmed the cache.
        let device = manifold_gpu::testkit::test_device();
        ScatterOnMesh::prewarm_pipelines(&device);
        for (entry, label) in [
            ("area_main", "node.scatter_on_mesh.area"),
            ("scan_main", "node.scatter_on_mesh.scan"),
            ("place_main", "node.scatter_on_mesh.place"),
        ] {
            let cache_before_use = device.compute_pipeline_cache_len();
            let _pipeline = device.create_compute_pipeline(SHADER_SRC, entry, label);
            assert_eq!(
                device.compute_pipeline_cache_len(),
                cache_before_use,
                "{entry}'s pipeline compile after prewarm must be a cache hit"
            );
        }

        // A second prewarm pass must also be a pure cache hit.
        let after_first_prewarm = device.compute_pipeline_cache_len();
        ScatterOnMesh::prewarm_pipelines(&device);
        assert_eq!(
            device.compute_pipeline_cache_len(),
            after_first_prewarm,
            "a second prewarm pass must be a pure cache hit, not add more entries"
        );
    }
}
