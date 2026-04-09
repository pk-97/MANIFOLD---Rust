// Real-time AdaIN neural style transfer effect.
//
// Async pipeline (same pattern as BlobTracking):
//   Frame N:   GPU downsample → ReadbackRequest (async GPU→CPU)
//   Frame N+1: Poll readback → submit to BackgroundWorker (ONNX inference)
//   Frame N+2: Poll worker → upload result to GPU → blend with source
//
// The BackgroundWorker owns the ONNX Runtime session on a dedicated thread.
// Style image is encoded once when the user selects a new reference image.

use crate::background_worker::BackgroundWorker;
use crate::effect::{EffectContext, PostProcessEffect, StatefulEffect};
use crate::effects::compute_dual_blit_helper::ComputeDualBlitHelper;
use crate::gpu_encoder::GpuEncoder;
use crate::gpu_readback::ReadbackRequest;
use crate::render_target::RenderTarget;
use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effects::EffectInstance;
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use std::sync::Arc;

// ── Request/response for the inference worker ──

struct StyleRequest {
    owner_key: i64,
    content_pixels: Vec<u8>, // RGBA8, from GPU readback
    content_w: u32,
    content_h: u32,
    style_pixels: Arc<Vec<u8>>, // RGB8, loaded once from style image
    style_w: u32,
    style_h: u32,
    inference_size: u32, // 256 or 512
}

struct StyleResponse {
    owner_key: i64,
    stylized_pixels: Vec<u8>, // RGBA8, inference_size x inference_size
    width: u32,
    height: u32,
}

// ── Per-owner state ──

struct OwnerState {
    readback: ReadbackRequest,
    /// GPU texture holding the latest inference result, uploaded from CPU.
    result_rt: Option<RenderTarget>,
    has_result: bool,
    current_inference_size: u32,
    last_readback_frame: i64,
}

// ── Uniform struct for the blend shader (16-byte aligned) ──

#[repr(C)]
struct BlendUniforms {
    strength: f32,
    has_result: u32,
    _pad: [f32; 2],
}

// ── Main effect struct ──

pub struct NeuralStyleFX {
    blend_helper: ComputeDualBlitHelper,
    worker: Option<BackgroundWorker<StyleRequest, StyleResponse>>,
    owner_states: AHashMap<i64, OwnerState>,
    // Cached style image (shared across all owners).
    loaded_style_path: Option<String>,
    style_pixels: Option<Arc<Vec<u8>>>, // RGB8
    style_w: u32,
    style_h: u32,
}

unsafe impl Send for NeuralStyleFX {}

impl NeuralStyleFX {
    pub fn new(device: &GpuDevice) -> Self {
        let worker = BackgroundWorker::try_new(|| {
            let session = create_ort_session()?;
            Some(move |req: StyleRequest| -> StyleResponse { run_inference(&session, req) })
        });
        if worker.is_none() {
            log::warn!(
                "[NeuralStyleFX] ONNX model or runtime not found. \
                 Run tools/export_adain_onnx.py and ensure libonnxruntime.dylib is available."
            );
        }

        let blend_helper = ComputeDualBlitHelper::new(
            device,
            include_str!("shaders/fx_neural_style_blend.wgsl"),
            "NeuralStyle Blend",
        );

        Self {
            blend_helper,
            worker,
            owner_states: AHashMap::new(),
            loaded_style_path: None,
            style_pixels: None,
            style_w: 0,
            style_h: 0,
        }
    }

    /// Check if the style image path changed, and reload if needed.
    fn maybe_reload_style_image(&mut self, path: Option<&String>) {
        let path = match path {
            Some(p) if !p.is_empty() => p,
            _ => {
                if self.loaded_style_path.is_some() {
                    self.loaded_style_path = None;
                    self.style_pixels = None;
                    self.style_w = 0;
                    self.style_h = 0;
                }
                return;
            }
        };

        if self.loaded_style_path.as_deref() == Some(path.as_str()) {
            return; // Already loaded.
        }

        match image::open(path) {
            Ok(img) => {
                let rgb = img.to_rgb8();
                self.style_w = rgb.width();
                self.style_h = rgb.height();
                self.style_pixels = Some(Arc::new(rgb.into_raw()));
                self.loaded_style_path = Some(path.to_string());
                log::info!(
                    "[NeuralStyleFX] Loaded style image: {} ({}x{})",
                    path,
                    self.style_w,
                    self.style_h,
                );
            }
            Err(e) => {
                log::error!(
                    "[NeuralStyleFX] Failed to load style image '{}': {}",
                    path,
                    e
                );
                self.loaded_style_path = None;
                self.style_pixels = None;
            }
        }
    }

    /// Poll the background worker and GPU readback.
    fn poll_readback(&mut self, device: &GpuDevice, owner_key: i64) {
        // ── Phase 1: check if the background worker has a result ──
        if let Some(worker) = &mut self.worker
            && let Some(response) = worker.try_recv()
            && let Some(state) = self.owner_states.get_mut(&response.owner_key)
        {
            // Create or recreate result texture if size changed.
            if state.result_rt.is_none()
                || state.current_inference_size != response.width
            {
                state.result_rt = Some(RenderTarget::new(
                    device,
                    response.width,
                    response.height,
                    GpuTextureFormat::Rgba8Unorm,
                    &format!("NeuralStyle_Result_{}", response.owner_key),
                ));
                state.current_inference_size = response.width;
            }

            // Upload stylized pixels to GPU texture.
            if let Some(rt) = &state.result_rt {
                device.upload_texture(&rt.texture, &response.stylized_pixels);
                state.has_result = true;
            }
        }

        // ── Phase 2: check for new pixel data from GPU readback ──
        let Some(worker) = &self.worker else { return };
        if worker.is_busy() {
            return;
        }

        let Some(style_pixels) = &self.style_pixels else {
            return;
        };
        let style_pixels = Arc::clone(style_pixels);
        let style_w = self.style_w;
        let style_h = self.style_h;

        let Some(state) = self.owner_states.get_mut(&owner_key) else {
            return;
        };
        let inference_size = state.current_inference_size.max(256);

        let pixels = match state.readback.try_read() {
            Some(p) => p,
            None => return,
        };

        let Some(worker) = &mut self.worker else {
            return;
        };

        worker.submit(StyleRequest {
            owner_key,
            content_pixels: pixels,
            content_w: inference_size,
            content_h: inference_size,
            style_pixels,
            style_w,
            style_h,
            inference_size,
        });
    }
}

// ── PostProcessEffect impl ──

impl PostProcessEffect for NeuralStyleFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::NEURAL_STYLE
    }

    fn should_skip(&self, fx: &EffectInstance) -> bool {
        // Skip if strength is 0, or no worker, or no style image.
        fx.param_values.first().copied().unwrap_or(0.0) <= 0.0
            || self.worker.is_none()
            || fx.style_image_path.is_none()
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        let strength = fx.param_values.first().copied().unwrap_or(0.5);
        let inference_size = if fx.param_values.get(1).copied().unwrap_or(0.0) > 0.5 {
            512
        } else {
            256
        };

        // Reload style image if path changed.
        self.maybe_reload_style_image(fx.style_image_path.as_ref());

        // Poll worker result and GPU readback.
        self.poll_readback(gpu.device, ctx.owner_key);

        // Ensure owner state exists and update inference size.
        let owner_key = ctx.owner_key;
        let state = self.owner_states.entry(owner_key).or_insert_with(|| OwnerState {
            readback: ReadbackRequest::new(),
            result_rt: None,
            has_result: false,
            current_inference_size: inference_size,
            last_readback_frame: 0,
        });
        state.current_inference_size = inference_size;

        // Submit readback if not pending and enough time has passed.
        let has_style = self.style_pixels.is_some();
        if !state.readback.is_pending()
            && has_style
            && ctx.frame_count - state.last_readback_frame >= 1
        {
            state
                .readback
                .submit(gpu, source, inference_size, inference_size);
            state.last_readback_frame = ctx.frame_count;
        }

        // Blend pass: mix original with stylized result.
        let has_result = state.has_result;
        let uniforms = BlendUniforms {
            strength,
            has_result: u32::from(has_result),
            _pad: [0.0; 2],
        };
        let uniform_bytes = unsafe {
            std::slice::from_raw_parts(
                &uniforms as *const BlendUniforms as *const u8,
                std::mem::size_of::<BlendUniforms>(),
            )
        };

        if has_result
            && let Some(rt) = &state.result_rt
        {
            // Extract texture ref before dropping state borrow.
            let tex = &rt.texture as *const manifold_gpu::GpuTexture;
            // SAFETY: rt.texture lives in self.owner_states which we won't
            // mutate during the dispatch call.
            let tex_ref = unsafe { &*tex };
            self.blend_helper.dispatch(
                gpu,
                source,
                tex_ref,
                target,
                uniform_bytes,
                "NeuralStyle Blend",
                ctx.width,
                ctx.height,
            );
            return;
        }

        // No result yet — pass through source via a_only dispatch.
        self.blend_helper.dispatch_a_only(
            gpu,
            source,
            target,
            uniform_bytes,
            "NeuralStyle Passthrough",
            ctx.width,
            ctx.height,
        );
    }

    fn clear_state(&mut self) {
        for state in self.owner_states.values_mut() {
            state.has_result = false;
        }
    }

    fn flush_background_work(&mut self) {
        if let Some(worker) = &mut self.worker {
            worker.recv_blocking();
        }
    }

    fn cleanup_owner_state(&mut self, owner_key: i64) {
        self.owner_states.remove(&owner_key);
    }
}

impl StatefulEffect for NeuralStyleFX {
    fn clear_state_for_owner(&mut self, owner_key: i64) {
        if let Some(state) = self.owner_states.get_mut(&owner_key) {
            state.has_result = false;
        }
    }

    fn cleanup_owner(&mut self, owner_key: i64) {
        self.owner_states.remove(&owner_key);
    }

    fn cleanup_all_owners(&mut self, _device: &GpuDevice) {
        self.owner_states.clear();
    }
}

// ── ONNX Runtime integration ──

/// Resolve the path to the ONNX model file.
fn resolve_model_path() -> Option<std::path::PathBuf> {
    // Try: relative to executable (app bundle)
    if let Ok(exe) = std::env::current_exe() {
        let bundle_path = exe
            .parent()?
            .parent()
            .unwrap_or(exe.parent()?)
            .join("Resources/models/adain_style_transfer.onnx");
        if bundle_path.exists() {
            return Some(bundle_path);
        }
        // Alongside executable
        let sibling = exe.parent()?.join("adain_style_transfer.onnx");
        if sibling.exists() {
            return Some(sibling);
        }
    }

    // Try: project root assets
    let project_assets = std::path::Path::new("assets/models/adain_style_transfer.onnx");
    if project_assets.exists() {
        return Some(project_assets.to_path_buf());
    }

    None
}

/// Create an ONNX Runtime session with CoreML + CPU fallback.
fn create_ort_session() -> Option<ort::session::Session> {
    let model_path = resolve_model_path()?;
    log::info!(
        "[NeuralStyleFX] Loading ONNX model from: {}",
        model_path.display()
    );

    let session = ort::session::Session::builder()
        .ok()?
        .with_execution_providers([
            #[cfg(target_os = "macos")]
            ort::execution_providers::CoreMLExecutionProvider::default().build(),
            ort::execution_providers::CPUExecutionProvider::default().build(),
        ])
        .ok()?
        .commit_from_file(&model_path)
        .ok()?;

    log::info!("[NeuralStyleFX] ONNX session created successfully.");
    Some(session)
}

/// Run AdaIN inference on the worker thread.
fn run_inference(session: &ort::session::Session, req: StyleRequest) -> StyleResponse {
    let size = req.inference_size as usize;

    // Preprocess content: RGBA8 → f32 RGB [0,1], reshape to [1, 3, H, W]
    let content_tensor = rgba8_to_nchw(&req.content_pixels, req.content_w, req.content_h, size);

    // Preprocess style: RGB8 → f32 RGB [0,1], resize, reshape to [1, 3, H, W]
    let style_tensor = rgb8_to_nchw(&req.style_pixels, req.style_w, req.style_h, size);

    // Run inference — ort::inputs! returns Result, needs unwrapping
    let content_view = content_tensor.view();
    let style_view = style_tensor.view();
    let inputs = match ort::inputs![content_view, style_view] {
        Ok(i) => i,
        Err(e) => {
            log::error!("[NeuralStyleFX] Failed to create inputs: {}", e);
            return StyleResponse {
                owner_key: req.owner_key,
                stylized_pixels: vec![128u8; size * size * 4],
                width: size as u32,
                height: size as u32,
            };
        }
    };
    let outputs = match session.run(inputs) {
        Ok(o) => o,
        Err(e) => {
            log::error!("[NeuralStyleFX] Inference failed: {}", e);
            return StyleResponse {
                owner_key: req.owner_key,
                stylized_pixels: vec![128u8; size * size * 4],
                width: size as u32,
                height: size as u32,
            };
        }
    };

    // Postprocess: [1, 3, H, W] f32 → RGBA8
    let output_tensor = match outputs[0].try_extract_tensor::<f32>() {
        Ok(t) => t,
        Err(e) => {
            log::error!("[NeuralStyleFX] Failed to extract output tensor: {}", e);
            return StyleResponse {
                owner_key: req.owner_key,
                stylized_pixels: vec![128u8; size * size * 4],
                width: size as u32,
                height: size as u32,
            };
        }
    };
    let output_view = output_tensor.view();
    let stylized_pixels = nchw_to_rgba8(&output_view, size);

    StyleResponse {
        owner_key: req.owner_key,
        stylized_pixels,
        width: size as u32,
        height: size as u32,
    }
}

// ── Tensor conversion helpers ──

/// Convert RGBA8 pixel buffer to NCHW f32 tensor, resizing to `size x size`.
fn rgba8_to_nchw(pixels: &[u8], src_w: u32, src_h: u32, size: usize) -> ndarray::Array4<f32> {
    let mut tensor = ndarray::Array4::<f32>::zeros((1, 3, size, size));
    let sw = src_w as f32;
    let sh = src_h as f32;
    let stride = src_w as usize * 4;

    for y in 0..size {
        let sy = ((y as f32 + 0.5) * sh / size as f32).min(sh - 1.0) as usize;
        for x in 0..size {
            let sx = ((x as f32 + 0.5) * sw / size as f32).min(sw - 1.0) as usize;
            let idx = sy * stride + sx * 4;
            if idx + 2 < pixels.len() {
                tensor[[0, 0, y, x]] = pixels[idx] as f32 / 255.0;
                tensor[[0, 1, y, x]] = pixels[idx + 1] as f32 / 255.0;
                tensor[[0, 2, y, x]] = pixels[idx + 2] as f32 / 255.0;
            }
        }
    }
    tensor
}

/// Convert RGB8 pixel buffer to NCHW f32 tensor, resizing to `size x size`.
fn rgb8_to_nchw(pixels: &[u8], src_w: u32, src_h: u32, size: usize) -> ndarray::Array4<f32> {
    let mut tensor = ndarray::Array4::<f32>::zeros((1, 3, size, size));
    let sw = src_w as f32;
    let sh = src_h as f32;
    let stride = src_w as usize * 3;

    for y in 0..size {
        let sy = ((y as f32 + 0.5) * sh / size as f32).min(sh - 1.0) as usize;
        for x in 0..size {
            let sx = ((x as f32 + 0.5) * sw / size as f32).min(sw - 1.0) as usize;
            let idx = sy * stride + sx * 3;
            if idx + 2 < pixels.len() {
                tensor[[0, 0, y, x]] = pixels[idx] as f32 / 255.0;
                tensor[[0, 1, y, x]] = pixels[idx + 1] as f32 / 255.0;
                tensor[[0, 2, y, x]] = pixels[idx + 2] as f32 / 255.0;
            }
        }
    }
    tensor
}

/// Convert NCHW f32 tensor [1, 3, H, W] to RGBA8 pixel buffer.
fn nchw_to_rgba8(tensor: &ndarray::ArrayViewD<'_, f32>, size: usize) -> Vec<u8> {
    let mut pixels = vec![255u8; size * size * 4]; // Alpha = 255
    for y in 0..size {
        for x in 0..size {
            let idx = (y * size + x) * 4;
            pixels[idx] = (tensor[[0, 0, y, x]].clamp(0.0, 1.0) * 255.0) as u8;
            pixels[idx + 1] = (tensor[[0, 1, y, x]].clamp(0.0, 1.0) * 255.0) as u8;
            pixels[idx + 2] = (tensor[[0, 2, y, x]].clamp(0.0, 1.0) * 255.0) as u8;
        }
    }
    pixels
}
