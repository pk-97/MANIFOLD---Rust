//! `node.smooth_normals` — welded, angle-weighted vertex normals for an
//! `Array<MeshVertex>` flat triangle list.
//!
//! A triangle list stores a hard corner once per face, each copy with its own
//! face's normal (a built-in cube's corner is three vertices). Pushing along
//! those normals pulls the copies apart and the mesh cracks along its edges.
//! This node gives every copy of one position the same normal: the sum of the
//! face normals meeting there, each weighted by its corner angle (Blender's
//! vertex normal). Positions, uv and colour pass through.
//!
//! Three passes over one hash table keyed by the exact position bits (clear,
//! accumulate with atomics, resolve), so it is a barriered reduction, not a
//! fusable per-element atom (docs/ADDING_PRIMITIVES.md exclusion 1).

use manifold_gpu::BufferUse;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::primitive::Primitive;

const SHADER: &str = include_str!("shaders/smooth_normals.wgsl");

/// Bytes per hash slot: an owner word and three fixed-point sums.
const SLOT_BYTES: u64 = 16;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SmoothUniforms {
    count: u32,
    slots: u32,
    _pad0: u32,
    _pad1: u32,
}

/// The hash table and the three pass pipelines, grown with the mesh.
#[derive(Default)]
pub struct SmoothState {
    table: Option<GpuBuffer>,
    slots: u32,
    clear: Option<GpuComputePipeline>,
    accumulate: Option<GpuComputePipeline>,
    resolve: Option<GpuComputePipeline>,
}

manifold_node_engine::primitive! {
    name: SmoothNormals,
    type_id: "node.smooth_normals",
    purpose: "Replace each vertex normal of an Array<MeshVertex> flat triangle list with the welded vertex normal: the sum of the face normals of every triangle corner at exactly that position, each weighted by its corner angle, normalized (Blender's vertex normal). Every copy of a split corner gets the same normal, so a push along normals keeps a hard-edged mesh closed. Positions, uv, tangent and colour pass through; a vertex of a degenerate or trailing partial triangle keeps its own normal.",
    inputs: {
        in: Array(MeshVertex) required,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Feed node.push_along_normals's `direction` input with this node's output to inflate or displace a built-in cube or any flat-shaded mesh without cracking it open, while the pushed mesh keeps its own shading normals. Wire it as the main mesh instead to smooth-shade a faceted mesh. Positions must match bit for bit to weld, which split copies from one source always do.",
    examples: [],
    picker: { label: "Smooth Normals", category: Atom },
    summary: "Gives every corner of a mesh one shared, smooth direction, so pushing the surface outward keeps hard edges joined instead of splitting the faces apart.",
    category: Geometry3D,
    role: Filter,
    aliases: ["smooth normals", "weld normals", "vertex normals", "average normals"],
    boundary_reason: BarrieredReduction,
    extra_fields: { state: SmoothState = SmoothState::default() },
}

fn pipeline<'a>(
    slot: &'a mut Option<GpuComputePipeline>,
    device: &manifold_gpu::GpuDevice,
    entry: &str,
    label: &str,
) -> &'a GpuComputePipeline {
    slot.get_or_insert_with(|| device.create_compute_pipeline(SHADER, entry, label))
}

impl Primitive for SmoothNormals {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "out")
            .then(|| {
                input_capacities
                    .iter()
                    .find(|(p, _)| *p == "in")
                    .map(|(_, n)| *n)
            })
            .flatten()
    }

    /// Only normals are written: topology and positions track `in`.
    fn mesh_output_rule(
        &self,
        port: &str,
    ) -> manifold_node_engine::scene::mesh_change::MeshOutputRule<'_> {
        use manifold_node_engine::scene::mesh_change::{
            MeshAspect, MeshDependency, MeshOutputRule, MeshRevisionRule,
        };
        if port == "out" {
            return MeshOutputRule {
                topology: MeshRevisionRule::Dependencies(&[MeshDependency {
                    input: std::borrow::Cow::Borrowed("in"),
                    aspect: MeshAspect::Topology,
                }]),
                positions: MeshRevisionRule::Dependencies(&[MeshDependency {
                    input: std::borrow::Cow::Borrowed("in"),
                    aspect: MeshAspect::Positions,
                }]),
            };
        }
        MeshOutputRule {
            topology: MeshRevisionRule::Written,
            positions: MeshRevisionRule::Written,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let count = ((src.size / vertex_size) as u32).min((dst.size / vertex_size) as u32);
        if count == 0 {
            return;
        }
        // Half full at most, so probes stay short; grows with the mesh.
        let slots = (count.saturating_mul(2)).max(64).next_power_of_two();
        let gpu = ctx.gpu_encoder();
        let state = &mut self.state;
        if state.slots < slots || state.table.is_none() {
            state.table = Some(
                gpu.device
                    .create_buffer(u64::from(slots) * SLOT_BYTES, BufferUse::Gpu),
            );
            state.slots = slots;
        }
        let table = state
            .table
            .as_ref()
            .expect("smooth-normals table allocated");
        let uniforms = SmoothUniforms {
            count,
            slots: state.slots,
            _pad0: 0,
            _pad1: 0,
        };
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: src,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: table,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: dst,
                offset: 0,
            },
        ];
        let clear = pipeline(
            &mut state.clear,
            gpu.device,
            "clear_main",
            "node.smooth_normals.clear",
        );
        gpu.native_enc.dispatch_compute(
            clear,
            &bindings,
            [state.slots.div_ceil(256), 1, 1],
            "node.smooth_normals.clear",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
        let accumulate = pipeline(
            &mut state.accumulate,
            gpu.device,
            "accumulate_main",
            "node.smooth_normals.accumulate",
        );
        gpu.native_enc.dispatch_compute(
            accumulate,
            &bindings,
            [count.div_ceil(256), 1, 1],
            "node.smooth_normals.accumulate",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
        let resolve = pipeline(
            &mut state.resolve,
            gpu.device,
            "resolve_main",
            "node.smooth_normals.resolve",
        );
        gpu.native_enc.dispatch_compute(
            resolve,
            &bindings,
            [count.div_ceil(256), 1, 1],
            "node.smooth_normals.resolve",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level proof: the three passes, dispatched as `run`
    //! dispatches them, against Blender's angle-weighted vertex normal
    //! computed on the CPU.
    use super::*;

    fn vertex(position: [f32; 3], normal: [f32; 3]) -> MeshVertex {
        MeshVertex {
            position,
            normal,
            uv: [position[0], position[2]],
            color: [1.0; 4],
            ..bytemuck::Zeroable::zeroed()
        }
    }

    fn sub(a: [f32; 3], b: [f32; 3]) -> [f64; 3] {
        std::array::from_fn(|k| f64::from(a[k]) - f64::from(b[k]))
    }

    fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    fn length(a: [f64; 3]) -> f64 {
        dot(a, a).sqrt()
    }

    /// The formula: every copy of one position gets the normalized sum of
    /// the face normals meeting there, each facing the way that corner's own
    /// normal does and weighted by its corner angle. A position no whole,
    /// non-degenerate triangle reaches keeps its own normal.
    fn expected(src: &[MeshVertex]) -> Vec<[f32; 3]> {
        let mut sums: Vec<([f32; 3], [f64; 3])> = Vec::new();
        for i in 0..src.len() {
            let base = i / 3 * 3;
            if base + 2 >= src.len() {
                continue;
            }
            let p = src[i].position;
            let a = sub(src[base + (i - base + 1) % 3].position, p);
            let b = sub(src[base + (i - base + 2) % 3].position, p);
            let face = cross(
                sub(src[base + 1].position, src[base].position),
                sub(src[base + 2].position, src[base].position),
            );
            let (area, la, lb) = (length(face), length(a), length(b));
            if area <= 1e-12 || la <= 1e-12 || lb <= 1e-12 {
                continue;
            }
            let own: [f64; 3] = std::array::from_fn(|k| f64::from(src[i].normal[k]));
            let sign = if dot(face, own) < 0.0 { -1.0 } else { 1.0 };
            let angle = (dot(a, b) / (la * lb)).clamp(-1.0, 1.0).acos();
            let add: [f64; 3] = std::array::from_fn(|k| sign * face[k] / area * angle);
            match sums.iter_mut().find(|(at, _)| *at == p) {
                Some((_, sum)) => (0..3).for_each(|k| sum[k] += add[k]),
                None => sums.push((p, add)),
            }
        }
        src.iter()
            .map(|v| match sums.iter().find(|(at, _)| *at == v.position) {
                Some((_, sum)) => std::array::from_fn(|k| (sum[k] / length(*sum)) as f32),
                None => v.normal,
            })
            .collect()
    }

    fn dispatch(src: &[MeshVertex]) -> Vec<MeshVertex> {
        let device = manifold_gpu::testkit::test_device();
        let count = src.len() as u32;
        let slots = (count * 2).max(64).next_power_of_two();
        let input = device.create_buffer(std::mem::size_of_val(src) as u64, BufferUse::Upload);
        unsafe {
            input.write(0, bytemuck::cast_slice(src));
        }
        let table = device.create_buffer(u64::from(slots) * SLOT_BYTES, BufferUse::Gpu);
        let output = device.create_buffer(std::mem::size_of_val(src) as u64, BufferUse::Readback);
        let uniforms = SmoothUniforms {
            count,
            slots,
            _pad0: 0,
            _pad1: 0,
        };
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &input,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &table,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &output,
                offset: 0,
            },
        ];
        let mut enc = device.create_encoder("smooth-normals-test");
        for (entry, threads) in [
            ("clear_main", slots),
            ("accumulate_main", count),
            ("resolve_main", count),
        ] {
            let pipeline = device.create_compute_pipeline(SHADER, entry, "smooth-normals-test");
            enc.dispatch_compute(
                &pipeline,
                &bindings,
                [threads.div_ceil(256), 1, 1],
                "smooth-normals-test",
            );
            enc.compute_memory_barrier_buffers();
        }
        enc.commit_and_wait().expect("GPU work completes");
        let ptr = output.mapped_ptr().expect("shared output buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, src.len()) }.to_vec()
    }

    #[test]
    fn split_corners_share_the_angle_weighted_normal() {
        let (o, x, y, z) = (
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        );
        let src = vec![
            // A cube's corner: three faces meet at the origin, each corner a
            // split copy with its own face's normal.
            vertex(o, [0.0, 0.0, -1.0]),
            vertex(y, [0.0, 0.0, -1.0]),
            vertex(x, [0.0, 0.0, -1.0]),
            vertex(o, [-1.0, 0.0, 0.0]),
            vertex(z, [-1.0, 0.0, 0.0]),
            vertex(y, [-1.0, 0.0, 0.0]),
            vertex(o, [0.0, -1.0, 0.0]),
            vertex(x, [0.0, -1.0, 0.0]),
            vertex(z, [0.0, -1.0, 0.0]),
            // A degenerate triangle keeps its own normals.
            vertex([3.0, 3.0, 3.0], [0.0, 1.0, 0.0]),
            vertex([3.0, 3.0, 3.0], [0.0, 1.0, 0.0]),
            vertex([4.0, 3.0, 3.0], [0.0, 1.0, 0.0]),
            // A trailing partial triangle passes through.
            vertex([9.0, 9.0, 9.0], [1.0, 0.0, 0.0]),
        ];
        let got = dispatch(&src);
        let want = expected(&src);
        for (i, (got, want)) in got.iter().zip(&want).enumerate() {
            for k in 0..3 {
                assert!(
                    (got.normal[k] - want[k]).abs() < 1e-3,
                    "vertex {i} normal {:?}, want {want:?}",
                    got.normal
                );
            }
            assert_eq!(
                got.position, src[i].position,
                "vertex {i} keeps its position"
            );
            assert_eq!(got.uv, src[i].uv, "vertex {i} keeps its uv");
        }
        let corner = -1.0 / 3f32.sqrt();
        for i in [0, 3, 6] {
            assert!(
                got[i].normal.iter().all(|n| (n - corner).abs() < 1e-3),
                "the corner's three copies weld: {:?}",
                got[i].normal
            );
        }
    }
}
