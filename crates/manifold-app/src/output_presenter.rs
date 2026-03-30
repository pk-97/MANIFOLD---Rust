//! Dedicated output-window presenter thread.
//!
//! On macOS, putting the output window into native fullscreen causes the
//! CAMetalLayer to override `displaySyncEnabled` and lock `nextDrawable`
//! (wgpu's `get_current_texture()`) to the TV's vsync rate. If this blocking
//! call happens on the UI thread it stalls workspace rendering, producing the
//! frame-time spikes visible in the perf HUD.
//!
//! This module runs a single background thread that owns the output surface.
//! It polls the IOSurface bridge for new content frames and presents them
//! independently of the UI frame loop. The UI thread is never blocked.

use std::sync::{
    Arc,
    mpsc::{Receiver, Sender, TryRecvError, channel},
};

use manifold_renderer::{surface::SurfaceWrapper, tonemap_blit::TonemapBlitPipeline};

use crate::shared_texture::{SharedTextureBridge, SURFACE_COUNT};

// ---------------------------------------------------------------------------
// Commands sent from the UI thread to the presenter thread
// ---------------------------------------------------------------------------

pub enum OutputCommand {
    /// Output window was resized (e.g. entering/leaving fullscreen).
    Resize { width: u32, height: u32, scale: f64 },
    /// EDR headroom changed (window moved to a different display).
    UpdateEdrHeadroom(f64),
    /// Stop the presenter thread and return.
    Stop,
}

// ---------------------------------------------------------------------------
// Handle held by Application on the UI thread
// ---------------------------------------------------------------------------

pub struct OutputPresenterHandle {
    pub sender: Sender<OutputCommand>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for OutputPresenterHandle {
    fn drop(&mut self) {
        // Signal the thread to stop and wait for it to exit so the wgpu
        // surface is fully dropped before the window is destroyed.
        let _ = self.sender.send(OutputCommand::Stop);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Internal presenter state (lives on the presenter thread)
// ---------------------------------------------------------------------------

struct OutputPresenter {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    surface: SurfaceWrapper,
    tonemap_blit: TonemapBlitPipeline,
    bridge: Arc<SharedTextureBridge>,

    last_front: usize,
    ui_textures: [Option<wgpu::Texture>; SURFACE_COUNT],
    ui_views: [Option<wgpu::TextureView>; SURFACE_COUNT],
    last_bridge_gen: u64,

    edr_headroom: f64,
}

impl OutputPresenter {
    /// Re-import all IOSurface-backed wgpu textures after a bridge resize.
    /// Safe to call from any thread that owns `Arc<wgpu::Device>`.
    fn reimport_textures(&mut self) {
        // SAFETY: bridge outlives presenter (Arc), device is the same wgpu device.
        let ui_textures: [wgpu::Texture; SURFACE_COUNT] =
            std::array::from_fn(|i| unsafe { self.bridge.import_texture(&self.device, i) });
        let ui_views: [wgpu::TextureView; SURFACE_COUNT] = std::array::from_fn(|i| {
            ui_textures[i].create_view(&wgpu::TextureViewDescriptor::default())
        });
        self.ui_textures = ui_textures.map(Some);
        self.ui_views = ui_views.map(Some);
    }

    fn run(mut self, rx: Receiver<OutputCommand>) {
        loop {
            // --- Drain all pending commands (non-blocking) ---
            loop {
                match rx.try_recv() {
                    Ok(OutputCommand::Stop) => return,
                    Ok(OutputCommand::Resize { width, height, scale }) => {
                        self.surface.resize(&self.device, width, height, scale);
                    }
                    Ok(OutputCommand::UpdateEdrHeadroom(h)) => {
                        self.edr_headroom = h;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }

            // --- Re-import textures if bridge was resized ---
            let bridge_gen = self.bridge.generation();
            if bridge_gen != self.last_bridge_gen {
                self.last_bridge_gen = bridge_gen;
                self.reimport_textures();
            }

            // --- Wait for a new content frame ---
            let front = self.bridge.front_index() as usize;
            if front == self.last_front {
                // No new frame yet — brief sleep to avoid busy-spinning.
                // 1 ms gives ≤ 1 ms extra latency at 60 Hz content rate.
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            self.last_front = front;

            let Some(view) = self.ui_views[front].as_ref() else {
                continue;
            };

            // --- Acquire drawable ---
            // In fullscreen this call may block at the TV's vsync.
            // That's fine — only this thread is blocked, not the UI thread.
            let surface_texture = match self.surface.get_current_texture() {
                Ok(t) => t,
                Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                    self.surface.resize(
                        &self.device,
                        self.surface.width,
                        self.surface.height,
                        self.surface.scale_factor,
                    );
                    continue;
                }
                Err(e) => {
                    log::error!("[OutputPresenter] Surface error: {e}");
                    continue;
                }
            };

            let surface_view =
                surface_texture.texture.create_view(&wgpu::TextureViewDescriptor::default());
            let surface_w = self.surface.width;
            let surface_h = self.surface.height;

            let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Output Presenter Blit"),
            });

            // Clear to black first so letterbox / pillarbox areas are black.
            {
                let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Output Clear"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &surface_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }

            // Source aspect ratio from the bridge's current dimensions.
            let src_w = self.bridge.width();
            let src_h = self.bridge.height();
            let source_aspect = src_w as f32 / src_h.max(1) as f32;
            let sdr_tonemap = self.edr_headroom <= 1.0;

            self.tonemap_blit.blit_to_rect_fit(
                &self.device,
                &self.queue,
                &mut encoder,
                view,
                &surface_view,
                0.0,
                0.0,
                surface_w as f32,
                surface_h as f32,
                source_aspect,
                sdr_tonemap,
            );

            self.queue.submit(std::iter::once(encoder.finish()));
            surface_texture.present();
        }
    }
}

// ---------------------------------------------------------------------------
// Public API: spawn a presenter thread for one output window
// ---------------------------------------------------------------------------

/// Spawn the output-presenter thread.
///
/// Takes ownership of the `surface` and `tonemap_blit` pipeline — neither
/// should be retained on the UI thread after this call.
///
/// Returns a handle that stops and joins the thread on drop.
pub fn spawn(
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    surface: SurfaceWrapper,
    tonemap_blit: TonemapBlitPipeline,
    bridge: Arc<SharedTextureBridge>,
    edr_headroom: f64,
) -> OutputPresenterHandle {
    let (tx, rx) = channel();

    let bridge_gen = bridge.generation();
    let mut presenter = OutputPresenter {
        device,
        queue,
        surface,
        tonemap_blit,
        bridge,
        last_front: usize::MAX,
        ui_textures: [None, None, None],
        ui_views: [None, None, None],
        last_bridge_gen: bridge_gen,
        edr_headroom,
    };

    // Import IOSurface textures on the spawning (UI) thread before handing
    // off. This avoids a one-frame delay on first present.
    presenter.reimport_textures();

    let thread = std::thread::Builder::new()
        .name("output-presenter".into())
        .spawn(move || presenter.run(rx))
        .expect("failed to spawn output-presenter thread");

    OutputPresenterHandle { sender: tx, thread: Some(thread) }
}
