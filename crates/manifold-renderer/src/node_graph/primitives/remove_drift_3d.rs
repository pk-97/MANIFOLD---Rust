//! `node.remove_drift_3d` — subtract the mean force over live particles
//! from every per-particle force, so the internal field forces sum to
//! zero and the fluid stops riding a net tide into a wall.
//!
//! Root-cause fix for BUG-066 (FluidSim3D corner drift): on a discrete
//! grid the density-gradient slope force does not sum to exactly zero
//! the way the continuum math promises — a deterministic residue of
//! ~0.5% of peak force survives, and the sim's feedback loop amplifies
//! it into a slow, always-same-direction bulk drift (measured by the
//! `fluid3d_bias` harness meter, 2026-07-10). This node enforces the
//! conservation law directly. Insert AFTER the internal field forces
//! (slope sample + turbulence) and BEFORE wall/container forces, which
//! are *supposed* to exert net force.
//!
//! Three-pass dispatch (barriers between): grid-stride workgroup partial
//! sums over live particles → single-workgroup reduce to the mean →
//! elementwise subtract. Fixed tree + fixed stride = bit-deterministic.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Workgroups in the partial pass = entries in the partials scratch.
/// 256 workgroups × 256 threads grid-stride any active_count; the scratch
/// is 256 × 16 B = 4 KB, allocated once.
const NUM_PARTIALS: u32 = 256;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RemoveDriftUniforms {
    active_count: u32,
    num_partials: u32,
    amount: f32,
    _pad0: u32,
}

crate::primitive! {
    name: RemoveDrift3D,
    type_id: "node.remove_drift_3d",
    purpose: "Subtract the mean of an Array<[f32;3]> per-particle force field (over live particles) from every entry, so internal forces sum to zero and a particle fluid stops accumulating net momentum. Fixes the discrete-grid conservation residue that otherwise drifts a confined sim into a corner (BUG-066). amount 1 = full balance, 0 = passthrough; port-shadowed so it can be performed.",
    inputs: {
        in: Array([f32; 3]) required,
        particles: Array(Particle) required,
        active_count: ScalarF32 optional,
        amount: ScalarF32 optional,
    },
    outputs: {
        out: Array([f32; 3]),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("active_count"),
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(100_000.0),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Insert between the internal field forces (node.sample_volume_at_particles + node.turbulence_3d) and the boundary forces (node.push_from_walls_3d) in a particle force chain: internal forces should conserve momentum, wall forces should not — balancing after the walls would cancel the container cushion. The mean is computed over particles with life > 0 only. amount blends the correction (1 = conservation enforced, 0 = passthrough) and is port-shadowed for live modulation. Output capacity follows the `in` array.",
    examples: ["FluidSim3D"],
    picker: { label: "Remove Drift (3D)", category: Atom },
    summary: "Balances the forces on a particle system so it stops slowly sliding in one direction — a long-running fluid stays centered instead of silting into a corner.",
    category: Particles3D,
    role: Filter,
    aliases: ["remove drift", "balance forces", "zero mean force", "conserve momentum", "center forces", "drift correction"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        // The macro-provided `pipeline` field holds partial_main; these hold
        // the other two entry points of the three-pass dispatch.
        finalize_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        apply_pipeline: Option<manifold_gpu::GpuComputePipeline> = None,
        // NUM_PARTIALS × vec4<f32> reduction scratch, allocated once.
        partials: Option<manifold_gpu::GpuBuffer> = None
    },
}

impl Primitive for RemoveDrift3D {
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

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count = ctx.scalar_or_param("active_count", 100_000.0).round().max(0.0) as u32;
        let amount = ctx.scalar_or_param("amount", 1.0).clamp(0.0, 1.0);

        let Some(in_forces) = ctx.inputs.array("in") else {
            return;
        };
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(out_forces) = ctx.outputs.array("out") else {
            return;
        };

        let force_size = std::mem::size_of::<[f32; 3]>() as u64;
        let capacity = (in_forces.size / force_size) as u32;
        let particle_capacity = (particles.size / std::mem::size_of::<Particle>() as u64) as u32;
        let active_count = active_count.min(capacity).min(particle_capacity);
        if active_count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();

        const SHADER_SRC: &str = include_str!("shaders/remove_drift_3d.wgsl");
        if self.pipeline.is_none() {
            self.pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER_SRC,
                "partial_main",
                "node.remove_drift_3d.partial",
            ));
        }
        if self.finalize_pipeline.is_none() {
            self.finalize_pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER_SRC,
                "finalize_main",
                "node.remove_drift_3d.finalize",
            ));
        }
        if self.apply_pipeline.is_none() {
            self.apply_pipeline = Some(gpu.device.create_compute_pipeline(
                SHADER_SRC,
                "apply_main",
                "node.remove_drift_3d.apply",
            ));
        }
        if self.partials.is_none() {
            self.partials = Some(gpu.device.create_buffer(u64::from(NUM_PARTIALS) * 16));
        }

        let partial_pipeline = self.pipeline.as_ref().expect("just inserted");
        let finalize_pipeline = self.finalize_pipeline.as_ref().expect("just inserted");
        let apply_pipeline = self.apply_pipeline.as_ref().expect("just inserted");
        let partials = self.partials.as_ref().expect("just allocated");

        let uniforms = RemoveDriftUniforms {
            active_count,
            num_partials: NUM_PARTIALS,
            amount,
            _pad0: 0,
        };

        let bindings = [
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
                buffer: partials,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: out_forces,
                offset: 0,
            },
        ];

        gpu.native_enc.dispatch_compute(
            partial_pipeline,
            &bindings,
            [NUM_PARTIALS, 1, 1],
            "node.remove_drift_3d.partial",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
        gpu.native_enc.dispatch_compute(
            finalize_pipeline,
            &bindings,
            [1, 1, 1],
            "node.remove_drift_3d.finalize",
        );
        gpu.native_enc.compute_memory_barrier_buffers();
        gpu.native_enc.dispatch_compute(
            apply_pipeline,
            &bindings,
            [active_count.div_ceil(256), 1, 1],
            "node.remove_drift_3d.apply",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_forces_particles_in_and_forces_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let vec3_layout = ArrayType::of_known::<[f32; 3]>();
        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(RemoveDrift3D::TYPE_ID, "node.remove_drift_3d");
        let names: Vec<&str> = RemoveDrift3D::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["in", "particles", "active_count", "amount"]);
        assert_eq!(RemoveDrift3D::INPUTS[0].ty, PortType::Array(vec3_layout));
        assert!(RemoveDrift3D::INPUTS[0].required);
        assert_eq!(RemoveDrift3D::INPUTS[1].ty, PortType::Array(particle_layout));
        assert!(RemoveDrift3D::INPUTS[1].required);

        assert_eq!(RemoveDrift3D::OUTPUTS.len(), 1);
        assert_eq!(RemoveDrift3D::OUTPUTS[0].name, "out");
        assert_eq!(RemoveDrift3D::OUTPUTS[0].ty, PortType::Array(vec3_layout));
    }

    #[test]
    fn uniform_struct_is_16_bytes() {
        assert_eq!(std::mem::size_of::<RemoveDriftUniforms>(), 16);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = RemoveDrift3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.remove_drift_3d");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Value oracle: upload a force array with a known nonzero mean plus a
    //! mix of live and dead particles, run the three passes, and assert
    //! (a) out[i] = in[i] − mean_live × amount for every live index,
    //! (b) the mean is computed over LIVE particles only, and
    //! (c) amount scales the correction.
    use super::*;
    use crate::generators::compute_common::Particle;

    fn mk_particle(life: f32) -> Particle {
        Particle {
            position: [0.5; 3],
            _pad0: 0.0,
            velocity: [0.0; 3],
            life,
            age: 0.0,
            _pad1: [0.0; 3],
            color: [0.0; 4],
        }
    }

    fn run_remove_drift(
        device: &manifold_gpu::GpuDevice,
        forces: &[[f32; 3]],
        particles: &[Particle],
        amount: f32,
    ) -> Vec<[f32; 3]> {
        let n = forces.len() as u32;
        const SRC: &str = include_str!("shaders/remove_drift_3d.wgsl");
        let partial = device.create_compute_pipeline(SRC, "partial_main", "rd-partial");
        let finalize = device.create_compute_pipeline(SRC, "finalize_main", "rd-finalize");
        let apply = device.create_compute_pipeline(SRC, "apply_main", "rd-apply");

        let in_buf = device.create_buffer_shared(std::mem::size_of_val(forces) as u64);
        let p_buf = device.create_buffer_shared(std::mem::size_of_val(particles) as u64);
        let partials = device.create_buffer_shared(u64::from(NUM_PARTIALS) * 16);
        let out_buf = device.create_buffer_shared(std::mem::size_of_val(forces) as u64);
        unsafe {
            in_buf.write(0, bytemuck::cast_slice(forces));
            p_buf.write(0, bytemuck::cast_slice(particles));
        }

        let uniforms = RemoveDriftUniforms {
            active_count: n,
            num_partials: NUM_PARTIALS,
            amount,
            _pad0: 0,
        };
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &in_buf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &p_buf, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &partials, offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: &out_buf, offset: 0 },
        ];

        let mut enc = device.create_encoder("rd-oracle");
        enc.dispatch_compute(&partial, &bindings, [NUM_PARTIALS, 1, 1], "rd-partial");
        enc.compute_memory_barrier_buffers();
        enc.dispatch_compute(&finalize, &bindings, [1, 1, 1], "rd-finalize");
        enc.compute_memory_barrier_buffers();
        enc.dispatch_compute(&apply, &bindings, [n.div_ceil(256), 1, 1], "rd-apply");
        enc.commit_and_wait_completed();

        let ptr = out_buf.mapped_ptr().expect("shared out buffer");
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const [f32; 3], n as usize) };
        slice.to_vec()
    }

    #[test]
    fn subtracts_live_mean_and_respects_amount() {
        let device = crate::test_device();

        // 1000 particles; every 5th is dead. Live forces have a known mean.
        let n = 1000usize;
        let mut forces = Vec::with_capacity(n);
        let mut particles = Vec::with_capacity(n);
        for i in 0..n {
            let alive = i % 5 != 0;
            // Dead entries carry huge garbage — they must NOT pollute the mean.
            let f = if alive {
                [(i % 7) as f32 * 0.01, -((i % 3) as f32) * 0.02, 0.005]
            } else {
                [1000.0, -1000.0, 1000.0]
            };
            forces.push(f);
            particles.push(mk_particle(if alive { 1.0 } else { 0.0 }));
        }

        let mut mean = [0f64; 3];
        let mut live = 0f64;
        for (f, p) in forces.iter().zip(&particles) {
            if p.life > 0.0 {
                for c in 0..3 {
                    mean[c] += f64::from(f[c]);
                }
                live += 1.0;
            }
        }
        for m in &mut mean {
            *m /= live;
        }

        for amount in [1.0f32, 0.5, 0.0] {
            let out = run_remove_drift(&device, &forces, &particles, amount);
            for (i, (f, o)) in forces.iter().zip(&out).enumerate() {
                for c in 0..3 {
                    let expected = f[c] - mean[c] as f32 * amount;
                    assert!(
                        (o[c] - expected).abs() < 1e-4,
                        "amount {amount} particle {i} axis {c}: out {} != {expected}",
                        o[c]
                    );
                }
            }
            // The corrected live set must have ~zero mean at amount = 1.
            if amount == 1.0 {
                let mut sum = [0f64; 3];
                for (o, p) in out.iter().zip(&particles) {
                    if p.life > 0.0 {
                        for c in 0..3 {
                            sum[c] += f64::from(o[c]);
                        }
                    }
                }
                for (c, s) in sum.iter().enumerate() {
                    assert!(
                        (s / live).abs() < 1e-6,
                        "axis {c}: corrected live mean {} not ~0",
                        s / live
                    );
                }
            }
        }
    }
}
