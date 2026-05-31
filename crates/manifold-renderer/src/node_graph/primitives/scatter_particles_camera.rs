//! `node.scatter_particles_camera` — fused 3D-particle camera projection +
//! 2D scatter for FluidSim3D's display path. Bit-exact wrap of
//! `generators/shaders/fluid_scatter_3d.wgsl`'s `splat_projected` entry via
//! include_str.
//!
//! Takes a `camera: Camera` input (typically from `node.camera_orbit`) and
//! reads its basis vectors + position to project each 3D particle through
//! orthographic (with toroidal wrap) or perspective camera math, then atomic-
//! adds `scaled_energy` into a 2D u32 accumulator buffer sized
//! `disp_w × disp_h`. Pair with `node.resolve_accumulator` downstream to lift
//! the u32 grid into a float Texture2D for display.
//!
//! Sibling to `node.scatter_particles` (2D in → 2D grid out) and
//! `node.scatter_particles_3d` (3D in → 3D grid out): this one is 3D in → 2D
//! grid out via a camera.
//!
//! The `mode` enum param dispatches between Perspective and Orthographic
//! projection. The camera's own [`CameraMode`] is ignored — the splat
//! primitive owns the projection style choice because it changes the actual
//! pixel-write behavior (perspective culls behind-camera particles; ortho
//! wraps toroidally so edges connect seamlessly).

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const SCATTER_CAMERA_MODES: &[&str] = &["Perspective", "Orthographic"];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProjectedUniforms {
    active_count: u32,
    disp_w: u32,
    disp_h: u32,
    ortho: u32,
    scaled_energy: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
    cam_pos_x: f32,
    cam_pos_y: f32,
    cam_pos_z: f32,
    _pad3: f32,
    cam_fwd_x: f32,
    cam_fwd_y: f32,
    cam_fwd_z: f32,
    _pad4: f32,
    cam_right_x: f32,
    cam_right_y: f32,
    cam_right_z: f32,
    _pad5: f32,
    cam_up_x: f32,
    cam_up_y: f32,
    cam_up_z: f32,
    _pad6: f32,
    aspect: f32,
    _pad7: f32,
    _pad8: f32,
    _pad9: f32,
}

crate::primitive! {
    name: ScatterParticlesCamera,
    type_id: "node.scatter_particles_camera",
    purpose: "Fused 3D→2D camera projection + atomic-add scatter. Takes 3D particles and a Camera; projects each particle through orthographic (with toroidal wrap) or perspective camera math; atomic-adds scaled_energy into a 2D u32 accumulator. Pair downstream with node.resolve_accumulator → texture for display. Sibling to node.scatter_particles (2D in/2D out) and node.scatter_particles_3d (3D in/3D out) — this one bridges 3D in to 2D-grid out via a camera. Used by FluidSim3D's display path.",
    inputs: {
        particles: Array(Particle) required,
        camera: Camera required,
        active_count: ScalarF32 optional,
        disp_w: ScalarF32 optional,
        disp_h: ScalarF32 optional,
        scaled_energy: ScalarF32 optional,
        mode: ScalarF32 optional,
    },
    outputs: {
        accum: Array(u32),
    },
    params: [
        ParamDef {
            name: "active_count",
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(100_000.0),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "disp_w",
            label: "Display Width",
            ty: ParamType::Int,
            default: ParamValue::Float(1920.0),
            range: Some((16.0, 8192.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "disp_h",
            label: "Display Height",
            ty: ParamType::Int,
            default: ParamValue::Float(1080.0),
            range: Some((16.0, 8192.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "mode",
            label: "Projection",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: SCATTER_CAMERA_MODES,
        },
        ParamDef {
            name: "scaled_energy",
            label: "Energy per Particle",
            ty: ParamType::Int,
            default: ParamValue::Float(4096.0),
            range: Some((1.0, 65536.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Reads cam.pos, cam.fwd, cam.right, cam.up from the input camera; ignores cam.fov_y (the splat math is implicit-FOV — basis vectors set the projection scale). `mode` dispatches between Perspective (geometrically correct + culls behind-camera) and Orthographic (toroidal wrap on screen edges). Aspect is derived from disp_w / disp_h. Downstream node.resolve_accumulator self-clears the accumulator after reading it — no scatter-side pre-clear needed.",
    examples: [],
    picker: { label: "Draw Particles (camera)", category: Atom },
    summary: "Projects 3D particles through a camera and splats them onto a 2D image in one step. The display path for a 3D particle sim.",
    category: Particles3D,
    role: Filter,
    aliases: ["draw particles camera", "project scatter", "3d to 2d"],
}

// Legacy type-ID alias — projects authored before the rename from
// `node.fluid_project_scatter_2d` → `node.scatter_particles_camera` still
// load. Hidden from the palette (`picker: None`) so the new name is the
// only visible choice.
inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: "node.fluid_project_scatter_2d",
        create: || Box::new(ScatterParticlesCamera::new()),
        picker: None,
    }
}

impl Primitive for ScatterParticlesCamera {
    /// Display accumulator is sized to the host canvas — same shape as
    /// `scatter_particles` (2D). The chain builder allocates `canvas_w ×
    /// canvas_h` u32 cells via `canvas_sized_array_outputs()` below;
    /// the wires on `disp_w` / `disp_h` (typically from
    /// `system.generator_input.output_width/height`) feed the dispatch
    /// math so the per-frame splat indexes the same grid the buffer was
    /// sized for. Without this override the buffer would size off the
    /// node's inline param defaults (1920 × 1080) while the dispatch
    /// would use the wired canvas dims — a 4K render writes past the
    /// buffer end into adjacent GPU memory (the FluidSim3D white-out
    /// root cause before this was added).
    fn canvas_sized_array_outputs(&self) -> &'static [&'static str] {
        &["accum"]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count = ctx.scalar_or_param("active_count", 100_000.0).round().max(0.0) as u32;
        let disp_w = ctx.scalar_or_param("disp_w", 1920.0).round().max(1.0) as u32;
        let disp_h = ctx.scalar_or_param("disp_h", 1080.0).round().max(1.0) as u32;
        let scaled_energy =
            ctx.scalar_or_param("scaled_energy", 4096.0).round().max(0.0) as u32;
        // `mode` is port-shadows-param: wired Float wins (FluidSim3D
        // computes `1 if container == 0 else 0` via a math chain so the
        // outer `container` enum maps None → Ortho-toroidal vs Cube /
        // Sphere / Torus → Perspective). Falls back to the inline Enum
        // param for direct authoring.
        let mode_value = ctx
            .inputs
            .scalar("mode")
            .and_then(|v| v.as_scalar())
            .map(|f| f.round().max(0.0) as u32)
            .unwrap_or_else(|| match ctx.params.get("mode") {
                Some(ParamValue::Enum(n)) => *n,
                Some(ParamValue::Float(f)) => f.round().max(0.0) as u32,
                _ => 0,
            });
        let ortho = if mode_value == 1 { 1u32 } else { 0u32 };

        let cam = ctx.inputs.camera("camera").unwrap_or_else(Camera::default_perspective);

        let Some(particles) = ctx.inputs.array("particles") else {
            return;
        };
        let Some(accum) = ctx.outputs.array("accum") else {
            return;
        };

        let particle_size = std::mem::size_of::<Particle>() as u64;
        let particle_capacity = (particles.size / particle_size) as u32;
        let active_count = active_count.min(particle_capacity);

        let aspect = disp_w as f32 / disp_h.max(1) as f32;

        let uniforms = ProjectedUniforms {
            active_count,
            disp_w,
            disp_h,
            ortho,
            scaled_energy,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
            cam_pos_x: cam.pos[0],
            cam_pos_y: cam.pos[1],
            cam_pos_z: cam.pos[2],
            _pad3: 0.0,
            cam_fwd_x: cam.fwd[0],
            cam_fwd_y: cam.fwd[1],
            cam_fwd_z: cam.fwd[2],
            _pad4: 0.0,
            cam_right_x: cam.right[0],
            cam_right_y: cam.right[1],
            cam_right_z: cam.right[2],
            _pad5: 0.0,
            cam_up_x: cam.up[0],
            cam_up_y: cam.up[1],
            cam_up_z: cam.up[2],
            _pad6: 0.0,
            aspect,
            _pad7: 0.0,
            _pad8: 0.0,
            _pad9: 0.0,
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("../../generators/shaders/fluid_scatter_3d.wgsl"),
                "splat_projected",
                "node.scatter_particles_camera",
            )
        });

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Buffer {
                    binding: 0,
                    buffer: particles,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: accum,
                    offset: 0,
                },
                GpuBinding::Bytes {
                    binding: 2,
                    data: bytemuck::bytes_of(&uniforms),
                },
            ],
            [active_count.div_ceil(256), 1, 1],
            "node.scatter_particles_camera",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn scatter_particles_camera_declares_particle_camera_in_and_u32_array_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let particle_layout = ArrayType::of_known::<Particle>();
        let u32_layout = ArrayType::of_known::<u32>();

        assert_eq!(ScatterParticlesCamera::TYPE_ID, "node.scatter_particles_camera");
        assert!(ScatterParticlesCamera::INPUTS.len() >= 2);
        assert_eq!(ScatterParticlesCamera::INPUTS[0].name, "particles");
        assert!(ScatterParticlesCamera::INPUTS[0].required);
        assert_eq!(
            ScatterParticlesCamera::INPUTS[0].ty,
            PortType::Array(particle_layout)
        );
        assert_eq!(ScatterParticlesCamera::INPUTS[1].name, "camera");
        assert!(ScatterParticlesCamera::INPUTS[1].required);
        assert_eq!(ScatterParticlesCamera::INPUTS[1].ty, PortType::Camera);
        assert_eq!(ScatterParticlesCamera::OUTPUTS.len(), 1);
        assert_eq!(ScatterParticlesCamera::OUTPUTS[0].name, "accum");
        assert_eq!(
            ScatterParticlesCamera::OUTPUTS[0].ty,
            PortType::Array(u32_layout)
        );
    }

    #[test]
    fn scatter_particles_camera_uniform_struct_is_112_bytes() {
        assert_eq!(std::mem::size_of::<ProjectedUniforms>(), 112);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ScatterParticlesCamera::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.scatter_particles_camera");
    }

    #[test]
    fn legacy_type_id_alias_resolves_to_scatter_particles_camera() {
        let registry = crate::node_graph::persistence::PrimitiveRegistry::with_builtin();
        let node = registry
            .construct("node.fluid_project_scatter_2d")
            .expect("legacy alias must be registered");
        assert_eq!(
            node.type_id().as_str(),
            "node.scatter_particles_camera",
            "legacy alias should construct the new primitive",
        );
    }
}
