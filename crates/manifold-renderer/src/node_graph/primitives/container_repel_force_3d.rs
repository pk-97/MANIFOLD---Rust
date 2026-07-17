//! `node.push_from_walls_3d` — soft container-boundary repulsion
//! added in place to an `Array<[f32; 3]>` force buffer.
//!
//! The pre-integration "cushion" half of the FluidSim3D container
//! behaviour: when a particle is within a small margin of the container
//! wall, a gentle inward force pushes it back along the SDF's outward
//! normal, preventing pile-up at the boundary. Bit-exact with the soft
//! boundary repulsion in the legacy fused `node.fluid_simulate_3d`.
//!
//! Distinct from `node.keep_in_box_3d`, which applies the
//! *post-integration* hard containment (toroidal wrap or SDF reflect +
//! clamp). Both read the same `container` enum (None / Cube / Sphere /
//! Torus); compose this one before `node.move_particles_3d` and
//! the bounds atom after it.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Container SDF modes — shared label set with `node.keep_in_box_3d`
/// and the legacy fluid_simulate_3d / fluid_seed_3d.
pub const CONTAINER_3D_MODES: &[&str] = &["None", "Cube", "Sphere", "Torus"];

/// Generated-codegen uniform layout: scalar params in PARAMS order (`container`
/// Enum → u32, `ctr_scale` f32, `active_count` Int → i32) then the codegen-
/// injected `dispatch_count`. 4 words = 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RepelUniforms {
    container: u32,
    ctr_scale: f32,
    active_count: i32,
    dispatch_count: u32,
}

crate::primitive! {
    name: ContainerRepelForce3D,
    type_id: "node.push_from_walls_3d",
    purpose: "Soft container-boundary repulsion added in-place to an Array<[f32; 3]> force buffer. When a particle is within a 0.1 margin of the container SDF (Cube/Sphere/Torus), a gentle inward force `n * (t*t*0.15)` cushions it back along the outward normal, preventing wall pile-up. container = None disables it. The pre-integration cushion half of the FluidSim3D container behaviour (the post-integration hard wall is node.keep_in_box_3d). Decomposed from the fused node.fluid_simulate_3d.",
    inputs: {
        in: Array([f32; 3]) required,
        particles: Array(Particle) required,
        ctr_scale: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array([f32; 3]),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("container"),
            label: "Container",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: CONTAINER_3D_MODES,
        },
        ParamDef {
            name: Cow::Borrowed("ctr_scale"),
            label: "Container Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(0.8),
            range: Some((0.2, 1.0)),
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
    ],
    depth_rule: Terminal,
    composition_notes: "Aliased Array<[f32; 3]> in/out (one force buffer, in-place subtract). `container` is a mode enum (0 None / 1 Cube / 2 Sphere / 3 Torus) — None makes the dispatch a no-op so the atom is inert at the default. `ctr_scale` is port-shadow (sizes the SDF). Wire upstream of node.move_particles_3d so the cushion is integrated through speed*dt, and pair with node.keep_in_box_3d (post-integration hard wall) downstream.",
    examples: ["FluidSim3D"],
    picker: { label: "Push From Walls (3D)", category: Atom },
    summary: "Pushes 3D particles gently away from the walls of their container as they get close, keeping them inside without a hard bounce.",
    category: Particles3D,
    role: Filter,
    aliases: ["push from walls", "container repel force 3d", "repel", "boundary", "container"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/container_repel_force_3d_body.wgsl"),
}

impl Primitive for ContainerRepelForce3D {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities
                .iter()
                .find(|(p, _)| *p == "in")
                .map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("in", "out")]
    }

    // run() dispatches `active_count` threads, not pool capacity — a fused
    // region containing this atom caps its dispatch the same way.
    fn fused_dispatch_count_param(&self) -> Option<&'static str> {
        Some("active_count")
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let container = match ctx.params.get("container") {
            Some(ParamValue::Enum(n)) => *n,
            Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
            _ => 0,
        };
        let ctr_scale = ctx.scalar_or_param("ctr_scale", 0.8);
        let active_count = ctx
            .scalar_or_param("active_count", 100_000.0)
            .round()
            .max(0.0) as u32;

        let Some(in_forces) = ctx.inputs.array("in") else {
            return;
        };
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let _ = out;

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let particle_capacity = (particles.size / particle_size) as u32;
        let force_capacity = (in_forces.size / 12) as u32;
        let active_count = active_count.min(particle_capacity).min(force_capacity);
        if active_count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // multi-input coincident; SDF helpers inlined).
            // container_repel_force_3d.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.push_from_walls_3d standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.push_from_walls_3d",
            )
        });

        let uniforms = RepelUniforms {
            container,
            ctr_scale,
            active_count: active_count as i32,
            dispatch_count: active_count,
        };

        // Generated binding order follows INPUTS: `in` (force) → buf_in(1),
        // `particles` → buf_particles(2), output → buf_out(3). `in`/`out` alias
        // the force buffer, so bind it to BOTH 1 and 3; particles to 2.
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: in_forces,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: in_forces,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.push_from_walls_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_aliased_vec3_in_out_required_particles() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let vec3_layout = ArrayType::of_known::<[f32; 3]>();

        assert_eq!(
            ContainerRepelForce3D::TYPE_ID,
            "node.push_from_walls_3d"
        );
        let names: Vec<&str> = ContainerRepelForce3D::INPUTS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(names, vec!["in", "particles", "ctr_scale", "active_count"]);
        assert_eq!(
            ContainerRepelForce3D::INPUTS[0].ty,
            PortType::Array(vec3_layout)
        );
        assert!(ContainerRepelForce3D::INPUTS[0].required);

        assert_eq!(ContainerRepelForce3D::OUTPUTS.len(), 1);
        assert_eq!(
            ContainerRepelForce3D::OUTPUTS[0].ty,
            PortType::Array(vec3_layout)
        );

        let prim = ContainerRepelForce3D::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn has_four_container_modes() {
        assert_eq!(CONTAINER_3D_MODES, &["None", "Cube", "Sphere", "Torus"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ContainerRepelForce3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.push_from_walls_3d");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain multi-input coincident parity oracle (freeze §12) — the
    //! generated kernel reads the force `in` + `particles`, adds the SDF-wall
    //! cushion to the force, writes it back (aliased). Must reproduce the hand
    //! kernel force-for-force across the SDF container modes. The hand binds
    //! particles@1/forces@2; the generated binds forces@1/particles@2/forces@3.
    use super::*;
    use crate::generators::compute_common::Particle;

    fn dispatch_repel(
        wgsl: &str,
        forces: &[[f32; 3]],
        particles: &[Particle],
        uniform: &[u8],
        count: u32,
        generated: bool,
    ) -> Vec<[f32; 3]> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "repel-oracle");
        let f_buf = device.create_buffer_shared(std::mem::size_of_val(forces) as u64);
        let p_buf = device.create_buffer_shared(std::mem::size_of_val(particles) as u64);
        unsafe {
            f_buf.write(0, bytemuck::cast_slice(forces));
            p_buf.write(0, bytemuck::cast_slice(particles));
        }
        let mut bindings = vec![GpuBinding::Bytes { binding: 0, data: uniform }];
        if generated {
            bindings.push(GpuBinding::Buffer { binding: 1, buffer: &f_buf, offset: 0 });
            bindings.push(GpuBinding::Buffer { binding: 2, buffer: &p_buf, offset: 0 });
            bindings.push(GpuBinding::Buffer { binding: 3, buffer: &f_buf, offset: 0 });
        } else {
            bindings.push(GpuBinding::Buffer { binding: 1, buffer: &p_buf, offset: 0 });
            bindings.push(GpuBinding::Buffer { binding: 2, buffer: &f_buf, offset: 0 });
        }
        let mut enc = device.create_encoder("repel-oracle");
        enc.dispatch_compute(&pipeline, &bindings, [count.div_ceil(256), 1, 1], "repel-oracle");
        enc.commit_and_wait_completed();
        let ptr = f_buf.mapped_ptr().expect("shared force buffer");
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const [f32; 3], forces.len()) };
        slice.to_vec()
    }

    #[test]
    fn generated_repel_matches_hand_kernel_all_modes() {
        let mk = |pos: [f32; 3], life: f32| Particle {
            position: pos,
            _pad0: 0.0,
            velocity: [0.0; 3],
            life,
            age: 0.0,
            _pad1: [0.0; 3],
            color: [0.0; 4],
        };
        // Positions near / past the container walls so the repel triggers.
        let particles = [
            mk([0.88, 0.5, 0.5], 1.0),
            mk([0.5, 0.12, 0.5], 1.0),
            mk([0.5, 0.5, 0.5], 1.0),   // centre → no repel
            mk([0.92, 0.92, 0.92], 0.0), // dead → unchanged
        ];
        let forces: [[f32; 3]; 4] =
            [[0.1, 0.2, 0.3], [-0.1, 0.0, 0.2], [0.05, 0.05, 0.05], [0.9, 0.9, 0.9]];
        let n = particles.len() as u32;
        let ctr_scale = 0.8f32;

        for container in 1u32..4u32 {
            // Hand layout: active_count(u32), container(u32), ctr_scale(f32), pad.
            let mut hand = Vec::new();
            hand.extend_from_slice(&n.to_le_bytes());
            hand.extend_from_slice(&container.to_le_bytes());
            hand.extend_from_slice(&ctr_scale.to_le_bytes());
            hand.extend_from_slice(&0u32.to_le_bytes());

            // Generated layout: container(u32), ctr_scale(f32), active_count(i32), dispatch_count(u32).
            let mut gen_bytes = Vec::new();
            gen_bytes.extend_from_slice(&container.to_le_bytes());
            gen_bytes.extend_from_slice(&ctr_scale.to_le_bytes());
            gen_bytes.extend_from_slice(&(n as i32).to_le_bytes());
            gen_bytes.extend_from_slice(&n.to_le_bytes());

            let hand_wgsl = include_str!("shaders/container_repel_force_3d.wgsl");
            let gen_wgsl =
                crate::node_graph::freeze::codegen::standalone_for_spec::<ContainerRepelForce3D>()
                    .expect("container_repel_force_3d buffer codegen");

            let from_hand = dispatch_repel(hand_wgsl, &forces, &particles, &hand, n, false);
            let from_gen = dispatch_repel(&gen_wgsl, &forces, &particles, &gen_bytes, n, true);

            for i in 0..forces.len() {
                for c in 0..3 {
                    assert!(
                        (from_hand[i][c] - from_gen[i][c]).abs() < 1e-6,
                        "container {container} force {i}[{c}]: hand={} gen={}",
                        from_hand[i][c],
                        from_gen[i][c]
                    );
                }
            }
        }
    }
}
