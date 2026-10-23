//! `node.merge_meshes` — several meshes, each placed by its own transform,
//! as one triangle-list mesh in a shared frame.
//!
//! `parts` sets how many `mesh_k` / `place_k` / `within_k` triples the node
//! exposes, with no cap: a Group of any size merges whole. Each part's
//! vertices land at
//! `(frame_within × frame)⁻¹ × within_k × place_k` (each unwired one is
//! identity): a part's transform inside the frame it is drawn in, as a scene
//! object takes its transform and parent transform, brought into `frame`,
//! itself placed inside `frame_within`. A Group's parts sit where they sit
//! inside the group. The output holds every part back to
//! back, in port order; positions and normals are placed, UVs and colours
//! copied.
//!
//! A swarm emitting from a Group seeds from this mesh, so every part emits
//! at its own place. Wired to the seed's `reset_trigger`, it merges only on
//! that value's integer edges (and the first frame), as the seed reads it.
//!
//! With one part it carries a mesh between spaces: a modifier frame brings an
//! imported part into the frame (place and within the part's transforms, frame
//! the frame) and back out (place the frame, frame and frame_within the
//! part's), so the part bends in the frame's axes and units.
//!
//! Hand-written because the port list follows `parts` (`node.camera_switch`
//! pattern); one dispatch per part of one copy kernel.

use std::borrow::Cow;
use std::sync::OnceLock;

use manifold_gpu::{GpuBinding, GpuComputePipeline};
use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType, ParamValues};
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::ports::{ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use manifold_node_engine::scene::mesh_change::{MeshAspect, MeshDependency, MeshOutputRule, MeshRevisionRule};
use manifold_node_engine::scene::transform::Mat4;

use crate::node_graph::gltf_load::{MAT4_IDENTITY, cross3, mat4_mul};

pub const MERGE_MESHES_TYPE_ID: &str = "node.merge_meshes";

const DEFAULT_PARTS: usize = 2;
const SHADER: &str = include_str!("shaders/merge_meshes.wgsl");
const VERTEX_BYTES: u64 = std::mem::size_of::<MeshVertex>() as u64;

/// Ports before the part triples.
const FIXED_INPUTS: usize = 3;
/// Ports per part: mesh, place, within.
const PART_PORTS: usize = 3;

fn mesh_type() -> PortType {
    PortType::Array(ArrayType::of_known::<MeshVertex>())
}

static OUTPUTS: OnceLock<[NodeOutput; 1]> = OnceLock::new();

fn outputs() -> &'static [NodeOutput; 1] {
    OUTPUTS.get_or_init(|| [NodePort { name: Cow::Borrowed("vertices"), ty: mesh_type(), kind: PortKind::Output, required: false }])
}

const PARAMS: [ParamDef; 1] = [ParamDef {
    name: Cow::Borrowed("parts"),
    label: "Parts",
    ty: ParamType::Int,
    default: ParamValue::Float(DEFAULT_PARTS as f32),
    // The slider's reach; typed values go past it.
    range: Some((1.0, 64.0)),
    enum_values: &[],
}];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PartUniforms {
    matrix: [[f32; 4]; 4],
    normals: [[f32; 4]; 3],
    offset: u32,
    count: u32,
    clear: u32,
    _pad: u32,
}

pub struct MergeMeshes {
    inputs: Vec<NodeInput>,
    /// Each mesh_k's topology: the merged triangles change only when a
    /// part's do (a moved transform moves positions, never triangles).
    topology: Vec<MeshDependency>,
    pipeline: Option<GpuComputePipeline>,
    /// Last `reset_trigger` integer seen; `None` until the first frame.
    last_reset_trigger: Option<i32>,
}

impl MergeMeshes {
    pub fn new() -> Self {
        let mut node = Self { inputs: Vec::new(), topology: Vec::new(), pipeline: None, last_reset_trigger: None };
        node.rebuild_ports(DEFAULT_PARTS);
        node
    }

    fn rebuild_ports(&mut self, parts: usize) {
        let port = |name: Cow<'static, str>, ty| NodePort { name, ty, kind: PortKind::Input, required: false };
        self.inputs = vec![
            port(Cow::Borrowed("frame"), PortType::Transform),
            port(Cow::Borrowed("frame_within"), PortType::Transform),
            port(Cow::Borrowed("reset_trigger"), PortType::Scalar(ScalarType::F32)),
        ];
        for k in 0..parts.max(1) {
            self.inputs.push(port(Cow::Owned(format!("mesh_{k}")), mesh_type()));
            self.inputs.push(port(Cow::Owned(format!("place_{k}")), PortType::Transform));
            self.inputs.push(port(Cow::Owned(format!("within_{k}")), PortType::Transform));
        }
        self.topology = (0..parts.max(1))
            .map(|k| MeshDependency { input: Cow::Owned(format!("mesh_{k}")), aspect: MeshAspect::Topology })
            .collect();
    }

    fn parts(&self) -> usize {
        (self.inputs.len() - FIXED_INPUTS) / PART_PORTS
    }
}

impl Default for MergeMeshes {
    fn default() -> Self {
        Self::new()
    }
}

fn cached_type_id() -> &'static EffectNodeType {
    static CELL: OnceLock<EffectNodeType> = OnceLock::new();
    CELL.get_or_init(|| EffectNodeType::new(MERGE_MESHES_TYPE_ID))
}

fn column(m: &Mat4, c: usize) -> [f32; 3] {
    [m[c][0], m[c][1], m[c][2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The rows of the inverse of `m`'s linear part, or `None` when it flattens
/// an axis.
fn inverse_rows(m: &Mat4) -> Option<[[f32; 3]; 3]> {
    let [a, b, c] = [0, 1, 2].map(|i| column(m, i));
    let rows = [cross3(b, c), cross3(c, a), cross3(a, b)];
    let det = dot(a, rows[0]);
    let size = [a, b, c].iter().map(|v| dot(*v, *v).sqrt()).product::<f32>();
    (det.is_finite() && det.abs() > 1e-6 * size).then(|| rows.map(|row| row.map(|v| v / det)))
}

/// The inverse of an affine `m`, or `None` when it flattens an axis.
fn affine_inverse(m: &Mat4) -> Option<Mat4> {
    let rows = inverse_rows(m)?;
    let t = column(m, 3);
    let moved = rows.map(|row| -dot(row, t));
    Some([
        [rows[0][0], rows[1][0], rows[2][0], 0.0],
        [rows[0][1], rows[1][1], rows[2][1], 0.0],
        [rows[0][2], rows[1][2], rows[2][2], 0.0],
        [moved[0], moved[1], moved[2], 1.0],
    ])
}


impl EffectNode for MergeMeshes {
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }

    fn type_id(&self) -> &EffectNodeType {
        cached_type_id()
    }

    /// A per-vertex copy, but the codegen has no variadic array inputs.
    fn boundary_reason(&self) -> Option<manifold_node_engine::freeze::classify::BoundaryReason> {
        Some(manifold_node_engine::freeze::classify::BoundaryReason::Blocked)
    }

    fn param_contract(&self, param_name: &str) -> Option<manifold_core::effects::RangeContract> {
        (param_name == "parts").then_some(manifold_core::effects::RangeContract {
            min: Some(1.0),
            max: None,
            reason: manifold_core::effects::RangeReason::Count,
        })
    }

    fn inputs(&self) -> &[NodeInput] {
        &self.inputs
    }

    fn outputs(&self) -> &[NodeOutput] {
        outputs()
    }

    fn mesh_output_rule(&self, _port: &str) -> MeshOutputRule<'_> {
        MeshOutputRule { topology: MeshRevisionRule::Dependencies(&self.topology), positions: MeshRevisionRule::Written }
    }

    fn parameters(&self) -> &[ParamDef] {
        &PARAMS
    }

    fn reconfigure(&mut self, params: &ParamValues) {
        let parts = params.get("parts").and_then(|v| v.as_scalar()).map_or(DEFAULT_PARTS, |f| f.round().max(1.0) as usize);
        if parts != self.parts() {
            self.rebuild_ports(parts);
        }
    }

    /// Every part's vertices, back to back.
    fn array_output_capacity(&self, port_name: &str, _params: &ParamValues, input_capacities: &[(&str, u32)]) -> Option<u32> {
        (port_name == "vertices").then(|| {
            let total: u64 =
                input_capacities.iter().filter(|(port, _)| port.starts_with("mesh_")).map(|&(_, n)| u64::from(n)).sum();
            total.clamp(1, u64::from(u32::MAX)) as u32
        })
    }

    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        self.pipeline.get_or_insert_with(|| device.create_compute_pipeline(SHADER, "cs_main", "node.merge_meshes"));
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if let Some(ParamValue::Float(v)) = ctx.inputs.scalar("reset_trigger") {
            let current = v.round() as i32;
            let edge = self.last_reset_trigger != Some(current);
            self.last_reset_trigger = Some(current);
            if !edge {
                return;
            }
        }
        let placed = |port: &str| ctx.inputs.transform(port).map_or(MAT4_IDENTITY, |t| t.world());
        let Some(to_frame) = affine_inverse(&mat4_mul(&placed("frame_within"), &placed("frame"))) else {
            ctx.error("Merge Meshes: the frame must be finite with no zero scale".to_string());
            return;
        };
        let Some(out) = ctx.outputs.array("vertices") else {
            return;
        };
        let capacity = (out.size / VERTEX_BYTES) as u32;
        // The encoder and the inputs are separate fields, so the parts can be
        // read while dispatching.
        ctx.mark_gpu_accessed();
        let Some(gpu) = ctx.gpu.as_deref_mut() else {
            return;
        };
        let pipeline = self.pipeline.get_or_insert_with(|| gpu.device.create_compute_pipeline(SHADER, "cs_main", "node.merge_meshes"));
        let mut offset = 0u32;
        for ports in self.inputs[FIXED_INPUTS..].chunks_exact(PART_PORTS) {
            let Some(part) = ctx.inputs.array(&ports[0].name) else {
                continue;
            };
            let count = ((part.size / VERTEX_BYTES) as u32).min(capacity - offset);
            if count == 0 {
                continue;
            }
            let world = |port: &NodeInput| ctx.inputs.transform(&port.name).map_or(MAT4_IDENTITY, |t| t.world());
            let place = mat4_mul(&world(&ports[2]), &world(&ports[1]));
            let matrix = mat4_mul(&to_frame, &place);
            let normals = inverse_rows(&matrix).map_or([[0.0; 4]; 3], |rows| rows.map(|r| [r[0], r[1], r[2], 0.0]));
            let uniforms = PartUniforms { matrix, normals, offset, count, clear: 0, _pad: 0 };
            dispatch(gpu, pipeline, &uniforms, part, out);
            offset += count;
        }
        // A part smaller than planned leaves a tail: zeroed, so it adds no
        // area, volume or stale vertex.
        if offset < capacity {
            let uniforms = PartUniforms { matrix: MAT4_IDENTITY, normals: [[0.0; 4]; 3], offset, count: capacity - offset, clear: 1, _pad: 0 };
            dispatch(gpu, pipeline, &uniforms, out, out);
        }
    }
}

fn dispatch(
    gpu: &mut manifold_node_engine::gpu::gpu_encoder::GpuEncoder<'_>,
    pipeline: &GpuComputePipeline,
    uniforms: &PartUniforms,
    part: &manifold_gpu::GpuBuffer,
    out: &manifold_gpu::GpuBuffer,
) {
    gpu.native_enc.dispatch_compute(
        pipeline,
        &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: part, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
        ],
        [uniforms.count.div_ceil(64), 1, 1],
        "node.merge_meshes",
    );
}

inventory::submit! {
    manifold_node_engine::persistence::PrimitiveFactory {
        type_id: MERGE_MESHES_TYPE_ID,
        create: || Box::new(MergeMeshes::new()),
        picker: Some(manifold_node_engine::palette::PickerInfo {
            label: "Merge Meshes",
            category: manifold_node_engine::palette::PaletteCategory::Atom,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_size_the_port_list_with_no_cap() {
        let mut node = MergeMeshes::new();
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("parts"), ParamValue::Float(300.0));
        node.reconfigure(&params);
        assert_eq!(node.parts(), 300);
        assert_eq!(node.inputs().last().unwrap().name, "within_299");
        let capacities = [("mesh_0", 36), ("mesh_7", 100), ("frame", 1)];
        assert_eq!(node.array_output_capacity("vertices", &params, &capacities), Some(136));
    }

    #[test]
    fn affine_inverse_undoes_a_turned_scaled_moved_frame() {
        let frame = manifold_node_engine::scene::transform::trs_matrix([1.0, -2.0, 3.0], [0.3, -0.7, 1.1], [2.0, 0.5, 3.0]);
        let back = mat4_mul(&affine_inverse(&frame).unwrap(), &frame);
        for (c, col) in back.iter().enumerate() {
            for (r, v) in col.iter().enumerate() {
                assert!((v - MAT4_IDENTITY[c][r]).abs() < 1e-5, "{back:?}");
            }
        }
        let flat = manifold_node_engine::scene::transform::trs_matrix([0.0; 3], [0.0; 3], [1.0, 0.0, 1.0]);
        assert!(affine_inverse(&flat).is_none());
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! The real kernel through the node: two parts, each placed by its own
    //! transform inside a moved frame, land back to back in the frame.
    use super::*;
    use manifold_node_engine::scene::transform::{Transform, transform_point};
    use manifold_node_engine::testkit::array_harness::{Harness, read};

    fn vertex(position: [f32; 3]) -> MeshVertex {
        MeshVertex {
            position,
            _pad0: 0.0,
            normal: [0.0, 1.0, 0.0],
            _pad1: 0.0,
            uv: [0.25, 0.75],
            _pad2: [0.0; 2],
            tangent: [1.0, 0.0, 0.0, 1.0],
            color: [1.0; 4],
        }
    }

    #[test]
    fn parts_land_at_their_places_in_the_frame() {
        let mut h = Harness::new();
        let a: Vec<MeshVertex> = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]].map(vertex).to_vec();
        let b: Vec<MeshVertex> = [[0.0, 0.0, 1.0], [0.0, 2.0, 0.0], [3.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.5, 0.5, 0.5], [0.0, 0.0, -1.0]]
            .map(vertex)
            .to_vec();
        let place_a = Transform { pos: [2.0, 0.0, 0.0], rot_euler: [0.0, 0.0, 0.0], scale: [1.0; 3], ..Default::default() };
        let place_b = Transform { pos: [0.0, 1.0, -4.0], rot_euler: [0.0, 1.2, 0.4], scale: [2.0; 3], ..Default::default() };
        let within_b = Transform { pos: [-1.0, 0.5, 0.0], rot_euler: [0.7, 0.0, 0.0], scale: [1.5; 3], ..Default::default() };
        let frame = Transform { pos: [5.0, 0.0, 0.0], rot_euler: [0.0, 0.0, 0.5], scale: [0.5; 3], ..Default::default() };
        let frame_within = Transform { pos: [0.0, -1.0, 2.0], rot_euler: [0.3, 0.0, 0.0], scale: [1.0, 2.0, 1.0], ..Default::default() };
        let (mesh_a, _) = h.array(&a, a.len());
        let (mesh_b, _) = h.array(&b, b.len());
        let (out, _) = h.array::<MeshVertex>(&[], a.len() + b.len() + 3);
        let slots = [
            ("frame", h.transform_input(frame)),
            ("frame_within", h.transform_input(frame_within)),
            ("mesh_0", mesh_a),
            ("place_0", h.transform_input(place_a)),
            ("mesh_1", mesh_b),
            ("place_1", h.transform_input(place_b)),
            ("within_1", h.transform_input(within_b)),
        ];
        let mut node = MergeMeshes::new();
        let (_, errors) = h.run(&mut node, &slots, &[("vertices", out)], &ParamValues::default());
        assert!(errors.is_empty(), "{errors:?}");
        let merged: Vec<MeshVertex> = read(&h.buffer(out), a.len() + b.len() + 3);
        let to_frame = affine_inverse(&mat4_mul(&frame_within.world(), &frame.world())).unwrap();
        let world_a = place_a.world();
        let world_b = mat4_mul(&within_b.world(), &place_b.world());
        let expected = a.iter().map(|v| (world_a, v)).chain(b.iter().map(|v| (world_b, v)));
        for (i, (place, source)) in expected.enumerate() {
            let want = transform_point(&mat4_mul(&to_frame, &place), source.position);
            let got = merged[i].position;
            assert!((0..3).all(|d| (got[d] - want[d]).abs() < 1e-4), "vertex {i}: {got:?} want {want:?}");
            assert!((dot(merged[i].normal, merged[i].normal) - 1.0).abs() < 1e-4, "vertex {i} normal stays unit");
            assert_eq!(merged[i].uv, source.uv);
        }
        for tail in &merged[a.len() + b.len()..] {
            assert_eq!(tail.position, [0.0; 3], "the tail past every part is zeroed");
        }
    }
}
