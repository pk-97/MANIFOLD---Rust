//! `node.particles_to_copies` — one copy transform per liquid particle, so any
//! particle frame of the seam can be drawn as copies of a mesh.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::mesh::InstanceTransform;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct Uniforms {
    pub(crate) live_count: f32,
    pub(crate) dispatch_count: u32,
    pub(crate) _pad0: u32,
    pub(crate) _pad1: u32,
}

manifold_node_engine::primitive! {
    name: ParticlesToCopies,
    type_id: "node.particles_to_copies",
    purpose: "Turn each liquid particle into a copy transform: positioned at the particle, uniformly scaled by its radius, unrotated. Unused particle slots (radius 0) and slots at or past the live count become zero-scale holes.",
    inputs: {
        particles: Array(FluidParticle) required,
        live_count: ScalarF32 optional,
    },
    outputs: {
        copies: Array(InstanceTransform),
    },
    params: [
        ParamDef { name: Cow::Borrowed("live_count"), label: "Live Count", ty: ParamType::Float, default: ParamValue::Float(-1.0), range: Some((-1.0, 16_777_216.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire live_count from the frame producer's count (a liquid frame's count_b, a matter frame's count) so stale records past it never draw; -1 converts every slot. Output capacity follows particles. Draw with node.render_instanced_3d_mesh and a unit-radius sphere so each copy is its particle's size.",
    examples: [],
    picker: { label: "Particles To Copies", category: Atom },
    summary: "Places a copy of a shape at every liquid particle, sized by the particle, so you can see the particles themselves.",
    category: Particles3D,
    role: Map,
    aliases: ["particle view", "particles to instances", "draw particles", "liquid particles"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/particles_to_copies_body.wgsl"),
}

impl Primitive for ParticlesToCopies {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "copies" {
            return None;
        }
        input_capacities
            .iter()
            .find(|(name, _)| *name == "particles")
            .map(|&(_, capacity)| capacity)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let live_count = ctx.scalar_or_param("live_count", -1.0);
        let (Some(particles), Some(copies)) = (ctx.inputs.array("particles"), ctx.outputs.array("copies")) else {
            return;
        };
        let count = ((particles.size / std::mem::size_of::<FluidParticle>() as u64) as u32)
            .min((copies.size / std::mem::size_of::<InstanceTransform>() as u64) as u32);
        if count == 0 {
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = Uniforms { live_count, dispatch_count: count, _pad0: 0, _pad1: 0 };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: particles, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: copies, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.particles_to_copies",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use manifold_node_engine::exec::effect_node::NodeInstanceId;
    use manifold_node_engine::freeze::classify::FusionKind;
    use manifold_node_engine::freeze::codegen::{ENTRY, FusionRegion, InputSource, RegionNode, generate_fused, standalone_for_spec};
    use manifold_node_engine::primitive::PrimitiveSpec;
    use crate::node_graph::primitives::displace_copies::DisplaceCopies;
    use manifold_gpu::{GpuBuffer, GpuDevice};

    fn particles() -> Vec<FluidParticle> {
        vec![
            FluidParticle { position_radius: [0.5, 1.25, -2.0, 0.03], velocity: [1.0, 2.0, 3.0], id: 1 },
            FluidParticle { position_radius: [-1.0, 0.0, 4.5, 0.0], velocity: [0.0; 3], id: 0 },
            FluidParticle { position_radius: [2.0, -3.0, 0.25, 0.05], velocity: [-1.0, 0.0, 0.5], id: 3 },
            FluidParticle { position_radius: [7.0, 7.0, 7.0, 0.04], velocity: [0.0; 3], id: 9 },
        ]
    }

    /// CPU statement of the contract.
    fn expected(src: &[FluidParticle], live_count: f32) -> Vec<InstanceTransform> {
        src.iter()
            .enumerate()
            .map(|(i, p)| {
                if live_count >= 0.0 && i as f32 >= live_count {
                    InstanceTransform { pos_scale: [0.0; 4], rot_pad: [0.0; 4] }
                } else {
                    InstanceTransform { pos_scale: p.position_radius, rot_pad: [0.0; 4] }
                }
            })
            .collect()
    }

    fn raw(copies: &[InstanceTransform]) -> Vec<[[f32; 4]; 2]> {
        copies.iter().map(|c| [c.pos_scale, c.rot_pad]).collect()
    }

    fn shared<T: bytemuck::Pod>(device: &GpuDevice, data: &[T]) -> GpuBuffer {
        let buffer = device.create_buffer_shared(std::mem::size_of_val(data).max(16) as u64);
        // SAFETY: shared storage, no GPU work in flight.
        unsafe { buffer.write(0, bytemuck::cast_slice(data)) };
        buffer
    }

    fn read<T: bytemuck::Pod>(buffer: &GpuBuffer, n: usize) -> Vec<T> {
        let ptr = buffer.mapped_ptr().expect("shared storage");
        // SAFETY: the dispatch completed; `n` whole elements fit the buffer.
        unsafe { std::slice::from_raw_parts(ptr.cast::<T>().cast_const(), n).to_vec() }
    }

    fn standalone(device: &GpuDevice, src: &[FluidParticle], live_count: f32) -> Vec<InstanceTransform> {
        let pipeline = device.create_compute_pipeline(&standalone_for_spec::<ParticlesToCopies>().unwrap(), ENTRY, "particles-to-copies");
        let input = shared(device, src);
        let out = device.create_buffer_shared((src.len() * std::mem::size_of::<InstanceTransform>()) as u64);
        let uniforms = Uniforms { live_count, dispatch_count: src.len() as u32, _pad0: 0, _pad1: 0 };
        let mut enc = device.create_encoder("particles-to-copies");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &input, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &out, offset: 0 },
            ],
            [1, 1, 1],
            "particles-to-copies",
        );
        enc.commit_and_wait_completed();
        read(&out, src.len())
    }

    #[test]
    fn particles_to_copies_matches_cpu() {
        let device = manifold_gpu::testkit::test_device();
        let src = particles();
        for live_count in [-1.0, 3.0, 0.0] {
            assert_eq!(raw(&standalone(&device, &src, live_count)), raw(&expected(&src, live_count)), "live_count {live_count}");
        }
    }

    /// particles_to_copies → displace_copies in one fused region matches the two
    /// standalone kernels and the CPU.
    #[test]
    fn particles_to_copies_fused_with_displace_matches_unfused() {
        let device = manifold_gpu::testkit::test_device();
        let src = particles();
        let weights = [1.0_f32, 0.5, -2.0, 4.0];
        let (live_count, amount, direction) = (3.0_f32, 0.25_f32, [0.0_f32, 1.0, 0.5]);
        let node = |n: u32, type_id: &str, body, params, inputs, input_access, node_inputs, node_outputs, node_includes, derived_uniforms| RegionNode {
            node_id: NodeInstanceId(n),
            fusion_kind: FusionKind::Pointwise,
            body,
            params,
            inputs,
            input_access,
            node_inputs,
            node_outputs,
            node_includes,
            derived_uniforms,
            type_id: type_id.to_string(),
            derived_camera_ext: None,
            output_storage: "rgba16float",
            stencil_fetch: false,
            quantize_f16: false,
        };
        let region = FusionRegion {
            nodes: vec![
                node(
                    0,
                    ParticlesToCopies::TYPE_ID,
                    ParticlesToCopies::WGSL_BODY.unwrap(),
                    ParticlesToCopies::PARAMS,
                    vec![InputSource::External(0)],
                    ParticlesToCopies::INPUT_ACCESS.to_vec(),
                    ParticlesToCopies::INPUTS,
                    ParticlesToCopies::OUTPUTS,
                    ParticlesToCopies::WGSL_INCLUDES,
                    ParticlesToCopies::DERIVED_UNIFORMS,
                ),
                node(
                    1,
                    DisplaceCopies::TYPE_ID,
                    DisplaceCopies::WGSL_BODY.unwrap(),
                    DisplaceCopies::PARAMS,
                    vec![InputSource::Node(NodeInstanceId(0)), InputSource::External(1)],
                    DisplaceCopies::INPUT_ACCESS.to_vec(),
                    DisplaceCopies::INPUTS,
                    DisplaceCopies::OUTPUTS,
                    DisplaceCopies::WGSL_INCLUDES,
                    DisplaceCopies::DERIVED_UNIFORMS,
                ),
            ],
            num_external_inputs: 2,
            outputs: vec![(NodeInstanceId(1), "instances".to_string())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: Vec::new(),
            sampled_externals: Vec::new(),
            camera_externals: 0,
            output_capacity: None,
        };
        let fused = generate_fused(&region).expect("particles_to_copies → displace_copies fuses");
        assert!(naga::front::wgsl::parse_str(&fused.wgsl).is_ok(), "fused WGSL parses:\n{}", fused.wgsl);

        // Unfused: the two standalone kernels.
        let copies = standalone(&device, &src, live_count);
        let copies_buf = shared(&device, &copies);
        let weights_buf = shared(&device, &weights);
        let unfused_out = device.create_buffer_shared(std::mem::size_of_val(copies.as_slice()) as u64);
        let displace = device.create_compute_pipeline(&standalone_for_spec::<DisplaceCopies>().unwrap(), ENTRY, "displace");
        let du = super::super::displace_copies::Uniforms {
            amount,
            direction_x: direction[0],
            direction_y: direction[1],
            direction_z: direction[2],
            dispatch_count: src.len() as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let mut enc = device.create_encoder("displace");
        enc.dispatch_compute(
            &displace,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&du) },
                GpuBinding::Buffer { binding: 1, buffer: &copies_buf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &weights_buf, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: &unfused_out, offset: 0 },
            ],
            [1, 1, 1],
            "displace",
        );
        enc.commit_and_wait_completed();
        let unfused: Vec<InstanceTransform> = read(&unfused_out, src.len());

        // Fused: one kernel, uniforms in the region's parameter order.
        let mut words: Vec<u32> = Vec::new();
        for &(node, name) in &fused.param_order {
            let value = match (node.0, name) {
                (0, "live_count") => live_count,
                (1, "amount") => amount,
                (1, "direction_x") => direction[0],
                (1, "direction_y") => direction[1],
                (1, "direction_z") => direction[2],
                other => panic!("unexpected fused param {other:?}"),
            };
            words.push(value.to_bits());
        }
        while !words.len().is_multiple_of(4) {
            words.push(0);
        }
        let particles_buf = shared(&device, &src);
        let fused_out = device.create_buffer_shared(std::mem::size_of_val(copies.as_slice()) as u64);
        let pipeline = device.create_compute_pipeline(&fused.wgsl, ENTRY, "particles-copies-fused");
        let mut enc = device.create_encoder("fused");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::cast_slice(&words) },
                GpuBinding::Buffer { binding: 1, buffer: &particles_buf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &weights_buf, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: &fused_out, offset: 0 },
            ],
            [1, 1, 1],
            "fused",
        );
        enc.commit_and_wait_completed();
        let fused_result: Vec<InstanceTransform> = read(&fused_out, src.len());

        let cpu: Vec<InstanceTransform> = expected(&src, live_count)
            .into_iter()
            .zip(weights)
            .map(|(c, w)| {
                if amount == 0.0 || c.pos_scale[3] == 0.0 {
                    return c;
                }
                let mut out = c;
                for (axis, d) in direction.iter().enumerate() {
                    out.pos_scale[axis] += amount * w * d;
                }
                out
            })
            .collect();
        assert_eq!(raw(&unfused), raw(&cpu), "standalone chain against CPU");
        assert_eq!(raw(&fused_result), raw(&unfused), "fused against standalone");
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
