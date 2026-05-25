//! `node.fluid_simulate_3d` — per-frame integrator for the FluidSim3D
//! family. Bit-exact wrap of `generators/shaders/fluid_simulate_3d.wgsl::main`
//! via include_str.
//!
//! Reads 3D particle positions, samples the blurred 3D vector force field at
//! each particle's position, adds 3-plane simplex-noise advection (density-
//! adaptive), per-particle diffusion, soft container boundary repulsion
//! (Box / Sphere / Torus / None), integrates one Euler step, and applies a
//! camera-aware flatten that compresses particles toward the viewing plane.
//! Optional 3D injection burst pushes particles outward from one of four
//! hardcoded tetrahedron-vertex zones.
//!
//! Wiring shape: paired upstream with `node.fluid_seed_3d` (init / clip-trigger)
//! and `node.fluid_gradient_curl_3d` (force field). Takes a `camera: Camera`
//! input (typically from `node.camera_orbit`) and reads `cam.fwd` for the
//! flatten direction. Sibling to `node.fluid_simulate` (the 2D version).

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::generators::compute_common::Particle;
use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const FLUID_3D_CONTAINER_MODES: &[&str] = &["None", "Cube", "Sphere", "Torus"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Sim3DUniforms {
    active_count: u32,
    frame_count: u32,
    use_vector_field: u32,
    container: u32,
    ctr_scale: f32,
    speed: f32,
    turbulence: f32,
    anti_clump: f32,
    diffusion: f32,
    respawn_rate: f32,
    dense_respawn: f32,
    flatten: f32,
    cam_fwd_x: f32,
    cam_fwd_y: f32,
    cam_fwd_z: f32,
    _pad0: f32,
    inject_index: i32,
    inject_force: f32,
    inject_phase: f32,
    time2: f32,
    dt: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
}

const _: () = assert!(std::mem::size_of::<Sim3DUniforms>() == 96);

crate::primitive! {
    name: FluidSimulate3D,
    type_id: "node.fluid_simulate_3d",
    purpose: "Per-frame FluidSim3D integrator. Reads particle positions, samples a blurred 3D vector force field + 3D density volume, adds 3-plane simplex-noise advection (density-adaptive), per-particle diffusion, soft container SDF repulsion (Box/Sphere/Torus/None), integrates one Euler step, and applies camera-aware flatten. Optional 3D injection burst at one of four tetrahedron zones. Takes a `camera: Camera` input — reads `cam.fwd` for the flatten direction. Pair upstream with node.fluid_seed_3d (init) and node.fluid_gradient_curl_3d (force field). Sibling to node.fluid_simulate (2D).",
    inputs: {
        in: Array(Particle) required,
        field: Texture3D required,
        density: Texture3D required,
        camera: Camera required,
        active_count: ScalarF32 optional,
        speed: ScalarF32 optional,
        turbulence: ScalarF32 optional,
        anti_clump: ScalarF32 optional,
        diffusion: ScalarF32 optional,
        ctr_scale: ScalarF32 optional,
        flatten: ScalarF32 optional,
        inject_index: ScalarF32 optional,
        inject_force: ScalarF32 optional,
        inject_phase: ScalarF32 optional,
        // Seed-edge skip gate. Wired from the same trigger chain that
        // drives node.fluid_seed_3d so the freshly-seeded particles
        // aren't immediately displaced. Matches node.fluid_simulate's
        // trigger semantics.
        trigger: ScalarF32 optional,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [
        ParamDef {
            name: "active_count",
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(2_000_000.0),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "speed",
            label: "Speed",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "turbulence",
            label: "Turbulence",
            ty: ParamType::Float,
            default: ParamValue::Float(0.001),
            range: Some((0.0, 0.05)),
            enum_values: &[],
        },
        ParamDef {
            name: "anti_clump",
            label: "Anti-Clump",
            ty: ParamType::Float,
            default: ParamValue::Float(20.0),
            range: Some((0.0, 60.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "diffusion",
            label: "Diffusion",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0166),
            range: Some((0.0, 0.5)),
            enum_values: &[],
        },
        ParamDef {
            name: "container",
            label: "Container",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: FLUID_3D_CONTAINER_MODES,
        },
        ParamDef {
            name: "ctr_scale",
            label: "Container Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(0.8),
            range: Some((0.2, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "flatten",
            label: "Flatten",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "inject_index",
            label: "Inject Zone",
            ty: ParamType::Int,
            default: ParamValue::Float(-1.0),
            range: Some((-1.0, 3.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "inject_force",
            label: "Inject Force",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "inject_phase",
            label: "Inject Phase",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Aliased in→out: the kernel mutates the input particle buffer in place; downstream consumers reading `out` see the mutated buffer. `inject_index = -1` disables the injection burst (the shader gates on `inject_index >= 0`); the four valid zones (0..3) are hardcoded tetrahedron-vertex positions inside the shader. The `container` enum picks an SDF for soft boundary repulsion; `None` (= 0) uses toroidal wrap on all three axes. Reads cam.fwd for the camera-aware flatten — drive the camera from `node.camera_orbit`. The `trigger` port mirrors `node.fluid_simulate`'s seed-edge skip — wire from the same trigger chain feeding `node.fluid_seed_3d`.",
    examples: [],
    picker: { label: "Fluid Simulate 3D", category: Atom },
    extra_fields: {
        density_sampler: Option<manifold_gpu::GpuSampler> = None,
        // Tracks the last `trigger` input value to detect edges. On
        // edge frames the dispatch is skipped (legacy frame-skip pattern,
        // same as node.fluid_simulate).
        last_trigger: Option<i32> = None,
    },
}

impl Primitive for FluidSimulate3D {
    /// Output `out` is sized to match input `in` — simulation is in-place.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
        } else {
            None
        }
    }

    /// `in` and `out` resolve to the same physical buffer (same pattern as
    /// `node.fluid_simulate` and `node.integrate_particles`). The simulate
    /// kernel binds the particle buffer once and mutates positions in place;
    /// downstream consumers of `out` read the mutated buffer.
    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("in", "out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Seed-edge skip — mirrors node.fluid_simulate's trigger semantics.
        // The aliased in→out buffer wasn't written this frame, but downstream
        // reads it via the same slot so the aliased-IO debug assertion fires
        // unless we mark the GPU as accessed (per the 3de536e4 fix on the 2D
        // sibling).
        if let Some(ParamValue::Float(v)) = ctx.inputs.scalar("trigger") {
            let current = v.round() as i32;
            let edge = match self.last_trigger {
                Some(prev) => current != prev,
                None => false,
            };
            self.last_trigger = Some(current);
            if edge {
                ctx.mark_gpu_accessed();
                return;
            }
        }

        let active_count =
            ctx.scalar_or_param("active_count", 2_000_000.0).round().max(0.0) as u32;
        let speed = ctx.scalar_or_param("speed", 1.0);
        let turbulence = ctx.scalar_or_param("turbulence", 0.001);
        let anti_clump = ctx.scalar_or_param("anti_clump", 20.0);
        let diffusion = ctx.scalar_or_param("diffusion", 0.0166);
        let ctr_scale = ctx.scalar_or_param("ctr_scale", 0.8);
        let flatten = ctx.scalar_or_param("flatten", 0.0);
        let inject_index = ctx.scalar_or_param("inject_index", -1.0).round() as i32;
        let inject_force = ctx.scalar_or_param("inject_force", 0.0);
        let inject_phase = ctx.scalar_or_param("inject_phase", 0.0);

        let container = match ctx.params.get("container") {
            Some(ParamValue::Enum(n)) => *n,
            Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
            _ => 0,
        };

        let cam = ctx.inputs.camera("camera").unwrap_or_else(Camera::default_perspective);

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(field) = ctx.inputs.texture_3d("field") else {
            return;
        };
        let Some(density) = ctx.inputs.texture_3d("density") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let _ = out_buf; // aliased to in_buf — see aliased_array_io()

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (in_buf.size / particle_size) as u32;
        let active_count = active_count.min(capacity);

        let time2 = ctx.time.seconds.0 as f32;
        let dt = ctx.time.delta.0 as f32;
        let frame_count = ctx.time.frame_count as u32;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("../../generators/shaders/fluid_simulate_3d.wgsl"),
                "main",
                "node.fluid_simulate_3d",
            )
        });
        let field_sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        let density_sampler = self
            .density_sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        let _ = density_sampler; // shared linear-clamp; the shader uses s_field for both

        let uniforms = Sim3DUniforms {
            active_count,
            frame_count,
            use_vector_field: 1,
            container,
            ctr_scale,
            speed,
            turbulence,
            anti_clump,
            diffusion,
            respawn_rate: 0.0,
            dense_respawn: 0.0,
            flatten,
            cam_fwd_x: cam.fwd[0],
            cam_fwd_y: cam.fwd[1],
            cam_fwd_z: cam.fwd[2],
            _pad0: 0.0,
            inject_index,
            inject_force,
            inject_phase,
            time2,
            dt,
            _pad1: 0.0,
            _pad2: 0.0,
            _pad3: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Buffer {
                    binding: 0,
                    buffer: in_buf,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: field,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler: field_sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: density,
                },
                GpuBinding::Bytes {
                    binding: 4,
                    data: bytemuck::bytes_of(&uniforms),
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.fluid_simulate_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn fluid_simulate_3d_declares_required_inputs_and_aliased_output() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(FluidSimulate3D::TYPE_ID, "node.fluid_simulate_3d");
        assert_eq!(FluidSimulate3D::INPUTS[0].name, "in");
        assert_eq!(FluidSimulate3D::INPUTS[0].ty, PortType::Array(particle_layout));
        assert!(FluidSimulate3D::INPUTS[0].required);
        assert_eq!(FluidSimulate3D::INPUTS[1].name, "field");
        assert_eq!(FluidSimulate3D::INPUTS[1].ty, PortType::Texture3D);
        assert_eq!(FluidSimulate3D::INPUTS[2].name, "density");
        assert_eq!(FluidSimulate3D::INPUTS[2].ty, PortType::Texture3D);
        assert_eq!(FluidSimulate3D::INPUTS[3].name, "camera");
        assert_eq!(FluidSimulate3D::INPUTS[3].ty, PortType::Camera);
        assert!(FluidSimulate3D::INPUTS[3].required);

        let prim = FluidSimulate3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.aliased_array_io(), &[("in", "out")]);
    }

    #[test]
    fn fluid_simulate_3d_uniform_struct_is_96_bytes() {
        assert_eq!(std::mem::size_of::<Sim3DUniforms>(), 96);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = FluidSimulate3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.fluid_simulate_3d");
    }
}
