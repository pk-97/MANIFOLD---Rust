//! `node.camera_sky` — the equirect environment as the camera sees it.
//!
//! render_scene leaves alpha 0 wherever nothing was drawn, so a scene with an
//! HDRI lights its objects from the sky but shows black behind them. This atom
//! draws that sky: per pixel it builds the camera ray from the camera basis
//! and `tan(fov_y/2)`, then looks the ray up with render_scene's own
//! direction→uv convention (`pbr_equirect_uv`), so the backdrop matches the
//! reflections exactly. Composite the scene over it with `node.over`.
//!
//! The camera is read only through derived uniforms, never as a GPU binding,
//! so the atom fuses with its neighbours (`docs/CINEMATIC_POST_DESIGN.md` D7).
//! Aspect comes from the output's own dims inside the body.

use manifold_gpu::GpuSamplerDesc;

use manifold_node_engine::scene::camera::{Camera, CameraMode};
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::{dispatch_standalone_2d, standalone_pipeline};

/// Generated layout: no params, then the ten derived fields in declaration
/// order, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraSkyUniforms {
    fwd_x: f32,
    fwd_y: f32,
    fwd_z: f32,
    right_x: f32,
    right_y: f32,
    right_z: f32,
    up_x: f32,
    up_y: f32,
    up_z: f32,
    tan_y: f32,
    _pad0: f32,
    _pad1: f32,
}

manifold_node_engine::primitive! {
    name: CameraSky,
    type_id: "node.camera_sky",
    purpose: "Draws an equirect environment (an HDRI) as the wired Camera sees it: each pixel's ray d = normalize(fwd + ndc.x·tan(fov_y/2)·aspect·right + ndc.y·tan(fov_y/2)·up) is looked up at uv = (atan2(d.z, d.x)/2π + 0.5, asin(d.y)/π + 0.5), render_scene's own env convention, so the backdrop lines up with the scene's reflections. Alpha is 1. Composite render_scene's colour over it with node.over to put the sky behind a scene. An orthographic camera falls back to a 60° field of view.",
    inputs: {
        sky: Texture2D required,
        camera: Camera required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [],
    depth_rule: Warp,
    composition_notes: "Wire the same environment texture render_scene's `envmap` reads (after any exposure node) into `sky`, and the same Camera render_scene reads into `camera`. Output is canvas-sized. Then node.over with render_scene.color as `top` and this as `bottom`, before bokeh/motion blur/tone map so the sky is graded with the scene.",
    examples: ["preset.generator.ocean"],
    picker: { label: "Camera Sky", category: Atom },
    summary: "Shows the HDRI sky behind a 3D scene, seen through the scene's camera so it moves and lines up with the reflections.",
    category: Generate,
    role: Source,
    aliases: ["sky", "background", "skybox", "environment", "hdri background", "backdrop"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/camera_sky_body.wgsl"),
    input_access: [Gather],
    derived_uniforms: ["fwd_x", "fwd_y", "fwd_z", "right_x", "right_y", "right_z", "up_x", "up_y", "up_z", "tan_y"],
}

/// The ten derived fields in declaration order — shared by `run()` and the
/// fused-path recompute so the two cannot drift.
fn derive(cam: &Camera) -> [f32; 10] {
    let fov_y = match cam.mode {
        CameraMode::Perspective { fov_y } => fov_y,
        CameraMode::Orthographic { .. } => std::f32::consts::FRAC_PI_3,
    };
    let [fx, fy, fz] = cam.fwd;
    let [rx, ry, rz] = cam.right;
    let [ux, uy, uz] = cam.up;
    [fx, fy, fz, rx, ry, rz, ux, uy, uz, (0.5 * fov_y).tan()]
}

inventory::submit! {
    manifold_node_engine::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.camera_sky",
        array_ports: &[],
        recompute: |ctx| ctx.camera.map(derive).map(|v| v.to_vec()),
    }
}

fn uniforms(cam: &Camera) -> CameraSkyUniforms {
    let [fwd_x, fwd_y, fwd_z, right_x, right_y, right_z, up_x, up_y, up_z, tan_y] = derive(cam);
    CameraSkyUniforms { fwd_x, fwd_y, fwd_z, right_x, right_y, right_z, up_x, up_y, up_z, tan_y, _pad0: 0.0, _pad1: 0.0 }
}

impl Primitive for CameraSky {
    /// A view through the camera, so always canvas-sized. The sky input is a
    /// scene resource: without this the plan's max-of-input-dims default sizes
    /// the output to the HDRI (2:1), and the body's aspect from its own dims
    /// no longer matches the frame's (BUG-140 class).
    fn output_canvas_scale(
        &self,
        _port: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        Some((1, 1))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cam = ctx.inputs.camera("camera").unwrap_or_else(Camera::default_perspective);
        let Some(sky) = ctx.inputs.texture_2d("sky") else {
            return;
        };
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        if out.width == 0 || out.height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        dispatch_standalone_2d(
            gpu,
            pipeline,
            bytemuck::bytes_of(&uniforms(&cam)),
            &[sky],
            Some(sampler),
            out,
            "node.camera_sky",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::CameraSky;
    use manifold_node_engine::exec::execution_plan::compile;
    use manifold_node_engine::graph::Graph;
    use manifold_node_engine::parameters::ParamValue;
    use crate::node_graph::primitives::{FreeCamera, HdriSource};
    use manifold_node_engine::scene::boundary_nodes::FinalOutput;

    /// A 2:1 HDRI feeding the sky must not size it: the output is the frame.
    #[test]
    fn camera_sky_is_canvas_sized_whatever_the_hdri() {
        let mut g = Graph::new();
        let hdri = g.add_node(Box::new(HdriSource::new()));
        let camera = g.add_node(Box::new(FreeCamera::new()));
        let sky = g.add_node(Box::new(CameraSky::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(hdri, "width", ParamValue::Float(4096.0)).unwrap();
        g.set_param(hdri, "height", ParamValue::Float(2048.0)).unwrap();
        g.connect((hdri, "out"), (sky, "sky")).unwrap();
        g.connect((camera, "out"), (sky, "camera")).unwrap();
        g.connect((sky, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let hdri_out = plan.steps().iter().find(|s| s.node == hdri).unwrap().outputs[0].1;
        assert_eq!(plan.resource_dims(hdri_out), Some((4096, 2048)), "the HDRI is concrete");
        let sky_out = plan.steps().iter().find(|s| s.node == sky).unwrap().outputs[0].1;
        assert_eq!(plan.resource_dims(sky_out), None);
        assert_eq!(plan.resource_canvas_scale(sky_out), Some((1, 1)));
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! The kernel against a CPU ray → equirect lookup. The sky stores its own
    //! texel-centre uv in R and G, which bilinear filtering reproduces exactly
    //! away from the edges, so every output pixel must read back the uv its
    //! ray maps to.

    use half::f16;
    use manifold_gpu::{
        GpuBinding, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension,
        GpuTextureFormat, GpuTextureUsage,
    };

    use super::{CameraSky, uniforms};
    use manifold_node_engine::scene::camera::{Camera, CameraMode};
    use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
    use manifold_node_engine::gpu::render_target::RenderTarget;

    fn uv_sky(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                px[i] = f16::from_f32((x as f32 + 0.5) / w as f32);
                px[i + 1] = f16::from_f32((y as f32 + 0.5) / h as f32);
                px[i + 2] = f16::from_f32(0.5);
                px[i + 3] = f16::from_f32(0.25);
            }
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
            label: "camera-sky-uv",
            mip_levels: 1,
        });
        let bytes = unsafe { std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice())) };
        device.upload_texture(&tex, bytes);
        tex
    }

    fn readback(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        let buf = device.create_buffer_shared(u64::from(w * h * 8));
        let mut enc = device.create_encoder("camera-sky-readback");
        enc.copy_texture_to_buffer(tex, &buf, w, h, w * 8);
        enc.commit_and_wait_completed();
        let ptr = buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        halves.chunks_exact(4).map(|c| std::array::from_fn(|k| f16::from_bits(c[k]).to_f32())).collect()
    }

    fn camera(yaw: f32, pitch: f32) -> Camera {
        let fwd = [pitch.cos() * yaw.sin(), pitch.sin(), -pitch.cos() * yaw.cos()];
        let right = [yaw.cos(), 0.0, yaw.sin()];
        let up = [
            right[1] * fwd[2] - right[2] * fwd[1],
            right[2] * fwd[0] - right[0] * fwd[2],
            right[0] * fwd[1] - right[1] * fwd[0],
        ];
        let mut cam = Camera::default_perspective();
        cam.fwd = fwd;
        cam.right = right;
        cam.up = up;
        cam.mode = CameraMode::Perspective { fov_y: 0.8 };
        cam
    }

    /// CPU reference: the env uv this pixel's ray looks up.
    fn expected_uv(cam: &Camera, x: u32, y: u32, w: u32, h: u32) -> [f32; 2] {
        let CameraMode::Perspective { fov_y } = cam.mode else { unreachable!() };
        let t = (0.5 * fov_y as f64).tan();
        let uv = [(x as f64 + 0.5) / w as f64, (y as f64 + 0.5) / h as f64];
        let ndc = [uv[0] * 2.0 - 1.0, 1.0 - uv[1] * 2.0];
        let aspect = w as f64 / h as f64;
        let d: [f64; 3] = std::array::from_fn(|i| {
            cam.fwd[i] as f64 + ndc[0] * t * aspect * cam.right[i] as f64 + ndc[1] * t * cam.up[i] as f64
        });
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let d = d.map(|c| c / len);
        let pi = std::f64::consts::PI;
        [((d[2].atan2(d[0]) / (2.0 * pi) + 0.5).rem_euclid(1.0)) as f32, ((d[1].asin()) / pi + 0.5) as f32]
    }

    #[test]
    fn camera_sky_matches_cpu() {
        let device = manifold_gpu::testkit::test_device();
        let (sw, sh) = (256u32, 128u32);
        let (w, h) = (48u32, 27u32);
        let sky = uv_sky(&device, sw, sh);
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let mut slot = None;
        let pipeline = standalone_pipeline::<CameraSky>(&mut slot, &device);

        // Level toward +x (u = 0.5), toward −z pitched up, and looking down
        // steeply — none of them crosses the u seam at −x.
        for (yaw, pitch) in [(std::f32::consts::FRAC_PI_2, 0.0), (0.0, 0.5), (0.9, -1.0)] {
            let cam = camera(yaw, pitch);
            let out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "camera-sky-out");
            let u = uniforms(&cam);
            let mut enc = device.create_encoder("camera-sky");
            enc.dispatch_compute(
                pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&u) },
                    GpuBinding::Texture { binding: 1, texture: &sky },
                    GpuBinding::Sampler { binding: 2, sampler: &sampler },
                    GpuBinding::Texture { binding: 3, texture: &out.texture },
                ],
                [w.div_ceil(16), h.div_ceil(16), 1],
                "camera-sky",
            );
            enc.commit_and_wait_completed();
            let got = readback(&device, &out.texture, w, h);
            let mut worst = 0.0f32;
            for y in 0..h {
                for x in 0..w {
                    let [eu, ev] = expected_uv(&cam, x, y, w, h);
                    let px = got[(y * w + x) as usize];
                    worst = worst.max((px[0] - eu).abs()).max((px[1] - ev).abs());
                    assert_eq!(px[3], 1.0, "sky alpha is opaque");
                    assert!((px[2] - 0.5).abs() < 1e-3);
                }
            }
            assert!(worst < 2e-3, "yaw {yaw} pitch {pitch}: worst uv error {worst}");
        }
    }
}

#[cfg(any(test, feature = "gpu-proofs"))]
mod extent;
