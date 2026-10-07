//! `node.over` — Porter-Duff "over" for premultiplied colour:
//! `out = top + bottom · (1 − top.a)`, alpha included.
//!
//! render_scene writes premultiplied colour with alpha 0 where nothing was
//! drawn, so `over(top: scene, bottom: sky)` puts a backdrop behind a scene
//! without touching its edges. `node.mix`'s modes are RGB blends with a
//! scalar weight; none of them reads the top layer's own coverage.
//! No params: codegen binds no uniform, so the standalone bindings are
//! `top(0)`, `bottom(1)`, `samp(2)`, `dst(3)`.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;
use super::standalone_pipeline::standalone_pipeline;

crate::primitive! {
    name: Over,
    type_id: "node.over",
    purpose: "Porter-Duff over for premultiplied colour: out = top + bottom·(1 − top.a), alpha included. Puts `top` in front of `bottom` using top's own coverage. render_scene's colour is premultiplied with alpha 0 where nothing was drawn, so over(top: scene, bottom: node.camera_sky) draws a sky behind a 3D scene. Straight-alpha (unpremultiplied) inputs give fringed edges.",
    inputs: {
        top: Texture2D required,
        bottom: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [],
    depth_rule: CombineNearest,
    composition_notes: "Both inputs premultiplied. The usual chain: render_scene.color -> top, node.camera_sky.out -> bottom, then bokeh/motion blur/tone map downstream so the backdrop is graded with the scene.",
    examples: ["preset.generator.ocean"],
    picker: { label: "Over", category: Atom },
    summary: "Places one image in front of another using the front image's transparency, like stacking a cut-out on a background.",
    category: Composite,
    role: Filter,
    aliases: ["over", "alpha over", "composite", "layer", "background", "premultiplied"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/over_body.wgsl"),
}

impl Primitive for Over {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(top) = ctx.inputs.texture_2d("top") else {
            return;
        };
        let Some(bottom) = ctx.inputs.texture_2d("bottom") else {
            return;
        };
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out.width, out.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Texture { binding: 0, texture: top },
                GpuBinding::Texture { binding: 1, texture: bottom },
                GpuBinding::Sampler { binding: 2, sampler },
                GpuBinding::Texture { binding: 3, texture: out },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.over",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use half::f16;
    use manifold_gpu::{
        GpuBinding, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension,
        GpuTextureFormat, GpuTextureUsage,
    };

    use super::Over;
    use crate::node_graph::primitives::standalone_pipeline::standalone_pipeline;
    use crate::render_target::RenderTarget;

    fn upload(device: &GpuDevice, w: u32, h: u32, px: &[[f32; 4]]) -> GpuTexture {
        let halves: Vec<f16> = px.iter().flatten().map(|&v| f16::from_f32(v)).collect();
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
            label: "over-input",
            mip_levels: 1,
        });
        let bytes = unsafe { std::slice::from_raw_parts(halves.as_ptr().cast::<u8>(), halves.len() * 2) };
        device.upload_texture(&tex, bytes);
        tex
    }

    /// The kernel against `top + bottom·(1 − top.a)` on a premultiplied top
    /// whose coverage runs 0 → 1 across x, over an opaque bottom.
    #[test]
    fn over_matches_cpu() {
        let device = crate::test_device();
        let (w, h) = (32u32, 16u32);
        let n = (w * h) as usize;
        let top: Vec<[f32; 4]> = (0..n)
            .map(|i| {
                let (x, y) = ((i as u32 % w) as f32, (i as u32 / w) as f32);
                let a = x / (w - 1) as f32;
                [0.9 * a, (y / h as f32) * a, 0.2 * a, a]
            })
            .collect();
        let bottom: Vec<[f32; 4]> = (0..n).map(|i| [0.1, 0.4 + (i % 7) as f32 * 0.05, 1.5, 1.0]).collect();
        let (t, b) = (upload(&device, w, h, &top), upload(&device, w, h, &bottom));
        let out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "over-out");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let mut slot = None;
        let pipeline = standalone_pipeline::<Over>(&mut slot, &device);
        let mut enc = device.create_encoder("over");
        enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Texture { binding: 0, texture: &t },
                GpuBinding::Texture { binding: 1, texture: &b },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "over",
        );
        enc.commit_and_wait_completed();

        let buf = device.create_buffer_shared(u64::from(w * h * 8));
        let mut enc = device.create_encoder("over-readback");
        enc.copy_texture_to_buffer(&out.texture, &buf, w, h, w * 8);
        enc.commit_and_wait_completed();
        let ptr = buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), n * 4) };
        for i in 0..n {
            // Inputs are f16 on the GPU; the reference uses the same rounding.
            let q = |v: f32| f16::from_f32(v).to_f32();
            let ta = q(top[i][3]);
            for c in 0..4 {
                let want = q(top[i][c]) + q(bottom[i][c]) * (1.0 - ta);
                let got = f16::from_bits(halves[i * 4 + c]).to_f32();
                assert!((got - want).abs() <= 2e-3 * want.abs().max(1.0), "texel {i} channel {c}: {got} vs {want}");
            }
        }
    }
}

#[cfg(any(test, feature = "gpu-proofs"))]
mod extent;
