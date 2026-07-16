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

        // 7+8. Level-0 blit + mip regen, gated together
        // (RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P1/R1): both rewrite
        // `out`'s content, so both are skipped together whenever nothing
        // that determines that content changed since we last wrote it —
        // the decoded pixels (`fresh_upload`), the repack mode (affects
        // the blit's output bytes directly), or `out`'s own physical
        // identity (pool recycle/resize hands back a different texture,
        // which must be re-blitted even if the source pixels and mode
        // didn't change — the `last_mip_identity` precedent this extends).
        let out_identity = out.identity_key();
        let unchanged =
            !fresh_upload && out_identity == self.last_mip_identity && mode == self.last_blit_mode;

        if unchanged {
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

            // Regenerate the output's mip chain (IMPORT_FIDELITY F-P6).
            // Guarded on the chain actually existing — tests that
            // pre-bind a flat texture skip the pass cleanly.
            if out.mip_level_count() > 1 {
                gpu.native_enc.generate_mipmaps(out);
            }

            self.last_mip_identity = out_identity;
            self.last_blit_mode = mode;
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

    /// Run one frame directly against a real GPU backend (no Graph/Executor
    /// needed — this Source primitive has zero inputs). Returns whether
    /// `mark_outputs_unchanged` was declared this frame.
    fn run_once(
        prim: &mut GltfTextureSource,
        backend: &MetalBackend,
        device: &manifold_gpu::GpuDevice,
        output_scratch: &[(&'static str, Slot)],
        params: &ParamValues,
        time: FrameTime,
    ) -> bool {
        let mut scalar_ws = Vec::new();
        let mut camera_ws = Vec::new();
        let mut light_ws = Vec::new();
        let mut material_ws = Vec::new();
        let mut transform_ws = Vec::new();
        let mut atmosphere_ws = Vec::new();
        let backend_ref: &dyn Backend = backend;
        let inputs = NodeInputs::new(&[], backend_ref);
        let outputs = NodeOutputs::new(
            output_scratch,
            backend_ref,
            &mut scalar_ws,
            &mut camera_ws,
            &mut light_ws,
            &mut material_ws,
            &mut transform_ws,
            &mut atmosphere_ws,
        );
        let mut native_enc = device.create_encoder("gltf-texture-source-test");
        let unchanged;
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, device);
            let mut ctx = EffectNodeContext::new(time, params, inputs, outputs, Some(&mut gpu));
            prim.run(&mut ctx);
            unchanged = ctx.outputs_unchanged;
        }
        native_enc.commit_and_wait_completed();
        unchanged
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

        let unchanged = run_once(&mut prim, &backend, &device, &output_scratch, &params, frame_time());
        assert!(unchanged, "settled static frame must declare mark_outputs_unchanged");
        let frame2 = readback(&device, &backend, out_slot, w, h);
        assert_eq!(frame1, frame2, "frame 2 must be bit-identical to frame 1 on a static asset");
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

        let params_flipped = params_at(path.to_str().unwrap(), 0.0, 1, w as f32, h as f32);
        let unchanged = run_once(&mut prim_a, &backend_a, &device, &scratch_a, &params_flipped, frame_time());
        assert!(!unchanged, "a mode flip must NOT be gated as unchanged");
        let flipped_output = readback(&device, &backend_a, slot_a, w, h);

        // Fresh executor: mode=gloss_to_roughness baked in from the start.
        let mut backend_b = MetalBackend::new(device.arc(), w, h, format);
        let target_b = RenderTarget::new(&device, w, h, format, "gltf-texture-source-b");
        let slot_b = backend_b.pre_bind_texture_2d(r_out, target_b);
        let scratch_b: Vec<(&'static str, Slot)> = vec![("out", slot_b)];
        let mut prim_b = GltfTextureSource::new();
        settle(&mut prim_b, &backend_b, &device, &scratch_b, &params_flipped);
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
