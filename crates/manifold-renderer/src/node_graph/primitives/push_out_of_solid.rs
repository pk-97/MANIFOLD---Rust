//! `node.push_out_of_solid` — move liquid particles out of a negative solid
//! distance lattice.  The particle frame stays a generated buffer atom: each
//! particle reads one gathered solid lattice and writes one `FluidParticle`.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: the nine scalar params in `PARAMS`, then the
/// injected element count and padding to a 16-byte multiple.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PushUniforms {
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

const _: () = assert!(std::mem::size_of::<PushUniforms>() == 48);

crate::primitive! {
    name: PushOutOfSolid,
    type_id: "node.push_out_of_solid",
    purpose: "Push each liquid particle out of a negative solid distance lattice. The solid is sampled manually and trilinearly at the particle position; a central-difference gradient supplies the outward normal, and a particle with phi < 0 receives up to four fixed normalized-gradient projections by -phi. Position is the only changed field: radius, velocity and id pass through unchanged.",
    inputs: {
        particles: Array(FluidParticle) required,
        solid: Array(f32) required,
        center_x: ScalarF32 optional, center_y: ScalarF32 optional, center_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(FluidParticle),
    },
    params: [
        float_param!("center_x", "Grid Center X", 0.0, -1.0e4, 1.0e4),
        float_param!("center_y", "Grid Center Y", 0.0, -1.0e4, 1.0e4),
        float_param!("center_z", "Grid Center Z", 0.0, -1.0e4, 1.0e4),
        float_param!("size_x", "Grid Size X", 4.0, 0.0001, 1.0e4),
        float_param!("size_y", "Grid Size Y", 4.0, 0.0001, 1.0e4),
        float_param!("size_z", "Grid Size Z", 4.0, 0.0001, 1.0e4),
        float_param!("nodes_x", "Solid Nodes X", 5.0, 2.0, 4096.0),
        float_param!("nodes_y", "Solid Nodes Y", 5.0, 2.0, 4096.0),
        float_param!("nodes_z", "Solid Nodes Z", 5.0, 2.0, 4096.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Place after the liquid solver's particle output and before surface extraction or any collider-sensitive particle consumer. Wire solid and center/size/nodes from the same grid owner. The lattice values must be signed distance in scene metres (negative inside). Manual trilinear sampling is clamped to the lattice boundary, and up to four fixed normalized-gradient projections reduce curved-field and interpolation error. A negative sample with a zero or non-finite central-difference gradient is left unchanged: without a normal there is no safe direction to push, so callers should provide a valid signed-distance field rather than relying on this atom to invent one.",
    examples: [],
    picker: { label: "Push Out Of Solid", category: Atom },
    summary: "Moves liquid particles out of solid regions while keeping their size, velocity and identity.",
    category: Particles3D,
    role: Filter,
    aliases: ["push particles out", "solid projection", "particle collision", "remove penetration"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/push_out_of_solid_body.wgsl"),
    input_access: [Coincident, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "particles" },
}

/// Shared standalone/extent geometry contract. WGSL uses floor(n + 0.5)
/// for positive node counts, matching Rust's round (not WGSL's ties-to-even).
pub(crate) fn solid_shape(read: impl Fn(&str, f32) -> f32) -> Result<([u32; 3], u64), String> {
    for name in ["center_x", "center_y", "center_z"] {
        if !read(name, 0.0).is_finite() {
            return Err(format!("Push Out Of Solid: {name} must be finite"));
        }
    }
    for name in ["size_x", "size_y", "size_z"] {
        let value = read(name, 4.0);
        if !value.is_finite() || value <= 0.0 {
            return Err(format!("Push Out Of Solid: {name} must be finite and positive"));
        }
    }
    let mut nodes = [0; 3];
    let mut total = 1u32;
    for (axis, name) in ["nodes_x", "nodes_y", "nodes_z"].into_iter().enumerate() {
        let value = read(name, 5.0).round();
        if !(2.0..=16_777_216.0).contains(&value) {
            return Err(format!("Push Out Of Solid: {name} must round to 2..=16777216"));
        }
        nodes[axis] = value as u32;
        total = total.checked_mul(nodes[axis])
            .ok_or_else(|| "Push Out Of Solid: lattice exceeds u32 indexing".to_owned())?;
    }
    Ok((nodes, u64::from(total) * std::mem::size_of::<f32>() as u64))
}

impl Primitive for PushOutOfSolid {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "particles")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(solid) = ctx.inputs.array("solid") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let count =
            (particles.size.min(out.size) / std::mem::size_of::<FluidParticle>() as u64) as u32;
        if count == 0 {
            return;
        }
        let (nodes, solid_bytes) = match solid_shape(|name, default| ctx.scalar_or_param(name, default)) {
            Ok(shape) => shape,
            Err(error) => { ctx.error(error); return; }
        };
        if solid_bytes > solid.size {
            ctx.error(format!(
                "Push Out Of Solid: a {:?}-node lattice has fewer solid values than required",
                nodes
            ));
            return;
        }
        let [center_x, center_y, center_z] =
            ["center_x", "center_y", "center_z"].map(|name| ctx.scalar_or_param(name, 0.0));
        let [size_x, size_y, size_z] =
            ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        let uniforms = PushUniforms {
            center_x,
            center_y,
            center_z,
            size_x,
            size_y,
            size_z,
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
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
                    buffer: solid,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: out,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.push_out_of_solid",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_shape_rounding_and_invalid_extents() {
        assert_eq!(solid_shape(|name, default| if name.starts_with("nodes_") { 4.5 } else { default }).unwrap(), ([5; 3], 500));
        for bad in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
            assert!(solid_shape(|name, default| if name == "size_x" { bad } else { default }).is_err());
        }
        for bad in [f32::NAN, f32::INFINITY, 1.0, 16_777_218.0] {
            assert!(solid_shape(|name, default| if name == "nodes_x" { bad } else { default }).is_err());
        }
        assert!(solid_shape(|name, default| if name.starts_with("nodes_") { 4096.0 } else { default }).is_err());
        assert!(solid_shape(|name, default| if name == "center_z" { f32::NAN } else { default }).is_err());
    }

    #[test]
    fn generated_wgsl_has_gathered_solid_and_particle_output() {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<PushOutOfSolid>()
            .expect("push_out_of_solid codegen");
        let module = naga::front::wgsl::parse_str(&wgsl)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        assert!(
            wgsl.contains("var<storage, read> buf_solid: array<f32>"),
            "{wgsl}"
        );
        assert!(
            wgsl.contains("buf_out[idx] = body(idx, params.dispatch_count"),
            "{wgsl}"
        );
        assert_eq!(std::mem::size_of::<PushUniforms>(), 48);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;

    fn cpu_expected(p: FluidParticle, phi: f32, gradient: [f32; 3]) -> FluidParticle {
        let length =
            (gradient[0] * gradient[0] + gradient[1] * gradient[1] + gradient[2] * gradient[2])
                .sqrt();
        if phi >= 0.0 || phi.is_nan() || length <= 1.0e-6 || !length.is_finite() {
            return p;
        }
        let mut out = p;
        out.position_radius[0] -= phi * gradient[0] / length;
        out.position_radius[1] -= phi * gradient[1] / length;
        out.position_radius[2] -= phi * gradient[2] / length;
        out
    }

    fn dispatch(
        particles: &[FluidParticle],
        solid: &[f32],
        uniforms: PushUniforms,
    ) -> Vec<FluidParticle> {
        let device = crate::test_device();
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<PushOutOfSolid>()
            .expect("push_out_of_solid codegen");
        let pipeline = device.create_compute_pipeline(
            &wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "push-out-of-solid-test",
        );
        let src = device.create_buffer_shared(std::mem::size_of_val(particles) as u64);
        let field = device.create_buffer_shared(std::mem::size_of_val(solid) as u64);
        let dst = device.create_buffer_shared(std::mem::size_of_val(particles) as u64);
        unsafe {
            src.write(0, bytemuck::cast_slice(particles));
            field.write(0, bytemuck::cast_slice(solid));
        }
        let mut enc = device.create_encoder("push-out-of-solid-test");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &src,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &field,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &dst,
                    offset: 0,
                },
            ],
            [(particles.len() as u32).div_ceil(256), 1, 1],
            "push-out-of-solid-test",
        );
        enc.commit_and_wait_completed();
        let ptr = dst.mapped_ptr().expect("shared particle output");
        unsafe { std::slice::from_raw_parts(ptr as *const FluidParticle, particles.len()) }.to_vec()
    }

    #[test]
    fn fluid_push_out_penetration_bounded() {
        let nodes = [5_u32; 3];
        let mut solid = Vec::with_capacity(125);
        for _z in 0..nodes[2] {
            for y in 0..nodes[1] {
                for _x in 0..nodes[0] {
                    solid.push(y as f32 - 2.0);
                }
            }
        }
        let particles = [
            FluidParticle {
                position_radius: [0.0, -0.5, 0.0, 0.2],
                velocity: [1.0, 2.0, 3.0],
                id: 17,
            },
            FluidParticle {
                position_radius: [0.0, 0.5, 0.0, 0.3],
                velocity: [-1.0, 0.5, 4.0],
                id: 23,
            },
        ];
        let uniforms = PushUniforms {
            center_x: 0.0,
            center_y: 0.0,
            center_z: 0.0,
            size_x: 4.0,
            size_y: 4.0,
            size_z: 4.0,
            nodes_x: 5.0,
            nodes_y: 5.0,
            nodes_z: 5.0,
            dispatch_count: particles.len() as u32,
            _pad0: 0,
            _pad1: 0,
        };
        let got = dispatch(&particles, &solid, uniforms);
        let expected = [
            cpu_expected(particles[0], -0.5, [0.0, 1.0, 0.0]),
            particles[1],
        ];
        for (i, (actual, want)) in got.iter().zip(expected).enumerate() {
            assert!(
                (actual.position_radius[0] - want.position_radius[0]).abs() < 1.0e-4,
                "x {i}: {actual:?} vs {want:?}"
            );
            assert!(
                (actual.position_radius[1] - want.position_radius[1]).abs() < 1.0e-4,
                "y {i}: {actual:?} vs {want:?}"
            );
            assert!(
                (actual.position_radius[2] - want.position_radius[2]).abs() < 1.0e-4,
                "z {i}: {actual:?} vs {want:?}"
            );
            assert_eq!(actual.position_radius[3], want.position_radius[3]);
            assert_eq!(actual.velocity, want.velocity);
            assert_eq!(actual.id, want.id);
        }
        assert!(
            got[0].position_radius[1] >= -1.0e-4,
            "particle remains inside solid: {:?}",
            got[0]
        );

        // A curved signed-distance field exercises the fixed Newton projection
        // beyond the affine plane. The bound is one tenth of a lattice cell,
        // matching the P3 penetration contract.
        let nodes = [16_u32; 3];
        let mut sphere = Vec::with_capacity(16 * 16 * 16);
        for z in 0..nodes[2] {
            for y in 0..nodes[1] {
                for x in 0..nodes[0] {
                    let p = [
                        x as f32 / 15.0 * 4.0 - 2.0,
                        y as f32 / 15.0 * 4.0 - 2.0,
                        z as f32 / 15.0 * 4.0 - 2.0,
                    ];
                    sphere.push((p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt() - 0.75);
                }
            }
        }
        let curved = [FluidParticle {
            position_radius: [0.19, 0.22, 0.17, 0.11],
            velocity: [0.25, -0.5, 1.5],
            id: 71,
        }];
        let curved_uniforms = PushUniforms {
            center_x: 0.0,
            center_y: 0.0,
            center_z: 0.0,
            size_x: 4.0,
            size_y: 4.0,
            size_z: 4.0,
            nodes_x: 16.0,
            nodes_y: 16.0,
            nodes_z: 16.0,
            dispatch_count: 1,
            _pad0: 0,
            _pad1: 0,
        };
        let curved_got = dispatch(&curved, &sphere, curved_uniforms);
        let q = curved_got[0].position_radius;
        let penetration = 0.75 - (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt();
        assert!(
            penetration <= 0.1 * (4.0 / 15.0) + 1.0e-4,
            "curved penetration {penetration}: {q:?}"
        );
        assert_eq!(
            curved_got[0].position_radius[3],
            curved[0].position_radius[3]
        );
        assert_eq!(curved_got[0].velocity, curved[0].velocity);
        assert_eq!(curved_got[0].id, curved[0].id);
    }

    #[test]
    fn fluid_push_out_zero_gradient_is_finite_and_preserved() {
        let particles = [FluidParticle {
            position_radius: [0.1, -0.2, 0.3, 0.09],
            velocity: [2.0, -1.0, 0.5],
            id: 101,
        }];
        let uniforms = PushUniforms {
            center_x: 0.0,
            center_y: 0.0,
            center_z: 0.0,
            size_x: 4.0,
            size_y: 4.0,
            size_z: 4.0,
            nodes_x: 8.0,
            nodes_y: 8.0,
            nodes_z: 8.0,
            dispatch_count: 1,
            _pad0: 0,
            _pad1: 0,
        };
        let got = dispatch(&particles, &vec![-0.5; 8 * 8 * 8], uniforms);
        assert_eq!(got[0], particles[0]);
        assert!(got[0].position_radius.iter().all(|x| x.is_finite()));

        // One infinite lattice corner makes the central difference infinite
        // while the particle's own phi remains negative. It must take the
        // same safe no-op path as a zero gradient rather than emit NaNs.
        let mut infinite_gradient = vec![-0.5; 8 * 8 * 8];
        infinite_gradient[4 + 8 * (3 + 8 * 3)] = f32::INFINITY;
        let q3 = -2.0 + 2.75 * (4.0 / 7.0);
        let infinite_particle = [FluidParticle {
            position_radius: [q3, 0.0, 0.0, 0.09],
            velocity: [2.0, -1.0, 0.5],
            id: 102,
        }];
        let infinite_got = dispatch(&infinite_particle, &infinite_gradient, uniforms);
        assert_eq!(infinite_got[0], infinite_particle[0]);
        assert!(
            infinite_got[0]
                .position_radius
                .iter()
                .all(|x| x.is_finite())
        );
    }
}

#[cfg(any(test, feature = "gpu-proofs"))]
mod extent;
