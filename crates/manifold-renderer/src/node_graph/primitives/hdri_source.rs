//! `node.hdri_source` — read a linear HDR equirectangular environment map
//! (`.exr`) off disk and emit it as a `Texture2D` wire, so
//! `node.render_scene`'s `envmap` input can be lit by a real-world capture
//! instead of (or alongside) the procedural `node.bake_environment` studio.
//!
//! Shaped identically to `node.gltf_texture_source`
//! (GLB_CONFORMANCE_DESIGN.md D6/§3: "copy `gltf_texture_source.rs` wholesale
//! as the shape") — background decode thread → `Rgba16Float` upload →
//! stretch-blit into the chain-allocated `out` slot every frame → mipmapped
//! output (IMPORT_FIDELITY F-P6's mip contract: `render_scene` samples
//! `envmap` under heavy minification during IBL prefilter/irradiance
//! convolution, so a flat-uploaded map would alias the same way the F-P6
//! material maps did before mips landed). File I/O + the `image` crate's EXR
//! decode happen on a background thread (`std::thread::spawn` + `mpsc::
//! channel`) so the content thread never stalls on a multi-megabyte HDR
//! decode.
//!
//! **No `color_space` param** — unlike `node.gltf_texture_source`, an EXR
//! environment map is always linear light (D6: "EXR is linear — upload
//! `Rgba16Float` directly, no color_space param at all"). There is nothing
//! to pick: sRGB decoding would be a correctness bug on this node, not a
//! missing feature.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use half::f16;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HdriBlitUniforms {
    out_width: f32,
    out_height: f32,
}

/// Decode one linear-HDR equirect environment map off disk. Runs entirely on
/// the caller's thread — this function is only ever invoked from inside a
/// `std::thread::spawn` closure (`run()` step 2 below), never on the content
/// thread, per the `node.hdri_source never blocks the content thread`
/// invariant (GLB_CONFORMANCE_DESIGN.md §4).
///
/// `image::open` dispatches on the file's magic number (the `exr` feature
/// registers OpenEXR's), so this accepts any `.exr` regardless of extension
/// case. `into_rgb32f()` gives the decoded linear float triples directly —
/// no gamma table involved anywhere in this path, matching D6's "EXR is
/// linear, full stop." Output is packed RGBA16Float bytes (alpha = 1.0,
/// EXRs carry no alpha channel in the equirect-environment convention this
/// primitive targets) ready for `GpuEncoder::upload_texture`.
fn load_hdri(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let img = image::open(path).map_err(|e| format!("image::open({}): {e}", path.display()))?;
    let rgb = img.into_rgb32f();
    let (w, h) = rgb.dimensions();
    if w == 0 || h == 0 {
        return Err(format!("{}: zero-sized image ({w}x{h})", path.display()));
    }
    let raw = rgb.into_raw();
    let mut out = Vec::with_capacity(raw.len() / 3 * 4 * 2);
    for px in raw.chunks_exact(3) {
        out.extend_from_slice(&f16::from_f32(px[0]).to_le_bytes());
        out.extend_from_slice(&f16::from_f32(px[1]).to_le_bytes());
        out.extend_from_slice(&f16::from_f32(px[2]).to_le_bytes());
        out.extend_from_slice(&f16::from_f32(1.0).to_le_bytes());
    }
    Ok((w, h, out))
}

crate::primitive! {
    name: HdriSource,
    type_id: "node.hdri_source",
    purpose: "Read a linear-HDR equirectangular environment map (.exr) off disk and emit it as a Texture2D wire, so node.render_scene's envmap input can be lit by a real-world HDRI capture instead of the procedural node.bake_environment studio. No color_space param — EXR is always linear light, full stop. width/height set the output resolution (default 2048x1024): the decoded source is stretch-blit into that slot every frame, so a lower output resolution trades reflection sharpness for prefilter-convolution cost on node.render_scene's envmap-sampling passes.",
    inputs: {},
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("path"),
            label: "File",
            ty: ParamType::String,
            default: ParamValue::Float(0.0), // String default supplied via stringBindings; this slot is never read.
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("width"),
            label: "Width",
            ty: ParamType::Int,
            default: ParamValue::Float(2048.0),
            range: Some((1.0, 8192.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("height"),
            label: "Height",
            ty: ParamType::Int,
            default: ParamValue::Float(1024.0),
            range: Some((1.0, 4096.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "path comes via presetMetadata.stringBindings — wire the JSON-graph generator's outer-card Browse field into this primitive's `path` param, same convention as node.gltf_texture_source's `path`. Wire `out` into node.render_scene's `envmap` input directly (D6: the bake/prefilter path is unchanged — render_scene already consumes any equirect texture on that port) or through node.switch_texture alongside a node.bake_environment output to let a card enum (Softbox vs HDRI) pick between them live. width/height set the output resolution — the decoded EXR is stretched to fill it every frame; the default 2048x1024 keeps the split-sum IBL prefilter's per-frame convolution cost bounded (GLB_CONFORMANCE_DESIGN.md G-P6's cost measurement).",
    examples: [],
    picker: { label: "HDRI Source", category: Atom },
    summary: "Loads a linear-HDR .exr environment map from disk as a texture, so a real-world HDRI capture flows into node.render_scene's envmap input like any other texture source.",
    category: Generate,
    role: Source,
    aliases: ["hdri", "exr", "environment map", "ibl source", "equirect"],
    boundary_reason: IoBridge,
    extra_fields: {
        // Path last parsed (or in flight). Any change re-triggers a
        // background decode. Unlike node.gltf_texture_source there is no
        // texture_index — an HDRI file is one image.
        last_path: String = String::new(),
        // The decoded source image, resident on its own GPU texture.
        // `None` until the first successful decode lands.
        source_texture: Option<manifold_gpu::GpuTexture> = None,
        // Dimensions of `source_texture`.
        src_w: u32 = 0,
        src_h: u32 = 0,
        // Background loader channel. `Some` means a decode is in flight; we
        // don't spawn another until it returns.
        pending_load: Option<mpsc::Receiver<Result<(u32, u32, Vec<u8>), String>>> = None,
        // A decoded-but-not-yet-uploaded result, handed off from the drain
        // step to the upload step (texture creation needs the GPU device,
        // which only `run()`'s `ctx` has).
        pending_upload: Option<(u32, u32, Vec<u8>)> = None,
        // Whether `source_texture` currently reflects the last decode.
        uploaded: bool = false,
        // Identity of the `out` texture whose mip chain was last
        // regenerated (IMPORT_FIDELITY F-P6 contract, mirrored from
        // node.gltf_texture_source). The blit rewrites level 0 every frame;
        // levels 1.. only need regenerating when the content changed
        // (fresh upload) or the output slot was handed a different physical
        // texture (pool recycle / resize).
        last_mip_identity: usize = 0,
    },
}

impl Primitive for HdriSource {
    fn output_dims(
        &self,
        port: &str,
        _canvas_dims: (u32, u32),
        _input_dims: &[(&str, (u32, u32))],
        params: &ParamValues,
    ) -> Option<(u32, u32)> {
        if port != "out" {
            return None;
        }
        let w = match params.get("width") {
            Some(ParamValue::Float(f)) => f.round().max(1.0) as u32,
            _ => 2048,
        };
        let h = match params.get("height") {
            Some(ParamValue::Float(f)) => f.round().max(1.0) as u32,
            _ => 1024,
        };
        Some((w, h))
    }

    fn output_mipmapped(&self, port: &str) -> bool {
        // IMPORT_FIDELITY F-P6 mip contract, mirrored here per D6: the
        // envmap is sampled under heavy minification by render_scene's IBL
        // irradiance/prefilter convolution — the output slot carries a full
        // mip chain, filled by `generate_mipmaps` in `run()` step 8.
        port == "out"
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // 1. Params.
        let path = match ctx.params.get("path") {
            Some(ParamValue::String(s)) => s.as_str().to_owned(),
            _ => String::new(),
        };

        // 2. Re-trigger a background decode if the path changed since the
        // last one we started. Decode + `std::fs`/`image::open` happen ONLY
        // inside this spawned thread — never on the content thread (§4
        // invariant, gated by the G-P6 grep gate).
        if path != self.last_path && self.pending_load.is_none() {
            self.last_path = path.clone();
            self.source_texture = None;
            self.src_w = 0;
            self.src_h = 0;
            self.uploaded = false;
            self.pending_upload = None;
            if !path.is_empty() {
                let path_buf = PathBuf::from(&path);
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(load_hdri(&path_buf));
                });
                self.pending_load = Some(rx);
            }
        }

        // 3. Drain any completed background decode.
        if self.pending_load.is_some() {
            let rx = self.pending_load.take().unwrap();
            match rx.try_recv() {
                Ok(Ok((w, h, rgba16f))) => {
                    self.pending_upload = Some((w, h, rgba16f));
                    self.uploaded = false;
                }
                Ok(Err(e)) => {
                    log::error!("node.hdri_source: {e}");
                }
                Err(mpsc::TryRecvError::Empty) => {
                    // Still in flight — put the receiver back.
                    self.pending_load = Some(rx);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::error!("node.hdri_source: background load channel disconnected");
                }
            }
        }

        // 4. Upload a freshly decoded image to the GPU. Always
        // Rgba16Float — EXR is linear, there is no color_space branch.
        let mut fresh_upload = false;
        if let Some((w, h, rgba16f)) = self.pending_upload.take() {
            self.ensure_texture(ctx, w, h);
            if let Some(tex) = &self.source_texture {
                ctx.gpu_encoder()
                    .native_enc
                    .upload_texture(tex, w, h, 1, &rgba16f);
            }
            self.src_w = w;
            self.src_h = h;
            self.uploaded = true;
            fresh_upload = true;
        }

        // 5. Output buffer.
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out.width, out.height);
        if w == 0 || h == 0 {
            return;
        }

        // 6. Nothing decoded yet (first-frame race, empty path, or a decode
        // error) — emit black rather than whatever pool leftover is sitting
        // in the output slot.
        let Some(source_texture) = self.source_texture.as_ref() else {
            let gpu = ctx.gpu_encoder();
            gpu.clear_texture(out, 0.0, 0.0, 0.0, 1.0);
            if out.mip_level_count() > 1 {
                let out_identity = out.identity_key();
                if out_identity != self.last_mip_identity {
                    gpu.native_enc.generate_mipmaps(out);
                    self.last_mip_identity = out_identity;
                }
            }
            return;
        };

        // 7. Dispatch the stretch-blit compute kernel.
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/hdri_source_blit.wgsl"),
                "cs_main",
                "node.hdri_source",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = HdriBlitUniforms {
            out_width: w as f32,
            out_height: h as f32,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: source_texture,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.hdri_source",
        );

        // 8. Regenerate the output's mip chain (IMPORT_FIDELITY F-P6
        // contract). The blit above rewrote level 0; levels 1.. are stale
        // whenever the content changed (fresh upload) or the slot handed us
        // a different physical texture than the one we last mipped.
        if out.mip_level_count() > 1 {
            let out_identity = out.identity_key();
            if fresh_upload || out_identity != self.last_mip_identity {
                gpu.native_enc.generate_mipmaps(out);
                self.last_mip_identity = out_identity;
            }
        }
    }
}

impl HdriSource {
    /// Compile the stretch-blit compute pipeline into `device`'s shared
    /// compute-pipeline cache ahead of time, mirroring
    /// `GltfTextureSource::prewarm_pipeline` (BUG-037's fix for the analogous
    /// glTF texture node): without this, an imported rig's first HDRI decode
    /// pays a real MSL compile on the same content-thread frame the decode
    /// lands. Called from `GeneratorRegistry::prewarm_all`.
    pub fn prewarm_pipeline(device: &manifold_gpu::GpuDevice) {
        device.create_compute_pipeline(
            include_str!("shaders/hdri_source_blit.wgsl"),
            "cs_main",
            "node.hdri_source",
        );
    }

    fn ensure_texture(&mut self, ctx: &mut EffectNodeContext<'_, '_>, w: u32, h: u32) {
        if self.src_w == w && self.src_h == h && self.source_texture.is_some() {
            return;
        }
        let device = ctx.gpu_encoder().device;
        let tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                | manifold_gpu::GpuTextureUsage::CPU_UPLOAD,
            label: "node.hdri_source source",
            mip_levels: 1,
        });
        self.source_texture = Some(tex);
        self.src_w = w;
        self.src_h = h;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::ports::PortType;

    #[test]
    fn hdri_source_declares_zero_inputs_and_texture_output() {
        assert_eq!(HdriSource::TYPE_ID, "node.hdri_source");
        assert!(HdriSource::INPUTS.is_empty());
        assert_eq!(HdriSource::OUTPUTS.len(), 1);
        assert_eq!(HdriSource::OUTPUTS[0].name, "out");
        assert_eq!(HdriSource::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn hdri_source_param_names_in_order_and_no_color_space() {
        let names: Vec<&str> = HdriSource::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        // D6: EXR is linear, full stop — no color_space param exists on
        // this node, unlike node.gltf_texture_source.
        assert_eq!(names, vec!["path", "width", "height"]);
    }

    #[test]
    fn primitive_registers() {
        let prim = HdriSource::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.hdri_source");
    }

    fn params_at(width: f32, height: f32) -> ParamValues {
        let mut p = ahash::AHashMap::default();
        p.insert(std::borrow::Cow::Borrowed("width"), ParamValue::Float(width));
        p.insert(std::borrow::Cow::Borrowed("height"), ParamValue::Float(height));
        p
    }

    #[test]
    fn output_dims_default_to_2048x1024() {
        let prim = HdriSource::new();
        let node: &dyn EffectNode = &prim;
        let params = params_at(2048.0, 1024.0);
        let dims = node.output_dims("out", (1920, 1080), &[], &params);
        assert_eq!(dims, Some((2048, 1024)));
    }

    #[test]
    fn output_dims_honor_custom_resolution_not_canvas() {
        let prim = HdriSource::new();
        let node: &dyn EffectNode = &prim;
        let params = params_at(4096.0, 2048.0);
        let dims = node.output_dims("out", (1920, 1080), &[], &params);
        assert_eq!(dims, Some((4096, 2048)));
    }

    #[test]
    fn output_dims_returns_none_for_unknown_port() {
        let prim = HdriSource::new();
        let node: &dyn EffectNode = &prim;
        let params = params_at(2048.0, 1024.0);
        assert_eq!(node.output_dims("nonexistent", (1920, 1080), &[], &params), None);
    }

    /// Generates a tiny 64x32 EXR fixture in-process (via the `image`
    /// crate's `exr` feature) rather than committing a binary — D1's
    /// skip-if-absent convention doesn't apply here since this fixture is
    /// synthetic, not fetched, so there's nothing to skip. Confirms the
    /// decode path round-trips: a known solid-color EXR decodes to the
    /// expected linear values with alpha forced to 1.0, and the resulting
    /// bytes are the exact size `run()`'s upload step expects
    /// (`w * h * 4 channels * 2 bytes-per-f16`).
    #[test]
    fn decode_roundtrips_a_synthetic_exr_fixture() {
        let dir = std::env::temp_dir().join(format!(
            "manifold-hdri-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fixture.exr");

        let (w, h) = (64u32, 32u32);
        let mut buf: image::Rgb32FImage = image::ImageBuffer::new(w, h);
        for px in buf.pixels_mut() {
            *px = image::Rgb([2.5f32, 0.125, 4.0]);
        }
        image::DynamicImage::ImageRgb32F(buf)
            .save_with_format(&path, image::ImageFormat::OpenExr)
            .expect("write synthetic EXR fixture");

        let (dw, dh, bytes) = load_hdri(&path).expect("decode synthetic EXR fixture");
        assert_eq!((dw, dh), (w, h));
        assert_eq!(bytes.len(), (w * h * 4 * 2) as usize);

        // First texel, decoded back from LE f16 bytes.
        let r = f16::from_le_bytes([bytes[0], bytes[1]]).to_f32();
        let g = f16::from_le_bytes([bytes[2], bytes[3]]).to_f32();
        let b = f16::from_le_bytes([bytes[4], bytes[5]]).to_f32();
        let a = f16::from_le_bytes([bytes[6], bytes[7]]).to_f32();
        assert!((r - 2.5).abs() < 0.01, "r={r}");
        assert!((g - 0.125).abs() < 0.001, "g={g}");
        assert!((b - 4.0).abs() < 0.01, "b={b}");
        assert_eq!(a, 1.0, "alpha must be forced to 1.0 — EXR carries none");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decode_reports_a_loud_error_for_a_missing_file() {
        let err = load_hdri(Path::new("/nonexistent/does-not-exist.exr")).unwrap_err();
        assert!(err.contains("does-not-exist.exr"), "error must name the path: {err}");
    }
}

/// GPU-backed proof: the decoded EXR bytes actually upload and blit
/// correctly, and `prewarm_pipeline` populates the shared compute cache
/// (mirroring `gltf_texture_source`'s BUG-037 proof). Run deliberately:
/// `cargo test -p manifold-renderer --features gpu-proofs
/// node_graph::primitives::hdri_source::gpu_tests`.
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;

    #[test]
    fn prewarm_pipeline_populates_the_shared_compute_cache() {
        let device = crate::test_device();
        HdriSource::prewarm_pipeline(&device);
        let after = device.compute_pipeline_cache_len();
        assert!(
            after > 0,
            "prewarm_pipeline must leave the compute cache populated: after={after}"
        );
        HdriSource::prewarm_pipeline(&device);
        assert_eq!(
            device.compute_pipeline_cache_len(),
            after,
            "a second prewarm pass must be a pure cache hit"
        );
    }

    /// Decode a synthetic EXR, upload it exactly as `run()` step 4 does,
    /// and blit it into a chain-sized output — the numbers must survive the
    /// GPU round-trip unchanged (linear, no gamma anywhere on this path).
    #[test]
    fn decoded_exr_uploads_and_blits_without_gamma_or_clamping() {
        let device = crate::test_device();

        let dir = std::env::temp_dir().join(format!(
            "manifold-hdri-gputest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fixture.exr");
        let (w, h) = (64u32, 32u32);
        let mut buf: image::Rgb32FImage = image::ImageBuffer::new(w, h);
        for px in buf.pixels_mut() {
            *px = image::Rgb([2.0f32, 1.0, 0.25]);
        }
        image::DynamicImage::ImageRgb32F(buf)
            .save_with_format(&path, image::ImageFormat::OpenExr)
            .unwrap();

        let (dw, dh, rgba16f) = load_hdri(&path).unwrap();

        let src = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: dw,
            height: dh,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                | manifold_gpu::GpuTextureUsage::CPU_UPLOAD
                | manifold_gpu::GpuTextureUsage::COPY_SRC,
            label: "hdri gpu test src",
            mip_levels: 1,
        });
        device.upload_texture(&src, &rgba16f);

        let out = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: dw,
            height: dh,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::SHADER_WRITE
                | manifold_gpu::GpuTextureUsage::SHADER_READ
                | manifold_gpu::GpuTextureUsage::COPY_SRC,
            label: "hdri gpu test out",
            mip_levels: 1,
        });

        let pipeline = device.create_compute_pipeline(
            include_str!("shaders/hdri_source_blit.wgsl"),
            "cs_main",
            "hdri gpu test",
        );
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let uniforms = HdriBlitUniforms {
            out_width: dw as f32,
            out_height: dh as f32,
        };

        let mut enc = device.create_encoder("hdri gpu test");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Texture { binding: 1, texture: &src },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &out },
            ],
            [dw.div_ceil(16), dh.div_ceil(16), 1],
            "hdri gpu test",
        );
        let bytes_per_row = dw * 8; // Rgba16Float = 8 bytes/texel
        let readback = device.create_buffer_shared(u64::from(dh * bytes_per_row));
        enc.copy_texture_to_buffer(&out, &readback, dw, dh, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared readback buffer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (dw * dh * 4) as usize) };
        let px = |x: u32, y: u32, c: usize| -> f32 {
            let idx = ((y * dw + x) * 4) as usize + c;
            f16::from_bits(halves[idx]).to_f32()
        };
        let (r, g, b, a) = (px(dw / 2, dh / 2, 0), px(dw / 2, dh / 2, 1), px(dw / 2, dh / 2, 2), px(dw / 2, dh / 2, 3));
        assert!((r - 2.0).abs() < 0.05, "r={r} — must survive unclamped (>1.0 HDR value)");
        assert!((g - 1.0).abs() < 0.05, "g={g}");
        assert!((b - 0.25).abs() < 0.05, "b={b}");
        assert!((a - 1.0).abs() < 0.05, "a={a}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
