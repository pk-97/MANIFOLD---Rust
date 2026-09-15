//! `node.mesh_spatial_mask` — scene-relative band or sphere weights from a
//! mesh's current positions.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::standalone_pipeline::standalone_pipeline;
use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const SHAPES: &[&str] = &["Band", "Sphere", "Half Space"];
const SAMPLE_MODES: &[&str] = &["Vertex", "Triangle Centroid", "Patch Cell"];
const PATCH_CELL_COMMON: &str = include_str!("shaders/patch_cell_common.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MeshSpatialMaskUniforms {
    shape: u32,
    sample_mode: u32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    yaw: f32,
    pitch: f32,
    width: f32,
    feather: f32,
    invert: f32,
    amount: f32,
    scale: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    cell_size: f32,
    low: f32,
    high: f32,
    weights_len: u32,
    dispatch_count: u32,
}

crate::primitive! {
    name: MeshSpatialMask,
    type_id: "node.mesh_spatial_mask",
    purpose: "Generate scene-relative Array<f32> weights from an Array<MeshVertex>. A Band uses signed distance abs(dot(p, direction)) - width, a Sphere uses length(p) - width, and a Half Space uses dot(p, direction) - width, with p = (sample_position + source_offset) / scene_radius - center. Vertex, triangle-centroid, or fixed patch-cell sampling is selectable; amount maps the post-invert mask from low to high and blends that result against one.",
    inputs: {
        in: Array(MeshVertex) required,
        weights: Array(f32) optional,
        shape: ScalarF32 optional,
        center_x: ScalarF32 optional,
        center_y: ScalarF32 optional,
        center_z: ScalarF32 optional,
        yaw: ScalarF32 optional,
        pitch: ScalarF32 optional,
        width: ScalarF32 optional,
        feather: ScalarF32 optional,
        invert: ScalarF32 optional,
        amount: ScalarF32 optional,
        scale: ScalarF32 optional,
        source_offset_x: ScalarF32 optional,
        source_offset_y: ScalarF32 optional,
        source_offset_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        low: ScalarF32 optional,
        high: ScalarF32 optional,
    },
    outputs: { weights: Array(f32), },
    params: [
        ParamDef { name: Cow::Borrowed("shape"), label: "Shape", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 2.0)), enum_values: SHAPES },
        ParamDef { name: Cow::Borrowed("sample_mode"), label: "Sample Mode", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 2.0)), enum_values: SAMPLE_MODES },
        ParamDef { name: Cow::Borrowed("center_x"), label: "Center X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-10.0, 10.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("center_y"), label: "Center Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-10.0, 10.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("center_z"), label: "Center Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-10.0, 10.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("yaw"), label: "Yaw", ty: ParamType::Angle, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("pitch"), label: "Pitch", ty: ParamType::Angle, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("width"), label: "Width", ty: ParamType::Float, default: ParamValue::Float(0.35), range: Some((0.0, 10.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("feather"), label: "Feather", ty: ParamType::Float, default: ParamValue::Float(0.15), range: Some((0.0, 10.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("invert"), label: "Invert", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("amount"), label: "Amount", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("scale"), label: "Scene Radius", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.000001, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_x"), label: "Source Offset X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_y"), label: "Source Offset Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_z"), label: "Source Offset Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.2), range: Some((0.000001, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("low"), label: "Low", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("high"), label: "High", ty: ParamType::Float, default: ParamValue::Float(1.0), range: None, enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Pair `weights` with node.morph_mesh or another weighted mesh response. `sample_mode=Vertex` gives per-vertex wave masks; `Triangle Centroid` gives one coherent value to all three corners of each flat-list triangle; `Patch Cell` quantizes each triangle centroid into a shared fixed spatial cell, so disconnected faces may intentionally share a weight. An incoming `weights` array is multiplied per vertex; an unwired array uses identity weight 1 and a short wired array is rejected. Width, feather, and cell_size are scene-radius units after the center/direction transform. The Half Space shape uses the signed plane distance dot(p, direction) - width. The post-invert mask maps from low to high before amount blends against one. Amount 0 preserves incoming weights exactly (one when unwired). This atom has no clock and all float controls are scalar-shadowed.",
    examples: [],
    picker: { label: "Mesh Spatial Mask", category: Atom },
    summary: "Makes soft band or sphere weights from mesh positions for driving a staged surface response.",
    category: Geometry3D,
    role: Source,
    aliases: ["mesh mask", "spatial mask", "band mask", "sphere mask", "mesh weights"],
    pure: true,
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/mesh_spatial_mask_body.wgsl"),
    input_access: [BufferGather, BufferGather],
    derived_uniforms: ["weights_len:u32"],
    wgsl_includes: [PATCH_CELL_COMMON],
}

impl Primitive for MeshSpatialMask {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "weights")
            .then(|| {
                input_capacities
                    .iter()
                    .find(|(p, _)| *p == "in")
                    .map(|(_, n)| *n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if ["in", "weights"].iter().any(|name| ctx.inputs.slot(name).is_some_and(|slot| !ctx.inputs.slot_content_ready(slot))) {
            ctx.mark_outputs_pending();
            return;
        }
        let shape = match ctx.inputs.scalar("shape").as_ref() {
            Some(ParamValue::Enum(v)) => (*v).min(2),
            Some(ParamValue::Float(v)) => v.round().clamp(0.0, 2.0) as u32,
            _ => match ctx.params.get("shape") {
                Some(ParamValue::Enum(v)) => (*v).min(2),
                Some(ParamValue::Float(v)) => v.round().clamp(0.0, 2.0) as u32,
                _ => 0,
            },
        };
        let sample_mode = match ctx.params.get("sample_mode") {
            Some(ParamValue::Enum(v)) => (*v).min(2),
            _ => 0,
        };
        let center_x = ctx.scalar_or_param("center_x", 0.0);
        let center_y = ctx.scalar_or_param("center_y", 0.0);
        let center_z = ctx.scalar_or_param("center_z", 0.0);
        let yaw = ctx.scalar_or_param("yaw", 0.0);
        let pitch = ctx.scalar_or_param("pitch", 0.0);
        let width = ctx.scalar_or_param("width", 0.35);
        let feather = ctx.scalar_or_param("feather", 0.15);
        let invert = ctx.scalar_or_param("invert", 0.0);
        let amount = ctx.scalar_or_param("amount", 0.0);
        let scale = ctx.scalar_or_param("scale", 1.0);
        let source_offset_x = ctx.scalar_or_param("source_offset_x", 0.0);
        let source_offset_y = ctx.scalar_or_param("source_offset_y", 0.0);
        let source_offset_z = ctx.scalar_or_param("source_offset_z", 0.0);
        let cell_size = ctx.scalar_or_param("cell_size", 0.2);
        let low = ctx.scalar_or_param("low", 0.0);
        let high = ctx.scalar_or_param("high", 1.0);
        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("weights") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let count = ((src.size / vertex_size) as u32).min((dst.size / 4) as u32);
        if count == 0 {
            return;
        }
        let weights_wired = ctx.inputs.array("weights");
        if let Some(weights) = weights_wired {
            let weights_len = (weights.size / 4) as u32;
            if weights_len < count {
                ctx.error(format!(
                    "node.mesh_spatial_mask: incoming weights length {weights_len} is shorter than mesh length {count}"
                ));
                return;
            }
        }
        let weights_buf = weights_wired.unwrap_or(src);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = MeshSpatialMaskUniforms {
            shape,
            sample_mode,
            center_x,
            center_y,
            center_z,
            yaw,
            pitch,
            width,
            feather,
            invert,
            amount,
            scale,
            source_offset_x,
            source_offset_y,
            source_offset_z,
            cell_size,
            low,
            high,
            weights_len: weights_wired.map(|b| (b.size / 4) as u32).unwrap_or(0),
            dispatch_count: count,
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
                    buffer: weights_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.mesh_spatial_mask",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn mesh_spatial_mask_declares_modes_shadows_and_capacity() {
        use crate::node_graph::effect_node::ParamValues;
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh = ArrayType::of_known::<MeshVertex>();
        let prim = MeshSpatialMask::new();
        assert_eq!(MeshSpatialMask::TYPE_ID, "node.mesh_spatial_mask");
        assert_eq!(MeshSpatialMask::INPUTS[0].ty, PortType::Array(mesh));
        assert!(MeshSpatialMask::INPUTS[0].required);
        assert_eq!(MeshSpatialMask::INPUTS[1].name, "weights");
        assert_eq!(
            MeshSpatialMask::INPUTS[1].ty,
            PortType::Array(ArrayType::of_known::<f32>())
        );
        assert!(!MeshSpatialMask::INPUTS[1].required);
        for name in [
            "shape",
            "center_x",
            "center_y",
            "center_z",
            "yaw",
            "pitch",
            "width",
            "feather",
            "invert",
            "amount",
            "scale",
            "source_offset_x",
            "source_offset_y",
            "source_offset_z",
            "cell_size",
            "low",
            "high",
        ] {
            let port = MeshSpatialMask::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap();
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
            assert!(!port.required);
        }
        assert_eq!(
            MeshSpatialMask::OUTPUTS[0].ty,
            PortType::Array(ArrayType::of_known::<f32>())
        );
        assert_eq!(
            Primitive::array_output_capacity(
                &prim,
                "weights",
                &ParamValues::default(),
                &[("in", 12)]
            ),
            Some(12)
        );
    }

    #[test]
    fn mesh_spatial_mask_registers_as_palette_atom() {
        let prim = MeshSpatialMask::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.mesh_spatial_mask");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::node_graph::effect_node::NodeInstanceId;
    use crate::node_graph::freeze::classify::{FusionKind, InputAccess};
    use crate::node_graph::freeze::codegen::{
        ENTRY, FusionRegion, InputSource, RegionNode, generate_fused,
    };
    use crate::node_graph::primitive::PrimitiveSpec;

    fn vertex(position: [f32; 3]) -> MeshVertex {
        MeshVertex {
            position,
            _pad0: 0.0,
            normal: [0.0, 0.0, 1.0],
            _pad1: 0.0,
            uv: [0.0, 0.0],
            _pad2: [0.0; 2],
            tangent: [0.0; 4],
        }
    }
    fn dispatch(
        wgsl: &str,
        src_vertices: &[MeshVertex],
        u: MeshSpatialMaskUniforms,
        label: &str,
    ) -> Vec<f32> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, label);
        let src = device.create_buffer_shared(std::mem::size_of_val(src_vertices) as u64);
        let dst = device.create_buffer_shared(src_vertices.len() as u64 * 4);
        unsafe {
            src.write(0, bytemuck::cast_slice(src_vertices));
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
                    buffer: &src,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &dst,
                    offset: 0,
                },
            ],
            [(src_vertices.len() as u32).div_ceil(256), 1, 1],
            label,
        );
        encoder.commit_and_wait_completed();
        let ptr = dst.mapped_ptr().expect("shared output");
        unsafe { std::slice::from_raw_parts(ptr as *const f32, src_vertices.len()) }.to_vec()
    }
    fn uniforms(shape: u32, sample_mode: u32, amount: f32) -> MeshSpatialMaskUniforms {
        MeshSpatialMaskUniforms {
            shape,
            sample_mode,
            center_x: 0.0,
            center_y: 0.0,
            center_z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            width: 0.35,
            feather: 0.15,
            invert: 0.0,
            amount,
            scale: 2.0,
            source_offset_x: 0.0,
            source_offset_y: 0.0,
            source_offset_z: 0.0,
            cell_size: 0.2,
            low: 0.0,
            high: 1.0,
            weights_len: 0,
            dispatch_count: 6,
        }
    }

    #[test]
    fn overnight_modifier_mesh_spatial_mask_proves_band_sphere_amount_and_triangle_coherence() {
        let src = vec![
            vertex([-1.0, -0.2, 0.0]),
            vertex([0.0, -0.2, 0.0]),
            vertex([1.0, -0.2, 0.0]),
            vertex([-0.2, 0.8, 0.0]),
            vertex([0.2, 0.8, 0.0]),
            vertex([0.0, 1.2, 0.0]),
        ];
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<MeshSpatialMask>()
            .expect("mask codegen");
        let mut band = uniforms(0, 0, 1.0);
        // Pitch Y into Z, then yaw Z into X; yaw alone leaves Y unchanged.
        band.pitch = std::f32::consts::FRAC_PI_2;
        band.yaw = std::f32::consts::FRAC_PI_2;
        band.width = 0.2;
        band.feather = 0.0;
        let band_values = dispatch(&wgsl, &src, band, "mask-band");
        assert_eq!(band_values[1], 1.0);
        assert_eq!(band_values[0], 0.0);
        let sphere = dispatch(&wgsl, &src, uniforms(1, 0, 1.0), "mask-sphere");
        assert!(sphere.iter().any(|v| *v > 0.0 && *v < 1.0));
        let mut centroid = uniforms(0, 1, 1.0);
        centroid.width = 0.35;
        centroid.feather = 0.15;
        let centroid_values = dispatch(&wgsl, &src, centroid, "mask-centroid");
        assert_eq!(centroid_values[0], centroid_values[1]);
        assert_eq!(centroid_values[1], centroid_values[2]);
        let mut inverted = band;
        inverted.invert = 1.0;
        let inverted_values = dispatch(&wgsl, &src, inverted, "mask-invert");
        assert_eq!(inverted_values[1], 0.0);
        assert_eq!(inverted_values[0], 1.0);
        let zero_amount = dispatch(&wgsl, &src, uniforms(1, 0, 0.0), "mask-amount-zero");
        assert!(zero_amount.iter().all(|v| *v == 1.0));
        let mut offset = uniforms(1, 0, 1.0);
        offset.source_offset_y = 1.0;
        let offset_values = dispatch(&wgsl, &src, offset, "mask-offset");
        let scaled_src: Vec<_> = src
            .iter()
            .map(|v| {
                vertex([
                    v.position[0] * 2.0,
                    v.position[1] * 2.0,
                    v.position[2] * 2.0,
                ])
            })
            .collect();
        let mut scaled = offset;
        scaled.scale = 4.0;
        scaled.source_offset_y = 2.0;
        let scaled_values = dispatch(&wgsl, &scaled_src, scaled, "mask-offset-normalized");
        for (a, b) in offset_values.iter().zip(scaled_values) {
            assert!((a - b).abs() < 2e-5);
        }
    }

    #[test]
    fn overnight_modifier_mesh_spatial_mask_proves_patch_cells_half_space_and_highlight() {
        let shared_cell_src = vec![
            vertex([0.08, 0.08, 0.0]),
            vertex([0.08, 0.08, 0.0]),
            vertex([0.08, 0.08, 0.0]),
            vertex([0.13, 0.11, 0.0]),
            vertex([0.13, 0.11, 0.0]),
            vertex([0.13, 0.11, 0.0]),
        ];
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<MeshSpatialMask>()
            .expect("mask codegen");
        let mut shared = uniforms(2, 2, 1.0);
        shared.scale = 2.0;
        shared.source_offset_x = 0.2;
        shared.source_offset_y = 0.4;
        shared.cell_size = 0.2;
        shared.center_x = 0.2;
        shared.center_y = 0.2;
        shared.width = 0.0;
        shared.feather = 0.0;
        shared.low = 0.25;
        shared.high = 2.0;
        let shared_values = dispatch(&wgsl, &shared_cell_src, shared, "mask-patch-cell");
        assert_eq!(shared_values[0], 2.0);
        assert_eq!(shared_values[1], shared_values[0]);
        assert_eq!(shared_values[2], shared_values[0]);
        assert_eq!(shared_values[3], shared_values[0]);
        assert_eq!(shared_values[4], shared_values[3]);
        assert_eq!(shared_values[5], shared_values[3]);

        let half_space_src = vec![
            vertex([0.0, -0.5, 0.0]),
            vertex([0.0, -0.5, 0.0]),
            vertex([0.0, -0.5, 0.0]),
            vertex([0.0, 0.5, 0.0]),
            vertex([0.0, 0.5, 0.0]),
            vertex([0.0, 0.5, 0.0]),
        ];
        let mut half_space = uniforms(2, 0, 1.0);
        half_space.width = 0.0;
        half_space.feather = 0.0;
        half_space.low = 0.25;
        half_space.high = 2.0;
        let half_space_values = dispatch(&wgsl, &half_space_src, half_space, "mask-half-space");
        assert_eq!(half_space_values[0], 2.0);
        assert_eq!(half_space_values[3], 0.25);

        half_space.amount = 0.0;
        let identity_values = dispatch(&wgsl, &half_space_src, half_space, "mask-amount-zero");
        assert!(identity_values.iter().all(|v| *v == 1.0));
    }

    /// BUG-x72p: the old pin asserted `generate_fused` fails for a gathered
    /// region — stale once the `BufferGather` admission landed — and passed
    /// only because its hand-built region was MALFORMED (one input source vs
    /// two `BufferGather` access entries, a shape `BadInput`), pinning the
    /// validation, not the gather semantics. The atom's true live-path status:
    /// the gathered kernel IS expressible, and the identity probe PASSES
    /// (`weights` capacity = the `in` capacity) — what refuses fusion is the
    /// derived uniform: `weights_len:u32` has NO registered recompute, so
    /// install's `has_recompute` gate fails any region containing it closed
    /// (unfused, always correct). Same shape as `node.mesh_stagger_envelope`.
    #[test]
    fn overnight_modifier_mesh_spatial_mask_refuses_at_derived_uniform_recompute() {
        let id = NodeInstanceId;
        let region = FusionRegion {
            nodes: vec![RegionNode {
                node_id: id(0),
                fusion_kind: FusionKind::Pointwise,
                body: MeshSpatialMask::WGSL_BODY.unwrap(),
                params: MeshSpatialMask::PARAMS,
                inputs: vec![InputSource::External(0), InputSource::External(1)],
                input_access: vec![InputAccess::BufferGather, InputAccess::BufferGather],
                node_inputs: MeshSpatialMask::INPUTS,
                node_outputs: MeshSpatialMask::OUTPUTS,
                node_includes: MeshSpatialMask::WGSL_INCLUDES,
                derived_uniforms: MeshSpatialMask::DERIVED_UNIFORMS,
                type_id: MeshSpatialMask::TYPE_ID.to_string(),
                derived_camera_ext: None,
                output_storage: "rgba32float",
                stencil_fetch: false,
                quantize_f16: false,
            }],
            num_external_inputs: 2,
            outputs: vec![(id(0), "weights".to_string())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: Vec::new(),
            sampled_externals: Vec::new(),
            camera_externals: 0,
        };
        let g = generate_fused(&region).expect("the gathered mask kernel fuses");
        assert!(
            naga::front::wgsl::parse_str(&g.wgsl).is_ok(),
            "fused gathered mask kernel parses:\n{}",
            g.wgsl
        );
        let prim = MeshSpatialMask::new();
        let node: &dyn crate::node_graph::effect_node::EffectNode = &prim;
        assert_eq!(
            node.array_output_capacity("weights", &Default::default(), &[("in", 1009), ("weights", 2009)]),
            Some(1009),
            "identity capacity — passes the gather identity probe"
        );
        assert!(
            !crate::node_graph::freeze::derived_uniform_registry::has_recompute(
                MeshSpatialMask::TYPE_ID
            ),
            "weights_len has no registered recompute — install refuses the region"
        );
    }
}
