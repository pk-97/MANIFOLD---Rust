//! `node.spawn_from_mesh` — seed particles from a mesh's own geometry so
//! an imported/procedural model can dissolve or explode into the existing
//! 3D particle stack (`node.spawn_from_image` → this, sourced from
//! `Array(MeshVertex)` instead of a `Texture2D`).
//!
//! Two modes via the `mode` enum-param:
//!
//! - **vertices** — one particle per vertex: particle `i` gets
//!   `vertices[i].position` for `i` up to `min(vertex_count, active_count,
//!   capacity)`. Exact silhouette. Single-pass dispatch.
//! - **surface** — area-weighted random triangle sampling, so particle
//!   density is uniform across the surface regardless of triangulation.
//!   Three-pass dispatch (mirrors the precedent's deterministic-scan
//!   shape, minus the atomics — a cumulative-area table has nothing to
//!   race on):
//!     1. **area** — per-triangle area, one thread per triangle.
//!     2. **scan** — single-thread inclusive prefix sum over the areas
//!        (last entry = total surface area).
//!     3. **place** — for each active particle, draw a uniform value
//!        over `[0, total)`, binary-search the cumulative table for the
//!        triangle it lands in, barycentric-sample a point inside it.
//!
//! Positions are emitted in the mesh's LOCAL space — no transform is
//! applied here, matching the mesh itself (a transform upstream of the
//! renderer applies later).
//!
//! Vertices are read as flat triangle-list triples: triangle `t` reads
//! `vertices[t*3 .. t*3+3]`. `triangle_count = vertex_count / 3` (a
//! trailing partial triangle is ignored, floor division).
//!
//! Respects the same `reset_trigger` recompute gate as
//! `seed_particles_from_texture.rs` — seeding happens once per trigger
//! edge (+ the first frame), never every frame.

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const SPAWN_MODES: &[&str] = &["vertices", "surface"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SpawnFromMeshUniforms {
    mode: u32,
    active_count: u32,
    frame_seed: u32,
    vertex_count: u32,
    triangle_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: SpawnFromMesh,
    type_id: "node.spawn_from_mesh",
    purpose: "Seed particles from a mesh's own geometry (Array<MeshVertex>) so an imported or procedural model can dissolve/explode into the existing 3D particle stack. `vertices` mode: one particle per vertex, exact silhouette. `surface` mode: area-weighted random triangle sampling for uniform surface density regardless of triangulation (three-pass dispatch: per-triangle area, prefix-sum scan, barycentric place). Positions are emitted in the mesh's LOCAL space, same convention as the mesh itself — an upstream transform applies later. Pair with node.apply_radial_burst_3d_to_particles + node.euler_step_particles_3d to blow the seeded cloud apart, crossfading the intact mesh render out as the particles render in.",
    inputs: {
        vertices: Array(MeshVertex) required,
        active_count: ScalarF32 optional,
        frame_seed: ScalarF32 optional,
        // Optional execution gate: when wired, the seed only RECOMPUTES on this
        // value's integer edges (+ the first frame). Wire it from the same
        // trigger that drives the downstream node.array_feedback's reset —
        // between resets the multi-pass surface sampling is pure waste.
        // Unwired → recompute every frame (direct per-frame seeding).
        reset_trigger: ScalarF32 optional,
    },
    outputs: {
        particles: Array(Particle),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(1_048_576.0),
            range: Some((1024.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("active_count"),
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(100_000.0),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("frame_seed"),
            label: "Frame Seed",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0), // vertices
            range: Some((0.0, (SPAWN_MODES.len() - 1) as f32)),
            enum_values: SPAWN_MODES,
        },
    ],
    depth_rule: Terminal,
    composition_notes: "vertices mode gives an exact silhouette (one particle per vertex) — best for meshes whose vertex density already reads as a point cloud (imported scans, dense procedural geometry). surface mode gives uniform density independent of triangulation — best for low-poly meshes where per-vertex seeding would visibly clump at dense corners and leave large flat faces empty. active_count / frame_seed are port-shadows-param — wire from system.generator_input or a math chain to drive them live. Triangles are read as flat [v0,v1,v2] triples from the vertices array (standard triangle-list layout, matching node.cube_mesh / node.render_mesh's expected input); a trailing partial triangle (vertex_count % 3 != 0) is ignored. Internal per-triangle cumulative-area scratch is sized to vertex_count/3 elements; reallocs when vertex_count changes.",
    examples: [],
    picker: { label: "Spawn From Mesh", category: Atom },
    summary: "Creates particles from a mesh's own geometry — one per vertex for an exact silhouette, or scattered evenly across its surface. The way an imported model dissolves into particles.",
    category: Particles3D,
    role: Source,
    aliases: ["spawn from mesh", "seed particles from mesh", "mesh explode", "mesh dissolve", "mesh particles"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        // The macro-allocated `pipeline` field holds vertices_main; these hold
        // the other three entry points of the surface-mode three-pass dispatch.
        area_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        scan_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        place_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        // Per-triangle cumulative-area scratch (surface mode only; harmlessly
        // allocated-but-unused in vertices mode). Reallocated when the mesh's
        // triangle count changes.
        cumulative: Option<manifold_gpu::GpuBuffer> = None,
        cached_triangle_count: u32 = 0,
        // Last observed `reset_trigger` integer, for edge-gated recompute.
        // `None` until the first frame (which always recomputes).
        last_reset_trigger: Option<i32> = None
    },
}

impl Primitive for SpawnFromMesh {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let mode_idx = match ctx.params.get("mode") {
            Some(ParamValue::Enum(v)) => (*v).min((SPAWN_MODES.len() - 1) as u32),
            Some(ParamValue::Float(f)) => {
                f.round().clamp(0.0, (SPAWN_MODES.len() - 1) as f32) as u32
            }
            _ => 0,
        };
        let active_count_param = ctx
            .scalar_or_param("active_count", 100_000.0)
            .round()
            .max(0.0) as u32;
        let frame_seed = ctx.scalar_or_param("frame_seed", 0.0).round() as u32;

        // Execution gate (see the `reset_trigger` input): when wired, recompute
        // the seed only on the trigger's integer edges (+ the first frame).
        // Cheap edge check before any allocation or dispatch.
        if let Some(ParamValue::Float(v)) = ctx.inputs.scalar("reset_trigger") {
            let current = v.round() as i32;
            let edge = self.last_reset_trigger != Some(current);
            self.last_reset_trigger = Some(current);
            if !edge {
                return;
            }
        }

        let Some(src) = ctx.inputs.array("vertices") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("particles") else {
            return;
        };

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let particle_size = std::mem::size_of::<Particle>() as u64;
        let vertex_count = (src.size / vertex_size) as u32;
        let capacity = (out_buf.size / particle_size) as u32;
        if capacity == 0 {
            return;
        }
        let active_count = active_count_param.min(capacity);
        let triangle_count = vertex_count / 3;

        let gpu = ctx.gpu_encoder();

        // Lazy-compile all four entry points from the shared shader source.
        // `self.pipeline` (macro-provided) holds vertices_main.
        const SHADER_SRC: &str = include_str!("shaders/spawn_from_mesh.wgsl");
        if self.pipeline.is_none() {
            self.pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER_SRC,
                "vertices_main",
                "node.spawn_from_mesh.vertices",
            ));
        }
        if self.area_pipeline.is_none() {
            self.area_pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER_SRC,
                "area_main",
                "node.spawn_from_mesh.area",
            ));
        }
        if self.scan_pipeline.is_none() {
            self.scan_pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER_SRC,
                "scan_main",
                "node.spawn_from_mesh.scan",
            ));
        }
        if self.place_pipeline.is_none() {
            self.place_pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER_SRC,
                "place_main",
                "node.spawn_from_mesh.place",
            ));
        }

        // Lazy-allocate / realloc the per-triangle cumulative-area scratch.
        // Sized to `triangle_count` elements (min 1 so a zero-triangle mesh
        // still gets a valid binding); unused by vertices_main but always
        // bound (same "bind everything, each pass uses its subset" shape as
        // seed_particles_from_texture.rs).
        let needs_alloc =
            self.cumulative.is_none() || self.cached_triangle_count != triangle_count;
        if needs_alloc {
            let bytes = u64::from(triangle_count.max(1)) * 4;
            self.cumulative = Some(gpu.device.create_buffer(bytes));
            self.cached_triangle_count = triangle_count;
        }
        let cumulative = self.cumulative.as_ref().expect("cumulative just allocated");

        let vertices_pipeline = self.pipeline.as_ref().expect("just inserted");
        let area_pipeline = self.area_pipeline.as_ref().expect("just inserted");
        let scan_pipeline = self.scan_pipeline.as_ref().expect("just inserted");
        let place_pipeline = self.place_pipeline.as_ref().expect("just inserted");

        let uniforms = SpawnFromMeshUniforms {
            mode: mode_idx,
            active_count,
            frame_seed,
            vertex_count,
            triangle_count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
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

        if mode_idx == 1 {
            // Surface mode: area → scan → place, each pass reads what the
            // previous wrote, so barrier between every stage.
            if triangle_count > 0 {
                gpu.native_enc.dispatch_compute(
                    area_pipeline,
                    &bindings,
                    [triangle_count.div_ceil(64), 1, 1],
                    "node.spawn_from_mesh.area",
                );
                gpu.native_enc.compute_memory_barrier_buffers();

                gpu.native_enc.dispatch_compute(
                    scan_pipeline,
                    &bindings,
                    [1, 1, 1],
                    "node.spawn_from_mesh.scan",
                );
                gpu.native_enc.compute_memory_barrier_buffers();
            }
            // place_main guards internally on triangle_count == 0 (parks every
            // particle dead) so it always runs, even for a degenerate mesh.
            gpu.native_enc.dispatch_compute(
                place_pipeline,
                &bindings,
                [active_count.div_ceil(256), 1, 1],
                "node.spawn_from_mesh.place",
            );
        } else {
            gpu.native_enc.dispatch_compute(
                vertices_pipeline,
                &bindings,
                [active_count.div_ceil(256), 1, 1],
                "node.spawn_from_mesh.vertices",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn spawn_from_mesh_declares_mesh_in_and_particle_out() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(SpawnFromMesh::TYPE_ID, "node.spawn_from_mesh");

        let vertices = SpawnFromMesh::INPUTS
            .iter()
            .find(|p| p.name == "vertices")
            .expect("vertices input");
        assert_eq!(vertices.ty, PortType::Array(mesh_layout));
        assert!(vertices.required);

        for name in ["active_count", "frame_seed", "reset_trigger"] {
            let port = SpawnFromMesh::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow"));
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
            assert!(!port.required, "{name} should be optional (port-shadow)");
        }

        assert_eq!(SpawnFromMesh::OUTPUTS.len(), 1);
        assert_eq!(SpawnFromMesh::OUTPUTS[0].name, "particles");
        assert_eq!(SpawnFromMesh::OUTPUTS[0].ty, PortType::Array(particle_layout));
    }

    #[test]
    fn spawn_from_mesh_has_full_param_surface_with_mode_enum() {
        let names: Vec<&str> = SpawnFromMesh::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["max_capacity", "active_count", "frame_seed", "mode"]
        );
        let mode = SpawnFromMesh::PARAMS.iter().find(|p| p.name == "mode").unwrap();
        assert_eq!(mode.ty, ParamType::Enum);
        assert_eq!(mode.enum_values, SPAWN_MODES);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SpawnFromMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.spawn_from_mesh");
    }

    #[test]
    fn declares_optional_reset_trigger_gate_input() {
        use crate::node_graph::ports::{PortType, ScalarType};
        let rt = SpawnFromMesh::INPUTS
            .iter()
            .find(|p| p.name == "reset_trigger")
            .expect("reset_trigger input");
        assert_eq!(rt.ty, PortType::Scalar(ScalarType::F32));
        assert!(
            !rt.required,
            "reset_trigger is optional (unwired ⇒ recompute every frame)"
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level tests. Multi-pass boundary primitive (like
    //! seed_particles_from_texture.rs) — no freeze/fusion codegen exists to
    //! compare against, so these dispatch the hand-written WGSL entry points
    //! directly (same shape as generate_cube_mesh.rs's `dispatch_cube` /
    //! apply_radial_burst_3d_to_particles.rs's `dispatch_burst3d` helpers),
    //! rather than driving `Primitive::run` through a mock backend.
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
    fn dispatch_spawn(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        vertices: &[MeshVertex],
        active_count: u32,
        capacity: u32,
        mode: u32,
        frame_seed: u32,
    ) -> Vec<Particle> {
        let vertex_count = vertices.len() as u32;
        let triangle_count = vertex_count / 3;

        let vbuf = device.create_buffer_shared(std::mem::size_of_val(vertices) as u64);
        unsafe {
            vbuf.write(0, bytemuck::cast_slice(vertices));
        }
        let pbuf = device.create_buffer_shared(capacity as u64 * std::mem::size_of::<Particle>() as u64);
        let cumulative = device.create_buffer_shared(u64::from(triangle_count.max(1)) * 4);

        let uniforms = SpawnFromMeshUniforms {
            mode,
            active_count,
            frame_seed,
            vertex_count,
            triangle_count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        let bindings = [
            GpuBinding::Buffer { binding: 0, buffer: &pbuf, offset: 0 },
            GpuBinding::Bytes { binding: 1, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 2, buffer: &vbuf, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &cumulative, offset: 0 },
        ];

        let mut enc = device.create_encoder("spawn-from-mesh-test");
        if mode == 1 {
            if triangle_count > 0 {
                let area = device.create_compute_pipeline(wgsl, "area_main", "spawn-area-test");
                enc.dispatch_compute(&area, &bindings, [triangle_count.div_ceil(64), 1, 1], "area");
                enc.compute_memory_barrier_buffers();
                let scan = device.create_compute_pipeline(wgsl, "scan_main", "spawn-scan-test");
                enc.dispatch_compute(&scan, &bindings, [1, 1, 1], "scan");
                enc.compute_memory_barrier_buffers();
            }
            let place = device.create_compute_pipeline(wgsl, "place_main", "spawn-place-test");
            enc.dispatch_compute(&place, &bindings, [active_count.div_ceil(256), 1, 1], "place");
        } else {
            let verts_pipeline =
                device.create_compute_pipeline(wgsl, "vertices_main", "spawn-vertices-test");
            enc.dispatch_compute(
                &verts_pipeline,
                &bindings,
                [active_count.div_ceil(256), 1, 1],
                "vertices",
            );
        }
        enc.commit_and_wait_completed();

        let ptr = pbuf.mapped_ptr().expect("shared particle buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const Particle, capacity as usize) }.to_vec()
    }

    #[test]
    fn surface_mode_samples_stay_on_the_single_triangle() {
        let device = crate::test_device();
        let wgsl = include_str!("shaders/spawn_from_mesh.wgsl");

        // One right triangle in the z=0 plane: v0=(0,0,0), v1=(4,0,0), v2=(0,3,0).
        let v0 = [0.0f32, 0.0, 0.0];
        let v1 = [4.0f32, 0.0, 0.0];
        let v2 = [0.0f32, 3.0, 0.0];
        let vertices = vec![mk_vertex(v0), mk_vertex(v1), mk_vertex(v2)];

        let capacity = 256u32;
        let particles = dispatch_spawn(&device, wgsl, &vertices, capacity, capacity, 1, 42);

        // Plane normal: the triangle lies in z=0, so normal is +Z (or -Z);
        // any point with position.z == 0 satisfies the plane equation here.
        let normal = {
            let e1 = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
            let e2 = [v2[0] - v0[0], v2[1] - v0[1], v2[2] - v0[2]];
            [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ]
        };

        for (i, p) in particles.iter().enumerate() {
            assert_eq!(p.life, 1.0, "particle {i} should be alive");

            // Plane equation: dot(normal, pos - v0) ≈ 0.
            let d = [
                p.position[0] - v0[0],
                p.position[1] - v0[1],
                p.position[2] - v0[2],
            ];
            let plane_dist = normal[0] * d[0] + normal[1] * d[1] + normal[2] * d[2];
            assert!(
                plane_dist.abs() < 1e-4,
                "particle {i} at {:?} off the triangle's plane (dist {plane_dist})",
                p.position
            );

            // Barycentric bounds: solve pos = v0 + u*(v1-v0) + v*(v2-v0) for this
            // axis-aligned right triangle (v1-v0 along +X, v2-v0 along +Y), so
            // u = d.x / 4, v = d.y / 3 directly.
            let u = d[0] / 4.0;
            let v = d[1] / 3.0;
            assert!(u >= -1e-4 && v >= -1e-4 && u + v <= 1.0 + 1e-4,
                "particle {i} barycentric (u={u}, v={v}) outside the triangle");
        }
    }

    #[test]
    fn vertices_mode_on_cube_dedups_to_eight_corners() {
        let device = crate::test_device();
        let wgsl = include_str!("shaders/spawn_from_mesh.wgsl");

        // Same 36 triangle-list positions node.cube_mesh emits (unit cube,
        // size = 1.0) — 6 faces × 2 triangles × 3 vertices, 8 distinct corners.
        const CUBE_POSITIONS: [[f32; 3]; 36] = [
            // Front face (+Z)
            [-0.5, -0.5, 0.5], [0.5, -0.5, 0.5], [0.5, 0.5, 0.5],
            [-0.5, -0.5, 0.5], [0.5, 0.5, 0.5], [-0.5, 0.5, 0.5],
            // Back face (-Z)
            [0.5, -0.5, -0.5], [-0.5, -0.5, -0.5], [-0.5, 0.5, -0.5],
            [0.5, -0.5, -0.5], [-0.5, 0.5, -0.5], [0.5, 0.5, -0.5],
            // Right face (+X)
            [0.5, -0.5, 0.5], [0.5, -0.5, -0.5], [0.5, 0.5, -0.5],
            [0.5, -0.5, 0.5], [0.5, 0.5, -0.5], [0.5, 0.5, 0.5],
            // Left face (-X)
            [-0.5, -0.5, -0.5], [-0.5, -0.5, 0.5], [-0.5, 0.5, 0.5],
            [-0.5, -0.5, -0.5], [-0.5, 0.5, 0.5], [-0.5, 0.5, -0.5],
            // Top face (+Y)
            [-0.5, 0.5, 0.5], [0.5, 0.5, 0.5], [0.5, 0.5, -0.5],
            [-0.5, 0.5, 0.5], [0.5, 0.5, -0.5], [-0.5, 0.5, -0.5],
            // Bottom face (-Y)
            [-0.5, -0.5, -0.5], [0.5, -0.5, -0.5], [0.5, -0.5, 0.5],
            [-0.5, -0.5, -0.5], [0.5, -0.5, 0.5], [-0.5, -0.5, 0.5],
        ];
        let vertices: Vec<MeshVertex> = CUBE_POSITIONS.iter().map(|p| mk_vertex(*p)).collect();

        let capacity = 64u32;
        let particles = dispatch_spawn(&device, wgsl, &vertices, 36, capacity, 0, 0);

        // First 36 particles alive, one per vertex; rest parked dead.
        let mut distinct: Vec<[f32; 3]> = Vec::new();
        for (i, p) in particles.iter().enumerate() {
            if i < 36 {
                assert_eq!(p.life, 1.0, "particle {i} should be alive");
                if !distinct
                    .iter()
                    .any(|q| (q[0] - p.position[0]).abs() < 1e-6
                        && (q[1] - p.position[1]).abs() < 1e-6
                        && (q[2] - p.position[2]).abs() < 1e-6)
                {
                    distinct.push(p.position);
                }
            } else {
                assert_eq!(p.life, 0.0, "particle {i} past vertex_count should be parked dead");
            }
        }

        assert_eq!(distinct.len(), 8, "cube corners should dedup to exactly 8 distinct positions");
        assert!(distinct.len() <= capacity as usize);
    }
}
