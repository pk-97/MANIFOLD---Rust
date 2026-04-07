// Kerr black hole generator — cached deflection lookup.
//
// Loads a pre-baked `.bhcache` file containing deflection maps for a 2D grid
// of (cam_dist, tilt). At runtime, the 4 nearest grid neighbors are loaded
// asynchronously and bilinearly blended in the display compute shader.
// Rotation and zoom are applied as UV transforms — no geodesic compute runs.
//
// The cache file MUST exist at `assets/black-hole.bhcache`. Production builds
// run `manifold bake-black-hole` once to generate it. If the file is missing
// the generator panics, ensuring no silent performance regression in a live set.

use crate::generator::Generator;
use crate::generator_context::GeneratorContext;
use crate::generators::bh_cache_loader::BhCacheLoader;
use crate::gpu_encoder::GpuEncoder;
use manifold_core::GeneratorTypeId;

const SPEED: usize = 0;
const CAM_DIST: usize = 1;
const TILT: usize = 2;
const ROTATE: usize = 3;
// const STEPS: usize = 4; — fixed at bake time, ignored at runtime
const DISK_INNER: usize = 5;
const DISK_OUTER: usize = 6;
const DISK_GLOW: usize = 7;
const SCALE: usize = 8;
const STARS: usize = 9;
const SPIN: usize = 10;

const CACHE_FILE_PATH: &str = "assets/black-hole.bhcache";

fn param(ctx: &GeneratorContext, idx: usize, default: f32) -> f32 {
    if ctx.param_count > idx as u32 {
        ctx.params[idx]
    } else {
        default
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CachedDisplayUniforms {
    time_val: f32,
    disk_inner: f32,
    disk_outer: f32,
    disk_glow: f32,
    orbit_angle: f32,
    stars_brightness: f32,
    spin: f32,
    rotate_rad: f32,
    uv_scale: f32,
    aspect: f32,
    tilt_mirror: f32,
    w_tl: f32,
    w_tr: f32,
    w_bl: f32,
    w_br: f32,
    bake_fov_half: f32,
}

pub struct BlackHoleGenerator {
    display_pipeline: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,
    cache_loader: BhCacheLoader,

    /// 4 neighbor slots, each holding 3 textures (defl1, defl2, sky_dir).
    /// Slot index matches `GridNeighbors::indices` order [TL, TR, BL, BR].
    neighbor_textures: [Option<[manifold_gpu::GpuTexture; 3]>; 4],
    /// Bake resolution from the loaded cache header.
    cache_tex_dim: u32,
}

impl BlackHoleGenerator {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let cache_path = std::path::Path::new(CACHE_FILE_PATH);
        if !cache_path.exists() {
            panic!(
                "BlackHole cache file not found at {}. \
                 Run `manifold bake-black-hole` to generate it before launching.",
                cache_path.display(),
            );
        }
        let cache_loader = BhCacheLoader::open(cache_path).unwrap_or_else(|e| {
            panic!(
                "BlackHole cache file {} exists but failed to open: {}. \
                 Run `manifold bake-black-hole` to regenerate.",
                cache_path.display(),
                e,
            )
        });
        let header = cache_loader.header();
        if header.tex_width != header.tex_height {
            panic!(
                "BlackHole cache must be square (got {}x{})",
                header.tex_width, header.tex_height,
            );
        }
        log::info!(
            "BlackHole: loaded cache file {} ({}x{} grid, {}x{} per entry, spin={})",
            cache_path.display(),
            header.grid_rows,
            header.grid_cols,
            header.tex_width,
            header.tex_height,
            header.spin,
        );
        let cache_tex_dim = header.tex_width;

        let display_pipeline = device.create_compute_pipeline(
            include_str!("shaders/black_hole_display_cached.wgsl"),
            "cs_main",
            "BlackHole Display Cached",
        );
        let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc::default());

        Self {
            display_pipeline,
            sampler,
            cache_loader,
            neighbor_textures: [None, None, None, None],
            cache_tex_dim,
        }
    }

    fn ensure_neighbor_textures(&mut self, device: &manifold_gpu::GpuDevice) {
        let dim = self.cache_tex_dim;
        for slot in 0..4 {
            if self.neighbor_textures[slot].is_some() {
                continue;
            }
            let make_tex = |label: &str| -> manifold_gpu::GpuTexture {
                device.create_texture(&manifold_gpu::GpuTextureDesc {
                    width: dim,
                    height: dim,
                    depth: 1,
                    format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                    dimension: manifold_gpu::GpuTextureDimension::D2,
                    usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                        | manifold_gpu::GpuTextureUsage::CPU_UPLOAD,
                    label,
                })
            };
            let label1 = format!("BHCache slot{slot} defl1");
            let label2 = format!("BHCache slot{slot} defl2");
            let label3 = format!("BHCache slot{slot} sky");
            self.neighbor_textures[slot] =
                Some([make_tex(&label1), make_tex(&label2), make_tex(&label3)]);
        }
    }

    fn drain_pending_uploads(&mut self, device: &manifold_gpu::GpuDevice, tex_bytes: usize) {
        for slot in 0..4 {
            let pending = self.cache_loader.take_pending_upload(slot);
            let Some(data) = pending else { continue };
            if data.len() != tex_bytes * 3 {
                log::error!(
                    "BlackHole cache slot {} bad data length {} (expected {})",
                    slot,
                    data.len(),
                    tex_bytes * 3,
                );
                continue;
            }
            let textures = self.neighbor_textures[slot]
                .as_ref()
                .expect("neighbor textures allocated");
            device.upload_texture(&textures[0], &data[0..tex_bytes]);
            device.upload_texture(&textures[1], &data[tex_bytes..2 * tex_bytes]);
            device.upload_texture(&textures[2], &data[2 * tex_bytes..3 * tex_bytes]);
        }
    }
}

impl Generator for BlackHoleGenerator {
    fn generator_type(&self) -> &GeneratorTypeId {
        &GeneratorTypeId::BLACK_HOLE
    }

    fn render(
        &mut self,
        gpu: &mut GpuEncoder,
        target: &manifold_gpu::GpuTexture,
        ctx: &GeneratorContext,
    ) -> f32 {
        if ctx.param_count == 0 {
            return ctx.anim_progress;
        }

        let speed = param(ctx, SPEED, 0.3);
        let cam_dist = param(ctx, CAM_DIST, 20.0);
        let tilt_deg = param(ctx, TILT, 75.0);
        let rotate_deg = param(ctx, ROTATE, 0.0);
        let disk_inner = param(ctx, DISK_INNER, 3.0);
        let disk_outer = param(ctx, DISK_OUTER, 10.0);
        let disk_glow = param(ctx, DISK_GLOW, 2.0);
        let scale = param(ctx, SCALE, 1.0);
        let stars = param(ctx, STARS, 0.5);
        let spin = param(ctx, SPIN, 0.0);

        let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };
        let rotate_rad = rotate_deg.to_radians();
        let orbit_angle = ctx.time as f32 * speed * 0.3;

        // Allocate neighbor textures up front (cheap once allocated).
        self.ensure_neighbor_textures(gpu.device);

        // ── Update loader: kick off any neighbor loads needed ──
        self.cache_loader.update_for(cam_dist, tilt_deg);
        self.cache_loader.poll();

        let tex_bytes = self.cache_loader.header().texture_bytes();

        // Drain whatever loads completed before this frame.
        self.drain_pending_uploads(gpu.device, tex_bytes);

        // ── Cold-start: block until all 4 neighbors arrive ──
        // Steady state hits the fast path immediately. Cold starts on the
        // very first frame block here for ~50-100ms total.
        let mut spin_count = 0;
        loop {
            let ready = self.cache_loader.neighbors_ready();
            if ready {
                break;
            }
            let in_flight = self.cache_loader.loads_in_flight();
            if !in_flight || spin_count >= 16 {
                // No more pending work or we've spun too long. Skip frame —
                // target keeps prior contents (clear black on first frame).
                return ctx.anim_progress;
            }
            self.cache_loader.block_until_any();
            spin_count += 1;
            self.drain_pending_uploads(gpu.device, tex_bytes);
        }

        let neighbors = *self.cache_loader.last_neighbors().unwrap();
        let weights = neighbors.weights();
        let tilt_mirror = if neighbors.tilt_mirrored { -1.0 } else { 1.0 };
        let bake_fov_half = self.cache_loader.header().bake_fov_half;

        let uniforms = CachedDisplayUniforms {
            time_val: ctx.time as f32,
            disk_inner,
            disk_outer,
            disk_glow,
            orbit_angle,
            stars_brightness: stars,
            spin,
            rotate_rad,
            uv_scale,
            aspect: ctx.aspect,
            tilt_mirror,
            w_tl: weights[0],
            w_tr: weights[1],
            w_bl: weights[2],
            w_br: weights[3],
            bake_fov_half,
        };

        let tl = self.neighbor_textures[0].as_ref().unwrap();
        let tr = self.neighbor_textures[1].as_ref().unwrap();
        let bl = self.neighbor_textures[2].as_ref().unwrap();
        let br = self.neighbor_textures[3].as_ref().unwrap();

        gpu.native_enc.dispatch_compute(
            &self.display_pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                manifold_gpu::GpuBinding::Texture { binding: 1, texture: &tl[0] },
                manifold_gpu::GpuBinding::Texture { binding: 2, texture: &tl[1] },
                manifold_gpu::GpuBinding::Texture { binding: 3, texture: &tl[2] },
                manifold_gpu::GpuBinding::Texture { binding: 4, texture: &tr[0] },
                manifold_gpu::GpuBinding::Texture { binding: 5, texture: &tr[1] },
                manifold_gpu::GpuBinding::Texture { binding: 6, texture: &tr[2] },
                manifold_gpu::GpuBinding::Texture { binding: 7, texture: &bl[0] },
                manifold_gpu::GpuBinding::Texture { binding: 8, texture: &bl[1] },
                manifold_gpu::GpuBinding::Texture { binding: 9, texture: &bl[2] },
                manifold_gpu::GpuBinding::Texture { binding: 10, texture: &br[0] },
                manifold_gpu::GpuBinding::Texture { binding: 11, texture: &br[1] },
                manifold_gpu::GpuBinding::Texture { binding: 12, texture: &br[2] },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 13,
                    sampler: &self.sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 14,
                    texture: target,
                },
            ],
            [ctx.width.div_ceil(16), ctx.height.div_ceil(16), 1],
            "BlackHole Display Cached",
        );

        ctx.anim_progress
    }

    fn resize(&mut self, _device: &manifold_gpu::GpuDevice, _width: u32, _height: u32) {
        // Cache textures are independent of output resolution.
    }

    fn internal_resolution_scale(&self) -> f32 {
        // Native output resolution — display pass is cheap and we want
        // pixel-perfect deflection lookup with no upscale blur.
        1.0
    }
}
