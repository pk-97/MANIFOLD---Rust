//! `node.fluid_seed_3d` — 8-pattern 3D particle seeder for the FluidSim3D
//! family. Bit-exact wrap of `generators/shaders/fluid_simulate_3d.wgsl::seed_pattern`
//! via include_str.
//!
//! On each dispatch, writes initial 3D positions to particles in an
//! `Array<Particle>`. Eight patterns:
//!   0 — Center cluster (3D Gaussian approximation)
//!   1 — Horizontal planes (6 slabs at fixed Y)
//!   2 — Vertical planes (6 slabs at fixed X)
//!   3 — Concentric shells (3 spherical shells)
//!   4 — 3D diagonal cross
//!   5 — Double helix around Y axis
//!   6 — Surface sphere (implodes inward)
//!   7 — Random uniform fill (was pattern=255 in the legacy code, used
//!       on init before any clip-trigger; surfaced as a regular enum
//!       value here so the JSON preset can default to it)
//!
//! Container-aware (Box / Sphere / Torus / None): particles outside the
//! container SDF get pulled back along the surface normal. Camera-aware:
//! when `flatten > 0`, seeded positions are compressed toward the camera
//! viewing plane.
//!
//! Dispatched on chain init or on clip-trigger — not every frame.

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const FLUID_SEED_3D_PATTERNS: &[&str] = &[
    "Center Cluster",
    "Horizontal Planes",
    "Vertical Planes",
    "Concentric Shells",
    "Diagonal Cross",
    "Helix",
    "Surface Sphere",
    "Random",
];

pub const FLUID_SEED_3D_CONTAINER_MODES: &[&str] = &["None", "Cube", "Sphere", "Torus"];

/// Shader's `default` case writes a uniform random fill (`hash_float3(seed)`).
/// We pick a large sentinel so legitimate enum dispatch never falls into the
/// default by accident; the legacy CPU code passed 255 for the init dispatch.
const RANDOM_PATTERN_SENTINEL: u32 = 255;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SeedUniforms {
    active_count: u32,
    pattern_type: u32,
    trigger_count: u32,
    _pad0: u32,
    container: u32,
    ctr_scale: f32,
    flatten: f32,
    _pad1: f32,
    cam_fwd_x: f32,
    cam_fwd_y: f32,
    cam_fwd_z: f32,
    _pad2: f32,
}

const _: () = assert!(std::mem::size_of::<SeedUniforms>() == 48);

crate::primitive! {
    name: FluidSeed3D,
    type_id: "node.fluid_seed_3d",
    purpose: "Seed an Array<Particle> with one of 8 3D patterns (center cluster, H planes, V planes, shells, 3D cross, helix, surface sphere, random). Bit-exact port of FluidSim3D's SeedPatternKernel. Container-aware (Box/Sphere/Torus/None) and camera-aware (flatten compresses positions toward the camera viewing plane). When `trigger` is wired the seed dispatches only on integer-edge changes (matches FluidSim3D's clip-trigger mode 3 re-seed); when unwired, dispatches every frame (pair with `node.array_feedback` to capture-once-and-loop). Sibling to `node.fluid_seed` (2D).",
    inputs: {
        camera: Camera required,
        trigger: ScalarF32 optional,
        active_count: ScalarF32 optional,
        trigger_count: ScalarF32 optional,
        pattern: ScalarF32 optional,
        ctr_scale: ScalarF32 optional,
        flatten: ScalarF32 optional,
    },
    outputs: {
        particles: Array(Particle),
    },
    params: [
        ParamDef {
            name: "max_capacity",
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(8_000_000.0),
            range: Some((1024.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "active_count",
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(2_000_000.0),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "pattern",
            label: "Pattern",
            ty: ParamType::Enum,
            default: ParamValue::Enum(7),
            range: Some((0.0, (FLUID_SEED_3D_PATTERNS.len() - 1) as f32)),
            enum_values: FLUID_SEED_3D_PATTERNS,
        },
        ParamDef {
            name: "trigger_count",
            label: "Trigger Count (hash seed)",
            ty: ParamType::Int,
            default: ParamValue::Float(42.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "container",
            label: "Container",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: FLUID_SEED_3D_CONTAINER_MODES,
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
    ],
    composition_notes: "Pattern enum 0..=6 maps to the 7 geometric variants verbatim; enum 7 (Random) maps to the legacy `pattern=255` default-case branch and is the right choice for the initial seed before any clip-trigger fires. Takes a `camera: Camera` input so the flatten compression direction tracks the live camera (mirrors `node.fluid_simulate_3d`'s flatten). Pair upstream of `node.fluid_simulate_3d` (or `node.scatter_particles_camera`) as the particle buffer source.",
    examples: [],
    picker: { label: "Fluid Seed 3D", category: Atom },
    extra_fields: {
        // Tracks the last observed `trigger` value to detect edges.
        // None = no observation yet; first observation always fires
        // (initial seed).
        last_trigger: Option<i32> = None,
    },
}

impl Primitive for FluidSeed3D {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "particles" {
            return None;
        }
        match params.get("max_capacity") {
            Some(ParamValue::Float(n)) => Some(n.round().max(1.0) as u32),
            _ => Some(1_048_576),
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Edge-triggered dispatch: only re-seed on integer-edge changes
        // of `trigger`. When `trigger` is unwired, dispatch every frame
        // (matches node.fluid_seed semantics).
        let should_dispatch = match ctx.inputs.scalar("trigger") {
            Some(ParamValue::Float(v)) => {
                let current = v.round() as i32;
                let fire = match self.last_trigger {
                    Some(prev) => current != prev,
                    None => true, // first observation always fires
                };
                self.last_trigger = Some(current);
                fire
            }
            _ => true,
        };
        if !should_dispatch {
            // Still mark GPU accessed so the aliased-output assertion
            // doesn't trip on the no-op frame — same defensive pattern
            // as node.fluid_simulate's trigger skip.
            ctx.mark_gpu_accessed();
            return;
        }

        let active_count =
            ctx.scalar_or_param("active_count", 2_000_000.0).round().max(0.0) as u32;
        let trigger_count =
            ctx.scalar_or_param("trigger_count", 42.0).round().max(0.0) as u32;
        let pattern_enum =
            ctx.scalar_or_param("pattern", 7.0).round().clamp(0.0, 7.0) as u32;
        let pattern = if pattern_enum == 7 {
            RANDOM_PATTERN_SENTINEL
        } else {
            pattern_enum
        };
        let container = match ctx.params.get("container") {
            Some(ParamValue::Enum(n)) => *n,
            Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
            _ => 0,
        };
        let ctr_scale = ctx.scalar_or_param("ctr_scale", 0.8);
        let flatten = ctx.scalar_or_param("flatten", 0.0);

        let cam = ctx.inputs.camera("camera").unwrap_or_else(Camera::default_perspective);

        let Some(out_buf) = ctx.outputs.array("particles") else {
            return;
        };

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (out_buf.size / particle_size) as u32;
        let active_count = active_count.min(capacity);

        let uniforms = SeedUniforms {
            active_count,
            pattern_type: pattern,
            trigger_count,
            _pad0: 0,
            container,
            ctr_scale,
            flatten,
            _pad1: 0.0,
            cam_fwd_x: cam.fwd[0],
            cam_fwd_y: cam.fwd[1],
            cam_fwd_z: cam.fwd[2],
            _pad2: 0.0,
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("../../generators/shaders/fluid_simulate_3d.wgsl"),
                "seed_pattern",
                "node.fluid_seed_3d",
            )
        });

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Buffer {
                    binding: 0,
                    buffer: out_buf,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 1,
                    data: bytemuck::bytes_of(&uniforms),
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.fluid_seed_3d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn fluid_seed_3d_declares_camera_in_and_particle_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(FluidSeed3D::TYPE_ID, "node.fluid_seed_3d");
        assert_eq!(FluidSeed3D::INPUTS[0].name, "camera");
        assert_eq!(FluidSeed3D::INPUTS[0].ty, PortType::Camera);
        assert!(FluidSeed3D::INPUTS[0].required);
        assert_eq!(FluidSeed3D::OUTPUTS.len(), 1);
        assert_eq!(FluidSeed3D::OUTPUTS[0].name, "particles");
        assert_eq!(
            FluidSeed3D::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );
    }

    #[test]
    fn fluid_seed_3d_uniform_struct_is_48_bytes() {
        assert_eq!(std::mem::size_of::<SeedUniforms>(), 48);
    }

    #[test]
    fn fluid_seed_3d_has_eight_patterns_including_random() {
        assert_eq!(FLUID_SEED_3D_PATTERNS.len(), 8);
        assert_eq!(FLUID_SEED_3D_PATTERNS[7], "Random");
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = FluidSeed3D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.fluid_seed_3d");
    }
}
