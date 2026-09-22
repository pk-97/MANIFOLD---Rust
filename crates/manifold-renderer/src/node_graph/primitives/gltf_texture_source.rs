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
use std::cell::RefCell;
use std::sync::{mpsc, Arc, Weak};

use ahash::AHashMap;
use sha2::{Digest, Sha256};

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::gltf_load::load_gltf_texture;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceTextureKey {
    rgba8_sha256: [u8; 32],
    width: u32,
    height: u32,
    format: manifold_gpu::GpuTextureFormat,
    device_scope_id: u64,
}

/// The converted texture is immutable after its conversion command buffer is
/// submitted.  The event is deliberately kept with the texture so a weak
/// cache hit can be adopted only after the GPU has completed the conversion
/// and mip generation.
pub struct ConvertedTextureBundle {
    texture: manifold_gpu::GpuTexture,
    ready: manifold_gpu::GpuEvent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConvertedTextureKey {
    source: Option<SourceTextureKey>,
    width: u32,
    height: u32,
    format: manifold_gpu::GpuTextureFormat,
    mip_levels: u32,
    mode_bits: u32,
    device_scope_id: u64,
}

thread_local! {
    static SOURCE_TEXTURE_CACHE: RefCell<AHashMap<SourceTextureKey, Weak<manifold_gpu::GpuTexture>>> =
        RefCell::new(AHashMap::new());
    static CONVERTED_TEXTURE_CACHE: RefCell<AHashMap<ConvertedTextureKey, Weak<ConvertedTextureBundle>>> =
        RefCell::new(AHashMap::new());
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GltfTextureBlitUniforms {
    out_width: f32,
    out_height: f32,
    mode: f32,
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
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Repack Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 1.0)),
            enum_values: &["passthrough", "gloss_to_roughness"],
        },
    ],
    // depth_rule: zero-input IO bridge that loads externally-authored image content (not procedurally defined) — treated like system.source's boundary Inherit rather than SourceHeight, since there's no formula whose own luminance is a meaningful height
    depth_rule: Inherit,
    composition_notes: "path comes via presetMetadata.stringBindings — wire the JSON-graph generator's outer-card Browse field into this primitive's `path` param, same convention as node.image_folder's `folder` and node.gltf_mesh_source's `path`. texture_index selects among document.textures() (not the raw image index — the primitive resolves each texture's image source internally). color_space is the one param that isn't cosmetic: sRGB (default) is correct for anything the eye reads as color — base-color/albedo maps — so the hardware linearizes on sample; Linear is for data maps (normal, metallic-roughness, occlusion) where the raw bytes ARE the value and gamma-decoding them would corrupt the data. width/height set the output resolution — the glTF importer sets them to the source image's exact dimensions so the stretch-blit is a 1:1 copy; manual drops before a File is picked resample (stretch-fill, no aspect-fit) to the 1024² default. mode=gloss_to_roughness (GLB_XFAIL_BURNDOWN_DESIGN.md D2) repacks a KHR_materials_pbrSpecularGlossiness specularGlossinessTexture's alpha (glossiness) into render_scene's glTF metal-rough packing (G=roughness=1-gloss, B=metallic=0) at blit time, so a spec-gloss texture can wire into the same `mrMap` input a real metal-rough texture uses — passthrough (default) is a byte-identical plain copy. Wire `out` into node.render_scene's `base_color_map_N` input.",
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
        // Immutable decoded source image, shared with identical live sources.
        // `None` until the first successful decode lands.
        source_texture: Option<Arc<manifold_gpu::GpuTexture>> = None,
        // Full content identity retained after upload.  This is also the
        // converted-output cache's source component; the output texture must
        // never be keyed by a physical source pointer alone.
        source_content_key: Option<SourceTextureKey> = None,
        // Dimensions of `source_texture`.
        src_w: u32 = 0,
        src_h: u32 = 0,
        // Background loader channel. `Some` means a decode is in
        // flight; we don't spawn another until it returns.
        pending_load: Option<mpsc::Receiver<Result<(u32, u32, Vec<u8>, [u8; 32]), String>>> = None,
        // A decoded-but-not-yet-uploaded result, handed off from the
        // drain step to the upload step (texture creation needs the
        // GPU device, which only `run()`'s `ctx` has).
        pending_upload: Option<(u32, u32, Vec<u8>, [u8; 32])> = None,
        // Whether `source_texture` currently reflects the last decode.
        uploaded: bool = false,
        // Identity of the `out` texture the level-0 blit + mip chain were
        // last written for (IMPORT_FIDELITY F-P6 introduced this for mips
        // only; RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P1/R1 extends it
        // to gate the level-0 blit dispatch itself). Comparing
        // `GpuTexture::identity_key` catches both a fresh decode (via
        // `fresh_upload`) and a pool recycle/resize (different physical
        // texture) without a per-frame mip pass or blit dispatch.
        last_mip_identity: usize = 0,
        // `mode` the blit last ran with. `mode` affects the blit's output
        // BYTES directly (gloss-to-roughness repack) without triggering a
        // re-decode (it isn't part of `last_key`), so it must gate the
        // blit independently of content/identity — a mode flip with
        // everything else unchanged must still re-blit.
        last_blit_mode: f32 = -1.0,
        last_blit_dims: (u32, u32) = (0, 0),
        last_blit_format: Option<manifold_gpu::GpuTextureFormat> = None,
        // True once a decoded image has been copied to an output at least
        // once. A later copy into a recycled output has the same logical
        // content even though it must still write physically.
        published_content: bool = false,
        // Dedicated immutable output (when the executor reserved one). The
        // pre-bound compatibility path continues to use `texture_2d("out")`.
        converted_output: Option<Arc<ConvertedTextureBundle>> = None,
        converted_key: Option<ConvertedTextureKey> = None,
        // A duplicate created while the canonical conversion is in flight
        // stays warmup-pending until a later evaluation adopts (or promotes)
        // a completed bundle.
        awaiting_canonical: bool = false,
    },
}

impl Primitive for GltfTextureSource {
    fn provides_texture_output(&self, port: &str) -> bool {
        port == "out"
    }

    fn provided_texture_output(&self, port: &str) -> Option<&manifold_gpu::GpuTexture> {
        (port == "out")
            .then(|| self.converted_output.as_ref().map(|bundle| &bundle.texture))
            .flatten()
    }

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

    fn warmup_pending(&self) -> bool {
        // Same lifetime as `io_pending` for this IoBridge source.
        self.io_pending() || self.awaiting_canonical
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
            self.source_content_key = None;
            self.src_w = 0;
            self.src_h = 0;
            self.uploaded = false;
            self.pending_upload = None;
            self.published_content = false;
            self.converted_output = None;
            self.converted_key = None;
            self.awaiting_canonical = false;
            if !path.is_empty() {
                let path_buf = std::path::PathBuf::from(&path);
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let decoded =
                        load_gltf_texture(&path_buf, texture_index as u32).map(|(w, h, rgba)| {
                            let rgba8_sha256: [u8; 32] = Sha256::digest(&rgba).into();
                            (w, h, rgba, rgba8_sha256)
                        });
                    let _ = tx.send(decoded);
                });
                self.pending_load = Some(rx);
            }
        }

        // 3. Drain any completed background decode.
        if self.pending_load.is_some() {
            let rx = self.pending_load.take().unwrap();
            match rx.try_recv() {
                Ok(Ok((w, h, rgba, rgba8_sha256))) => {
                    self.pending_upload = Some((w, h, rgba, rgba8_sha256));
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
        if let Some((w, h, rgba, rgba8_sha256)) = self.pending_upload.take() {
            let color_space = match ctx.params.get("color_space") {
                Some(ParamValue::Enum(v)) => *v,
                _ => 0,
            };
            let format = if color_space == 0 {
                manifold_gpu::GpuTextureFormat::Rgba8UnormSrgb
            } else {
                manifold_gpu::GpuTextureFormat::Rgba8Unorm
            };
            self.upload_source_texture(ctx, w, h, format, rgba8_sha256, &rgba);
            self.src_w = w;
            self.src_h = h;
            self.uploaded = true;
            self.published_content = false;
            fresh_upload = true;
        }

        // Read fresh every frame (like color_space at upload) rather than
        // cached in `last_key` — `mode` affects the per-frame blit
        // dispatch below, not the decode, so no re-decode is needed on
        // change. Read before `ctx.gpu_encoder()`/`ctx.outputs` borrow
        // `ctx` mutably below.
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(v)) => *v as f32,
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };

        // 5. A planned node-owned output is immutable and is created by this
        // primitive. Legacy/pre-bound callers deliberately take the writable
        // path below; those callers have no descriptor to query.
        if let Some(desc) = ctx.outputs.provided_texture_descriptor("out") {
            self.run_provided_output(ctx, desc, mode);
            return;
        }
        // A harvested/reused node can be evaluated by a host-prebound or
        // feedback path after previously owning a dedicated output. Drop the
        // shared bundle as soon as that ordinary contract is selected.
        self.converted_output = None;
        self.converted_key = None;
        self.awaiting_canonical = false;

        // 6. Output buffer.
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out.width, out.height);
        if w == 0 || h == 0 {
            return;
        }

        // 7. Nothing decoded yet (first-frame race, empty path, or a
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

        // 8+9. Level-0 blit + mip regen, gated together
        // (RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P1/R1): both rewrite
        // `out`'s content, so both are skipped together whenever nothing
        // that determines that content changed since we last wrote it —
        // the decoded pixels (`fresh_upload`), the repack mode (affects
        // the blit's output bytes directly), or `out`'s own physical
        // identity (pool recycle/resize hands back a different texture,
        // which must be re-blitted even if the source pixels and mode
        // didn't change — the `last_mip_identity` precedent this extends).
        let out_identity = out.identity_key();
        let content_unchanged = self.published_content
            && !fresh_upload
            && mode == self.last_blit_mode
            && (w, h) == self.last_blit_dims
            && Some(out.format) == self.last_blit_format;
        let unchanged =
            !fresh_upload && out_identity == self.last_mip_identity && mode == self.last_blit_mode;

        if unchanged && ctx.outputs_retained() {
            ctx.mark_outputs_unchanged();
        } else {
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
                mode,
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
                        texture: source_texture.as_ref(),
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

            // Regenerate the output's mip chain (IMPORT_FIDELITY F-P6).
            // Guarded on the chain actually existing — tests that
            // pre-bind a flat texture skip the pass cleanly.
            if out.mip_level_count() > 1 {
                gpu.native_enc.generate_mipmaps(out);
            }

            self.last_mip_identity = out_identity;
            self.last_blit_mode = mode;
            self.last_blit_dims = (w, h);
            self.last_blit_format = Some(out.format);
            if content_unchanged {
                ctx.mark_output_content_unchanged();
            }
            self.published_content = true;
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

    fn run_provided_output(
        &mut self,
        ctx: &mut EffectNodeContext<'_, '_>,
        desc: manifold_gpu::GpuTextureDesc<'static>,
        mode: f32,
    ) {
        let source_key = self.source_content_key;
        let device_scope_id = ctx.gpu_encoder().device.resource_scope_id();
        let key = ConvertedTextureKey {
            source: source_key,
            width: desc.width,
            height: desc.height,
            format: desc.format,
            mip_levels: desc.mip_levels,
            mode_bits: mode.to_bits(),
            device_scope_id,
        };

        // Keep using our own just-submitted bundle while its event is still
        // pending. A second source must not adopt it, but this source can
        // safely keep referring to it in the ordered command stream. Once a
        // duplicate's private bundle completes, let it re-enter the lookup
        // below so it can adopt the canonical bundle or promote itself if
        // the old canonical owner disappeared.
        let own_bundle = (self.converted_key == Some(key))
            .then_some(self.converted_output.as_ref())
            .flatten()
            .cloned();
        let previous_bundle = own_bundle.clone();
        let mut logical_same = self.converted_key == Some(key);
        // Always inspect the weak canonical entry before retaining our own
        // bundle. An earlier owner may have been unsubmitted while a peer
        // completed and promoted its duplicate; keeping our same-key bundle
        // forever would prevent convergence to that replacement.
        let ready_cached = CONVERTED_TEXTURE_CACHE.with(|cache| {
            cache
                .borrow()
                .get(&key)
                .and_then(Weak::upgrade)
                .filter(|bundle| bundle.ready.is_done(1))
        });
        let own_ready = own_bundle.as_ref().is_some_and(|bundle| bundle.ready.is_done(1));
        let bundle = if let Some(cached) = ready_cached {
            self.awaiting_canonical = false;
            cached
        } else if let Some(own_bundle) = own_bundle {
            if self.awaiting_canonical && own_ready {
                // No ready canonical survived. Promote this completed
                // duplicate so later runtimes converge on one bundle.
                CONVERTED_TEXTURE_CACHE.with(|cache| {
                    cache.borrow_mut().insert(key, Arc::downgrade(&own_bundle));
                });
                self.awaiting_canonical = false;
            }
            own_bundle
        } else {
            logical_same = false;
            let device = ctx.gpu_encoder().device;
            let created = Arc::new(ConvertedTextureBundle {
                texture: device.create_texture(&desc),
                ready: device.create_event(),
            });

            // A pending canonical entry remains canonical. This source's
            // duplicate is intentionally private until the next frame,
            // when the ready entry can be adopted without a wait.
            let keep_existing = CONVERTED_TEXTURE_CACHE.with(|cache| {
                let mut cache = cache.borrow_mut();
                cache.retain(|_, weak| weak.strong_count() != 0);
                let keep_existing = cache.get(&key).and_then(Weak::upgrade).is_some();
                if !keep_existing {
                    cache.insert(key, Arc::downgrade(&created));
                }
                keep_existing
            });
            self.awaiting_canonical = keep_existing;

            let source_texture = self.source_texture.as_ref();
            let gpu = ctx.gpu_encoder();
            if let Some(source_texture) = source_texture {
                let pipeline = self.pipeline.get_or_insert_with(|| {
                    gpu.device.create_compute_pipeline(
                        include_str!("shaders/gltf_texture_blit.wgsl"),
                        "cs_main",
                        "node.gltf_texture_source",
                    )
                });
                let sampler = self.sampler.get_or_insert_with(|| {
                    gpu.device.create_sampler(&GpuSamplerDesc::default())
                });
                let uniforms = GltfTextureBlitUniforms {
                    out_width: desc.width as f32,
                    out_height: desc.height as f32,
                    mode,
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
                            texture: source_texture.as_ref(),
                        },
                        GpuBinding::Sampler {
                            binding: 2,
                            sampler,
                        },
                        GpuBinding::Texture {
                            binding: 3,
                            texture: &created.texture,
                        },
                    ],
                    [desc.width.div_ceil(16), desc.height.div_ceil(16), 1],
                    "node.gltf_texture_source",
                );
            } else {
                // Loading, empty, and failed sources publish a real black
                // texture so no prior source content can leak through.
                gpu.clear_texture(&created.texture, 0.0, 0.0, 0.0, 1.0);
            }
            if desc.mip_levels > 1 {
                gpu.native_enc.generate_mipmaps(&created.texture);
            }
            // The event is the cache-adoption fence. It is signaled after
            // both level 0 and the mip chain are in the command stream.
            gpu.native_enc.signal_event_value(&created.ready, 1);
            created
        };

        let physical_same = previous_bundle
            .as_ref()
            .is_some_and(|old| old.texture.ptr_eq(&bundle.texture));
        self.converted_output = Some(bundle);
        self.converted_key = Some(key);

        if logical_same {
            if physical_same && ctx.outputs_retained() {
                ctx.mark_outputs_unchanged();
            } else {
                ctx.mark_output_content_unchanged();
            }
        }
    }

    fn upload_source_texture(
        &mut self,
        ctx: &mut EffectNodeContext<'_, '_>,
        w: u32,
        h: u32,
        format: manifold_gpu::GpuTextureFormat,
        rgba8_sha256: [u8; 32],
        rgba: &[u8],
    ) {
        let device = ctx.gpu_encoder().device;
        let key = SourceTextureKey {
            rgba8_sha256,
            width: w,
            height: h,
            format,
            device_scope_id: device.resource_scope_id(),
        };
        let cached = SOURCE_TEXTURE_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            cache.retain(|_, weak| weak.strong_count() != 0);
            cache.get(&key).and_then(Weak::upgrade)
        });

        let tex = if let Some(tex) = cached {
            tex
        } else {
            let tex = Arc::new(device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: w,
                height: h,
                depth: 1,
                format,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                    | manifold_gpu::GpuTextureUsage::CPU_UPLOAD,
                label: "node.gltf_texture_source source",
                mip_levels: 1,
            }));
            // CPU_UPLOAD uses synchronous Metal replaceRegion, so publishing
            // the weak cache entry after this call makes every shared source
            // fully initialized before another node can observe it.
            ctx.gpu_encoder()
                .native_enc
                .upload_texture(tex.as_ref(), w, h, 1, rgba);
            SOURCE_TEXTURE_CACHE.with(|cache| {
                cache.borrow_mut().insert(key, Arc::downgrade(&tex));
            });
            tex
        };
        self.source_content_key = Some(key);
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
            vec!["path", "texture_index", "color_space", "width", "height", "mode"]
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
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::{FrameTime, MetalBackend};
    use crate::node_graph::ports::PortType;
    use crate::render_target::RenderTarget;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    fn frame_time() -> FrameTime {
        FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(1.0 / 60.0), frame_count: 0 }
    }

    fn helmet_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf/DamagedHelmet.glb")
    }

    fn params_at(path: &str, texture_index: f32, mode: u32, w: f32, h: f32) -> ParamValues {
        let mut p = ahash::AHashMap::default();
        p.insert(Cow::Borrowed("path"), ParamValue::String(path.to_string().into()));
        p.insert(Cow::Borrowed("texture_index"), ParamValue::Float(texture_index));
        p.insert(Cow::Borrowed("color_space"), ParamValue::Enum(0));
        p.insert(Cow::Borrowed("width"), ParamValue::Float(w));
        p.insert(Cow::Borrowed("height"), ParamValue::Float(h));
        p.insert(Cow::Borrowed("mode"), ParamValue::Enum(mode));
        p
    }

    #[derive(Clone, Copy, Debug)]
    struct RunResult {
        storage_unchanged: bool,
        content_unchanged: bool,
    }

    /// Run one frame directly against a real GPU backend (no Graph/Executor
    /// needed — this Source primitive has zero inputs). Returns both the
    /// physical storage declaration and the logical content declaration.
    fn run_once(
        prim: &mut GltfTextureSource,
        backend: &MetalBackend,
        device: &manifold_gpu::GpuDevice,
        output_scratch: &[(&'static str, Slot)],
        params: &ParamValues,
        time: FrameTime,
    ) -> RunResult {
        run_once_with_retention(prim, backend, device, output_scratch, params, time, true)
    }

    fn run_once_with_retention(
        prim: &mut GltfTextureSource,
        backend: &MetalBackend,
        device: &manifold_gpu::GpuDevice,
        output_scratch: &[(&'static str, Slot)],
        params: &ParamValues,
        time: FrameTime,
        outputs_retained: bool,
    ) -> RunResult {
        let mut scalar_ws = Vec::new();
        let mut camera_ws = Vec::new();
        let mut light_ws = Vec::new();
        let mut material_ws = Vec::new();
        let mut transform_ws = Vec::new();
        let mut atmosphere_ws = Vec::new();
        let mut render_mode_ws = Vec::new();
        let mut object_ws = Vec::new();
        let backend_ref: &dyn Backend = backend;
        let inputs = NodeInputs::new(&[], backend_ref, &[]);
        let outputs = NodeOutputs::new(
            output_scratch,
            backend_ref,
            &mut scalar_ws,
            &mut camera_ws,
            &mut light_ws,
            &mut material_ws,
            &mut transform_ws,
            &mut atmosphere_ws,
            &mut render_mode_ws,
            &mut object_ws,
        );
        let mut native_enc = device.create_encoder("gltf-texture-source-test");
        let result;
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, device);
            let mut ctx = EffectNodeContext::new(time, params, inputs, outputs, Some(&mut gpu))
                .with_outputs_retained(outputs_retained);
            prim.run(&mut ctx);
            result = RunResult {
                storage_unchanged: ctx.outputs_unchanged,
                content_unchanged: ctx.output_content_unchanged,
            };
        }
        native_enc.commit_and_wait_completed();
        result
    }

    fn run_once_uncommitted(
        prim: &mut GltfTextureSource,
        backend: &MetalBackend,
        device: &manifold_gpu::GpuDevice,
        output_scratch: &[(&'static str, Slot)],
        params: &ParamValues,
    ) -> manifold_gpu::GpuEncoder {
        let mut scalar_ws = Vec::new();
        let mut camera_ws = Vec::new();
        let mut light_ws = Vec::new();
        let mut material_ws = Vec::new();
        let mut transform_ws = Vec::new();
        let mut atmosphere_ws = Vec::new();
        let mut render_mode_ws = Vec::new();
        let mut object_ws = Vec::new();
        let backend_ref: &dyn Backend = backend;
        let inputs = NodeInputs::new(&[], backend_ref, &[]);
        let outputs = NodeOutputs::new(
            output_scratch,
            backend_ref,
            &mut scalar_ws,
            &mut camera_ws,
            &mut light_ws,
            &mut material_ws,
            &mut transform_ws,
            &mut atmosphere_ws,
            &mut render_mode_ws,
            &mut object_ws,
        );
        let mut native_enc = device.create_encoder("gltf-texture-source-pending-test");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, device);
            let mut ctx = EffectNodeContext::new(frame_time(), params, inputs, outputs, Some(&mut gpu));
            prim.run(&mut ctx);
        }
        native_enc
    }

    fn provided_backend(
        device: &crate::TestDevice,
        w: u32,
        h: u32,
        mipmapped: bool,
    ) -> (MetalBackend, Slot) {
        let mut backend = MetalBackend::new(
            device.arc(),
            w,
            h,
            GpuTextureFormat::Rgba16Float,
        );
        let id = ResourceId(0);
        if mipmapped {
            backend.declare_mipmapped(&[id]);
        }
        let slot = backend.acquire_provided_texture(
            id,
            PortType::Texture2D,
            None,
            (w, h),
        );
        (backend, slot)
    }

    fn install_provided(prim: &GltfTextureSource, backend: &mut MetalBackend, slot: Slot) {
        let texture = prim
            .provided_texture_output("out")
            .expect("provided output published");
        backend.install_provided_texture(slot, texture);
    }

    fn readback_texture(
        device: &manifold_gpu::GpuDevice,
        backend: &dyn Backend,
        slot: Slot,
        w: u32,
        h: u32,
    ) -> Vec<u8> {
        let tex = backend.texture_2d(slot).expect("output texture retained");
        read_texture_bytes(device, tex, w, h)
    }

    fn read_texture_bytes(
        device: &manifold_gpu::GpuDevice, tex: &manifold_gpu::GpuTexture, w: u32, h: u32,
    ) -> Vec<u8> {
        let bytes_per_row = w * tex.format.bytes_per_pixel();
        let total = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("gltf-texture-source-provided-readback");
        enc.copy_texture_to_buffer(tex, &readback_buf, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        unsafe { std::slice::from_raw_parts(ptr, total as usize) }.to_vec()
    }

    fn overwrite_output(
        device: &manifold_gpu::GpuDevice,
        backend: &MetalBackend,
        slot: Slot,
    ) {
        let texture = backend.texture_2d(slot).expect("output texture retained");
        let mut enc = device.create_encoder("gltf-texture-source-tenant-overwrite");
        enc.clear_texture(texture, 1.0, 0.0, 1.0, 1.0);
        enc.commit_and_wait_completed();
    }

    fn readback(device: &manifold_gpu::GpuDevice, backend: &MetalBackend, slot: Slot, w: u32, h: u32) -> Vec<u8> {
        let tex = backend.texture_2d(slot).expect("texture retained");
        let bytes_per_row = w * 4; // Rgba8Unorm[Srgb]
        let total = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("gltf-texture-source-readback");
        enc.copy_texture_to_buffer(tex, &readback_buf, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        unsafe { std::slice::from_raw_parts(ptr, total as usize) }.to_vec()
    }

    /// Settle the async decode by re-running until it's no longer pending
    /// (bounded — a real fixture decode is milliseconds, not seconds).
    fn settle(
        prim: &mut GltfTextureSource,
        backend: &MetalBackend,
        device: &manifold_gpu::GpuDevice,
        output_scratch: &[(&'static str, Slot)],
        params: &ParamValues,
    ) {
        for _ in 0..200 {
            run_once(prim, backend, device, output_scratch, params, frame_time());
            if !prim.io_pending() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("gltf_texture_source: decode never settled");
    }

    fn synthetic_params(w: u32, h: u32, color_space: u32, mode: u32) -> ParamValues {
        let mut params = params_at("", 0.0, mode, w as f32, h as f32);
        params.insert(Cow::Borrowed("color_space"), ParamValue::Enum(color_space));
        params
    }

    fn inject_upload(prim: &mut GltfTextureSource, w: u32, h: u32, rgba: Vec<u8>) {
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        prim.last_key = (String::new(), 0);
        let rgba8_sha256: [u8; 32] = Sha256::digest(&rgba).into();
        prim.pending_upload = Some((w, h, rgba, rgba8_sha256));
    }

    #[test]
    fn provided_output_matches_writable_conversion_and_has_mips() {
        CONVERTED_TEXTURE_CACHE.with(|cache| cache.borrow_mut().clear());
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let rgba: Vec<u8> = (0..(w * h * 4)).map(|n| (n * 17) as u8).collect();
        let params = synthetic_params(w, h, 0, 0);

        let (mut provided_backend, provided_slot) = provided_backend(&device, w, h, true);
        let provided_scratch = vec![("out", provided_slot)];
        let mut provided = GltfTextureSource::new();
        inject_upload(&mut provided, w, h, rgba.clone());
        run_once(
            &mut provided,
            &provided_backend,
            &device,
            &provided_scratch,
            &params,
            frame_time(),
        );
        assert_eq!(
            provided
                .provided_texture_output("out")
                .expect("provided output")
                .mip_level_count(),
            manifold_gpu::GpuTextureDesc::max_mip_levels(w, h)
        );
        install_provided(&provided, &mut provided_backend, provided_slot);

        let mut writable_backend = MetalBackend::new(
            device.arc(),
            w,
            h,
            GpuTextureFormat::Rgba16Float,
        );
        let writable_slot = writable_backend.pre_bind_texture_2d(
            ResourceId(0),
            RenderTarget::new_mipmapped(
                &device,
                w,
                h,
                GpuTextureFormat::Rgba16Float,
                "gltf-texture-source-writable-parity",
            ),
        );
        let writable_scratch = vec![("out", writable_slot)];
        let mut writable = GltfTextureSource::new();
        inject_upload(&mut writable, w, h, rgba);
        run_once(
            &mut writable,
            &writable_backend,
            &device,
            &writable_scratch,
            &params,
            frame_time(),
        );
        assert_eq!(
            readback_texture(&device, &provided_backend, provided_slot, w, h),
            readback_texture(&device, &writable_backend, writable_slot, w, h),
            "immutable conversion must preserve the established blit output"
        );
        // Compare the actual mip pixels and repeat after a repack-mode edit.
        for mode in [0, 1] {
            let params = synthetic_params(w, h, 0, mode);
            run_once(&mut provided, &provided_backend, &device, &provided_scratch, &params, frame_time());
            run_once(&mut writable, &writable_backend, &device, &writable_scratch, &params, frame_time());
            install_provided(&provided, &mut provided_backend, provided_slot);
            for level in 0..3 {
                let side = w >> level;
                let actual = provided_backend.texture_2d(provided_slot).unwrap().mip_level_view(level, side, side);
                let expected = writable_backend.texture_2d(writable_slot).unwrap().mip_level_view(level, side, side);
                assert_eq!(read_texture_bytes(&device, &actual, side, side), read_texture_bytes(&device, &expected, side, side));
            }
        }
    }

    #[test]
    fn provided_cache_does_not_share_pending_work_then_converges() {
        CONVERTED_TEXTURE_CACHE.with(|cache| cache.borrow_mut().clear());
        let device = crate::test_device();
        let (w, h) = (2u32, 2u32);
        let rgba = vec![
            9, 31, 77, 255, 42, 80, 13, 255, 120, 4, 200, 255, 220, 90, 17, 255,
        ];
        let params = synthetic_params(w, h, 0, 0);
        let (mut backend_a, slot_a) = provided_backend(&device, w, h, false);
        let (mut backend_b, slot_b) = provided_backend(&device, w, h, false);
        let scratch_a = vec![("out", slot_a)];
        let scratch_b = vec![("out", slot_b)];
        let mut source_a = GltfTextureSource::new();
        let mut source_b = GltfTextureSource::new();
        inject_upload(&mut source_a, w, h, rgba.clone());
        inject_upload(&mut source_b, w, h, rgba);

        let encoder_a = run_once_uncommitted(
            &mut source_a,
            &backend_a,
            &device,
            &scratch_a,
            &params,
        );
        let encoder_b = run_once_uncommitted(
            &mut source_b,
            &backend_b,
            &device,
            &scratch_b,
            &params,
        );
        let output_a = source_a.provided_texture_output("out").unwrap().clone();
        let output_b = source_b.provided_texture_output("out").unwrap().clone();
        assert!(!output_a.ptr_eq(&output_b), "pending conversion must not be shared");
        assert!(source_b.awaiting_canonical);
        assert!(source_b.warmup_pending());

        // Submit B first: an unsubmitted original canonical must not prevent
        // B from finishing warmup or stop A from subsequently converging.
        encoder_b.commit_and_wait_completed();
        run_once(
            &mut source_b,
            &backend_b,
            &device,
            &scratch_b,
            &params,
            frame_time(),
        );
        assert!(!source_b.warmup_pending());
        encoder_a.commit_and_wait_completed();
        run_once(&mut source_a, &backend_a, &device, &scratch_a, &params, frame_time());
        let converged = source_a.provided_texture_output("out").unwrap();
        assert!(
            output_b.ptr_eq(converged),
            "the former canonical must adopt the ready replacement"
        );
        assert!(!source_b.warmup_pending());
        install_provided(&source_a, &mut backend_a, slot_a);
        install_provided(&source_b, &mut backend_b, slot_b);
    }

    #[test]
    fn provided_cache_isolates_mode_dimensions_and_opaque_black() {
        CONVERTED_TEXTURE_CACHE.with(|cache| cache.borrow_mut().clear());
        let device = crate::test_device();
        let rgba = vec![
            13, 37, 91, 255, 61, 122, 9, 255, 190, 4, 70, 255, 240, 100, 3, 255,
        ];
        let (mut backend_a, slot_a) = provided_backend(&device, 2, 2, false);
        let scratch_a = vec![("out", slot_a)];
        let mut source_a = GltfTextureSource::new();
        inject_upload(&mut source_a, 2, 2, rgba.clone());
        run_once(
            &mut source_a,
            &backend_a,
            &device,
            &scratch_a,
            &synthetic_params(2, 2, 0, 0),
            frame_time(),
        );
        install_provided(&source_a, &mut backend_a, slot_a);

        let (mut backend_mode, slot_mode) = provided_backend(&device, 2, 2, false);
        let scratch_mode = vec![("out", slot_mode)];
        let mut source_mode = GltfTextureSource::new();
        inject_upload(&mut source_mode, 2, 2, rgba.clone());
        run_once(
            &mut source_mode,
            &backend_mode,
            &device,
            &scratch_mode,
            &synthetic_params(2, 2, 0, 1),
            frame_time(),
        );
        assert!(!source_a
            .provided_texture_output("out")
            .unwrap()
            .ptr_eq(source_mode.provided_texture_output("out").unwrap()));

        let (mut backend_dims, slot_dims) = provided_backend(&device, 4, 1, false);
        let scratch_dims = vec![("out", slot_dims)];
        let mut source_dims = GltfTextureSource::new();
        inject_upload(&mut source_dims, 2, 2, rgba.clone());
        run_once(
            &mut source_dims,
            &backend_dims,
            &device,
            &scratch_dims,
            &synthetic_params(4, 1, 0, 0),
            frame_time(),
        );
        assert!(!source_a
            .provided_texture_output("out")
            .unwrap()
            .ptr_eq(source_dims.provided_texture_output("out").unwrap()));

        let (mut backend_black, slot_black) = provided_backend(&device, 2, 2, false);
        let scratch_black = vec![("out", slot_black)];
        let mut source_black = GltfTextureSource::new();
        run_once(
            &mut source_black,
            &backend_black,
            &device,
            &scratch_black,
            &synthetic_params(2, 2, 0, 0),
            frame_time(),
        );
        assert!(source_black.source_texture.is_none());
        assert!(!source_a
            .provided_texture_output("out")
            .unwrap()
            .ptr_eq(source_black.provided_texture_output("out").unwrap()));
        assert!(source_black.provided_texture_output("out").is_some());
        install_provided(&source_mode, &mut backend_mode, slot_mode);
        install_provided(&source_dims, &mut backend_dims, slot_dims);
        install_provided(&source_black, &mut backend_black, slot_black);
        let expected_black: Vec<u8> = [0u16, 0, 0, half::f16::ONE.to_bits()]
            .into_iter().flat_map(u16::to_ne_bytes).cycle().take(2 * 2 * 8).collect();
        assert_eq!(readback_texture(&device, &backend_black, slot_black, 2, 2), expected_black);
    }

    #[test]
    fn provided_outputs_share_through_executors_and_layer_edits_preserve_peer_pixels() {
        use crate::node_graph::{Graph, Executor, compile};
        use crate::node_graph::boundary_nodes::FinalOutput;
        CONVERTED_TEXTURE_CACHE.with(|cache| cache.borrow_mut().clear());
        let device = crate::test_device();
        let pixels = vec![40, 80, 160, 64, 90, 50, 20, 128, 200, 30, 10, 200, 7, 110, 220, 255];
        let build = || {
            let mut source = GltfTextureSource::new();
            inject_upload(&mut source, 2, 2, pixels.clone());
            let mut graph = Graph::new();
            let source = graph.add_node(Box::new(source));
            graph.set_param(source, "width", ParamValue::Float(2.0)).unwrap();
            graph.set_param(source, "height", ParamValue::Float(2.0)).unwrap();
            let output = graph.add_node(Box::new(FinalOutput::new()));
            graph.connect((source, "out"), (output, "in")).unwrap();
            let plan = compile(&graph).unwrap();
            let resource = plan.steps().iter().find(|step| step.node == source).unwrap().outputs[0].1;
            let backend = MetalBackend::new(device.arc(), 2, 2, GpuTextureFormat::Rgba16Float);
            (graph, plan, Executor::new(Box::new(backend)), source, resource)
        };
        let (mut a, plan_a, mut exec_a, source_a, resource_a) = build();
        let (mut b, plan_b, mut exec_b, _, resource_b) = build();
        let run = |graph: &mut Graph, plan: &crate::node_graph::ExecutionPlan, exec: &mut Executor| {
            let mut native = device.create_encoder("provided-output-executor");
            let mut gpu = RendererGpuEncoder::new(&mut native, &device);
            exec.execute_frame_with_gpu(graph, plan, frame_time(), &mut gpu);
            native.commit_and_wait_completed();
        };
        run(&mut a, &plan_a, &mut exec_a);
        run(&mut b, &plan_b, &mut exec_b);
        let slot_a = exec_a.backend().slot_for(resource_a).unwrap();
        let slot_b = exec_b.backend().slot_for(resource_b).unwrap();
        let original = exec_b.backend().texture_2d(slot_b).unwrap().clone();
        assert!(exec_a.backend().texture_2d(slot_a).unwrap().ptr_eq(&original));
        let peer_before = readback_texture(&device, exec_b.backend(), slot_b, 2, 2);
        // Repeated frames exercise retained-output reporting and held slots.
        run(&mut a, &plan_a, &mut exec_a);
        a.set_param(source_a, "mode", ParamValue::Enum(1)).unwrap();
        run(&mut a, &plan_a, &mut exec_a);
        run(&mut b, &plan_b, &mut exec_b);
        assert!(!exec_a.backend().texture_2d(slot_a).unwrap().ptr_eq(&original));
        assert!(exec_b.backend().texture_2d(slot_b).unwrap().ptr_eq(&original));
        assert_eq!(readback_texture(&device, exec_b.backend(), slot_b, 2, 2), peer_before);
        assert_ne!(readback_texture(&device, exec_a.backend(), slot_a, 2, 2), peer_before);
        let key = CONVERTED_TEXTURE_CACHE.with(|cache| *cache.borrow().iter().find(|(_, value)| {
            value.upgrade().is_some_and(|bundle| bundle.texture.ptr_eq(&original))
        }).unwrap().0);
        drop(b);
        drop(exec_b);
        CONVERTED_TEXTURE_CACHE.with(|cache| assert!(cache.borrow().get(&key).unwrap().upgrade().is_none()));
    }

    fn cache_key(
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
        format: GpuTextureFormat,
        rgba: &[u8],
    ) -> SourceTextureKey {
        SourceTextureKey {
            rgba8_sha256: Sha256::digest(rgba).into(),
            width: w,
            height: h,
            format,
            device_scope_id: device.resource_scope_id(),
        }
    }

    #[test]
    fn identical_synthetic_sources_share_immutable_input_and_independent_outputs() {
        let device = crate::test_device();
        let (w, h) = (2u32, 2u32);
        let format = GpuTextureFormat::Rgba8UnormSrgb;
        let params = synthetic_params(w, h, 0, 0);
        let rgba = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
        ];

        let mut backend_a = MetalBackend::new(device.arc(), w, h, format);
        let slot_a = backend_a.pre_bind_texture_2d(
            ResourceId(0),
            RenderTarget::new(&device, w, h, format, "gltf-source-sharing-a"),
        );
        let scratch_a = vec![("out", slot_a)];
        let mut source_a = GltfTextureSource::new();
        inject_upload(&mut source_a, w, h, rgba.clone());
        run_once(
            &mut source_a,
            &backend_a,
            &device,
            &scratch_a,
            &params,
            frame_time(),
        );
        let output_a_before = readback(&device, &backend_a, slot_a, w, h);

        let mut backend_b = MetalBackend::new(device.arc(), w, h, format);
        let slot_b = backend_b.pre_bind_texture_2d(
            ResourceId(0),
            RenderTarget::new(&device, w, h, format, "gltf-source-sharing-b"),
        );
        let scratch_b = vec![("out", slot_b)];
        let mut source_b = GltfTextureSource::new();
        inject_upload(&mut source_b, w, h, rgba.clone());
        run_once(
            &mut source_b,
            &backend_b,
            &device,
            &scratch_b,
            &params,
            frame_time(),
        );
        let output_b_before = readback(&device, &backend_b, slot_b, w, h);

        let texture_a = source_a.source_texture.as_ref().expect("source A uploaded");
        let texture_b = source_b.source_texture.as_ref().expect("source B uploaded");
        assert!(Arc::ptr_eq(texture_a, texture_b));
        assert!(texture_a.ptr_eq(texture_b));
        assert_eq!(output_a_before, output_b_before);

        let replacement = vec![
            0, 0, 0, 255, 32, 32, 32, 255, 64, 64, 64, 255, 96, 96, 96, 255,
        ];
        inject_upload(&mut source_a, w, h, replacement);
        run_once(
            &mut source_a,
            &backend_a,
            &device,
            &scratch_a,
            &params,
            frame_time(),
        );
        let output_a_after = readback(&device, &backend_a, slot_a, w, h);
        overwrite_output(&device, &backend_b, slot_b);
        run_once_with_retention(
            &mut source_b, &backend_b, &device, &scratch_b, &params, frame_time(), false,
        );
        let output_b_after = readback(&device, &backend_b, slot_b, w, h);
        assert_ne!(output_a_before, output_a_after);
        assert_eq!(output_b_before, output_b_after);
        assert!(!source_a
            .source_texture
            .as_ref()
            .expect("source A replacement uploaded")
            .ptr_eq(source_b.source_texture.as_ref().expect("source B retained")));
    }

    #[test]
    fn source_cache_separates_shape_format_and_device_scope_and_expires_weak_entries() {
        let device = crate::test_device();
        let (w, h) = (2u32, 2u32);
        let format = GpuTextureFormat::Rgba8UnormSrgb;
        let rgba = vec![
            17, 34, 51, 255, 68, 85, 102, 255, 119, 136, 153, 255, 170, 187, 204, 255,
        ];
        let params = synthetic_params(w, h, 0, 0);

        let mut backend_shape = MetalBackend::new(device.arc(), w, h, format);
        let slot_shape = backend_shape.pre_bind_texture_2d(
            ResourceId(0),
            RenderTarget::new(&device, w, h, format, "gltf-source-key-shape"),
        );
        let scratch_shape = vec![("out", slot_shape)];
        let mut source_shape = GltfTextureSource::new();
        inject_upload(&mut source_shape, w, h, rgba.clone());
        run_once(
            &mut source_shape,
            &backend_shape,
            &device,
            &scratch_shape,
            &params,
            frame_time(),
        );
        let shape_key = cache_key(&device, w, h, format, &rgba);
        let shape_weak = SOURCE_TEXTURE_CACHE.with(|cache| {
            cache
                .borrow()
                .get(&shape_key)
                .cloned()
                .expect("shape source cache entry")
        });

        let mut source_shape_b = GltfTextureSource::new();
        inject_upload(&mut source_shape_b, w, h, rgba.clone());
        run_once(
            &mut source_shape_b,
            &backend_shape,
            &device,
            &scratch_shape,
            &params,
            frame_time(),
        );
        assert!(Arc::ptr_eq(
            source_shape.source_texture.as_ref().unwrap(),
            source_shape_b.source_texture.as_ref().unwrap(),
        ));

        let mut backend_format = MetalBackend::new(device.arc(), w, h, format);
        let slot_format = backend_format.pre_bind_texture_2d(
            ResourceId(0),
            RenderTarget::new(&device, w, h, format, "gltf-source-key-format"),
        );
        let scratch_format = vec![("out", slot_format)];
        let mut source_format = GltfTextureSource::new();
        let linear_params = synthetic_params(w, h, 1, 0);
        inject_upload(&mut source_format, w, h, rgba.clone());
        run_once(
            &mut source_format,
            &backend_format,
            &device,
            &scratch_format,
            &linear_params,
            frame_time(),
        );
        assert!(!source_shape
            .source_texture
            .as_ref()
            .unwrap()
            .ptr_eq(source_format.source_texture.as_ref().unwrap()));
        let format_key = cache_key(
            &device,
            w,
            h,
            GpuTextureFormat::Rgba8Unorm,
            &rgba,
        );

        let second_device = Arc::new(manifold_gpu::GpuDevice::new());
        let mut backend_scope = MetalBackend::new(Arc::clone(&second_device), w, h, format);
        let slot_scope = backend_scope.pre_bind_texture_2d(
            ResourceId(0),
            RenderTarget::new(second_device.as_ref(), w, h, format, "gltf-source-key-scope"),
        );
        let scratch_scope = vec![("out", slot_scope)];
        let mut source_scope = GltfTextureSource::new();
        inject_upload(&mut source_scope, w, h, rgba.clone());
        run_once(
            &mut source_scope,
            &backend_scope,
            second_device.as_ref(),
            &scratch_scope,
            &params,
            frame_time(),
        );
        assert_ne!(
            device.resource_scope_id(),
            second_device.resource_scope_id()
        );
        assert!(!source_shape
            .source_texture
            .as_ref()
            .unwrap()
            .ptr_eq(source_scope.source_texture.as_ref().unwrap()));
        let scope_key = cache_key(
            second_device.as_ref(),
            w,
            h,
            format,
            &rgba,
        );

        let shape_4x1 = (4u32, 1u32);
        let mut backend_dimensions = MetalBackend::new(device.arc(), shape_4x1.0, shape_4x1.1, format);
        let slot_dimensions = backend_dimensions.pre_bind_texture_2d(
            ResourceId(0),
            RenderTarget::new(
                &device,
                shape_4x1.0,
                shape_4x1.1,
                format,
                "gltf-source-key-dimensions",
            ),
        );
        let scratch_dimensions = vec![("out", slot_dimensions)];
        let mut source_dimensions = GltfTextureSource::new();
        inject_upload(&mut source_dimensions, shape_4x1.0, shape_4x1.1, rgba.clone());
        let dimensions_params = synthetic_params(shape_4x1.0, shape_4x1.1, 0, 0);
        run_once(
            &mut source_dimensions,
            &backend_dimensions,
            &device,
            &scratch_dimensions,
            &dimensions_params,
            frame_time(),
        );
        assert!(!source_shape
            .source_texture
            .as_ref()
            .unwrap()
            .ptr_eq(source_dimensions.source_texture.as_ref().unwrap()));

        drop(source_shape);
        drop(source_shape_b);
        drop(source_format);
        drop(source_scope);
        assert!(shape_weak.upgrade().is_none());

        let mut backend_prune = MetalBackend::new(device.arc(), shape_4x1.0, shape_4x1.1, format);
        let slot_prune = backend_prune.pre_bind_texture_2d(
            ResourceId(0),
            RenderTarget::new(
                &device,
                shape_4x1.0,
                shape_4x1.1,
                format,
                "gltf-source-key-prune",
            ),
        );
        let scratch_prune = vec![("out", slot_prune)];
        let mut source_prune = GltfTextureSource::new();
        inject_upload(&mut source_prune, shape_4x1.0, shape_4x1.1, rgba.clone());
        run_once(
            &mut source_prune,
            &backend_prune,
            &device,
            &scratch_prune,
            &dimensions_params,
            frame_time(),
        );
        SOURCE_TEXTURE_CACHE.with(|cache| {
            let cache = cache.borrow();
            assert!(!cache.contains_key(&shape_key));
            assert!(!cache.contains_key(&format_key));
            assert!(!cache.contains_key(&scope_key));
            assert!(cache
                .get(&cache_key(&device, shape_4x1.0, shape_4x1.1, format, &rgba))
                .and_then(Weak::upgrade)
                .is_some());
        });
    }

    /// Seam-split for the kuma robot's black-body report: its baseColor is
    /// a 4096x4096 embedded JPEG (the conformance corpus tops out at
    /// 2048). The decoded bytes are proven bright CPU-side
    /// (`gltf_load::tests::robot_basecolor_jpeg_decodes_bright`), so the
    /// blit+mip path must emit a non-black 1024² output. If this fails,
    /// the 4K-source resample is the bug; if it passes, the black enters
    /// downstream (scene_object/pbr sampling).
    #[test]
    fn four_k_jpeg_source_blits_non_black() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/kuma_heavy_robot_r-9000s.glb");
        if !path.exists() {
            println!("four_k_jpeg_source_blits_non_black: fixture not found, skipping");
            return;
        }
        let device = crate::test_device();
        let (w, h) = (1024u32, 1024u32);
        let format = GpuTextureFormat::Rgba8UnormSrgb;
        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        let r_out = ResourceId(0);
        let target = RenderTarget::new(&device, w, h, format, "gltf-texture-source-4k-out");
        let out_slot = backend.pre_bind_texture_2d(r_out, target);
        let scratch: Vec<(&'static str, Slot)> = vec![("out", out_slot)];

        let params = params_at(path.to_str().unwrap(), 0.0, 0, w as f32, h as f32);
        let mut prim = GltfTextureSource::new();
        settle(&mut prim, &backend, &device, &scratch, &params);
        let frame = readback(&device, &backend, out_slot, w, h);
        let lit = frame
            .chunks_exact(4)
            .filter(|px| px[0] > 8 || px[1] > 8 || px[2] > 8)
            .count();
        assert!(
            lit > (w * h) as usize / 2,
            "the bright KUMA atlas must survive decode->upload->blit: only {lit}/{} lit texels",
            w * h
        );
    }

    /// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P1/R1 gate: on a static
    /// asset, frame 2's output is bit-identical to frame 1's, and the
    /// blit+mip skip (`mark_outputs_unchanged`) fires on frame 2.
    #[test]
    fn frame2_matches_frame1_on_static_asset_and_declares_unchanged() {
        let path = helmet_fixture_path();
        if !path.exists() {
            println!("frame2_matches_frame1_on_static_asset_and_declares_unchanged: fixture not found at {}, skipping", path.display());
            return;
        }
        let device = crate::test_device();
        let (w, h) = (64u32, 64u32);
        let format = GpuTextureFormat::Rgba8UnormSrgb;
        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        let r_out = ResourceId(0);
        let target = RenderTarget::new(&device, w, h, format, "gltf-texture-source-out");
        let out_slot = backend.pre_bind_texture_2d(r_out, target);
        let output_scratch: Vec<(&'static str, Slot)> = vec![("out", out_slot)];

        let params = params_at(path.to_str().unwrap(), 0.0, 0, w as f32, h as f32);
        let mut prim = GltfTextureSource::new();
        settle(&mut prim, &backend, &device, &output_scratch, &params);
        let frame1 = readback(&device, &backend, out_slot, w, h);

        let result = run_once(&mut prim, &backend, &device, &output_scratch, &params, frame_time());
        assert!(result.storage_unchanged, "settled static frame must declare mark_outputs_unchanged");
        assert!(result.content_unchanged, "a physical no-op also preserves logical content");
        let frame2 = readback(&device, &backend, out_slot, w, h);
        assert_eq!(frame1, frame2, "frame 2 must be bit-identical to frame 1 on a static asset");
    }

    /// A recycled destination has a different physical identity, so the
    /// source must copy the cached pixels again while retaining the logical
    /// content version. Alternating the same slot between two targets models
    /// the executor's physical output recycling without adding a graph
    /// harness to this source proof.
    #[test]
    fn recycled_output_is_rewritten_with_stable_content() {
        let path = helmet_fixture_path();
        assert!(path.exists(), "required glTF fixture missing: {}", path.display());
        let device = crate::test_device();
        let (w, h) = (64u32, 64u32);
        let format = GpuTextureFormat::Rgba8UnormSrgb;
        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        let out_slot = backend.allocate_slot(RenderTarget::new(
            &device,
            w,
            h,
            format,
            "gltf-texture-source-recycle-a",
        ));
        let output_scratch: Vec<(&'static str, Slot)> = vec![("out", out_slot)];
        let params = params_at(path.to_str().unwrap(), 0.0, 0, w as f32, h as f32);
        let mut prim = GltfTextureSource::new();
        settle(&mut prim, &backend, &device, &output_scratch, &params);
        let expected = readback(&device, &backend, out_slot, w, h);

        let old_target = backend
            .swap_texture_2d(
                out_slot,
                RenderTarget::new(
                    &device,
                    w,
                    h,
                    format,
                    "gltf-texture-source-recycle-b",
                ),
            )
            .expect("first output target retained");
        let recycled = run_once_with_retention(
            &mut prim,
            &backend,
            &device,
            &output_scratch,
            &params,
            frame_time(),
            false,
        );
        assert!(!recycled.storage_unchanged, "a recycled destination must be physically rewritten");
        assert!(recycled.content_unchanged, "recopying identical pixels must preserve logical content");
        assert_eq!(expected, readback(&device, &backend, out_slot, w, h));

        let recycled_target = backend
            .swap_texture_2d(out_slot, old_target)
            .expect("second output target retained");
        let recycled_again = run_once_with_retention(
            &mut prim,
            &backend,
            &device,
            &output_scratch,
            &params,
            frame_time(),
            false,
        );
        assert!(!recycled_again.storage_unchanged, "returning to a prior destination still requires a physical rewrite");
        assert!(recycled_again.content_unchanged, "alternating destinations must retain logical content");
        assert_eq!(expected, readback(&device, &backend, out_slot, w, h));
        drop(recycled_target);
    }

    /// Regression for the ownership hole in an identity-only physical gate:
    /// another tenant can overwrite a texture in place before it returns to
    /// this source. The source must detect that write and restore its cached
    /// pixels even though the allocation identity is unchanged.
    #[test]
    fn overwritten_same_output_identity_forces_refresh() {
        let path = helmet_fixture_path();
        assert!(path.exists(), "required glTF fixture missing: {}", path.display());
        let device = crate::test_device();
        let (w, h) = (64u32, 64u32);
        let format = GpuTextureFormat::Rgba8UnormSrgb;
        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        let out_slot = backend.allocate_slot(RenderTarget::new(
            &device,
            w,
            h,
            format,
            "gltf-texture-source-overwrite",
        ));
        let output_scratch: Vec<(&'static str, Slot)> = vec![("out", out_slot)];
        let params = params_at(path.to_str().unwrap(), 0.0, 0, w as f32, h as f32);
        let mut prim = GltfTextureSource::new();
        settle(&mut prim, &backend, &device, &output_scratch, &params);
        let expected = readback(&device, &backend, out_slot, w, h);

        overwrite_output(&device, &backend, out_slot);
        let result = run_once_with_retention(
            &mut prim,
            &backend,
            &device,
            &output_scratch,
            &params,
            frame_time(),
            false,
        );
        assert!(!result.storage_unchanged, "an in-place overwrite by another tenant invalidates the physical no-op declaration");
        assert!(result.content_unchanged, "restoring identical pixels must preserve logical content");
        assert_eq!(expected, readback(&device, &backend, out_slot, w, h));
    }

    /// A param change (mode flip) must NOT be skipped, and must produce the
    /// same output a FRESH executor baked with that param from the start
    /// would produce.
    #[test]
    fn mode_flip_matches_fresh_executor() {
        let path = helmet_fixture_path();
        if !path.exists() {
            println!("mode_flip_matches_fresh_executor: fixture not found at {}, skipping", path.display());
            return;
        }
        let device = crate::test_device();
        let (w, h) = (64u32, 64u32);
        let format = GpuTextureFormat::Rgba8UnormSrgb;

        // Existing executor: settle at mode=passthrough(0), then flip to
        // mode=gloss_to_roughness(1).
        let mut backend_a = MetalBackend::new(device.arc(), w, h, format);
        let r_out = ResourceId(0);
        let target_a = RenderTarget::new(&device, w, h, format, "gltf-texture-source-a");
        let slot_a = backend_a.pre_bind_texture_2d(r_out, target_a);
        let scratch_a: Vec<(&'static str, Slot)> = vec![("out", slot_a)];
        let params_pass = params_at(path.to_str().unwrap(), 0.0, 0, w as f32, h as f32);
        let mut prim_a = GltfTextureSource::new();
        settle(&mut prim_a, &backend_a, &device, &scratch_a, &params_pass);
        let pass_output = readback(&device, &backend_a, slot_a, w, h);

        let params_flipped = params_at(path.to_str().unwrap(), 0.0, 1, w as f32, h as f32);
        let result = run_once(&mut prim_a, &backend_a, &device, &scratch_a, &params_flipped, frame_time());
        assert!(!result.storage_unchanged, "a mode flip must NOT be gated as unchanged");
        let flipped_output = readback(&device, &backend_a, slot_a, w, h);
        assert_ne!(flipped_output, pass_output, "mode change must alter the emitted pixels");

        // Fresh executor: mode=gloss_to_roughness baked in from the start.
        let mut backend_b = MetalBackend::new(device.arc(), w, h, format);
        let target_b = RenderTarget::new(&device, w, h, format, "gltf-texture-source-b");
        let slot_b = backend_b.pre_bind_texture_2d(r_out, target_b);
        let scratch_b: Vec<(&'static str, Slot)> = vec![("out", slot_b)];
        let mut prim_b = GltfTextureSource::new();
        settle(&mut prim_b, &backend_b, &device, &scratch_b, &params_flipped);
        assert!(Arc::ptr_eq(
            prim_a.source_texture.as_ref().expect("source A uploaded"),
            prim_b.source_texture.as_ref().expect("source B uploaded"),
        ));
        let fresh_output = readback(&device, &backend_b, slot_b, w, h);

        assert_eq!(
            flipped_output, fresh_output,
            "mode flip on a live gated executor must match a fresh executor built with that mode"
        );
    }

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
