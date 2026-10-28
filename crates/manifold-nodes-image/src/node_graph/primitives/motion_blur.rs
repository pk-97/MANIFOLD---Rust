//! `node.motion_blur` — shutter motion blur from a scene's velocity and depth.
//!
//! Three passes (Jimenez 2014, the tile/neighbour reconstruction filter):
//! 1. `motion_blur_tiles.wgsl`: the longest smear in each 16 px tile.
//! 2. `motion_blur_neighbors.wgsl`: the longest smear reaching each tile from
//!    tiles within the longest possible smear, so pixels beside a moving
//!    object blur past its edge.
//! 3. `motion_blur.wgsl`: per-pixel taps along that smear, weighted by depth
//!    so a moving foreground covers the background and turns see-through at
//!    its own edges, and the background never bleeds over the foreground.
//!
//! A pixel's smear is `velocity_ndc * 0.5 * viewport * (shutter_angle / 360)`,
//! centred on the pixel, its length capped at `max_blur_px`. `velocity` is
//! `render_scene`'s NDC move since last frame and `depth` its raw reversed-Z
//! depth, both read at each pixel's spot in their own grid. `camera` supplies
//! the shutter angle and near/far. Tiles where nothing moves copy the source
//! and skip the taps; a closed shutter copies the whole frame.

use std::borrow::Cow;

use manifold_gpu::{
    GpuBinding, GpuComputePipeline, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc,
    GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::camera::Camera;

const DEPTH_COMMON: &str = include_str!("../../generators/shaders/depth_common.wgsl");
const COMMON: &str = include_str!("shaders/motion_blur_common.wgsl");

/// Tile edge in output pixels; `TILE` in motion_blur_common.wgsl matches.
const TILE: u32 = 16;

/// `Uniforms` in motion_blur_common.wgsl, shared by all three passes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MotionBlurUniforms {
    dims: [f32; 2],
    shutter_scale: f32,
    max_blur_px: f32,
    near: f32,
    far: f32,
    reach_tiles: u32,
    _pad: u32,
}

#[derive(Clone, Copy)]
struct MotionBlurSettings {
    shutter_angle: f32,
    max_blur_px: f32,
    near: f32,
    far: f32,
}

pub struct MotionBlurPipelines {
    tiles: GpuComputePipeline,
    neighbors: GpuComputePipeline,
    gather: GpuComputePipeline,
}

impl MotionBlurPipelines {
    fn new(device: &GpuDevice) -> Self {
        let make = |pass: &str, label| {
            let source = format!("{DEPTH_COMMON}\n{COMMON}\n{pass}");
            device.create_compute_pipeline(&source, "cs_main", label)
        };
        Self {
            tiles: make(
                include_str!("shaders/motion_blur_tiles.wgsl"),
                "motion blur tiles",
            ),
            neighbors: make(
                include_str!("shaders/motion_blur_neighbors.wgsl"),
                "motion blur neighbours",
            ),
            gather: make(
                include_str!("shaders/motion_blur.wgsl"),
                "motion blur gather",
            ),
        }
    }
}

pub struct MotionBlurTiles {
    tiles: GpuTexture,
    neighbors: GpuTexture,
}

impl MotionBlurTiles {
    fn new(device: &GpuDevice, width: u32, height: u32) -> Self {
        let make = |label| {
            device.create_texture(&GpuTextureDesc {
                width,
                height,
                depth: 1,
                format: GpuTextureFormat::Rgba16Float,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
                label,
                mip_levels: 1,
            })
        };
        Self {
            tiles: make("motion blur tiles"),
            neighbors: make("motion blur neighbours"),
        }
    }
}

/// Tiles a smear can cross: half the longest smear, plus the adjacent tile,
/// never more than the tile grid itself.
fn reach_tiles(max_blur_px: f32, tile_dims: (u32, u32)) -> u32 {
    let reach = (max_blur_px * 0.5 / TILE as f32).ceil() as u32 + 1;
    reach.min(tile_dims.0.max(tile_dims.1))
}

manifold_node_engine::primitive! {
    name: MotionBlur,
    type_id: "node.motion_blur",
    purpose: "Shutter motion blur (tile/neighbour reconstruction filter). smear_px = velocity_ndc * 0.5 * viewport * (shutter_angle/360), centred on the pixel, length capped at max_blur_px. Each pixel samples along the longest smear among nearby 16 px tiles, weighting taps by depth: a nearer tap counts where its own smear reaches the pixel, a farther tap where the pixel's smear reaches it, so moving objects blur past their edges and the background never bleeds over them. `velocity` expects render_scene's Rg16Float NDC-delta output and `depth` its raw reversed-Z depth, both from the same render_scene. `camera` supplies lens.shutter_angle (written by node.camera_lens) and near/far. Tiles with no visible motion copy the source; shutter_angle = 0 copies the frame; `enabled = false` aliases `in` to `out` with no GPU work.",
    inputs: {
        in: Texture2D required,
        velocity: Texture2D required,
        depth: Texture2D required,
        camera: Camera required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_blur_px"),
            label: "Max Blur",
            ty: ParamType::Float,
            default: ParamValue::Float(32.0),
            range: Some((0.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("enabled"),
            // "Motion Blur", not "Enabled": this param is stamped onto the
            // scene panel's Camera card next to bokeh_gather's own toggle,
            // and two rows both labeled "Enabled" are indistinguishable.
            label: "Motion Blur",
            ty: ParamType::Bool,
            default: ParamValue::Bool(true),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Warp,
    composition_notes: "Wire one render_scene's `velocity` and `depth` outputs into `velocity` and `depth` (both lazy, free unless wired), and the Camera used for `node.camera_lens` into `camera` so the shutter matches the lens exposure and DoF use. Drive the shutter from node.camera_lens's own `shutter_angle` port. `max_blur_px` caps the smear length in pixels. Sits at the end of the cinematic chain, after DoF and before tone mapping, so highlights streak in HDR.",
    examples: ["preset.generator.cinematic_scene"],
    picker: { label: "Motion Blur", category: Atom },
    summary: "Smears moving things along their screen motion for the camera's shutter angle, past their own edges and in front of what they pass, driven by real per-object movement.",
    category: BlurAndSharpen,
    role: Filter,
    aliases: ["motion blur", "shutter blur", "velocity blur", "smear"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        pipelines: Option<MotionBlurPipelines> = None,
        tiles: Option<MotionBlurTiles> = None,
    },
}

impl MotionBlur {
    pub fn prewarm_pipelines(device: &GpuDevice) {
        let _ = MotionBlurPipelines::new(device);
    }

    fn encode(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        src: &GpuTexture,
        velocity: &GpuTexture,
        depth: &GpuTexture,
        out: &GpuTexture,
        s: MotionBlurSettings,
    ) {
        let (w, h) = (out.width, out.height);
        if w == 0 || h == 0 {
            return;
        }
        if s.shutter_angle <= 0.0 || s.max_blur_px <= 0.0 {
            gpu.copy_texture_to_texture(src, out, w, h);
            return;
        }
        let (tw, th) = (w.div_ceil(TILE), h.div_ceil(TILE));
        if self
            .tiles
            .as_ref()
            .is_none_or(|t| t.tiles.width != tw || t.tiles.height != th)
        {
            self.tiles = Some(MotionBlurTiles::new(gpu.device, tw, th));
        }
        let t = self.tiles.as_ref().expect("motion blur tiles allocated");
        let p = self
            .pipelines
            .get_or_insert_with(|| MotionBlurPipelines::new(gpu.device));
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        let uniforms = MotionBlurUniforms {
            dims: [w as f32, h as f32],
            shutter_scale: s.shutter_angle / 360.0,
            max_blur_px: s.max_blur_px,
            near: s.near,
            far: s.far,
            reach_tiles: reach_tiles(s.max_blur_px, (tw, th)),
            _pad: 0,
        };
        let u = bytemuck::bytes_of(&uniforms);
        let tile_grid = [tw.div_ceil(8), th.div_ceil(8), 1];
        gpu.native_enc.dispatch_compute(
            &p.tiles,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: u,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: velocity,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: &t.tiles,
                },
            ],
            tile_grid,
            "motion blur tiles",
        );
        gpu.native_enc.dispatch_compute(
            &p.neighbors,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: u,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: &t.tiles,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: &t.neighbors,
                },
            ],
            tile_grid,
            "motion blur neighbours",
        );
        gpu.native_enc.dispatch_compute(
            &p.gather,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: u,
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: src,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: velocity,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: depth,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: &t.neighbors,
                },
                GpuBinding::Sampler {
                    binding: 5,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 6,
                    texture: out,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "motion blur gather",
        );
    }
}

impl Primitive for MotionBlur {
    /// `enabled = false`, or an unwired `max_blur_px <= 0`, aliases `in`
    /// onto `out` with no GPU work.
    fn skip_passthrough(
        &self,
        params: &ParamValues,
        wired_inputs: &[&str],
    ) -> Option<(&'static str, &'static str)> {
        if matches!(params.get("enabled"), Some(ParamValue::Bool(false))) {
            return Some(("in", "out"));
        }
        let blur_is_zero = match params.get("max_blur_px") {
            Some(ParamValue::Float(px)) => *px <= 0.0,
            _ => false,
        };
        (blur_is_zero && !wired_inputs.contains(&"max_blur_px")).then_some(("in", "out"))
    }

    /// Static declaration of the alias `skip_passthrough` may install —
    /// must agree with the dynamic hook (EffectNode contract).
    fn skip_passthrough_ports(&self) -> Option<(&'static str, &'static str)> {
        Some(("in", "out"))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let max_blur_px = ctx.param_f32("max_blur_px", 32.0);
        let cam = ctx
            .inputs
            .camera("camera")
            .unwrap_or_else(Camera::default_perspective);
        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(velocity) = ctx.inputs.texture_2d("velocity") else {
            return;
        };
        let Some(depth) = ctx.inputs.texture_2d("depth") else {
            return;
        };
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let settings = MotionBlurSettings {
            shutter_angle: cam.lens.shutter_angle,
            max_blur_px,
            near: cam.near,
            far: cam.far,
        };
        let gpu = ctx.gpu_encoder();
        self.encode(gpu, src, velocity, depth, out, settings);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use half::f16;
    use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension};
    use manifold_gpu::{GpuTextureFormat, GpuTextureUsage};
    use manifold_node_engine::gpu::render_target::RenderTarget;

    use super::{MotionBlur, MotionBlurSettings};

    fn upload(
        device: &GpuDevice,
        w: u32,
        h: u32,
        format: GpuTextureFormat,
        bytes: &[u8],
    ) -> GpuTexture {
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "motion-blur-test-input",
            mip_levels: 1,
        });
        device.upload_texture(&tex, bytes);
        tex
    }

    fn rgba16(px: &[[f32; 4]]) -> Vec<u8> {
        px.iter()
            .flatten()
            .flat_map(|v| f16::from_f32(*v).to_bits().to_le_bytes())
            .collect()
    }

    fn readback(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<f32> {
        let bytes_per_row = w * 8;
        let readback = device.create_buffer(
            u64::from(h * bytes_per_row),
            manifold_gpu::BufferUse::Readback,
        );
        let mut enc = device.create_encoder("motion-blur-readback");
        enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        enc.commit_and_wait().expect("GPU work completes");
        let ptr = readback.mapped_ptr().expect("shared readback buffer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        halves
            .chunks(4)
            .map(|c| f16::from_bits(c[0]).to_f32())
            .collect()
    }

    /// A white square moving right over a still black sky. With a 360°
    /// shutter its 16 px smear reaches 8 px past each side: the sky beside
    /// it picks up white, the square's own edge turns see-through, and sky
    /// beyond the smear or above the square stays black.
    #[test]
    fn moving_square_smears_past_its_edges() {
        let device = manifold_gpu::testkit::test_device();
        let (w, h) = (64u32, 32u32);
        let inside = |x: u32, y: u32| (24..40).contains(&x) && (8..24).contains(&y);
        let mut color = Vec::new();
        let mut velocity = Vec::new();
        let mut depth = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let sq = inside(x, y);
                let c = if sq { 1.0 } else { 0.0 };
                color.push([c, c, c, 1.0]);
                // 16 px smear = ndc * 0.5 * 64.
                velocity.push([if sq { 0.5 } else { 0.0 }, 0.0, 0.0, 1.0]);
                // Reversed-Z: the square sits near, empty sky is 0.
                depth.extend_from_slice(&(if sq { 0.5f32 } else { 0.0 }).to_le_bytes());
            }
        }
        let src = upload(
            &device,
            w,
            h,
            GpuTextureFormat::Rgba16Float,
            &rgba16(&color),
        );
        let vel = upload(
            &device,
            w,
            h,
            GpuTextureFormat::Rgba16Float,
            &rgba16(&velocity),
        );
        let dep = upload(&device, w, h, GpuTextureFormat::R32Float, &depth);
        let out = RenderTarget::new(
            &device,
            w,
            h,
            GpuTextureFormat::Rgba16Float,
            "motion-blur-out",
        );

        let mut blur = MotionBlur::new();
        let mut native = device.create_encoder("motion-blur-test");
        let mut gpu = manifold_node_engine::gpu::gpu_encoder::GpuEncoder::new(&mut native, &device);
        let settings = MotionBlurSettings {
            shutter_angle: 360.0,
            max_blur_px: 32.0,
            near: 0.1,
            far: 100.0,
        };
        blur.encode(&mut gpu, &src, &vel, &dep, &out.texture, settings);
        drop(gpu);
        native.commit_and_wait().expect("GPU work completes");
        let r = readback(&device, &out.texture, w, h);
        let at = |x: u32, y: u32| r[(y * w + x) as usize];

        assert!(
            at(42, 16) > 0.1,
            "sky 2 px right of the square: {}",
            at(42, 16)
        );
        assert!(
            at(21, 16) > 0.1,
            "sky 3 px left of the square: {}",
            at(21, 16)
        );
        assert!(at(38, 16) < 0.95, "square's own right edge: {}", at(38, 16));
        assert!(at(54, 16) < 0.01, "sky beyond the smear: {}", at(54, 16));
        assert!(at(30, 3) < 0.01, "sky above the square: {}", at(30, 3));
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
