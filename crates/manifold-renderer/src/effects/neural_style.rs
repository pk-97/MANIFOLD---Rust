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
    /// Downsampled source at inference resolution for readback.
    downsample_rt: Option<RenderTarget>,
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
    /// Separate workers for 256 and 512 resolution models.
    /// CoreML requires fixed input shapes, so we load one model per size.
    worker_256: Option<BackgroundWorker<StyleRequest, StyleResponse>>,
    worker_512: Option<BackgroundWorker<StyleRequest, StyleResponse>>,
    /// True after we've attempted (and possibly failed) to create workers.
    /// Prevents retrying every frame.
    worker_init_attempted: bool,
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
        // Do NOT initialize ort here. ONNX Runtime throws uncatchable C++
        // exceptions during init if anything goes wrong (version mismatch,
        // missing providers, etc). Worker is created lazily on first use.

        let blend_helper = ComputeDualBlitHelper::new(
            device,
            include_str!("shaders/fx_neural_style_blend.wgsl"),
            "NeuralStyle Blend",
        );

        Self {
            blend_helper,
            worker_256: None,
            worker_512: None,
            worker_init_attempted: false,
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

    /// Lazily create the ONNX Runtime worker on first use.
    /// Deferred from new() because ort throws uncatchable C++ exceptions
    /// during init, and we must not crash the app at startup.
    fn ensure_worker(&mut self) {
        if self.worker_init_attempted {
            return;
        }
        self.worker_init_attempted = true;

        let Some(dylib_path) = find_ort_dylib() else {
            log::warn!(
                "[NeuralStyleFX] libonnxruntime.dylib not found. \
                 Install via `brew install onnxruntime` or set ORT_DYLIB_PATH."
            );
            return;
        };

        // SAFETY: called from content thread before worker threads exist.
        unsafe { std::env::set_var("ORT_DYLIB_PATH", &dylib_path) };
        log::info!("[NeuralStyleFX] Found ONNX Runtime at: {dylib_path}");

        // Create workers for both resolutions (CoreML needs fixed input shapes).
        self.worker_256 = BackgroundWorker::try_new(|| {
            let session = create_ort_session(256)?;
            Some(move |req: StyleRequest| -> StyleResponse {
                run_inference(&session, req)
            })
        });
        self.worker_512 = BackgroundWorker::try_new(|| {
            let session = create_ort_session(512)?;
            Some(move |req: StyleRequest| -> StyleResponse {
                run_inference(&session, req)
            })
        });

        if self.worker_256.is_none() && self.worker_512.is_none() {
            log::warn!(
                "[NeuralStyleFX] Failed to create ONNX sessions. \
                 Run tools/export_adain_onnx.py to export the models."
            );
        } else {
            log::info!(
                "[NeuralStyleFX] Workers ready: 256={}, 512={}",
                self.worker_256.is_some(),
                self.worker_512.is_some(),
            );
        }
    }

    /// Poll both workers for completed results.
    fn poll_worker_results(&mut self, device: &GpuDevice) {
        // Poll both 256 and 512 workers for results.
        for worker in [&mut self.worker_256, &mut self.worker_512].into_iter().flatten() {
            if let Some(response) = worker.try_recv()
                && let Some(state) = self.owner_states.get_mut(&response.owner_key)
            {
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
                if let Some(rt) = &state.result_rt {
                    device.upload_texture(&rt.texture, &response.stylized_pixels);
                    state.has_result = true;
                }
            }
        }
    }

    /// Submit a readback result to the appropriate worker.
    fn try_submit_inference(&mut self, owner_key: i64) {
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

        // Check if the right worker is busy.
        let worker = if inference_size >= 512 {
            self.worker_512.as_ref().or(self.worker_256.as_ref())
        } else {
            self.worker_256.as_ref().or(self.worker_512.as_ref())
        };
        if worker.is_none_or(|w| w.is_busy()) {
            return;
        }

        // Re-borrow state mutably for readback.
        let Some(state) = self.owner_states.get_mut(&owner_key) else {
            return;
        };
        let pixels = match state.readback.try_read() {
            Some(p) => p,
            None => return,
        };

        let worker = if inference_size >= 512 {
            self.worker_512.as_mut().or(self.worker_256.as_mut())
        } else {
            self.worker_256.as_mut().or(self.worker_512.as_mut())
        };
        let Some(worker) = worker else { return };

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
        // Skip if strength is 0 or no style image selected.
        // Don't check worker here — it's lazily created in apply().
        fx.param_values.first().copied().unwrap_or(0.0) <= 0.0
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
        // Lazy-init the ONNX worker on first apply (not at startup).
        self.ensure_worker();

        let strength = fx.param_values.first().copied().unwrap_or(0.5);
        let inference_size = if fx.param_values.get(1).copied().unwrap_or(0.0) > 0.5 {
            512
        } else {
            256
        };

        // Reload style image if path changed.
        self.maybe_reload_style_image(fx.style_image_path.as_ref());

        // Poll worker results and try to submit new inference.
        self.poll_worker_results(gpu.device);
        self.try_submit_inference(ctx.owner_key);

        // Ensure owner state exists and update inference size.
        let owner_key = ctx.owner_key;
        let state = self.owner_states.entry(owner_key).or_insert_with(|| OwnerState {
            downsample_rt: None,
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
            // Create or resize downsample target.
            if state.downsample_rt.is_none()
                || state.downsample_rt.as_ref().unwrap().width != inference_size
            {
                state.downsample_rt = Some(RenderTarget::new(
                    gpu.device,
                    inference_size,
                    inference_size,
                    GpuTextureFormat::Rgba8Unorm,
                    &format!("NeuralStyle_Down_{owner_key}"),
                ));
            }

            // Downsample source to inference resolution via the blend shader
            // in passthrough mode. The sampler handles bilinear scaling.
            let ds_uniforms = BlendUniforms {
                strength: 0.0,
                has_result: 0,
                _pad: [0.0; 2],
            };
            let ds_bytes = unsafe {
                std::slice::from_raw_parts(
                    &ds_uniforms as *const BlendUniforms as *const u8,
                    std::mem::size_of::<BlendUniforms>(),
                )
            };
            let ds_tex = &state.downsample_rt.as_ref().unwrap().texture
                as *const manifold_gpu::GpuTexture;
            let ds_ref = unsafe { &*ds_tex };
            self.blend_helper.dispatch_a_only(
                gpu,
                source,
                ds_ref,
                ds_bytes,
                "NeuralStyle Downsample",
                inference_size,
                inference_size,
            );

            // Readback from the downsample target (correct size, not the source).
            let ds_tex_ref = unsafe { &*ds_tex };
            state
                .readback
                .submit(gpu, ds_tex_ref, inference_size, inference_size);
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
        if let Some(worker) = &mut self.worker_256 {
            worker.recv_blocking();
        }
        if let Some(worker) = &mut self.worker_512 {
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

/// Probe for the ONNX Runtime dylib on disk before touching ort.
/// Returns the path if found, None if not. This prevents ort from
/// panicking with an uncatchable C++ exception at init time.
fn find_ort_dylib() -> Option<String> {
    // 1. Explicit env var (user override)
    if let Ok(path) = std::env::var("ORT_DYLIB_PATH")
        && std::path::Path::new(&path).exists()
    {
        return Some(path);
    }

    // 2. Homebrew (Apple Silicon)
    let brew = "/opt/homebrew/lib/libonnxruntime.dylib";
    if std::path::Path::new(brew).exists() {
        return Some(brew.to_string());
    }

    // 3. Homebrew (Intel Mac)
    let brew_intel = "/usr/local/lib/libonnxruntime.dylib";
    if std::path::Path::new(brew_intel).exists() {
        return Some(brew_intel.to_string());
    }

    // 4. App bundle Frameworks
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let bundle = parent
            .parent()
            .unwrap_or(parent)
            .join("Frameworks/libonnxruntime.dylib");
        if bundle.exists() {
            return Some(bundle.to_string_lossy().to_string());
        }
    }

    None
}

/// Resolve the path to the ONNX model file for a given resolution.
fn resolve_model_path(size: u32) -> Option<std::path::PathBuf> {
    let filename = format!("adain_style_transfer_{size}.onnx");

    // Try: relative to executable (app bundle)
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let bundle_path = parent
            .parent()
            .unwrap_or(parent)
            .join("Resources/models")
            .join(&filename);
        if bundle_path.exists() {
            return Some(bundle_path);
        }
        let sibling = parent.join(&filename);
        if sibling.exists() {
            return Some(sibling);
        }
    }

    // Try: project root assets
    let project_assets = std::path::Path::new("assets/models").join(&filename);
    if project_assets.exists() {
        return Some(project_assets);
    }

    None
}

/// Create an ONNX Runtime session with CoreML + CPU fallback.
fn create_ort_session(size: u32) -> Option<ort::session::Session> {
    let model_path = resolve_model_path(size)?;
    log::info!(
        "[NeuralStyleFX] Loading ONNX model from: {}",
        model_path.display()
    );

    let builder = match ort::session::Session::builder() {
        Ok(b) => b,
        Err(e) => {
            log::error!("[NeuralStyleFX] Session::builder() failed: {e}");
            return None;
        }
    };

    let builder = match builder.with_execution_providers([
        #[cfg(target_os = "macos")]
        ort::execution_providers::CoreMLExecutionProvider::default().build(),
        ort::execution_providers::CPUExecutionProvider::default().build(),
    ]) {
        Ok(b) => b,
        Err(e) => {
            log::error!("[NeuralStyleFX] with_execution_providers() failed: {e}");
            return None;
        }
    };

    match builder.commit_from_file(&model_path) {
        Ok(session) => {
            log::info!("[NeuralStyleFX] ONNX session created successfully.");
            Some(session)
        }
        Err(e) => {
            log::error!(
                "[NeuralStyleFX] commit_from_file() failed: {e}\n  \
                 Model path: {}",
                model_path.display()
            );
            None
        }
    }
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
