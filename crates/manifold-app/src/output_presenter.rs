//! Output presenter — dedicated thread with manifold-gpu blit pipeline.
//!
//! `NativeOutputPresenter` spawns a dedicated thread that owns a `GpuSurface`
//! (CAMetalLayer) at project resolution and a WGSL fullscreen-triangle pipeline.
//! The thread polls the IOSurface bridge for new frames and blits them to the
//! output window's drawable, display-synchronized via the surface.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use manifold_gpu::{
    GpuBinding, GpuDevice, GpuFilterMode, GpuLoadAction, GpuRenderPipeline, GpuSampler,
    GpuSamplerDesc, GpuSurface, GpuTexture, GpuTextureFormat, GpuTextureUsage,
};

use crate::shared_texture::{SharedTextureBridge, SURFACE_COUNT};

const PRESENTER_WGSL: &str = r#"
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(t_source, s_source, in.uv);
}
"#;

/// Pixel-perfect output presenter backed by a dedicated thread.
pub struct NativeOutputPresenter {
    stop: Arc<AtomicBool>,
    edr_headroom: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl NativeOutputPresenter {
    pub fn new(
        _gpu_device: &GpuDevice,
        window: &winit::window::Window,
        bridge: Arc<SharedTextureBridge>,
        edr_headroom: f64,
    ) -> Self {
        // Create a dedicated GpuDevice for the presenter thread.
        // Same physical Metal device, separate command queue — prevents
        // GPU scheduler contention with the content thread.
        let presenter_device = GpuDevice::new();

        let proj_w = bridge.width();
        let proj_h = bridge.height();

        // Create the surface (CAMetalLayer) on the calling thread where the
        // window handle is valid, then move it to the presenter thread.
        let surface = presenter_device.create_surface(
            window,
            proj_w,
            proj_h,
            GpuTextureFormat::Rgba16Float,
            true, // display-sync: pace to output monitor refresh
        );
        surface.configure_edr();
        surface.set_contents_gravity_resize_aspect();
        surface.set_background_color(0.0, 0.0, 0.0, 1.0);

        let pipeline = presenter_device.create_render_pipeline(
            PRESENTER_WGSL,
            "vs_main",
            "fs_main",
            GpuTextureFormat::Rgba16Float,
            None,
            "Presenter Blit",
        );

        let sampler = presenter_device.create_sampler(&GpuSamplerDesc {
            min_filter: GpuFilterMode::Nearest,
            mag_filter: GpuFilterMode::Nearest,
            ..Default::default()
        });

        let bridge_gen = bridge.generation();
        let native_textures = import_textures(&presenter_device, &bridge);

        let stop = Arc::new(AtomicBool::new(false));
        let edr = Arc::new(AtomicU64::new(edr_headroom.to_bits()));

        let thread_state = PresenterThread {
            device: presenter_device,
            pipeline,
            sampler,
            surface,
            bridge,
            native_textures,
            last_bridge_gen: bridge_gen,
            last_front: usize::MAX,
            stop: Arc::clone(&stop),
            edr_headroom: Arc::clone(&edr),
        };

        let thread = std::thread::Builder::new()
            .name("output-presenter".into())
            .spawn(move || thread_state.run())
            .expect("failed to spawn output-presenter thread");

        Self {
            stop,
            edr_headroom: edr,
            thread: Some(thread),
        }
    }

    pub fn update_edr_headroom(&mut self, headroom: f64) {
        self.edr_headroom.store(headroom.to_bits(), Ordering::Relaxed);
    }
}

impl Drop for NativeOutputPresenter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

struct PresenterThread {
    device: GpuDevice,
    pipeline: GpuRenderPipeline,
    sampler: GpuSampler,
    surface: GpuSurface,
    bridge: Arc<SharedTextureBridge>,
    native_textures: [Option<GpuTexture>; SURFACE_COUNT],
    last_bridge_gen: u64,
    last_front: usize,
    stop: Arc<AtomicBool>,
    #[allow(dead_code)]
    edr_headroom: Arc<AtomicU64>,
}

unsafe impl Send for PresenterThread {}

impl PresenterThread {
    fn run(mut self) {
        loop {
            if self.stop.load(Ordering::Acquire) {
                break;
            }

            let bridge_gen = self.bridge.generation();
            if bridge_gen != self.last_bridge_gen {
                self.last_bridge_gen = bridge_gen;
                self.reimport_textures();
                self.sync_surface_to_bridge();
            }

            // Use the latest available frame every presenter tick. Drawable
            // acquisition is display-synchronized, so this thread stays aligned
            // with the output monitor's real refresh cadence.
            let front = self.bridge.front_index() as usize;
            self.last_front = front;

            let Some(source) = self.native_textures[front].as_ref() else {
                continue;
            };

            let Some(drawable) = self.surface.next_drawable() else {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            };

            let target = drawable.gpu_texture(GpuTextureFormat::Rgba16Float);

            let w = self.surface.width as f32;
            let h = self.surface.height as f32;

            let mut encoder = self.device.create_encoder("Output Present");
            encoder.draw_fullscreen_viewport(
                &self.pipeline,
                &target,
                &[
                    GpuBinding::Texture { binding: 0, texture: source },
                    GpuBinding::Sampler { binding: 1, sampler: &self.sampler },
                ],
                (0.0, 0.0, w, h),
                GpuLoadAction::DontCare,
                "Presenter Blit",
            );
            encoder.present_drawable(&drawable);
            encoder.commit();
        }
    }

    fn reimport_textures(&mut self) {
        self.native_textures = import_textures(&self.device, &self.bridge);
    }

    fn sync_surface_to_bridge(&mut self) {
        let w = self.bridge.width();
        let h = self.bridge.height();
        if w != self.surface.width || h != self.surface.height {
            self.surface.resize(w, h);
        }
    }
}

fn import_textures(
    device: &GpuDevice,
    bridge: &SharedTextureBridge,
) -> [Option<GpuTexture>; SURFACE_COUNT] {
    let width = bridge.width();
    let height = bridge.height();

    std::array::from_fn(|i| {
        let io_surface_ref = bridge.raw_io_surface(i);
        Some(unsafe {
            device.create_texture_from_io_surface(
                io_surface_ref,
                width,
                height,
                GpuTextureFormat::Rgba16Float,
                GpuTextureUsage::SHADER_READ,
            )
        })
    })
}
