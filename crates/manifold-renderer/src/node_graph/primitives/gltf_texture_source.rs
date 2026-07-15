//! `node.gltf_texture_source` — read one embedded image out of a
//! `.glb`/`.gltf` file and emit it as a `Texture2D` wire, so an imported
//! mesh's baked-in albedo/alpha map can feed `node.render_scene`'s
//! `base_color_map_N`.
//!
//! File I/O + the channel repack (`gltf_load::load_gltf_texture`) happen
//! on a background thread (`std::thread::spawn` + `mpsc::channel`), same
//! pattern as `node.image_folder` / `node.gltf_mesh_source`, so the
//! content thread never stalls on a multi-megabyte glTF parse. The last
//! successfully decoded image stays resident on its own source texture;
//! a stretch-blit compute kernel resamples it into the chain-allocated
//! `out` texture every frame. Unlike `node.image_folder` (which
//! aspect-fits into a canvas-sized output) this primitive's output is
//! `width`×`height`-param-sized and the source is stretched to fill it
//! — the glTF importer sets width/height to the source image's exact
//! dimensions so that stretch is a 1:1 copy in the common case.

use std::borrow::Cow;
use std::sync::mpsc;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::gltf_load::load_gltf_texture;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GltfTextureBlitUniforms {
    out_width: f32,
    out_height: f32,
}

crate::primitive! {
    name: GltfTextureSource,
    type_id: "node.gltf_texture_source",
    purpose: "Read one embedded image out of a glTF/.glb file and emit it as a Texture2D wire, so an imported mesh's baked-in albedo/alpha map can feed node.render_scene's base_color_map_N. texture_index selects among document.textures(); color_space picks sRGB (albedo/base-color — the default) vs Linear (normal/metallic/roughness maps) so the hardware linearizes correctly on sample. width/height set the output resolution: the glTF importer sets these to the source image's exact dimensions (a 1:1 stretch), while manual drops resample to the default 1024² until resized.",
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
            name: Cow::Borrowed("texture_index"),
            label: "Texture Index",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1024.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_space"),
            label: "Color Space",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 1.0)),
            enum_values: &["sRGB", "Linear"],
        },
        ParamDef {
            name: Cow::Borrowed("width"),
            label: "Width",
            ty: ParamType::Int,
            default: ParamValue::Float(1024.0),
            range: Some((1.0, 8192.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("height"),
            label: "Height",
            ty: ParamType::Int,
            default: ParamValue::Float(1024.0),
            range: Some((1.0, 8192.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "path comes via presetMetadata.stringBindings — wire the JSON-graph generator's outer-card Browse field into this primitive's `path` param, same convention as node.image_folder's `folder` and node.gltf_mesh_source's `path`. texture_index selects among document.textures() (not the raw image index — the primitive resolves each texture's image source internally). color_space is the one param that isn't cosmetic: sRGB (default) is correct for anything the eye reads as color — base-color/albedo maps — so the hardware linearizes on sample; Linear is for data maps (normal, metallic-roughness, occlusion) where the raw bytes ARE the value and gamma-decoding them would corrupt the data. width/height set the output resolution — the glTF importer sets them to the source image's exact dimensions so the stretch-blit is a 1:1 copy; manual drops before a File is picked resample (stretch-fill, no aspect-fit) to the 1024² default. Wire `out` into node.render_scene's `base_color_map_N` input.",
    examples: [],
    picker: { label: "glTF Texture", category: Atom },
    summary: "Loads an embedded image from a glTF/.glb file as a texture, so an imported model's baked-in albedo/alpha map flows into the render pipeline like any other texture source.",
    category: Generate,
    role: Source,
    aliases: ["gltf texture", "glb texture", "embedded texture", "import texture", "File In TOP"],
    boundary_reason: IoBridge,
    extra_fields: {
        // (path, texture_index) last parsed (or in flight). Any change
        // re-triggers a background decode.
        last_key: (String, i32) = (String::new(), i32::MIN),
        // The decoded source image, resident on its own GPU texture.
        // `None` until the first successful decode lands.
        source_texture: Option<manifold_gpu::GpuTexture> = None,
        // Dimensions of `source_texture`.
        src_w: u32 = 0,
        src_h: u32 = 0,
        // Background loader channel. `Some` means a decode is in
        // flight; we don't spawn another until it returns.
        pending_load: Option<mpsc::Receiver<Result<(u32, u32, Vec<u8>), String>>> = None,
        // A decoded-but-not-yet-uploaded result, handed off from the
        // drain step to the upload step (texture creation needs the
        // GPU device, which only `run()`'s `ctx` has).
        pending_upload: Option<(u32, u32, Vec<u8>)> = None,
        // Whether `source_texture` currently reflects the last decode.
        uploaded: bool = false,
        // Identity of the `out` texture whose mip chain was last
        // regenerated (IMPORT_FIDELITY F-P6). The blit rewrites level 0
        // every frame, but levels 1.. only need regenerating when the
        // content changed (fresh upload) or the output slot was handed a
        // different physical texture (pool recycle / resize) — comparing
        // `GpuTexture::identity_key` catches both without a per-frame
        // mip pass.
        last_mip_identity: usize = 0,
    },
}

impl Primitive for GltfTextureSource {
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
            _ => 1024,
        };
        let h = match params.get("height") {
            Some(ParamValue::Float(f)) => f.round().max(1.0) as u32,
            _ => 1024,
        };
        Some((w, h))
    }

    fn output_mipmapped(&self, port: &str) -> bool {
        // IMPORT_FIDELITY F-P6: material maps are sampled under heavy
        // minification in `render_scene` — the output slot carries a full
        // mip chain, filled by `generate_mipmaps` in `run()` step 8.
        port == "out"
    }

    fn io_pending(&self) -> bool {
        // True while a background decode is in flight or decoded-but-not-
        // uploaded — this node emits black (or stale content) until then,
        // so headless convergence loops must not count those frames as
        // settled. Added with `node.hdri_source` (GLB_CONFORMANCE G-P6
        // gate-review fix): the same latent race exists here, masked only
        // by glb-embedded textures decoding faster than the 50ms-paced
        // stability window.
        self.pending_load.is_some() || self.pending_upload.is_some()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // 1. Params.
        let path = match ctx.params.get("path") {
            Some(ParamValue::String(s)) => s.as_str().to_owned(),
            _ => String::new(),
        };
        let texture_index = match ctx.params.get("texture_index") {
            Some(ParamValue::Float(n)) => n.round().max(0.0) as i32,
            _ => 0,
        };

        // 2. Re-trigger a background decode if the effective selection
        // changed since the last one we started.
        let key = (path.clone(), texture_index);
        if key != self.last_key && self.pending_load.is_none() {
            self.last_key = key;
            self.source_texture = None;
            self.src_w = 0;
            self.src_h = 0;
            self.uploaded = false;
            self.pending_upload = None;
            if !path.is_empty() {
                let path_buf = std::path::PathBuf::from(&path);
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(load_gltf_texture(&path_buf, texture_index as u32));
                });
                self.pending_load = Some(rx);
            }
        }

        // 3. Drain any completed background decode.
        if self.pending_load.is_some() {
            let rx = self.pending_load.take().unwrap();
            match rx.try_recv() {
                Ok(Ok((w, h, rgba))) => {
                    self.pending_upload = Some((w, h, rgba));
                    self.uploaded = false;
                }
                Ok(Err(e)) => {
                    log::error!("node.gltf_texture_source: {e}");
                }
                Err(mpsc::TryRecvError::Empty) => {
                    // Still in flight — put the receiver back.
                    self.pending_load = Some(rx);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::error!("node.gltf_texture_source: background load channel disconnected");
                }
            }
        }

        // 4. Upload a freshly decoded image to the GPU. color_space is
        // read here (rather than cached) so it always reflects the
        // param at the moment of upload.
        let mut fresh_upload = false;
        if let Some((w, h, rgba)) = self.pending_upload.take() {
            let color_space = match ctx.params.get("color_space") {
                Some(ParamValue::Enum(v)) => *v,
                _ => 0,
            };
            let format = if color_space == 0 {
                manifold_gpu::GpuTextureFormat::Rgba8UnormSrgb
            } else {
                manifold_gpu::GpuTextureFormat::Rgba8Unorm
            };
            self.ensure_texture(ctx, w, h, format);
            if let Some(tex) = &self.source_texture {
                ctx.gpu_encoder()
                    .native_enc
                    .upload_texture(tex, w, h, 1, &rgba);
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

        // 6. Nothing decoded yet (first-frame race, empty path, or a
        // decode error) — emit black rather than whatever pool leftover
        // is sitting in the output slot.
        let Some(source_texture) = self.source_texture.as_ref() else {
            let gpu = ctx.gpu_encoder();
            gpu.clear_texture(out, 0.0, 0.0, 0.0, 1.0);
            // The clear writes level 0 only — propagate the black down the
            // chain so a downstream mip sample never reads a recycled
            // slot's leftover tails (same staleness rule as step 8).
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
                include_str!("shaders/gltf_texture_blit.wgsl"),
                "cs_main",
                "node.gltf_texture_source",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = GltfTextureBlitUniforms {
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
            "node.gltf_texture_source",
        );

        // 8. Regenerate the output's mip chain (IMPORT_FIDELITY F-P6).
        // The blit above rewrote level 0; levels 1.. are stale whenever
        // the content changed (fresh upload) or the slot handed us a
        // different physical texture than the one we last mipped (pool
        // recycle / resize). Guarded on the chain actually existing —
        // tests that pre-bind a flat texture skip the pass cleanly.
        if out.mip_level_count() > 1 {
            let out_identity = out.identity_key();
            if fresh_upload || out_identity != self.last_mip_identity {
                gpu.native_enc.generate_mipmaps(out);
                self.last_mip_identity = out_identity;
            }
        }
    }
}

impl GltfTextureSource {
    /// BUG-037: compile the stretch-blit compute pipeline into `device`'s
    /// shared compute-pipeline cache ahead of time. `run()` step 7 only
    /// reaches `self.pipeline.get_or_insert_with(...)` once a texture has
    /// actually decoded, so on a project's first glTF texture the compile
    /// (real MSL compile) lands on the same frame as the decode — part of
    /// the content-thread stall this bug reports. The shader source and
    /// entry point are fixed (no project data involved), and the device's
    /// pipeline cache is keyed by shader hash and shared across every
    /// `GltfTextureSource` instance, so warming it once here makes every
    /// later `get_or_insert_with` a cache hit regardless of which layer or
    /// asset triggers it first. Called from `GeneratorRegistry::prewarm_all`
    /// at app startup, alongside `RenderScene::prewarm_pipelines`.
    pub fn prewarm_pipeline(device: &manifold_gpu::GpuDevice) {
        device.create_compute_pipeline(
            include_str!("shaders/gltf_texture_blit.wgsl"),
            "cs_main",
            "node.gltf_texture_source",
        );
    }

    fn ensure_texture(
        &mut self,
        ctx: &mut EffectNodeContext<'_, '_>,
        w: u32,
        h: u32,
        format: manifold_gpu::GpuTextureFormat,
    ) {
        if self.src_w == w && self.src_h == h && self.source_texture.is_some() {
            return;
        }
        let device = ctx.gpu_encoder().device;
        let tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                | manifold_gpu::GpuTextureUsage::CPU_UPLOAD,
            label: "node.gltf_texture_source source",
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
    fn gltf_texture_source_declares_zero_inputs_and_texture_output() {
        assert_eq!(GltfTextureSource::TYPE_ID, "node.gltf_texture_source");
        assert!(GltfTextureSource::INPUTS.is_empty());
        assert_eq!(GltfTextureSource::OUTPUTS.len(), 1);
        assert_eq!(GltfTextureSource::OUTPUTS[0].name, "out");
        assert_eq!(GltfTextureSource::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn gltf_texture_source_param_names_in_order() {
        let names: Vec<&str> = GltfTextureSource::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["path", "texture_index", "color_space", "width", "height"]
        );
    }

    #[test]
    fn primitive_registers() {
        let prim = GltfTextureSource::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.gltf_texture_source");
    }

    fn params_at(width: f32, height: f32) -> ParamValues {
        let mut p = ahash::AHashMap::default();
        p.insert(std::borrow::Cow::Borrowed("width"), ParamValue::Float(width));
        p.insert(std::borrow::Cow::Borrowed("height"), ParamValue::Float(height));
        p
    }

    #[test]
    fn output_dims_default_to_1024_square() {
        let prim = GltfTextureSource::new();
        let node: &dyn EffectNode = &prim;
        let params = params_at(1024.0, 1024.0);
        let dims = node.output_dims("out", (1920, 1080), &[], &params);
        assert_eq!(dims, Some((1024, 1024)));
    }

    #[test]
    fn output_dims_honor_custom_resolution_not_canvas() {
        let prim = GltfTextureSource::new();
        let node: &dyn EffectNode = &prim;
        // Canvas is 1920x1080 but width/height say otherwise — the
        // output must follow the params, not the canvas.
        let params = params_at(2048.0, 512.0);
        let dims = node.output_dims("out", (1920, 1080), &[], &params);
        assert_eq!(dims, Some((2048, 512)));
    }

    #[test]
    fn output_dims_returns_none_for_unknown_port() {
        let prim = GltfTextureSource::new();
        let node: &dyn EffectNode = &prim;
        let params = params_at(1024.0, 1024.0);
        assert_eq!(node.output_dims("nonexistent", (1920, 1080), &[], &params), None);
    }
}

/// BUG-037 — GPU-backed proof `prewarm_pipeline` actually populates the
/// device's shared compute-pipeline cache, so the first glTF texture that
/// decodes in a live project hits `run()` step 7's
/// `self.pipeline.get_or_insert_with(...)` as a cache hit rather than
/// compiling the blit shader on the content thread. Run deliberately:
/// `cargo test -p manifold-renderer --features gpu-proofs
/// node_graph::primitives::gltf_texture_source::gpu_tests`.
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;

    #[test]
    fn prewarm_pipeline_populates_the_shared_compute_cache() {
        let device = crate::test_device();
        // Order-independent (BUG-144): the cache is process-global and
        // shared with other gpu_tests, so another test may already have
        // populated this exact entry, reading a zero before/after delta even
        // though prewarm worked. Assert the cache ends up populated instead
        // of asserting THIS call grew it.
        GltfTextureSource::prewarm_pipeline(&device);
        let after = device.compute_pipeline_cache_len();
        assert!(
            after > 0,
            "prewarm_pipeline must leave the compute cache populated: after={after}"
        );

        // Idempotent.
        GltfTextureSource::prewarm_pipeline(&device);
        assert_eq!(
            device.compute_pipeline_cache_len(),
            after,
            "a second prewarm pass must be a pure cache hit"
        );

        // The exact call `run()` step 7 makes must now be a cache hit.
        let cache_before_use = device.compute_pipeline_cache_len();
        device.create_compute_pipeline(
            include_str!("shaders/gltf_texture_blit.wgsl"),
            "cs_main",
            "node.gltf_texture_source",
        );
        assert_eq!(
            device.compute_pipeline_cache_len(),
            cache_before_use,
            "the blit pipeline compile after prewarm must be a cache hit"
        );
    }
}
