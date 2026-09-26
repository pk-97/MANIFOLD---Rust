//! `node.ordered_recon_mesh` — a deterministic, band-ordered pose-blended
//! return to a reference triangle stream.
//!
//! Each reference triangle is assigned to a directional band from its
//! reference centroid.  Bands ease in with a staggered `progress`; while a
//! band is waiting, its corners share one pivot and receive a restrained
//! translation plus a full-angle rotation pose blended by the band's `away`
//! weight.  The current mesh remains the source of the attributes during the
//! response so the atom composes after other mesh modifiers.  The reference
//! is used only for stable band membership and pivots; at `progress >= 1` the
//! incoming record is returned exactly.  Partial pose blends can compress
//! faces, while 0 and 2π rotation remain the same periodic pose.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::standalone_pipeline::standalone_pipeline;
use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: params in declaration order followed by
/// the injected dispatch count. The 16 scalar words are already aligned.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OrderedReconUniforms {
    progress: f32,
    bands: i32,
    separation: f32,
    rotation: f32,
    spread: f32,
    direction_x: f32,
    direction_y: f32,
    direction_z: f32,
    scale: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    enabled: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: OrderedReconMesh,
    type_id: "node.ordered_recon_mesh",
    purpose: "Animate an incoming Array<MeshVertex> through ordered pose-blended bands using a reference mesh only for stable centroid ordering. Each reference triangle is assigned by its centroid projection onto a normalized direction; staggered smoothstep progress makes bands settle in sequence. Waiting bands evaluate a full rotation around a shared directional pivot, blend the rotated pose by `away`, and translate by restrained separation/spread. UVs and orthonormalized frame attributes remain attached, and progress >= 1 returns the incoming record exactly.",
    inputs: {
        in: Array(MeshVertex) required,
        reference: Array(MeshVertex) required,
        progress: ScalarF32 optional,
        bands: ScalarF32 optional,
        separation: ScalarF32 optional,
        rotation: ScalarF32 optional,
        spread: ScalarF32 optional,
        direction_x: ScalarF32 optional,
        direction_y: ScalarF32 optional,
        direction_z: ScalarF32 optional,
        scale: ScalarF32 optional,
        source_offset_x: ScalarF32 optional,
        source_offset_y: ScalarF32 optional,
        source_offset_z: ScalarF32 optional,
        enabled: ScalarF32 optional,
    },
    outputs: { out: Array(MeshVertex), },
    params: [
        ParamDef { name: Cow::Borrowed("progress"), label: "Progress", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bands"), label: "Bands", ty: ParamType::Int, default: ParamValue::Float(8.0), range: Some((1.0, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("separation"), label: "Separation", ty: ParamType::Float, default: ParamValue::Float(0.22), range: Some((0.0, 2.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("rotation"), label: "Rotation", ty: ParamType::Angle, default: ParamValue::Float(0.45), range: Some((-std::f32::consts::PI, std::f32::consts::PI)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("spread"), label: "Spread", ty: ParamType::Float, default: ParamValue::Float(0.08), range: Some((-1.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("direction_x"), label: "Direction X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("direction_y"), label: "Direction Y", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((-1.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("direction_z"), label: "Direction Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("scale"), label: "Scene Radius", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.000001, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_x"), label: "Source Offset X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_y"), label: "Source Offset Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_z"), label: "Source Offset Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("enabled"), label: "Enabled", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Use the actual upstream mesh as `in` and its immutable source as `reference`. Reference triangle centroids are projected into ordered directional bands, so this is a staged assembly response rather than random shatter or fixed-cell peel. Waiting bands evaluate the full rotation angle and pose-blend the result by `away`, making 0 and 2π equivalent even during partial progress; this intentionally allows partial pose blending to compress faces. Separation and spread remain weighted translations. `progress >= 1`, a settled local band, and disabled all return the incoming record byte-exactly; reference attributes are never substituted. Keep separation, rotation and spread restrained for readable scan assembly. Direction and all scalar controls are port-shadowed for live modulation. The centroid reads are BufferGather, so this remains a standalone reference-driven boundary like node.transform_mesh_patches rather than pretending the reference lookup is coincident and fusable.",
    examples: ["OrderedRecon"],
    picker: { label: "Ordered Recon", category: Atom },
    summary: "Reassembles an incoming mesh in directional bands, with each band settling from a periodic pose blend.",
    category: Geometry3D,
    role: Filter,
    aliases: ["ordered recon", "ordered reconstruction", "banded assembly", "mesh assembly", "reconstruct mesh"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/ordered_recon_mesh_body.wgsl"),
    input_access: [Coincident, BufferGather],
}

impl Primitive for OrderedReconMesh {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        let current = input_capacities
            .iter()
            .find(|(p, _)| *p == "in")
            .map(|(_, n)| *n);
        let reference = input_capacities
            .iter()
            .find(|(p, _)| *p == "reference")
            .map(|(_, n)| *n);
        match (current, reference) {
            (Some(a), Some(b)) if a == b => Some(a),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let progress = ctx.scalar_or_param("progress", 0.0);
        let bands = ctx.scalar_or_param("bands", 8.0).round().clamp(1.0, 64.0) as i32;
        let separation = ctx.scalar_or_param("separation", 0.22);
        let rotation = ctx.scalar_or_param("rotation", 0.45);
        let spread = ctx.scalar_or_param("spread", 0.08);
        let direction_x = ctx.scalar_or_param("direction_x", 0.0);
        let direction_y = ctx.scalar_or_param("direction_y", 1.0);
        let direction_z = ctx.scalar_or_param("direction_z", 0.0);
        let scale = ctx.scalar_or_param("scale", 1.0);
        let source_offset_x = ctx.scalar_or_param("source_offset_x", 0.0);
        let source_offset_y = ctx.scalar_or_param("source_offset_y", 0.0);
        let source_offset_z = ctx.scalar_or_param("source_offset_z", 0.0);
        let enabled = ctx.scalar_or_param("enabled", 1.0);
        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let Some(reference) = ctx.inputs.array("reference") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let count = ((src.size / vertex_size) as u32)
            .min((reference.size / vertex_size) as u32)
            .min((dst.size / vertex_size) as u32);
        if count == 0 {
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = OrderedReconUniforms {
            progress,
            bands,
            separation,
            rotation,
            spread,
            direction_x,
            direction_y,
            direction_z,
            scale,
            source_offset_x,
            source_offset_y,
            source_offset_z,
            enabled,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
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
                    buffer: reference,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.ordered_recon_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn ordered_recon_declares_shadowed_controls_and_capacity_contract() {
        use crate::node_graph::effect_node::ParamValues;
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh = ArrayType::of_known::<MeshVertex>();
        let prim = OrderedReconMesh::new();
        assert_eq!(OrderedReconMesh::TYPE_ID, "node.ordered_recon_mesh");
        for name in ["in", "reference"] {
            let port = OrderedReconMesh::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap();
            assert!(port.required);
            assert_eq!(port.ty, PortType::Array(mesh));
        }
        for name in [
            "progress",
            "bands",
            "separation",
            "rotation",
            "spread",
            "direction_x",
            "direction_y",
            "direction_z",
            "scale",
            "source_offset_x",
            "source_offset_y",
            "source_offset_z",
            "enabled",
        ] {
            let port = OrderedReconMesh::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap();
            assert!(!port.required, "{name} must be a scalar shadow");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(OrderedReconMesh::OUTPUTS[0].ty, PortType::Array(mesh));
        assert_eq!(
            Primitive::array_output_capacity(
                &prim,
                "out",
                &ParamValues::default(),
                &[("in", 12), ("reference", 12)]
            ),
            Some(12)
        );
        assert_eq!(
            Primitive::array_output_capacity(
                &prim,
                "out",
                &ParamValues::default(),
                &[("in", 12), ("reference", 11)]
            ),
            None
        );
    }

    #[test]
    fn ordered_recon_registers_as_palette_atom() {
        let prim = OrderedReconMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.ordered_recon_mesh");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Bounded Metal proofs for the generated standalone atom. The reference
    //! gather admits into fused buffer regions (BUG-x72p: the wire stays
    //! external, bound read-only, indexed at band pivots — the kernel is
    //! expressible). What refuses fusion is the identity probe: the output
    //! capacity is identity ONLY when `in` and `reference` share a capacity
    //! and `None` otherwise, and the probe's distinct ascending synthetic
    //! capacities hit the `None` branch — so `build_region` refuses the
    //! region and the atom renders unfused. The probe pin below guards the
    //! band pivot contract from a future capacity change silently sizing the
    //! fused output differently than the unfused buffers.
    use super::*;
    use crate::node_graph::effect_node::NodeInstanceId;
    use crate::node_graph::freeze::classify::{FusionKind, InputAccess};
    use crate::node_graph::freeze::codegen::{
        ENTRY, FusionRegion, InputSource, RegionNode, generate_fused,
    };
    use crate::node_graph::primitive::PrimitiveSpec;

    fn vertex(position: [f32; 3], normal: [f32; 3], uv: [f32; 2], tangent: [f32; 4]) -> MeshVertex {
        MeshVertex {
            position,
            _pad0: 19.0,
            normal,
            _pad1: -7.0,
            uv,
            _pad2: [3.0, -5.0],
            tangent,
            color: [1.0; 4],
        }
}

    fn dispatch(
        wgsl: &str,
        current: &[MeshVertex],
        reference: &[MeshVertex],
        u: OrderedReconUniforms,
        label: &str,
    ) -> Vec<MeshVertex> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, label);
        let src = device.create_buffer_shared(std::mem::size_of_val(current) as u64);
        let rbuf = device.create_buffer_shared(std::mem::size_of_val(reference) as u64);
        let out = device.create_buffer_shared(std::mem::size_of_val(current) as u64);
        unsafe {
            src.write(0, bytemuck::cast_slice(current));
            rbuf.write(0, bytemuck::cast_slice(reference));
        }
        let mut encoder = device.create_encoder(label);
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&u),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &src,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &rbuf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &out,
                    offset: 0,
                },
            ],
            [(current.len() as u32).div_ceil(256), 1, 1],
            label,
        );
        encoder.commit_and_wait_completed();
        let ptr = out.mapped_ptr().expect("shared output");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, current.len()) }.to_vec()
    }

    fn uniforms(progress: f32, enabled: f32) -> OrderedReconUniforms {
        OrderedReconUniforms {
            progress,
            bands: 3,
            separation: 0.18,
            rotation: 0.35,
            spread: 0.06,
            direction_x: 0.0,
            direction_y: 1.0,
            direction_z: 0.0,
            scale: 1.0,
            source_offset_x: 0.0,
            source_offset_y: 0.0,
            source_offset_z: 0.0,
            enabled,
            dispatch_count: 6,
            _pad0: 0,
            _pad1: 0,
        }
    }

    #[test]
    fn structured_modifier_ordered_recon_standalone_proves_exact_endpoints_and_order() {
        let current = vec![
            vertex(
                [-0.2, -0.7, 0.0],
                [0.0, 0.0, 1.0],
                [0.0, 0.0],
                [1.0, 0.0, 0.0, 1.0],
            ),
            vertex(
                [0.2, -0.7, 0.0],
                [0.0, 0.0, 1.0],
                [1.0, 0.0],
                [1.0, 0.0, 0.0, 1.0],
            ),
            vertex(
                [0.0, -0.3, 0.0],
                [0.0, 0.0, 1.0],
                [0.5, 1.0],
                [1.0, 0.0, 0.0, 1.0],
            ),
            vertex(
                [-0.2, 0.3, 0.0],
                [0.0, 0.0, 1.0],
                [0.0, 0.0],
                [1.0, 0.0, 0.0, 1.0],
            ),
            vertex(
                [0.2, 0.3, 0.0],
                [0.0, 0.0, 1.0],
                [1.0, 0.0],
                [1.0, 0.0, 0.0, 1.0],
            ),
            vertex(
                [0.0, 0.7, 0.0],
                [0.0, 0.0, 1.0],
                [0.5, 1.0],
                [1.0, 0.0, 0.0, 1.0],
            ),
        ];
        let reference: Vec<_> = current
            .iter()
            .enumerate()
            .map(|(i, v)| {
                vertex(
                    [v.position[0] + 0.03 * i as f32, v.position[1], 0.05],
                    v.normal,
                    v.uv,
                    v.tangent,
                )
            })
            .collect();
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<OrderedReconMesh>()
            .expect("ordered recon standalone codegen");

        let disabled = dispatch(
            &wgsl,
            &current,
            &reference,
            uniforms(0.4, 0.0),
            "ordered-recon-disabled",
        );
        for (got, expected) in disabled.iter().zip(&current) {
            assert_eq!(
                (got.position, got.normal, got.uv, got.tangent),
                (
                    expected.position,
                    expected.normal,
                    expected.uv,
                    expected.tangent
                ),
                "disabled preserves current exactly"
            );
        }
        let returned = dispatch(
            &wgsl,
            &current,
            &reference,
            uniforms(1.0, 1.0),
            "ordered-recon-return",
        );
        for (got, expected) in returned.iter().zip(&current) {
            assert_eq!(
                (got.position, got.normal, got.uv, got.tangent),
                (
                    expected.position,
                    expected.normal,
                    expected.uv,
                    expected.tangent
                ),
                "progress 1 returns incoming current exactly"
            );
        }
        let early = dispatch(
            &wgsl,
            &current,
            &reference,
            uniforms(0.18, 1.0),
            "ordered-recon-early",
        );
        let late = dispatch(
            &wgsl,
            &current,
            &reference,
            uniforms(0.72, 1.0),
            "ordered-recon-late",
        );
        assert!(
            early
                .iter()
                .zip(&current)
                .any(|(a, b)| a.position != b.position),
            "an early ordered band should move"
        );
        let midpoint = dispatch(
            &wgsl,
            &current,
            &reference,
            uniforms(0.5, 1.0),
            "ordered-recon-midpoint",
        );
        for (got, expected) in midpoint[..3].iter().zip(&current[..3]) {
            assert_eq!(
                (got.position, got.normal, got.uv, got.tangent),
                (
                    expected.position,
                    expected.normal,
                    expected.uv,
                    expected.tangent
                ),
                "the early band must have settled to incoming current"
            );
        }
        assert!(
            midpoint[3..]
                .iter()
                .zip(&current[3..])
                .any(|(a, b)| a.position != b.position),
            "a later band must still be displaced at the midpoint"
        );
        assert!(
            late.iter()
                .zip(&current)
                .filter(|(a, b)| a.position == b.position)
                .count()
                >= early
                    .iter()
                    .zip(&current)
                    .filter(|(a, b)| a.position == b.position)
                    .count(),
            "later progress must not reduce the number of arrived bands"
        );
        assert!(
            late.iter().all(|v| v
                .position
                .iter()
                .chain(v.normal.iter())
                .all(|x| x.is_finite())),
            "ordered reconstruction stays finite"
        );
    }

    #[test]
    fn structured_modifier_ordered_recon_rotation_is_periodic_during_partial_pose_blend() {
        let current = vec![
            vertex(
                [0.52, 0.18, -0.11],
                [0.3, 0.8, 0.5],
                [0.0, 0.0],
                [0.9, -0.2, 0.35, -1.0],
            ),
            vertex(
                [0.91, 0.22, -0.11],
                [0.3, 0.8, 0.5],
                [1.0, 0.0],
                [0.9, -0.2, 0.35, -1.0],
            ),
            vertex(
                [0.52, 0.59, -0.11],
                [0.3, 0.8, 0.5],
                [0.0, 1.0],
                [0.9, -0.2, 0.35, -1.0],
            ),
        ];
        let reference = vec![
            vertex(
                [0.12, 0.17, 0.08],
                [0.0, 1.0, 0.0],
                [0.0, 0.0],
                [1.0, 0.0, 0.0, 1.0],
            ),
            vertex(
                [0.16, 0.17, 0.08],
                [0.0, 1.0, 0.0],
                [1.0, 0.0],
                [1.0, 0.0, 0.0, 1.0],
            ),
            vertex(
                [0.12, 0.21, 0.08],
                [0.0, 1.0, 0.0],
                [0.0, 1.0],
                [1.0, 0.0, 0.0, 1.0],
            ),
        ];
        let mut u = OrderedReconUniforms {
            // The single band settles at 0.28; sample inside its transition.
            progress: 0.1,
            bands: 1,
            separation: 0.17,
            rotation: 0.0,
            spread: -0.09,
            direction_x: 0.31,
            direction_y: 0.78,
            direction_z: -0.41,
            scale: 1.0,
            source_offset_x: 0.12,
            source_offset_y: -0.08,
            source_offset_z: 0.07,
            enabled: 1.0,
            dispatch_count: 3,
            _pad0: 0,
            _pad1: 0,
        };
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<OrderedReconMesh>()
            .expect("ordered recon standalone codegen");
        let zero = dispatch(
            &wgsl,
            &current,
            &reference,
            u,
            "ordered-recon-rotation-zero",
        );
        u.rotation = std::f32::consts::TAU;
        let turn = dispatch(
            &wgsl,
            &current,
            &reference,
            u,
            "ordered-recon-rotation-turn",
        );
        u.rotation = std::f32::consts::TAU - 1e-4;
        let before_seam = dispatch(
            &wgsl,
            &current,
            &reference,
            u,
            "ordered-recon-rotation-before-seam",
        );
        u.rotation = std::f32::consts::TAU + 1e-4;
        let after_seam = dispatch(
            &wgsl,
            &current,
            &reference,
            u,
            "ordered-recon-rotation-after-seam",
        );
        u.rotation = std::f32::consts::PI;
        let midpoint = dispatch(&wgsl, &current, &reference, u, "ordered-recon-rotation-mid");
        for (a, b) in zero.iter().zip(&turn) {
            for axis in 0..3 {
                assert!((a.position[axis] - b.position[axis]).abs() < 3e-5);
                assert!((a.normal[axis] - b.normal[axis]).abs() < 3e-5);
                assert!((a.tangent[axis] - b.tangent[axis]).abs() < 3e-5);
            }
            assert_eq!(a.uv, b.uv);
            assert_eq!(a.tangent[3], b.tangent[3]);
        }
        for (a, b) in before_seam.iter().zip(&after_seam) {
            for axis in 0..3 {
                assert!((a.position[axis] - b.position[axis]).abs() < 3e-4);
                assert!((a.normal[axis] - b.normal[axis]).abs() < 3e-4);
                assert!((a.tangent[axis] - b.tangent[axis]).abs() < 3e-4);
            }
        }
        assert!(midpoint.iter().zip(&zero).any(|(a, b)| {
            a.position
                .iter()
                .zip(b.position.iter())
                .any(|(x, y)| (x - y).abs() > 1e-4)
        }));
        for frame in midpoint {
            let normal_length = frame.normal.iter().map(|x| x * x).sum::<f32>().sqrt();
            let tangent = [frame.tangent[0], frame.tangent[1], frame.tangent[2]];
            let tangent_length = tangent.iter().map(|x| x * x).sum::<f32>().sqrt();
            let frame_dot = frame.normal[0] * tangent[0]
                + frame.normal[1] * tangent[1]
                + frame.normal[2] * tangent[2];
            assert!(
                frame
                    .normal
                    .iter()
                    .chain(tangent.iter())
                    .all(|component| component.is_finite())
            );
            assert!((normal_length - 1.0).abs() < 3e-5);
            assert!((tangent_length - 1.0).abs() < 3e-5);
            assert!(frame_dot.abs() < 3e-5);
        }
    }

    /// BUG-x72p: the reference gather ADMITS at the codegen — the kernel is
    /// expressible. What refuses fusion is the identity probe: this atom's
    /// output capacity is identity ONLY when `in` and `reference` share a
    /// capacity, `None` otherwise — and the probe's distinct ascending
    /// synthetic caps (1009, 2009) hit the `None` branch, so `build_region`
    /// refuses the region (unfused, always correct). A same-mesh fork with
    /// genuinely equal capacities would still refuse: the static probe cannot
    /// see real capacities, and the conservative answer is the safe one.
    #[test]
    fn structured_modifier_ordered_recon_reference_capacity_refuses_the_gather_identity_probe() {
        let id = NodeInstanceId;
        let region = FusionRegion {
            nodes: vec![RegionNode {
                node_id: id(0),
                fusion_kind: FusionKind::Pointwise,
                body: OrderedReconMesh::WGSL_BODY.unwrap(),
                params: OrderedReconMesh::PARAMS,
                inputs: vec![InputSource::External(0), InputSource::External(1)],
                input_access: vec![InputAccess::Coincident, InputAccess::BufferGather],
                node_inputs: OrderedReconMesh::INPUTS,
                node_outputs: OrderedReconMesh::OUTPUTS,
                node_includes: OrderedReconMesh::WGSL_INCLUDES,
                derived_uniforms: OrderedReconMesh::DERIVED_UNIFORMS,
                type_id: OrderedReconMesh::TYPE_ID.to_string(),
                derived_camera_ext: None,
                output_storage: "rgba32float",
                stencil_fetch: false,
                quantize_f16: false,
            }],
            num_external_inputs: 2,
            outputs: vec![(id(0), "out".to_string())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: Vec::new(),
            sampled_externals: Vec::new(),
            camera_externals: 0,
            output_capacity: None,
        };
        let g = generate_fused(&region).expect("the reference-gather kernel fuses");
        assert!(
            naga::front::wgsl::parse_str(&g.wgsl).is_ok(),
            "fused reference-gather kernel parses:\n{}",
            g.wgsl
        );
        let prim = OrderedReconMesh::new();
        let node: &dyn crate::node_graph::effect_node::EffectNode = &prim;
        assert_eq!(
            node.array_output_capacity("out", &Default::default(), &[("in", 1009), ("reference", 2009)]),
            None,
            "distinct capacities — the probe's case, refused"
        );
        assert_eq!(
            node.array_output_capacity("out", &Default::default(), &[("in", 1009), ("reference", 1009)]),
            Some(1009),
            "identity when the capacities match — the atom's real precondition"
        );
    }
}
