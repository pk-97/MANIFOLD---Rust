//! `node.draw_particles_3d` — atomic-add splat of particles into
//! a 3D `u32` fixed-point accumulator buffer.
//!
//! Bit-exact wrap of `generators/shaders/fluid_scatter_3d.wgsl`'s
//! `splat_3d` entry point via `include_str!`. Each live particle's
//! `position.xyz` indexes a voxel in the `vol_res × vol_res ×
//! vol_depth` grid and contributes `scaled_energy` via `atomicAdd`.
//! Pair with `node.resolve_scatter_3d` to lift the u32 grid into
//! a float Texture3D for downstream volumetric primitives.

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order
/// (`active_count`, `vol_res`, `vol_depth`, `scaled_energy` Int → i32), then
/// the codegen-injected `dispatch_count` (u32, the splat dispatch guard =
/// clamped active_count), padded to a 16-byte multiple. 5 words + 3 pad = 32
/// bytes. The legacy 112-byte padding the shared `fluid_scatter_3d.wgsl` module
/// needed (naga same-binding-same-size across its four entry points) is gone:
/// the generated standalone kernel is single-entry, so it carries only its own
/// uniform.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Splat3DUniforms {
    active_count: i32,
    vol_res: i32,
    vol_depth: i32,
    scaled_energy: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: ScatterParticles3D,
    type_id: "node.draw_particles_3d",
    purpose: "Atomic-add splat of an Array<Particle> into a u32 3D accumulator buffer sized vol_res × vol_res × vol_depth. Each live particle's nearest-voxel cell receives `scaled_energy` via atomicAdd. Pair with node.resolve_scatter_3d to lift the u32 grid into a float Texture3D for downstream volumetric primitives like blur_3d, gradient_curl_3d, project_particles_3d.",
    inputs: {
        particles: Array(Particle) required,
        active_count: ScalarF32 optional,
        scaled_energy: ScalarF32 optional,
    },
    outputs: {
        accum: Array(u32),
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
            name: Cow::Borrowed("vol_res"),
            label: "Volume Resolution",
            ty: ParamType::Int,
            default: ParamValue::Float(128.0),
            range: Some((16.0, 512.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("vol_depth"),
            label: "Volume Depth",
            ty: ParamType::Int,
            default: ParamValue::Float(128.0),
            range: Some((16.0, 512.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scaled_energy"),
            label: "Energy per Particle",
            ty: ParamType::Int,
            default: ParamValue::Float(4096.0),
            range: Some((1.0, 65536.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "vol_res × vol_res × vol_depth = total cells; accumulator buffer must be sized vol_res² × vol_depth × 4 bytes upstream. Particle positions are in [0,1]³; cells outside that range are toroidally wrapped (% vr / % vd). scaled_energy = 4096 ≈ 1.0 in float density after Resolve divides by 4096 (matches FluidSim3D convention).",
    examples: [],
    picker: { label: "Draw Particles (3D scatter)", category: Atom },
    summary: "Splats 3D particles into a volume buffer, building up a 3D density field from where they land. The 3D version of Draw Particles.",
    category: Particles3D,
    role: Filter,
    aliases: ["draw particles 3d", "scatter particles 3d", "scatter 3d", "splat", "volume"],
    fusion_kind: Boundary,
    boundary_reason: Blocked,
    wgsl_body: include_str!("shaders/scatter_particles_3d_body.wgsl"),
    atomic_outputs: ["accum"],
}

impl Primitive for ScatterParticles3D {
    /// Volume accumulator is `vol_res × vol_res × vol_depth` u32 cells.
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "accum" {
            return None;
        }
        let read_dim = |name| match params.get(name) {
            Some(ParamValue::Float(f)) => Some(f.round().max(1.0) as u32),
            _ => None,
        };
        let r = read_dim("vol_res")?;
        let d = read_dim("vol_depth")?;
        Some(r.saturating_mul(r).saturating_mul(d))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count =
            ctx.scalar_or_param("active_count", 100_000.0).round().max(0.0) as u32;
        let vol_res = match ctx.params.get("vol_res") {
            Some(ParamValue::Float(n)) => n.round().max(1_f32) as u32,
            _ => 128,
        };
        let vol_depth = match ctx.params.get("vol_depth") {
            Some(ParamValue::Float(n)) => n.round().max(1_f32) as u32,
            _ => 128,
        };
        let scaled_energy =
            ctx.scalar_or_param("scaled_energy", 4096.0).round().max(0.0) as u32;

        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(accum) = ctx.outputs.array("accum") else {
            return;
        };

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let particle_capacity = (particles.size / particle_size) as u32;
        let active_count = active_count.min(particle_capacity);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // ATOMIC 3D SCATTER — the body computes each particle's target voxel
            // and `atomicAdd`s into the `buf_accum` accumulator). The shared
            // fluid_scatter_3d.wgsl splat_3d is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.draw_particles_3d standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.draw_particles_3d",
            )
        });

        let uniforms = Splat3DUniforms {
            active_count: active_count as i32,
            vol_res: vol_res as i32,
            vol_depth: vol_depth as i32,
            scaled_energy: scaled_energy as i32,
            dispatch_count: active_count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        // Generated binding order follows INPUTS: uniform(0), particles(1, read),
        // accum(2, atomic read_write). The hand splat_3d bound them as
        // particles(0)/accum(1)/uniform(2) (shared-module convention) — rebind.
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: accum,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.draw_particles_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn scatter_3d_declares_particle_in_and_u32_array_out() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let particle_layout = ArrayType::of_known::<Particle>();
        let u32_layout = ArrayType::of_known::<u32>();

        assert_eq!(ScatterParticles3D::TYPE_ID, "node.draw_particles_3d");
        assert_eq!(ScatterParticles3D::INPUTS[0].name, "particles");
        assert_eq!(
            ScatterParticles3D::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert!(ScatterParticles3D::INPUTS[0].required);
        // Port-shadow inputs let the JSON drive active_count + scaled_energy
        // from the outer-card particle-count / energy chains.
        for name in ["active_count", "scaled_energy"] {
            let port = ScatterParticles3D::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required, "{name} should be optional port-shadow");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(ScatterParticles3D::OUTPUTS.len(), 1);
        assert_eq!(ScatterParticles3D::OUTPUTS[0].name, "accum");
        assert_eq!(
            ScatterParticles3D::OUTPUTS[0].ty,
            PortType::Array(u32_layout)
        );
    }

    #[test]
    fn scatter_3d_uniform_struct_matches_generated_layout() {
        // The generated standalone kernel is single-entry, so it carries only
        // its own uniform — no 112-byte same-binding-same-size padding the
        // shared fluid_scatter_3d.wgsl module needed. Params (4 × i32) +
        // dispatch_count (u32) + 3 pad words = 32 bytes.
        assert_eq!(std::mem::size_of::<Splat3DUniforms>(), 32);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ScatterParticles3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.draw_particles_3d");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain ATOMIC 3D SCATTER parity oracle (freeze §12). The generated
    //! standalone kernel must reproduce the shared `fluid_scatter_3d.wgsl`
    //! `splat_3d` voxel-for-voxel. Hand binds particles@0/accum@1/uniform@2
    //! (shared-module convention); generated binds uniform@0/particles@1/
    //! accum@2. Both `atomicAdd` into the same accumulator layout, so the u32
    //! volume reads back identically.
    use super::*;
    use crate::generators::compute_common::Particle;

    fn alive(x: f32, y: f32, z: f32) -> Particle {
        Particle {
            position: [x, y, z],
            _pad0: 0.0,
            velocity: [0.0; 3],
            life: 1.0,
            age: 0.0,
            _pad1: [0.0; 3],
            color: [0.0; 4],
        }
    }

    fn dispatch_splat3d(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        entry: &str,
        particles: &[Particle],
        cells: usize,
        uniform: &[u8],
        count: u32,
        generated: bool,
    ) -> Vec<u32> {
        let pipeline = device.create_compute_pipeline(wgsl, entry, "splat3d-oracle");
        let pbuf = device.create_buffer_shared(std::mem::size_of_val(particles) as u64);
        let abuf = device.create_buffer_shared((cells * 4) as u64);
        let zeros = vec![0u32; cells];
        unsafe {
            pbuf.write(0, bytemuck::cast_slice(particles));
            abuf.write(0, bytemuck::cast_slice(&zeros));
        }
        // Hand: particles@0, accum@1, uniform@2. Generated: uniform@0,
        // particles@1, accum@2.
        let bindings = if generated {
            vec![
                GpuBinding::Bytes { binding: 0, data: uniform },
                GpuBinding::Buffer { binding: 1, buffer: &pbuf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &abuf, offset: 0 },
            ]
        } else {
            vec![
                GpuBinding::Buffer { binding: 0, buffer: &pbuf, offset: 0 },
                GpuBinding::Buffer { binding: 1, buffer: &abuf, offset: 0 },
                GpuBinding::Bytes { binding: 2, data: uniform },
            ]
        };
        let mut enc = device.create_encoder("splat3d-oracle");
        enc.dispatch_compute(&pipeline, &bindings, [count.div_ceil(256), 1, 1], "splat3d-oracle");
        enc.commit_and_wait_completed();
        let ptr = abuf.mapped_ptr().expect("shared accumulator buffer");
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const u32, cells) };
        slice.to_vec()
    }

    #[test]
    fn generated_splat3d_matches_hand_kernel() {
        let device = crate::test_device();
        let vol_res = 4u32;
        let vol_depth = 4u32;
        let cells = (vol_res * vol_res * vol_depth) as usize;
        let scaled_energy = 4096u32;
        // Two particles share a voxel (collision → 2×energy), one dead (no-op).
        let particles = [
            alive(0.1, 0.1, 0.1),
            alive(0.1, 0.1, 0.1), // same voxel → atomic collision
            alive(0.9, 0.6, 0.4),
            Particle {
                position: [0.5, 0.5, 0.5],
                _pad0: 0.0,
                velocity: [0.0; 3],
                life: 0.0, // dead → skipped
                age: 0.0,
                _pad1: [0.0; 3],
                color: [0.0; 4],
            },
        ];
        let n = particles.len() as u32;

        // Hand layout: active_count(u32), vol_res(u32), vol_depth(u32),
        //   scaled_energy(u32), padded to 112 bytes.
        let mut hand = Vec::new();
        hand.extend_from_slice(&n.to_le_bytes());
        hand.extend_from_slice(&vol_res.to_le_bytes());
        hand.extend_from_slice(&vol_depth.to_le_bytes());
        hand.extend_from_slice(&scaled_energy.to_le_bytes());
        hand.extend_from_slice(&[0u8; 96]); // ProjectedUniforms = 112B

        // Generated layout: active_count(i32), vol_res(i32), vol_depth(i32),
        //   scaled_energy(i32), dispatch_count(u32), 3 pad.
        let mut gen_bytes = Vec::new();
        gen_bytes.extend_from_slice(&(n as i32).to_le_bytes());
        gen_bytes.extend_from_slice(&(vol_res as i32).to_le_bytes());
        gen_bytes.extend_from_slice(&(vol_depth as i32).to_le_bytes());
        gen_bytes.extend_from_slice(&(scaled_energy as i32).to_le_bytes());
        gen_bytes.extend_from_slice(&n.to_le_bytes());
        gen_bytes.extend_from_slice(&[0u8; 12]);

        let hand_wgsl = include_str!("../../generators/shaders/fluid_scatter_3d.wgsl");
        let gen_wgsl =
            crate::node_graph::freeze::codegen::standalone_for_spec::<ScatterParticles3D>()
                .expect("scatter_particles_3d buffer codegen");
        assert!(gen_wgsl.contains("atomic<u32>"), "atomic accumulator emitted");

        let from_hand =
            dispatch_splat3d(&device, hand_wgsl, "splat_3d", &particles, cells, &hand, n, false);
        let from_gen = dispatch_splat3d(
            &device, &gen_wgsl, "cs_main", &particles, cells, &gen_bytes, n, true,
        );

        assert_eq!(from_hand, from_gen, "3D scatter accumulator mismatch");
        // Sanity: the collision voxel holds 2× energy, total deposits = 3.
        assert_eq!(from_gen.iter().sum::<u32>(), 3 * scaled_energy);
    }
}
