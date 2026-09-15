//! `node.mesh_stagger_envelope` — an order-driven attack/hold/release weight
//! envelope over a mesh, with optional incoming per-vertex weights.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::standalone_pipeline::standalone_pipeline;
use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const SAMPLE_MODES: &[&str] = &["Vertex", "Triangle Centroid"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MeshStaggerEnvelopeUniforms {
    sample_mode: u32,
    elapsed_beats: f32,
    attack_beats: f32,
    hold_beats: f32,
    release_beats: f32,
    stagger_beats: f32,
    amount: f32,
    yaw: f32,
    pitch: f32,
    scale: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    weights_len: u32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: MeshStaggerEnvelope,
    type_id: "node.mesh_stagger_envelope",
    purpose: "Generate an attack/hold/release weight envelope ordered by scene-relative mesh position. Order is clamp(0.5 + 0.5 * dot((sample_position + source_offset) / scene_radius, direction), 0, 1), where direction is the rotated Y axis; stagger_beats delays later positions. Vertex or triangle-centroid sampling is selectable, amount blends from identity to the envelope, and an optional incoming weights array multiplies the result.",
    inputs: {
        in: Array(MeshVertex) required,
        weights: Array(f32) optional,
        elapsed_beats: ScalarF32 optional,
        attack_beats: ScalarF32 optional,
        hold_beats: ScalarF32 optional,
        release_beats: ScalarF32 optional,
        stagger_beats: ScalarF32 optional,
        amount: ScalarF32 optional,
        yaw: ScalarF32 optional,
        pitch: ScalarF32 optional,
        scale: ScalarF32 optional,
        source_offset_x: ScalarF32 optional,
        source_offset_y: ScalarF32 optional,
        source_offset_z: ScalarF32 optional,
    },
    outputs: { weights: Array(f32), },
    params: [
        ParamDef { name: Cow::Borrowed("sample_mode"), label: "Sample Mode", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 1.0)), enum_values: SAMPLE_MODES },
        ParamDef { name: Cow::Borrowed("elapsed_beats"), label: "Elapsed Beats", ty: ParamType::Float, default: ParamValue::Float(-1.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("attack_beats"), label: "Attack Beats", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("hold_beats"), label: "Hold Beats", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("release_beats"), label: "Release Beats", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("stagger_beats"), label: "Stagger Beats", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("amount"), label: "Amount", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("yaw"), label: "Yaw", ty: ParamType::Angle, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("pitch"), label: "Pitch", ty: ParamType::Angle, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("scale"), label: "Scene Radius", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.000001, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_x"), label: "Source Offset X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_y"), label: "Source Offset Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_z"), label: "Source Offset Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Use this after node.mesh_spatial_mask or node.mesh_ramp to gate a geometry response in a deterministic directional order. `amount=0` preserves incoming weights (one when unwired), including the explicit idle state `elapsed_beats < 0`; `amount=1` applies the full envelope and intermediate values blend between them. Attack, hold, and release are linear and treat each zero-duration segment exactly. `sample_mode=Triangle Centroid` duplicates one envelope value across each triangle's three corners. The operation has no internal clock; elapsed_beats is a direct scalar input. All float controls are scalar-shadowed.",
    examples: [],
    picker: { label: "Mesh Stagger Envelope", category: Atom },
    summary: "Turns elapsed beats into a directional attack/hold/release weight that arrives across a mesh in order.",
    category: Geometry3D,
    role: Source,
    aliases: ["mesh envelope", "stagger envelope", "mesh attack release", "ordered reveal", "mesh gate"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/mesh_stagger_envelope_body.wgsl"),
    input_access: [BufferGather, BufferGather],
    derived_uniforms: ["weights_len:u32"],
}

impl Primitive for MeshStaggerEnvelope {
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
        let sample_mode = match ctx.params.get("sample_mode") {
            Some(ParamValue::Enum(v)) => (*v).min(1),
            _ => 0,
        };
        let elapsed_beats = ctx.scalar_or_param("elapsed_beats", -1.0);
        let attack_beats = ctx.scalar_or_param("attack_beats", 0.0);
        let hold_beats = ctx.scalar_or_param("hold_beats", 0.0);
        let release_beats = ctx.scalar_or_param("release_beats", 1.0);
        let stagger_beats = ctx.scalar_or_param("stagger_beats", 0.0);
        let amount = ctx.scalar_or_param("amount", 1.0);
        let yaw = ctx.scalar_or_param("yaw", 0.0);
        let pitch = ctx.scalar_or_param("pitch", 0.0);
        let scale = ctx.scalar_or_param("scale", 1.0);
        let source_offset_x = ctx.scalar_or_param("source_offset_x", 0.0);
        let source_offset_y = ctx.scalar_or_param("source_offset_y", 0.0);
        let source_offset_z = ctx.scalar_or_param("source_offset_z", 0.0);
        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let weights_wired = ctx.inputs.array("weights");
        let weights_buf = weights_wired.unwrap_or(src);
        let Some(dst) = ctx.outputs.array("weights") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let count = ((src.size / vertex_size) as u32).min((dst.size / 4) as u32);
        if count == 0 {
            return;
        }
        let weights_len = weights_wired.map(|b| (b.size / 4) as u32).unwrap_or(0);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = MeshStaggerEnvelopeUniforms {
            sample_mode,
            elapsed_beats,
            attack_beats,
            hold_beats,
            release_beats,
            stagger_beats,
            amount,
            yaw,
            pitch,
            scale,
            source_offset_x,
            source_offset_y,
            source_offset_z,
            weights_len,
            dispatch_count: count,
            _pad0: 0,
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
            "node.mesh_stagger_envelope",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn mesh_stagger_envelope_declares_optional_weights_and_capacity() {
        use crate::node_graph::effect_node::ParamValues;
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh = ArrayType::of_known::<MeshVertex>();
        let scalar = ArrayType::of_known::<f32>();
        let prim = MeshStaggerEnvelope::new();
        assert_eq!(MeshStaggerEnvelope::TYPE_ID, "node.mesh_stagger_envelope");
        assert_eq!(MeshStaggerEnvelope::INPUTS[0].ty, PortType::Array(mesh));
        assert!(MeshStaggerEnvelope::INPUTS[0].required);
        let weights = MeshStaggerEnvelope::INPUTS
            .iter()
            .find(|p| p.name == "weights")
            .unwrap();
        assert_eq!(weights.ty, PortType::Array(scalar));
        assert!(!weights.required);
        for name in [
            "elapsed_beats",
            "attack_beats",
            "hold_beats",
            "release_beats",
            "stagger_beats",
            "amount",
            "yaw",
            "pitch",
            "scale",
            "source_offset_x",
            "source_offset_y",
            "source_offset_z",
        ] {
            let port = MeshStaggerEnvelope::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap();
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
            assert!(!port.required);
        }
        assert_eq!(MeshStaggerEnvelope::OUTPUTS[0].ty, PortType::Array(scalar));
        assert_eq!(
            Primitive::array_output_capacity(
                &prim,
                "weights",
                &ParamValues::default(),
                &[("in", 18)]
            ),
            Some(18)
        );
    }

    #[test]
    fn mesh_stagger_envelope_registers_as_palette_atom() {
        let prim = MeshStaggerEnvelope::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.mesh_stagger_envelope");
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
        weights: Option<&[f32]>,
        u: MeshStaggerEnvelopeUniforms,
        label: &str,
    ) -> Vec<f32> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, label);
        let src = device.create_buffer_shared(std::mem::size_of_val(src_vertices) as u64);
        unsafe {
            src.write(0, bytemuck::cast_slice(src_vertices));
        }
        let weight_buf = if let Some(weights) = weights {
            let b = device.create_buffer_shared(std::mem::size_of_val(weights) as u64);
            unsafe {
                b.write(0, bytemuck::cast_slice(weights));
            }
            b
        } else {
            device.create_buffer_shared(std::mem::size_of_val(src_vertices) as u64)
        };
        let dst = device.create_buffer_shared(src_vertices.len() as u64 * 4);
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
                    buffer: &weight_buf,
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
    fn uniforms(
        sample_mode: u32,
        elapsed_beats: f32,
        stagger_beats: f32,
        weights_len: u32,
    ) -> MeshStaggerEnvelopeUniforms {
        uniforms_with_amount(1.0, sample_mode, elapsed_beats, stagger_beats, weights_len)
    }
    fn uniforms_with_amount(
        amount: f32,
        sample_mode: u32,
        elapsed_beats: f32,
        stagger_beats: f32,
        weights_len: u32,
    ) -> MeshStaggerEnvelopeUniforms {
        MeshStaggerEnvelopeUniforms {
            sample_mode,
            elapsed_beats,
            attack_beats: 0.0,
            hold_beats: 0.0,
            release_beats: 1.0,
            stagger_beats,
            amount,
            yaw: 0.0,
            pitch: 0.0,
            scale: 1.0,
            source_offset_x: 0.0,
            source_offset_y: 0.0,
            source_offset_z: 0.0,
            weights_len,
            dispatch_count: 6,
            _pad0: 0,
        }
    }

    #[test]
    fn overnight_modifier_mesh_stagger_envelope_proves_ahr_order_idle_tail_and_weights() {
        let src = vec![
            vertex([-0.2, -0.8, 0.0]),
            vertex([0.2, -0.8, 0.0]),
            vertex([0.0, -0.4, 0.0]),
            vertex([-0.2, 0.4, 0.0]),
            vertex([0.2, 0.4, 0.0]),
            vertex([0.0, 0.8, 0.0]),
        ];
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<MeshStaggerEnvelope>()
            .expect("envelope codegen");
        let idle = dispatch(
            &wgsl,
            &src,
            None,
            uniforms(0, -1.0, 1.0, 0),
            "envelope-idle",
        );
        assert!(idle.iter().all(|v| *v == 0.0));
        let idle_passthrough = dispatch(
            &wgsl,
            &src,
            Some(&[0.25, 0.5, 0.75, 1.0, 0.125, 0.875]),
            uniforms_with_amount(0.0, 0, -1.0, 1.0, 6),
            "envelope-idle-passthrough",
        );
        assert_eq!(idle_passthrough, vec![0.25, 0.5, 0.75, 1.0, 0.125, 0.875]);
        let ordered = dispatch(
            &wgsl,
            &src,
            None,
            uniforms(0, 0.5, 1.0, 0),
            "envelope-ordered",
        );
        assert!(ordered[0] > 0.0 && ordered[0] < 1.0);
        assert_eq!(ordered[3], 0.0);
        let full = dispatch(
            &wgsl,
            &src,
            None,
            uniforms_with_amount(1.0, 0, 0.5, 1.0, 0),
            "envelope-full",
        );
        let half = dispatch(
            &wgsl,
            &src,
            None,
            uniforms_with_amount(0.5, 0, 0.5, 1.0, 0),
            "envelope-half",
        );
        for (full, half) in full.iter().zip(half) {
            assert!((half - 0.5 * (1.0 + full)).abs() < 2e-5);
        }
        let centroid = dispatch(
            &wgsl,
            &src,
            None,
            uniforms(1, 0.5, 0.0, 0),
            "envelope-centroid",
        );
        assert_eq!(centroid[0], centroid[1]);
        assert_eq!(centroid[1], centroid[2]);
        let tail = dispatch(&wgsl, &src, None, uniforms(0, 3.0, 0.0, 0), "envelope-tail");
        assert!(tail.iter().all(|v| *v == 0.0));
        let unweighted = dispatch(
            &wgsl,
            &src,
            None,
            uniforms(0, 0.5, 0.0, 0),
            "envelope-unweighted",
        );
        let wired = dispatch(
            &wgsl,
            &src,
            Some(&[0.5; 6]),
            uniforms(0, 0.5, 0.0, 6),
            "envelope-weights",
        );
        for (a, b) in wired.iter().zip(unweighted) {
            assert!((a - b * 0.5).abs() < 2e-5);
        }
    }

    /// BUG-x72p: the `BufferGather` inputs no longer force Boundary — the
    /// codegen emits the gathered kernel, and the identity probe PASSES
    /// (`weights` capacity = the `in` capacity). What keeps the envelope
    /// unfused is its derived uniform: `weights_len:u32` has NO registered
    /// recompute, so install's `has_recompute` gate fails any region
    /// containing it closed (unfused, always correct). A future recompute
    /// registration (reading the wired `weights` length) flips this atom to
    /// fusing with no codegen change.
    #[test]
    fn overnight_modifier_mesh_stagger_envelope_refuses_at_derived_uniform_recompute() {
        let id = NodeInstanceId;
        let region = FusionRegion {
            nodes: vec![RegionNode {
                node_id: id(0),
                fusion_kind: FusionKind::Pointwise,
                body: MeshStaggerEnvelope::WGSL_BODY.unwrap(),
                params: MeshStaggerEnvelope::PARAMS,
                inputs: vec![InputSource::External(0), InputSource::External(1)],
                input_access: vec![InputAccess::BufferGather, InputAccess::BufferGather],
                node_inputs: MeshStaggerEnvelope::INPUTS,
                node_outputs: MeshStaggerEnvelope::OUTPUTS,
                node_includes: MeshStaggerEnvelope::WGSL_INCLUDES,
                derived_uniforms: MeshStaggerEnvelope::DERIVED_UNIFORMS,
                type_id: MeshStaggerEnvelope::TYPE_ID.to_string(),
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
        let g = generate_fused(&region).expect("the gathered envelope kernel fuses");
        assert!(
            naga::front::wgsl::parse_str(&g.wgsl).is_ok(),
            "fused gathered envelope kernel parses:\n{}",
            g.wgsl
        );
        let prim = MeshStaggerEnvelope::new();
        let node: &dyn crate::node_graph::effect_node::EffectNode = &prim;
        assert_eq!(
            node.array_output_capacity("weights", &Default::default(), &[("in", 1009), ("weights", 2009)]),
            Some(1009),
            "identity capacity — passes the gather identity probe"
        );
        assert!(
            !crate::node_graph::freeze::derived_uniform_registry::has_recompute(
                MeshStaggerEnvelope::TYPE_ID
            ),
            "weights_len has no registered recompute — install refuses the region"
        );
    }
}
