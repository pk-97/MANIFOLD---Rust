//! `node.draw_particles_camera` — fused 3D-particle camera projection +
//! 2D scatter for FluidSim3D's display path. Bit-exact wrap of
//! `generators/shaders/fluid_scatter_3d.wgsl`'s `splat_projected` entry via
//! include_str.
//!
//! Takes a `camera: Camera` input (typically from `node.orbit_camera`) and
//! reads its basis vectors + position to project each 3D particle through
//! orthographic (with toroidal wrap) or perspective camera math, then atomic-
//! adds `scaled_energy` into a 2D u32 accumulator buffer sized
//! `disp_w × disp_h`. Pair with `node.resolve_scatter` downstream to lift
//! the u32 grid into a float Texture2D for display.
//!
//! Sibling to `node.draw_particles` (2D in → 2D grid out) and
//! `node.draw_particles_3d` (3D in → 3D grid out): this one is 3D in → 2D
//! grid out via a camera.
//!
//! The `mode` enum param dispatches between Perspective and Orthographic
//! projection. The camera's own [`CameraMode`] is ignored — the splat
//! primitive owns the projection style choice because it changes the actual
//! pixel-write behavior (perspective culls behind-camera particles; ortho
//! wraps toroidally so edges connect seamlessly).

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::camera::Camera;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const SCATTER_CAMERA_MODES: &[&str] = &["Perspective", "Orthographic"];

/// Generated-codegen uniform layout: scalar params in PARAMS order
/// (`active_count`, `disp_w`, `disp_h` Int → i32, `mode` Enum → u32,
/// `scaled_energy` Int → i32), then the four DERIVED camera basis vec3s as 12
/// consecutive f32 (run() resolves them from the wired Camera input), then the
/// codegen-injected `dispatch_count` (u32, the splat guard = clamped
/// active_count), padded to a 16-byte multiple. 18 words + 2 pad = 80 bytes.
/// `ortho` and `aspect` are derived IN-BODY from `mode` / `disp_w` / `disp_h`,
/// so they're no longer uniform fields. The legacy 112-byte same-binding-same-
/// size padding is gone — the generated standalone kernel is single-entry.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProjectedUniforms {
    active_count: i32,
    disp_w: i32,
    disp_h: i32,
    mode: u32,
    scaled_energy: i32,
    cam_pos_x: f32,
    cam_pos_y: f32,
    cam_pos_z: f32,
    cam_fwd_x: f32,
    cam_fwd_y: f32,
    cam_fwd_z: f32,
    cam_right_x: f32,
    cam_right_y: f32,
    cam_right_z: f32,
    cam_up_x: f32,
    cam_up_y: f32,
    cam_up_z: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: ScatterParticlesCamera,
    type_id: "node.draw_particles_camera",
    purpose: "Fused 3D→2D camera projection + atomic-add scatter. Takes 3D particles and a Camera; projects each particle through orthographic (with toroidal wrap) or perspective camera math; atomic-adds scaled_energy into a 2D u32 accumulator. Pair downstream with node.resolve_scatter → texture for display. Sibling to node.draw_particles (2D in/2D out) and node.draw_particles_3d (3D in/3D out) — this one bridges 3D in to 2D-grid out via a camera. Used by FluidSim3D's display path.",
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
            name: Cow::Borrowed("active_count"),
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(100_000.0),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("disp_w"),
            label: "Display Width",
            ty: ParamType::Int,
            default: ParamValue::Float(1920.0),
            range: Some((16.0, 8192.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("disp_h"),
            label: "Display Height",
            ty: ParamType::Int,
            default: ParamValue::Float(1080.0),
            range: Some((16.0, 8192.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Projection",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: SCATTER_CAMERA_MODES,
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
    composition_notes: "Reads cam.pos, cam.fwd, cam.right, cam.up from the input camera; ignores cam.fov_y (the splat math is implicit-FOV — basis vectors set the projection scale). `mode` dispatches between Perspective (geometrically correct + culls behind-camera) and Orthographic (toroidal wrap on screen edges). Aspect is derived from disp_w / disp_h. Downstream node.resolve_scatter self-clears the accumulator after reading it — no scatter-side pre-clear needed.",
    examples: [],
    picker: { label: "Draw Particles (camera)", category: Atom },
    summary: "Projects 3D particles through a camera and splats them onto a 2D image in one step. The display path for a 3D particle sim.",
    category: Particles3D,
    role: Filter,
    aliases: ["draw particles camera", "scatter particles camera", "project scatter", "3d to 2d"],
    fusion_kind: Boundary,
    boundary_reason: Blocked,
    wgsl_body: include_str!("shaders/scatter_particles_camera_body.wgsl"),
    derived_uniforms: ["cam_pos:vec3", "cam_fwd:vec3", "cam_right:vec3", "cam_up:vec3"],
    atomic_outputs: ["accum"],
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

        // `mode` is packed into the param slot (resolved port-shadow value); the
        // body derives ortho = (mode == 1) and aspect = disp_w / disp_h. The
        // camera basis flows as the four derived vec3 uniform fields.
        let uniforms = ProjectedUniforms {
            active_count: active_count as i32,
            disp_w: disp_w as i32,
            disp_h: disp_h as i32,
            mode: mode_value,
            scaled_energy: scaled_energy as i32,
            cam_pos_x: cam.pos[0],
            cam_pos_y: cam.pos[1],
            cam_pos_z: cam.pos[2],
            cam_fwd_x: cam.fwd[0],
            cam_fwd_y: cam.fwd[1],
            cam_fwd_z: cam.fwd[2],
            cam_right_x: cam.right[0],
            cam_right_y: cam.right[1],
            cam_right_z: cam.right[2],
            cam_up_x: cam.up[0],
            cam_up_y: cam.up[1],
            cam_up_z: cam.up[2],
            dispatch_count: active_count,
            _pad0: 0,
            _pad1: 0,
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer ATOMIC
            // SCATTER with camera projection — the body projects each particle
            // and `atomicAdd`s into `buf_accum`; the camera basis arrives as four
            // derived vec3 uniforms). The shared fluid_scatter_3d.wgsl
            // splat_projected is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.draw_particles_camera standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.draw_particles_camera",
            )
        });

        // Generated binding order follows INPUTS: uniform(0), particles(1, read),
        // accum(2, atomic read_write). The hand splat_projected bound them as
        // particles(0)/accum(1)/uniform(2) — rebind. (Camera is not a GPU
        // binding; its data is in the uniform.)
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
            "node.draw_particles_camera",
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

        assert_eq!(ScatterParticlesCamera::TYPE_ID, "node.draw_particles_camera");
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
    fn scatter_particles_camera_uniform_struct_is_generated_layout() {
        // Generated standalone layout: 5 params + 12 derived camera f32 +
        // dispatch_count + 2 pad = 20 words = 80 bytes. (The legacy 112-byte
        // same-binding padding the shared module needed is gone — single-entry.)
        assert_eq!(std::mem::size_of::<ProjectedUniforms>(), 80);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ScatterParticlesCamera::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.draw_particles_camera");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain ATOMIC SCATTER + CAMERA-PROJECTION parity oracle (freeze
    //! §12). The generated standalone kernel projects each particle through the
    //! camera basis (passed as four DERIVED vec3 uniforms) and `atomicAdd`s into
    //! a 2D accumulator — it must reproduce the shared `fluid_scatter_3d.wgsl`
    //! `splat_projected` cell-for-cell in BOTH projection modes. Hand binds
    //! particles@0/accum@1/uniform@2 (shared-module convention); generated binds
    //! uniform@0/particles@1/accum@2. The hand uniform precomputes ortho +
    //! aspect; the generated derives both in-body from mode / disp_w / disp_h.
    use super::*;

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

    #[allow(clippy::too_many_arguments)]
    fn dispatch_proj(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        entry: &str,
        particles: &[Particle],
        cells: usize,
        uniform: &[u8],
        count: u32,
        generated: bool,
    ) -> Vec<u32> {
        let pipeline = device.create_compute_pipeline(wgsl, entry, "proj-oracle");
        let pbuf = device.create_buffer_shared(std::mem::size_of_val(particles) as u64);
        let abuf = device.create_buffer_shared((cells * 4) as u64);
        let zeros = vec![0u32; cells];
        unsafe {
            pbuf.write(0, bytemuck::cast_slice(particles));
            abuf.write(0, bytemuck::cast_slice(&zeros));
        }
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
        let mut enc = device.create_encoder("proj-oracle");
        enc.dispatch_compute(&pipeline, &bindings, [count.div_ceil(256), 1, 1], "proj-oracle");
        enc.commit_and_wait_completed();
        let ptr = abuf.mapped_ptr().expect("shared accumulator buffer");
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const u32, cells) };
        slice.to_vec()
    }

    #[test]
    fn generated_splat_projected_matches_hand_kernel_both_modes() {
        let device = crate::test_device();
        let disp_w = 8u32;
        let disp_h = 8u32;
        let cells = (disp_w * disp_h) as usize;
        let scaled_energy = 4096u32;
        let aspect = disp_w as f32 / disp_h as f32;

        // A camera looking down +Z from in front of the unit cube's centre.
        let cam = Camera {
            pos: [0.0, 0.0, -1.5],
            fwd: [0.0, 0.0, 1.0],
            right: [1.0, 0.0, 0.0],
            up: [0.0, 1.0, 0.0],
            ..Camera::default_perspective()
        };

        let particles = [
            alive(0.5, 0.5, 0.5),
            alive(0.3, 0.7, 0.4),
            alive(0.7, 0.2, 0.6),
            alive(0.9, 0.9, 0.5),
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

        let push_vec3 = |buf: &mut Vec<u8>, v: [f32; 3]| {
            buf.extend_from_slice(&v[0].to_le_bytes());
            buf.extend_from_slice(&v[1].to_le_bytes());
            buf.extend_from_slice(&v[2].to_le_bytes());
        };

        for mode in [0u32, 1u32] {
            let ortho = if mode == 1 { 1u32 } else { 0u32 };

            // Hand layout (112 bytes): active_count, disp_w, disp_h, ortho,
            //   scaled_energy, 3 pad; then each cam vec3 + 1 pad; aspect + 3 pad.
            let mut hand = Vec::new();
            hand.extend_from_slice(&n.to_le_bytes());
            hand.extend_from_slice(&disp_w.to_le_bytes());
            hand.extend_from_slice(&disp_h.to_le_bytes());
            hand.extend_from_slice(&ortho.to_le_bytes());
            hand.extend_from_slice(&scaled_energy.to_le_bytes());
            hand.extend_from_slice(&[0u8; 12]); // _pad0.._pad2
            for v in [cam.pos, cam.fwd, cam.right, cam.up] {
                push_vec3(&mut hand, v);
                hand.extend_from_slice(&0f32.to_le_bytes()); // per-vec3 pad
            }
            hand.extend_from_slice(&aspect.to_le_bytes());
            hand.extend_from_slice(&[0u8; 12]); // _pad7.._pad9

            // Generated layout (80 bytes): active_count(i32), disp_w(i32),
            //   disp_h(i32), mode(u32), scaled_energy(i32); 4 cam vec3 (no pad);
            //   dispatch_count(u32), 2 pad.
            let mut gen_bytes = Vec::new();
            gen_bytes.extend_from_slice(&(n as i32).to_le_bytes());
            gen_bytes.extend_from_slice(&(disp_w as i32).to_le_bytes());
            gen_bytes.extend_from_slice(&(disp_h as i32).to_le_bytes());
            gen_bytes.extend_from_slice(&mode.to_le_bytes());
            gen_bytes.extend_from_slice(&(scaled_energy as i32).to_le_bytes());
            for v in [cam.pos, cam.fwd, cam.right, cam.up] {
                push_vec3(&mut gen_bytes, v);
            }
            gen_bytes.extend_from_slice(&n.to_le_bytes());
            gen_bytes.extend_from_slice(&[0u8; 8]);

            let hand_wgsl = include_str!("../../generators/shaders/fluid_scatter_3d.wgsl");
            let gen_wgsl =
                crate::node_graph::freeze::codegen::standalone_for_spec::<ScatterParticlesCamera>()
                    .expect("scatter_particles_camera buffer codegen");
            assert!(gen_wgsl.contains("atomic<u32>"), "atomic accumulator emitted");
            assert!(gen_wgsl.contains("cam_pos_x: f32"), "vec3 derived field expanded");

            let from_hand = dispatch_proj(
                &device, hand_wgsl, "splat_projected", &particles, cells, &hand, n, false,
            );
            let from_gen = dispatch_proj(
                &device, &gen_wgsl, "cs_main", &particles, cells, &gen_bytes, n, true,
            );

            assert_eq!(from_hand, from_gen, "mode={mode} projected scatter mismatch");
        }
    }
}
