//! `node.flatten_to_camera_plane` — compress particles toward the camera
//! viewing plane.
//!
//! The "flatten" depth-collapse FluidSim3D uses to make the volume read
//! as a flat sheet facing the camera. For each live particle, the
//! component of its offset-from-centre along the camera forward axis is
//! scaled down toward the viewing plane:
//!
//! ```text
//! depth = dot(position - 0.5, cam_fwd)
//! position -= cam_fwd * depth * flatten * 0.1
//! ```
//!
//! Bit-exact with the flatten step of the legacy fused
//! `node.fluid_simulate_3d`. Takes a `Camera` input (typically from
//! `node.orbit_camera`) and reads `cam.fwd`.

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order (`flatten`
/// f32, `active_count` Int → i32), then the THREE derived camera-forward fields
/// (resolved CPU-side from the wired Camera port), then the codegen-injected
/// `dispatch_count`, padded to a 16-byte multiple. 6 words + 2 pad = 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FlattenUniforms {
    flatten: f32,
    active_count: i32,
    cam_fwd_x: f32,
    cam_fwd_y: f32,
    cam_fwd_z: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: FlattenToCameraPlane,
    type_id: "node.flatten_to_camera_plane",
    purpose: "Compress particles toward the camera viewing plane. For each live particle: depth = dot(position - 0.5, cam.fwd); position -= cam.fwd * depth * flatten * 0.1. Collapses a 3D particle volume toward a flat sheet facing the camera (FluidSim3D's flatten control). Takes a Camera input and reads cam.fwd. Decomposed from the flatten step of the fused node.fluid_simulate_3d.",
    inputs: {
        in: Array(Particle) required,
        camera: Camera required,
        flatten: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("flatten"),
            label: "Flatten",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
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
    composition_notes: "Aliased in/out — mutates the particle buffer in place. `flatten` is port-shadow so a slider / LFO drives the depth-collapse live; flatten <= 0 is a no-op (inert at the default). Wire a Camera (node.orbit_camera) into `camera` so the compression direction tracks the live view — the same camera should feed the display projector (node.draw_particles_camera). Place last in the per-particle position chain (after node.keep_in_box_3d), matching the legacy order.",
    examples: ["FluidSim3D"],
    picker: { label: "Flatten to Camera Plane", category: Atom },
    summary: "Squashes a cloud of 3D particles flat toward the camera by a dial-able amount, from a full volume down to a pancake facing the screen.",
    category: Particles3D,
    role: Filter,
    aliases: ["flatten to camera", "squash", "billboard", "flatten"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/flatten_to_camera_plane_body.wgsl"),
    derived_uniforms: ["cam_fwd_x", "cam_fwd_y", "cam_fwd_z"],
}

// D7/P0 (`docs/CINEMATIC_POST_DESIGN.md`): per-frame recompute for a FUSED
// region's `cam_fwd_x`/`_y`/`_z` fields, IN DECLARATION ORDER — reads the
// region's routed Camera external (via install.rs's `camera_ext_N` wiring),
// matching `run()`'s own `cam.fwd` read below. `None` when unwired (no
// Camera reached this fused kernel) — the fields stay zeroed, same as the
// unfused atom's `Camera::default_perspective()` fallback would give a
// forward of `[0,0,1]`... except the zero-fallback here differs (all-zero,
// not a unit vector); this can only happen if install ever routes a
// `camera_ext_N` with nothing wired, which it does not — install only
// creates the port when a real producer wire exists.
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.flatten_to_camera_plane",
        recompute: |ctx| ctx.camera.map(|c| vec![c.fwd[0], c.fwd[1], c.fwd[2]]),
    }
}

impl Primitive for FlattenToCameraPlane {
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
        let flatten = ctx.scalar_or_param("flatten", 0.0);
        let active_count = ctx.scalar_or_param("active_count", 100_000.0).round().max(0.0) as u32;

        let cam = ctx.inputs.camera("camera").unwrap_or_else(Camera::default_perspective);

        let Some(particles) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let _ = out;

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (particles.size / particle_size) as u32;
        let active_count = active_count.min(capacity);
        if active_count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // coincident path; cam_fwd_x/y/z as derived fields).
            // flatten_to_camera_plane.wgsl (the hand-kernel parity oracle) was deleted 2026-07-20 (W1-B, migration scaffolding retired).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.flatten_to_camera_plane standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.flatten_to_camera_plane",
            )
        });

        let uniforms = FlattenUniforms {
            flatten,
            active_count: active_count as i32,
            cam_fwd_x: cam.fwd[0],
            cam_fwd_y: cam.fwd[1],
            cam_fwd_z: cam.fwd[2],
            dispatch_count: active_count,
            _pad0: 0,
            _pad1: 0,
        };

        // `in`/`out` alias one particle buffer; the generated kernel binds buf_in
        // (read, 1) + buf_out (read_write, 2) — bind it to both (pointwise).
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
                    buffer: particles,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.flatten_to_camera_plane",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_aliased_particle_in_out_and_camera() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(FlattenToCameraPlane::TYPE_ID, "node.flatten_to_camera_plane");
        let names: Vec<&str> = FlattenToCameraPlane::INPUTS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["in", "camera", "flatten", "active_count"]);
        assert_eq!(
            FlattenToCameraPlane::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert!(FlattenToCameraPlane::INPUTS[0].required);
        assert_eq!(FlattenToCameraPlane::INPUTS[1].name, "camera");
        assert_eq!(FlattenToCameraPlane::INPUTS[1].ty, PortType::Camera);
        assert!(FlattenToCameraPlane::INPUTS[1].required);

        assert_eq!(FlattenToCameraPlane::OUTPUTS.len(), 1);
        assert_eq!(
            FlattenToCameraPlane::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );

        let prim = FlattenToCameraPlane::new();
        let aliased = Primitive::aliased_array_io(&prim);
        assert_eq!(aliased, &[("in", "out")]);
    }

    #[test]
    fn uniform_struct_is_32_bytes() {
        assert_eq!(std::mem::size_of::<FlattenUniforms>(), 32);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = FlattenToCameraPlane::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.flatten_to_camera_plane");
    }
}

