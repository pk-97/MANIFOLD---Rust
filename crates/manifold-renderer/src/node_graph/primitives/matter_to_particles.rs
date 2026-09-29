//! `node.matter_to_particles` — matter points as seam particle records, index
//! for index (`docs/GPU_MPM_SOLVER_DESIGN.md` D6: the cell sort reads the
//! seam's record, so its order indexes the matter points).

use manifold_gpu::GpuBinding;

use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::matter::MatterPoint;
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct Uniforms {
    pub(crate) dispatch_count: u32,
    pub(crate) _pad0: u32,
    pub(crate) _pad1: u32,
    pub(crate) _pad2: u32,
}

crate::primitive! {
    name: MatterToParticles,
    type_id: "node.matter_to_particles",
    purpose: "Write each matter point as a liquid particle record at the same index: position, velocity, id, and the radius of its rest-volume sphere. Removed points and points with a non-finite position get radius 0, the unused-slot mark.",
    inputs: {
        points: Array(MatterPoint) required,
    },
    outputs: {
        particles: Array(FluidParticle),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Output capacity follows points and index i always describes point i, so a sort of the output (node.sort_particles_into_cells) orders the matter points themselves. Reads node.matter_state's out; slots past the live count hold removed points and come out with radius 0.",
    examples: [],
    picker: { label: "Matter To Particles", category: Atom },
    summary: "Turns the simulated matter into plain liquid particles without reordering them.",
    category: Particles3D,
    role: Map,
    aliases: ["matter particles", "points to particles", "matter view"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/matter_to_particles_body.wgsl"),
}

impl Primitive for MatterToParticles {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "particles" {
            return None;
        }
        input_capacities.iter().find(|(name, _)| *name == "points").map(|&(_, n)| n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(points), Some(particles)) = (ctx.inputs.array("points"), ctx.outputs.array("particles")) else {
            return;
        };
        let count = ((points.size / std::mem::size_of::<MatterPoint>() as u64) as u32)
            .min((particles.size / std::mem::size_of::<FluidParticle>() as u64) as u32);
        if count == 0 {
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = Uniforms { dispatch_count: count, _pad0: 0, _pad1: 0, _pad2: 0 };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: points, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: particles, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.matter_to_particles",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::generators::mesh_common::InstanceTransform;
    use crate::node_graph::effect_node::NodeInstanceId;
    use crate::node_graph::freeze::classify::FusionKind;
    use crate::node_graph::freeze::codegen::{
        ENTRY, FusionRegion, InputSource, RegionNode, generate_fused, standalone_for_spec,
    };
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::primitives::ParticlesToCopies;
    use manifold_gpu::{GpuBuffer, GpuDevice};

    fn points() -> Vec<MatterPoint> {
        let point = |position: [f32; 3], id: u32, v0: f32| MatterPoint {
            position,
            id,
            velocity: [0.5, -1.0, 2.0],
            volume_ratio: 1.02,
            affine_x: [0.1, 0.2, 0.3, 1.0],
            affine_y: [0.4, 0.5, 0.6, v0],
            affine_z: [0.7, 0.8, 0.9, 0.0],
        };
        vec![
            point([0.25, 1.5, -0.75], 7, 3.0e-5),
            point([1.0, 2.0, 3.0], 0, 3.0e-5),
            point([f32::NAN, 0.0, 0.0], 9, 3.0e-5),
            point([-2.0, 0.125, 0.5], 11, 2.4e-4),
        ]
    }

    /// CPU statement of the contract.
    fn expected(src: &[MatterPoint]) -> Vec<FluidParticle> {
        let scale = (3.0 / (4.0 * std::f32::consts::PI)).cbrt();
        src.iter()
            .map(|p| {
                if p.id == 0 || p.position.iter().any(|v| !v.is_finite()) {
                    FluidParticle::default()
                } else {
                    FluidParticle {
                        position_radius: [p.position[0], p.position[1], p.position[2], scale * p.affine_y[3].cbrt()],
                        velocity: p.velocity,
                        id: p.id,
                    }
                }
            })
            .collect()
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

    fn close(a: &FluidParticle, b: &FluidParticle) -> bool {
        a.id == b.id
            && a.velocity == b.velocity
            && a.position_radius[..3] == b.position_radius[..3]
            && (a.position_radius[3] - b.position_radius[3]).abs() <= 1e-6 * b.position_radius[3].max(1e-3)
    }

    #[test]
    fn matter_to_particles_matches_cpu() {
        let device = crate::test_device();
        let src = points();
        let pipeline = device.create_compute_pipeline(&standalone_for_spec::<MatterToParticles>().unwrap(), ENTRY, "matter-to-particles");
        let input = shared(&device, &src);
        let out = device.create_buffer_shared((src.len() * std::mem::size_of::<FluidParticle>()) as u64);
        let uniforms = Uniforms { dispatch_count: src.len() as u32, _pad0: 0, _pad1: 0, _pad2: 0 };
        let mut enc = device.create_encoder("matter-to-particles");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &input, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &out, offset: 0 },
            ],
            [1, 1, 1],
            "matter-to-particles",
        );
        enc.commit_and_wait_completed();
        let got: Vec<FluidParticle> = read(&out, src.len());
        for (i, (g, e)) in got.iter().zip(expected(&src)).enumerate() {
            assert!(close(g, &e), "slot {i}: {g:?} vs {e:?}");
        }
    }

    /// matter_to_particles → particles_to_copies fuse into one kernel that
    /// matches the CPU contract of both.
    #[test]
    fn matter_to_particles_fused_with_copies_matches_unfused() {
        let device = crate::test_device();
        let src = points();
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
                    MatterToParticles::TYPE_ID,
                    MatterToParticles::WGSL_BODY.unwrap(),
                    MatterToParticles::PARAMS,
                    vec![InputSource::External(0)],
                    MatterToParticles::INPUT_ACCESS.to_vec(),
                    MatterToParticles::INPUTS,
                    MatterToParticles::OUTPUTS,
                    MatterToParticles::WGSL_INCLUDES,
                    MatterToParticles::DERIVED_UNIFORMS,
                ),
                node(
                    1,
                    ParticlesToCopies::TYPE_ID,
                    ParticlesToCopies::WGSL_BODY.unwrap(),
                    ParticlesToCopies::PARAMS,
                    vec![InputSource::Node(NodeInstanceId(0))],
                    ParticlesToCopies::INPUT_ACCESS.to_vec(),
                    ParticlesToCopies::INPUTS,
                    ParticlesToCopies::OUTPUTS,
                    ParticlesToCopies::WGSL_INCLUDES,
                    ParticlesToCopies::DERIVED_UNIFORMS,
                ),
            ],
            num_external_inputs: 1,
            outputs: vec![(NodeInstanceId(1), "copies".to_string())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: Vec::new(),
            sampled_externals: Vec::new(),
            camera_externals: 0,
            output_capacity: None,
        };
        let fused = generate_fused(&region).expect("matter_to_particles → particles_to_copies fuses");
        assert!(naga::front::wgsl::parse_str(&fused.wgsl).is_ok(), "fused WGSL parses:\n{}", fused.wgsl);
        let mut words: Vec<u32> = fused
            .param_order
            .iter()
            .map(|&(node, name)| match (node.0, name) {
                (1, "live_count") => (-1.0f32).to_bits(),
                other => panic!("unexpected fused param {other:?}"),
            })
            .collect();
        while !words.len().is_multiple_of(4) {
            words.push(0);
        }
        let input = shared(&device, &src);
        let out = device.create_buffer_shared((src.len() * std::mem::size_of::<InstanceTransform>()) as u64);
        let pipeline = device.create_compute_pipeline(&fused.wgsl, ENTRY, "matter-copies-fused");
        let mut enc = device.create_encoder("matter-copies-fused");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::cast_slice(&words) },
                GpuBinding::Buffer { binding: 1, buffer: &input, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &out, offset: 0 },
            ],
            [1, 1, 1],
            "matter-copies-fused",
        );
        enc.commit_and_wait_completed();
        let got: Vec<InstanceTransform> = read(&out, src.len());
        for (i, (g, e)) in got.iter().zip(expected(&src)).enumerate() {
            let want = e.position_radius;
            let ok = g.rot_pad == [0.0; 4]
                && g.pos_scale[..3] == want[..3]
                && (g.pos_scale[3] - want[3]).abs() <= 1e-6 * want[3].max(1e-3);
            assert!(ok, "slot {i}: {:?} vs {want:?}", g.pos_scale);
        }
    }
}
