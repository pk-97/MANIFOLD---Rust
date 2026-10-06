//! `node.sea_horizon_env` — an equirect environment as seen from a calm sea.
//!
//! A captured HDRI's lower half is whatever stood under the camera: sand, a
//! road, a tripod. An ocean that reflects it shows brown streaks in every
//! trough. On a real sea a ray that leaves a wave below the horizon meets
//! the sea again, which reflects the sky by Fresnel at that grazing angle and
//! shows deep water for the rest. This atom writes exactly that into the
//! lower hemisphere and passes the sky through, so reflections, the IBL
//! prefilter and `node.camera_sky` all see one consistent sea horizon.

use std::borrow::Cow;
use manifold_gpu::GpuSamplerDesc;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::standalone_pipeline::{dispatch_standalone_2d, standalone_pipeline};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SeaHorizonUniforms {
    ior: f32,
    water_r: f32,
    water_g: f32,
    water_b: f32,
}

crate::primitive! {
    name: SeaHorizonEnv,
    type_id: "node.sea_horizon_env",
    purpose: "Replaces an equirect environment's lower hemisphere with a calm sea: for a ray at depression φ below the horizon, out = F·sky(mirrored) + (1 − F)·water, F = Schlick(ior, cos θ = sin φ). The upper hemisphere passes through unchanged and alpha is 1. Uses the renderer's env convention (v = elevation/π + 0.5). Put it between an HDRI and render_scene's envmap (and node.camera_sky) for open-ocean scenes, so troughs reflect sky and sea instead of the ground under the HDRI's camera.",
    inputs: {
        sky: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef { name: Cow::Borrowed("ior"), label: "IOR", ty: ParamType::Float, default: ParamValue::Float(1.333), range: Some((1.0, 2.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("water_r"), label: "Water Red", ty: ParamType::Float, default: ParamValue::Float(0.015), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("water_g"), label: "Water Green", ty: ParamType::Float, default: ParamValue::Float(0.03), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("water_b"), label: "Water Blue", ty: ParamType::Float, default: ParamValue::Float(0.04), range: Some((0.0, 1.0)), enum_values: &[] },
    ],
    depth_rule: Warp,
    composition_notes: "Water is radiance in the env's own units (linear, after any exposure upstream): the colour deep water shows looking down. Feed the output to render_scene's envmap and node.camera_sky. The input is sampled at the mirrored uv below the horizon, so the input's resolution is kept.",
    examples: ["preset.generator.ocean"],
    picker: { label: "Sea Horizon Environment", category: Atom },
    summary: "Turns the ground half of an HDRI into open sea, so water reflects sky and sea instead of the beach the photo was taken from.",
    // An environment map for lighting and reflections, not a composited image.
    category: MaterialsAndLighting,
    role: Filter,
    aliases: ["sea", "ocean", "horizon", "environment", "hdri", "sky", "lower hemisphere"],
    pure: true,
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/sea_horizon_env_body.wgsl"),
    input_access: [Gather],
}

impl Primitive for SeaHorizonEnv {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let param = |name: &str, default: f32| match ctx.params.get(name) {
            Some(ParamValue::Float(v)) => *v,
            _ => default,
        };
        let uniforms = SeaHorizonUniforms {
            ior: param("ior", 1.333),
            water_r: param("water_r", 0.015),
            water_g: param("water_g", 0.03),
            water_b: param("water_b", 0.04),
        };
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
            bytemuck::bytes_of(&uniforms),
            &[sky],
            Some(sampler),
            out,
            "node.sea_horizon_env",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! The kernel against the CPU formula on a sky that stores its own
    //! texel-centre uv, which bilinear filtering reproduces exactly.

    use half::f16;
    use manifold_gpu::{
        GpuBinding, GpuSamplerDesc, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
        GpuTextureUsage,
    };

    use super::{SeaHorizonEnv, SeaHorizonUniforms};
    use crate::node_graph::primitives::standalone_pipeline::standalone_pipeline;
    use crate::render_target::RenderTarget;

    #[test]
    fn sea_horizon_env_matches_cpu() {
        let device = crate::test_device();
        let (w, h) = (64u32, 32u32);
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                px[i] = f16::from_f32((x as f32 + 0.5) / w as f32);
                px[i + 1] = f16::from_f32((y as f32 + 0.5) / h as f32);
                px[i + 2] = f16::from_f32(2.0);
                px[i + 3] = f16::from_f32(0.5);
            }
        }
        let sky = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
            label: "sea-horizon-sky",
            mip_levels: 1,
        });
        let bytes = unsafe { std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), px.len() * 2) };
        device.upload_texture(&sky, bytes);
        let out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "sea-horizon-out");
        let u = SeaHorizonUniforms { ior: 1.333, water_r: 0.02, water_g: 0.05, water_b: 0.1 };
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let mut slot = None;
        let pipeline = standalone_pipeline::<SeaHorizonEnv>(&mut slot, &device);
        let mut enc = device.create_encoder("sea-horizon");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&u) },
                GpuBinding::Texture { binding: 1, texture: &sky },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "sea-horizon",
        );
        enc.commit_and_wait_completed();
        let buf = device.create_buffer_shared(u64::from(w * h * 8));
        let mut enc = device.create_encoder("sea-horizon-readback");
        enc.copy_texture_to_buffer(&out.texture, &buf, w, h, w * 8);
        enc.commit_and_wait_completed();
        let ptr = buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

        let f0 = ((1.333f64 - 1.0) / (1.333 + 1.0)).powi(2);
        let water = [0.02, 0.05, 0.1];
        let mut worst = 0.0f64;
        for y in 0..h {
            for x in 0..w {
                let (uu, vv) = ((f64::from(x) + 0.5) / f64::from(w), (f64::from(y) + 0.5) / f64::from(h));
                let depression = (0.5 - vv) * std::f64::consts::PI;
                let want = if depression <= 0.0 {
                    [uu, vv, 2.0]
                } else {
                    let f = f0 + (1.0 - f0) * (1.0 - depression.sin()).powi(5);
                    let sky = [uu, 1.0 - vv, 2.0];
                    std::array::from_fn(|c| f * sky[c] + (1.0 - f) * water[c])
                };
                let i = ((y * w + x) * 4) as usize;
                for c in 0..3 {
                    let got = f64::from(f16::from_bits(halves[i + c]).to_f32());
                    worst = worst.max((got - want[c]).abs() / want[c].abs().max(1.0));
                }
                assert_eq!(f16::from_bits(halves[i + 3]).to_f32(), 1.0);
            }
        }
        assert!(worst < 2e-3, "worst relative error {worst}");
    }
}
