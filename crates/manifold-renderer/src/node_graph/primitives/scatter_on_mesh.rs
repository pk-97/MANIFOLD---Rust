//! `node.scatter_on_mesh` — scatter `Array<InstanceTransform>` across a
//! mesh's own surface, area-weighted so density is uniform regardless of
//! triangulation (MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md D8, §3 P4 row).
//!
//! Same 3-pass area/scan/place shape as `spawn_from_mesh.rs` — the
//! committed precedent (D8): per-triangle area (one thread per triangle),
//! a single-thread inclusive prefix-sum scan, then a barycentric place pass
//! that writes one `InstanceTransform` per active instance. The scan's
//! result never leaves the GPU (no same-frame GPU→CPU readback, DECOMPOSING
//! §7's shared-buffer rule) — it feeds `place_main` directly, same as the
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

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::{InstanceTransform, MeshVertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
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
}

crate::primitive! {
    name: ScatterOnMesh,
    type_id: "node.scatter_on_mesh",
    purpose: "Scatter an Array<InstanceTransform> across a mesh's own surface (Array<MeshVertex>), area-weighted so instance density is uniform regardless of triangulation — the instance-producing sibling of node.spawn_from_mesh's surface mode, same 3-pass area/scan/place dispatch. Each instance gets a barycentric-sampled surface position, a uniform scale hashed into [scale_min, scale_max], and either a random upright yaw or (when align_to_normal is set) a rotation that additionally tilts the instance's local +Y onto the sampled triangle's flat face normal. Deterministic for a fixed (seed, mesh) — no true randomness. Pair with node.render_copies to draw the scattered instances: a field of scanned flowers on a terrain is terrain mesh -> node.scatter_on_mesh -> node.render_copies.",
    inputs: {
        vertices: Array(MeshVertex) required,
        count: ScalarF32 optional,
        seed: ScalarF32 optional,
        scale_min: ScalarF32 optional,
        scale_max: ScalarF32 optional,
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
        // Per-triangle cumulative-area scratch. Reallocated when the mesh's
        // triangle count changes.
        cumulative: Option<manifold_gpu::GpuBuffer> = None,
        cached_triangle_count: u32 = 0,
        // Last observed `reset_trigger` integer, for edge-gated recompute.
        last_reset_trigger: Option<i32> = None
    },
}

impl Primitive for ScatterOnMesh {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
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
        let align_to_normal = matches!(ctx.params.get("align_to_normal"), Some(ParamValue::Bool(true)));

        let Some(src) = ctx.inputs.array("vertices") else {
            return;
        };
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
        let triangle_count = vertex_count / 3;

        let gpu = ctx.gpu_encoder();

        // Lazy-compile all three entry points from the shared shader
        // source. `self.pipeline` (macro-provided) holds area_main.
        const SHADER_SRC: &str = include_str!("shaders/scatter_on_mesh.wgsl");
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

        // Lazy-allocate / realloc the per-triangle cumulative-area scratch.
        let needs_alloc =
            self.cumulative.is_none() || self.cached_triangle_count != triangle_count;
        if needs_alloc {
            let bytes = u64::from(triangle_count.max(1)) * 4;
            self.cumulative = Some(gpu.device.create_buffer(bytes));
            self.cached_triangle_count = triangle_count;
        }
        let cumulative = self.cumulative.as_ref().expect("cumulative just allocated");

        let area_pipeline = self.pipeline.as_ref().expect("just inserted");
        let scan_pipeline = self.scan_pipeline.as_ref().expect("just inserted");
        let place_pipeline = self.place_pipeline.as_ref().expect("just inserted");

        let uniforms = ScatterOnMeshUniforms {
            count,
            seed,
            vertex_count,
            triangle_count,
            scale_min,
            scale_max,
            align_to_normal: u32::from(align_to_normal),
            capacity,
        };

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
        ];

        // area -> scan -> place, each pass reads what the previous wrote,
        // so a barrier between every stage (same shape as
        // spawn_from_mesh's surface mode).
        if triangle_count > 0 {
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
        const SHADER_SRC: &str = include_str!("shaders/scatter_on_mesh.wgsl");
        device.create_compute_pipeline(SHADER_SRC, "area_main", "node.scatter_on_mesh.area");
        device.create_compute_pipeline(SHADER_SRC, "scan_main", "node.scatter_on_mesh.scan");
        device.create_compute_pipeline(SHADER_SRC, "place_main", "node.scatter_on_mesh.place");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn scatter_on_mesh_declares_mesh_in_and_instance_out() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let instance_layout = ArrayType::of_known::<InstanceTransform>();

        assert_eq!(ScatterOnMesh::TYPE_ID, "node.scatter_on_mesh");

        let vertices = ScatterOnMesh::INPUTS
            .iter()
            .find(|p| p.name == "vertices")
            .expect("vertices input");
        assert_eq!(vertices.ty, PortType::Array(mesh_layout));
        assert!(vertices.required);

        for name in ["count", "seed", "scale_min", "scale_max", "reset_trigger"] {
            let port = ScatterOnMesh::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow"));
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
            assert!(!port.required, "{name} should be optional (port-shadow)");
        }
        // align_to_normal is a structural bool, not a performance scalar —
        // it must NOT be port-shadowed.
        assert!(
            ScatterOnMesh::INPUTS.iter().all(|p| p.name != "align_to_normal"),
            "align_to_normal must not be port-shadowed"
        );

        assert_eq!(ScatterOnMesh::OUTPUTS.len(), 1);
        assert_eq!(ScatterOnMesh::OUTPUTS[0].name, "instances");
        assert_eq!(ScatterOnMesh::OUTPUTS[0].ty, PortType::Array(instance_layout));
    }

    #[test]
    fn scatter_on_mesh_has_full_param_surface() {
        let names: Vec<&str> = ScatterOnMesh::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["count", "max_capacity", "seed", "scale_min", "scale_max", "align_to_normal"]
        );
        let align = ScatterOnMesh::PARAMS
            .iter()
            .find(|p| p.name == "align_to_normal")
            .unwrap();
        assert_eq!(align.ty, ParamType::Bool);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ScatterOnMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.scatter_on_mesh");
    }

    #[test]
    fn declares_optional_reset_trigger_gate_input() {
        use crate::node_graph::ports::{PortType, ScalarType};
        let rt = ScatterOnMesh::INPUTS
            .iter()
            .find(|p| p.name == "reset_trigger")
            .expect("reset_trigger input");
        assert_eq!(rt.ty, PortType::Scalar(ScalarType::F32));
        assert!(
            !rt.required,
            "reset_trigger is optional (unwired ⇒ recompute every frame)"
        );
    }

    /// Scene 2 bug (2026-07-11): capacity sized from `count` at build time
    /// silently capped the density fader at whatever the card read when the
    /// graph was built (slider 48, 18 drawn). `max_capacity` > 0 must win;
    /// 0/absent falls back to count (Garden.json back-compat).
    #[test]
    fn max_capacity_overrides_count_as_ceiling() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = ScatterOnMesh::new();
        let node: &dyn EffectNode = &prim;
        let mut params: ParamValues = ParamValues::default();
        params.insert(Cow::Borrowed("count"), ParamValue::Float(18.0));
        params.insert(Cow::Borrowed("max_capacity"), ParamValue::Float(64.0));
        assert_eq!(node.array_output_capacity("instances", &params, &[]), Some(64));
        params.insert(Cow::Borrowed("max_capacity"), ParamValue::Float(0.0));
        assert_eq!(
            node.array_output_capacity("instances", &params, &[]),
            Some(18),
            "max_capacity 0 must fall back to count sizing"
        );
    }

    #[test]
    fn array_output_capacity_reads_count_param() {
        use crate::node_graph::effect_node::ParamValues;
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

        let vbuf = device.create_buffer_shared(std::mem::size_of_val(vertices) as u64);
        unsafe {
            vbuf.write(0, bytemuck::cast_slice(vertices));
        }
        let ibuf = device
            .create_buffer_shared(capacity as u64 * std::mem::size_of::<InstanceTransform>() as u64);
        let cumulative = device.create_buffer_shared(u64::from(triangle_count.max(1)) * 4);

        let uniforms = ScatterOnMeshUniforms {
            count,
            seed,
            vertex_count,
            triangle_count,
            scale_min,
            scale_max,
            align_to_normal: u32::from(align_to_normal),
            capacity,
        };

        let bindings = [
            GpuBinding::Buffer { binding: 0, buffer: &ibuf, offset: 0 },
            GpuBinding::Bytes { binding: 1, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 2, buffer: &vbuf, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &cumulative, offset: 0 },
        ];

        let mut enc = device.create_encoder("scatter-on-mesh-test");
        if triangle_count > 0 {
            let area = device.create_compute_pipeline(wgsl, "area_main", "scatter-area-test");
            enc.dispatch_compute(&area, &bindings, [triangle_count.div_ceil(64), 1, 1], "area");
            enc.compute_memory_barrier_buffers();
            let scan = device.create_compute_pipeline(wgsl, "scan_main", "scatter-scan-test");
            enc.dispatch_compute(&scan, &bindings, [1, 1, 1], "scan");
            enc.compute_memory_barrier_buffers();
        }
        let place = device.create_compute_pipeline(wgsl, "place_main", "scatter-place-test");
        enc.dispatch_compute(&place, &bindings, [capacity.div_ceil(256), 1, 1], "place");
        enc.commit_and_wait_completed();

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
            mk_vertex(a), mk_vertex(b), mk_vertex(c),
            mk_vertex(a), mk_vertex(c), mk_vertex(d),
        ]
    }

    #[test]
    fn same_seed_and_mesh_gives_identical_instance_buffer_across_two_runs() {
        let device = crate::test_device();
        let wgsl = include_str!("shaders/scatter_on_mesh.wgsl");
        let vertices = quad_mesh();
        let capacity = 128u32;

        let run1 = dispatch_scatter(&device, wgsl, &vertices, capacity, capacity, 42, 0.8, 1.2, false);
        let run2 = dispatch_scatter(&device, wgsl, &vertices, capacity, capacity, 42, 0.8, 1.2, false);

        for (i, (a, b)) in run1.iter().zip(run2.iter()).enumerate() {
            assert_eq!(a.pos_scale, b.pos_scale, "instance {i} pos_scale differs across identical runs");
            assert_eq!(a.rot_pad, b.rot_pad, "instance {i} rot_pad differs across identical runs");
        }
    }

    #[test]
    fn different_seed_gives_different_placement() {
        let device = crate::test_device();
        let wgsl = include_str!("shaders/scatter_on_mesh.wgsl");
        let vertices = quad_mesh();
        let capacity = 128u32;

        let run_a = dispatch_scatter(&device, wgsl, &vertices, capacity, capacity, 1, 0.8, 1.2, false);
        let run_b = dispatch_scatter(&device, wgsl, &vertices, capacity, capacity, 2, 0.8, 1.2, false);

        let any_differ = run_a
            .iter()
            .zip(run_b.iter())
            .any(|(a, b)| a.pos_scale != b.pos_scale || a.rot_pad != b.rot_pad);
        assert!(any_differ, "different seeds should not produce an identical placement");
    }

    #[test]
    fn instances_land_on_the_mesh_surface_within_scale_bounds() {
        let device = crate::test_device();
        let wgsl = include_str!("shaders/scatter_on_mesh.wgsl");
        let vertices = quad_mesh();
        let capacity = 64u32;

        let instances = dispatch_scatter(&device, wgsl, &vertices, capacity, capacity, 7, 0.5, 1.5, false);
        for (i, inst) in instances.iter().enumerate() {
            let [x, y, z, scale] = inst.pos_scale;
            assert!(y.abs() < 1e-4, "instance {i} y={y} should be on the y=0 quad plane");
            assert!((-1e-3..=4.0 + 1e-3).contains(&x), "instance {i} x={x} outside quad bounds");
            assert!((-1e-3..=4.0 + 1e-3).contains(&z), "instance {i} z={z} outside quad bounds");
            assert!((0.5 - 1e-4..=1.5 + 1e-4).contains(&scale), "instance {i} scale={scale} outside [0.5, 1.5]");
            // rot_pad.y (yaw) should vary; rot_pad.x/z stay 0 when
            // align_to_normal is off (flat quad normal is world-up anyway,
            // but this exercises the "off" branch specifically).
            assert_eq!(inst.rot_pad[0], 0.0, "instance {i} pitch should be 0 with align_to_normal off");
            assert_eq!(inst.rot_pad[2], 0.0, "instance {i} roll should be 0 with align_to_normal off");
        }
    }

    #[test]
    fn align_to_normal_tilts_instances_on_a_sloped_triangle() {
        let device = crate::test_device();
        let wgsl = include_str!("shaders/scatter_on_mesh.wgsl");

        // A single triangle tilted 45 degrees off the XZ plane: its face
        // normal is NOT world-up, so align_to_normal must produce nonzero
        // pitch/roll to tilt the instance onto it.
        let v0 = [0.0f32, 0.0, 0.0];
        let v1 = [1.0f32, 1.0, 0.0];
        let v2 = [0.0f32, 1.0, 1.0];
        let vertices = vec![mk_vertex(v0), mk_vertex(v1), mk_vertex(v2)];
        let capacity = 32u32;

        let instances = dispatch_scatter(&device, wgsl, &vertices, capacity, capacity, 3, 1.0, 1.0, true);

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
            let mapped_up = [
                -cz * sy * sx - sz * cx,
                -sz * sy * sx + cz * cx,
                cy * sx,
            ];
            for k in 0..3 {
                assert!(
                    (mapped_up[k] - n[k]).abs() < 1e-3,
                    "instance {i} axis {k}: decomposed rotation's up vector {mapped_up:?} != triangle normal {n:?}"
                );
            }
        }
    }

    /// BUG found by the Scene 2 look-dev (2026-07-11): lowering `count`
    /// below a previously-written value left the tail slots holding stale
    /// placements, and render_scene draws every buffer slot — the density
    /// fader appeared dead. place_main must park [count, capacity).
    #[test]
    fn slots_beyond_count_park_at_zero_scale() {
        let device = crate::test_device();
        let wgsl = include_str!("shaders/scatter_on_mesh.wgsl");
        let vertices = quad_mesh();
        let capacity = 64u32;

        // First fill every slot at full count, then re-run with count=10 —
        // the tail must be parked, not left stale.
        let _full = dispatch_scatter(&device, wgsl, &vertices, capacity, capacity, 5, 1.0, 1.0, false);
        let low = dispatch_scatter(&device, wgsl, &vertices, 10, capacity, 5, 1.0, 1.0, false);

        for (i, inst) in low.iter().enumerate() {
            if i < 10 {
                assert!(inst.pos_scale[3] > 0.0, "instance {i} below count should be live");
            } else {
                assert_eq!(
                    inst.pos_scale[3], 0.0,
                    "slot {i} at/beyond count must park at zero scale, got {:?}",
                    inst.pos_scale
                );
            }
        }
    }

    /// BUG found by the Scene 2 look-dev (2026-07-11): on NEAR-FLAT faces —
    /// the common terrain case — align_to_normal produced rotations that
    /// made ~98% of instances vanish from the render (BlossomField showed
    /// ~25 of 420 flowers; disabling align carpeted the field). Flat ground
    /// with align ON must behave like align OFF: every instance finite and
    /// upright (R·(0,1,0) ≈ (0,1,0)) for EVERY yaw the hash produces.
    #[test]
    fn align_on_flat_ground_keeps_instances_upright_and_finite() {
        let device = crate::test_device();
        let wgsl = include_str!("shaders/scatter_on_mesh.wgsl");
        let vertices = quad_mesh();
        let capacity = 256u32;

        let instances = dispatch_scatter(&device, wgsl, &vertices, capacity, capacity, 11, 1.0, 1.0, true);

        for (i, inst) in instances.iter().enumerate() {
            for (k, v) in inst.pos_scale.iter().chain(inst.rot_pad.iter()).enumerate() {
                assert!(v.is_finite(), "instance {i} field {k} is not finite: {v}");
            }
            let [rx, ry, rz, _] = inst.rot_pad;
            let (cx, sx) = (rx.cos(), rx.sin());
            let (cy, sy) = (ry.cos(), ry.sin());
            let (cz, sz) = (rz.cos(), rz.sin());
            let mapped_up = [
                -cz * sy * sx - sz * cx,
                -sz * sy * sx + cz * cx,
                cy * sx,
            ];
            assert!(
                (mapped_up[0].abs() < 1e-3) && ((mapped_up[1] - 1.0).abs() < 1e-3) && (mapped_up[2].abs() < 1e-3),
                "instance {i}: flat ground must keep instances upright, got up={mapped_up:?} from euler=({rx}, {ry}, {rz})"
            );
        }
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
        let device = crate::test_device();
        ScatterOnMesh::prewarm_pipelines(&device);
        const SHADER_SRC: &str = include_str!("shaders/scatter_on_mesh.wgsl");
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
