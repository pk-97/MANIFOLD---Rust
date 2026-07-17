//! `node.image_folder` — scrub through a folder of images by position.
//!
//! Outer-card use: the host wires a `folder` String param (typically
//! coming through a generator-level `stringBindings` entry whose
//! outer-card source is the `Browse` field on the clip inspector). The
//! primitive scans that directory for supported image files, sorts them
//! alphabetically, and presents them as a 1-D scrubbable strip via the
//! `position` scalar (0..1).
//!
//! File I/O happens on a background thread (`std::thread::spawn` + an
//! `mpsc::channel`) so the 60 FPS content thread never stalls on an
//! image decode. At most one load is in flight at a time; if the user
//! drags `position` fast enough that the channel hasn't returned the
//! previous slice yet, intermediate positions get skipped. The current
//! slice texture stays on screen until the next load lands — no black
//! frames during scrubs.
//!
//! Supported formats: PNG, JPEG, WebP, BMP, GIF (via the `image` crate)
//! plus TIFF. TIFF keeps its own dedicated path (`load_tiff_slice`,
//! lifted from the legacy `mri_volume_loader`) so the bit representation
//! of a u8 / u16 / f32 grayscale MRI scan stays identical — the `image`
//! crate can't decode f32-grayscale TIFFs and would clamp the others.
//! `load_image` dispatches by extension.
//!
//! Aspect-fit + uv_scale zoom are built in — same math as the legacy
//! `mri_slice_compute.wgsl`. Downstream window/level / sharpen / invert
//! live in their own primitives so this one stays a generic image
//! player.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ImageFolderUniforms {
    aspect_ratio: f32,
    uv_scale: f32,
    tex_width: f32,
    tex_height: f32,
}

/// Decoded image data ready for GPU upload. RGBA8 always so the
/// downstream sampler / shader sees a consistent format regardless of
/// source bit depth.
pub struct DecodedSlice {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

crate::primitive! {
    name: ImageFolder,
    type_id: "node.image_folder",
    purpose: "Scrub through a folder of images via a position scalar (0..1). The host sets the folder path on the outer-card String binding; the primitive scans it, sorts alphabetically, and loads slices on demand in a background thread. Built-in aspect-fit + uv_scale matches the legacy MRI volume display so downstream primitives don't have to reinvent it.",
    inputs: {
        position: ScalarF32 optional,
        uv_scale: ScalarF32 optional,
        next: ScalarF32 optional,
        prev: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
        trigger_count: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("folder"),
            label: "Folder",
            ty: ParamType::String,
            default: ParamValue::Float(0.0), // String default supplied via stringBindings; this slot is never read.
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("position"),
            label: "Position",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("uv_scale"),
            label: "Zoom",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.1, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("next"),
            label: "Next",
            ty: ParamType::Trigger,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("prev"),
            label: "Prev",
            ty: ParamType::Trigger,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    // depth_rule: zero-input IO bridge scrubbing externally-authored images off disk — same reasoning as gltf_texture_source/hdri_source
    depth_rule: Inherit,
    composition_notes: "Folder path comes via presetMetadata.stringBindings — wire the JSON-graph generator's outer-card text field into this primitive's `folder` param. Position is port-shadowed for LFO-driven scrubbing (the MRI scan slice sweep that drives the show). Output is rgba16float; color sources (png/jpg/webp/bmp/gif) decode straight to RGBA; grayscale TIFF sources are broadcast to R=G=B with A=1 so the texture is uniformly RGBA downstream. Use node.smoothstep downstream for window/level remapping and node.custom_convolution for sharpening. The `trigger_count` scalar output is a monotonically increasing counter that bumps each time a NEW slice actually lands on the GPU (not per position-slider movement — fast scrubs that outrun the background loader skip intermediate frames and only tick once per visible swap). Wire it into node.trigger_gate / node.sample_and_hold / node.clip_trigger_index downstream to drive per-image randomization. Fires once at startup when the first slice loads; never resets on folder change (downstream gates compare deltas, monotonic matches the system.generator_input.trigger_count convention). The `next` and `prev` scalar inputs are trigger_count-style monotonic counters: each rising edge bumps the displayed image by ±1 (clamped to [0, N-1]). Wire MIDI buttons, keyboard shortcuts, clip retriggers — anything that already emits a monotonic trigger_count. Last-input-wins between slider and buttons: a button press holds its position until the user moves the slider, at which point the slider takes over again.",
    examples: [],
    picker: { label: "Image Folder", category: Atom },
    summary: "Plays through a folder of images with a single position knob, so you can scrub or sequence stills. Point it at a folder and drive the position.",
    category: Generate,
    role: Source,
    aliases: ["image folder", "image sequence", "stills", "Movie File In TOP"],
    boundary_reason: IoBridge,
    extra_fields: {
        // The folder string we last scanned. Empty string = nothing
        // scanned yet; any change drops the file list and re-scans.
        last_folder: String = String::new(),
        // Sorted absolute paths to image files in `last_folder`.
        paths: Vec<PathBuf> = Vec::new(),
        // The slice texture currently on the GPU.
        slice_texture: Option<manifold_gpu::GpuTexture> = None,
        // Dimensions of `slice_texture` — drives the aspect-fit math.
        tex_width: u32 = 0,
        tex_height: u32 = 0,
        // Index into `paths` for whatever's on the GPU. -1 = nothing
        // loaded yet (the channel-pending state and "no files" state
        // are distinguished by `pending_load.is_some()`).
        current_index: i32 = -1,
        // Background loader channel + its target index. `Some` means
        // a load is in flight; we don't spawn another until it returns.
        pending_load: Option<mpsc::Receiver<Result<DecodedSlice, String>>> = None,
        pending_index: i32 = -1,
        // Monotonic count of slices that have actually landed on the GPU.
        // Bumped once per `current_index` advance (i.e. per visible image
        // change), emitted on the `trigger_count` scalar output for
        // downstream `node.trigger_gate` / `node.sample_and_hold` /
        // `node.clip_trigger_index` consumers. Matches the
        // `system.generator_input.trigger_count` convention — monotonic,
        // never resets, downstream gates compare deltas.
        trigger_count: u32 = 0,
        // Rising-edge detection for `next` / `prev` trigger inputs.
        // `None` on first frame after rebuild so the initial absorbed
        // counter value doesn't fire spurious advances — same cold-start
        // pattern as node.trigger_gate.
        last_next: Option<u32> = None,
        last_prev: Option<u32> = None,
        // Slider-vs-buttons arbitration. `manual_idx = Some(i)` means
        // the last input was a button press; we hold that index until
        // the user moves the slider, at which point `manual_idx` clears
        // and the slider takes over. `last_position` powers the
        // change-detection that drives the clear.
        last_position: Option<f32> = None,
        manual_idx: Option<i32> = None,
    },
}

/// Image extensions recognised when scanning a folder, lowercase. The
/// scan lowercases each file's extension before comparing, so `.JPG`,
/// `.Png`, etc. all match. Sort happens after filtering so the order
/// matches `ls` (alphabetical). TIFF decodes on the dedicated `tiff`
/// path (bit-exact MRI grayscale); the rest go through the `image` crate.
const IMAGE_EXTENSIONS: &[&str] =
    &["tiff", "tif", "png", "jpg", "jpeg", "webp", "bmp", "gif"];

impl Primitive for ImageFolder {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // 1. Folder param — re-scan if it changed since last frame.
        let folder = match ctx.params.get("folder") {
            Some(ParamValue::String(s)) => s.as_str().to_owned(),
            _ => String::new(),
        };
        if folder != self.last_folder {
            self.last_folder = folder.clone();
            self.paths = scan_folder(Path::new(&folder));
            // File list changed → drop everything cached, the indices
            // are no longer meaningful.
            self.slice_texture = None;
            self.tex_width = 0;
            self.tex_height = 0;
            self.current_index = -1;
            self.pending_load = None;
            self.pending_index = -1;
            // Fresh folder → slider drives initial display; any
            // accumulated button-driven override no longer makes sense.
            self.manual_idx = None;
        }

        // Emit the trigger_count scalar with the current value up front;
        // the bump in step 2 will overwrite with the new value if a slice
        // lands this frame. Covers every early-return path below.
        ctx.outputs
            .set_scalar("trigger_count", ParamValue::Float(self.trigger_count as f32));

        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out.width, out.height);
        if w == 0 || h == 0 {
            return;
        }

        // 2. Drain any completed background load and upload it.
        if self.pending_load.is_some() {
            let rx = self.pending_load.take().unwrap();
            match rx.try_recv() {
                Ok(Ok(slice)) => {
                    self.ensure_texture(ctx, slice.width, slice.height);
                    if let Some(tex) = &self.slice_texture {
                        ctx.gpu_encoder()
                            .native_enc
                            .upload_texture(tex, slice.width, slice.height, 1, &slice.rgba);
                    }
                    self.tex_width = slice.width;
                    self.tex_height = slice.height;
                    self.current_index = self.pending_index;
                    self.pending_index = -1;
                    self.trigger_count = self.trigger_count.saturating_add(1);
                    ctx.outputs.set_scalar(
                        "trigger_count",
                        ParamValue::Float(self.trigger_count as f32),
                    );
                }
                Ok(Err(e)) => {
                    log::error!("node.image_folder async load: {e}");
                    self.pending_index = -1;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    // Still in flight — put the receiver back.
                    self.pending_load = Some(rx);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::error!("node.image_folder async load: sender disconnected");
                    self.pending_index = -1;
                }
            }
        }

        // 3. Resolve the target slice index from position + prev/next
        // triggers. Slider drives the base index; rising edges on the
        // monotonic `next` / `prev` counters bump a separate
        // `manual_idx` override. Slider movement clears the override
        // (last-input-wins arbitration).
        if self.paths.is_empty() {
            let gpu = ctx.gpu_encoder();
            gpu.clear_texture(out, 0.0, 0.0, 0.0, 1.0);
            return;
        }
        let position = ctx.scalar_or_param("position", 0.5).clamp(0.0, 1.0);
        let max_idx = (self.paths.len() as i32 - 1).max(0);
        let slider_target = ((position * max_idx as f32).round() as i32).clamp(0, max_idx);

        // Rising-edge deltas on the trigger inputs. First-frame `None`
        // absorbs whatever cold-start value the upstream counter is
        // already at — same cold-start pattern as node.trigger_gate.
        let next_count = ctx.scalar_or_param("next", 0.0).round().max(0.0) as u32;
        let prev_count = ctx.scalar_or_param("prev", 0.0).round().max(0.0) as u32;
        let next_delta = self
            .last_next
            .map_or(0, |last| next_count.saturating_sub(last));
        let prev_delta = self
            .last_prev
            .map_or(0, |last| prev_count.saturating_sub(last));
        self.last_next = Some(next_count);
        self.last_prev = Some(prev_count);

        // Slider movement clears the manual override so the slider
        // takes over again. Sub-epsilon "noise" doesn't count — only a
        // real value change.
        let position_changed = self
            .last_position
            .is_some_and(|last| (position - last).abs() > 1e-6);
        self.last_position = Some(position);
        if position_changed {
            self.manual_idx = None;
        }
        if next_delta > 0 || prev_delta > 0 {
            let base = self.manual_idx.unwrap_or(slider_target);
            let bumped = base + (next_delta as i32) - (prev_delta as i32);
            self.manual_idx = Some(bumped.clamp(0, max_idx));
        }
        let target_idx = self.manual_idx.unwrap_or(slider_target);

        // 4. Kick off a load if the target moved and no load is in
        // flight. Background thread + mpsc keeps the content thread
        // free; if scrubbing outruns decode, intermediate frames get
        // skipped (we always chase the latest `target_idx`).
        let need_load = target_idx != self.current_index;
        if need_load && self.pending_load.is_none() {
            let path = self.paths[target_idx as usize].clone();
            let (tx, rx) = mpsc::channel();
            self.pending_index = target_idx;
            std::thread::spawn(move || {
                let _ = tx.send(load_image(&path));
            });
            self.pending_load = Some(rx);
        }

        // 5. If we still have no texture loaded (first-frame race),
        // emit black so the output isn't whatever pool leftover.
        let Some(slice_tex) = self.slice_texture.as_ref() else {
            let gpu = ctx.gpu_encoder();
            gpu.clear_texture(out, 0.0, 0.0, 0.0, 1.0);
            return;
        };

        // 6. Dispatch the sampling compute kernel.
        let uv_scale = ctx.scalar_or_param("uv_scale", 1.0).max(0.001);
        let inv_uv_scale = 1.0 / uv_scale;
        let aspect = w as f32 / h as f32;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/image_folder.wgsl"),
                "cs_main",
                "node.image_folder",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = ImageFolderUniforms {
            aspect_ratio: aspect,
            uv_scale: inv_uv_scale,
            tex_width: self.tex_width.max(1) as f32,
            tex_height: self.tex_height.max(1) as f32,
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
                    texture: slice_tex,
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
            "node.image_folder",
        );
    }
}

impl ImageFolder {
    fn ensure_texture(&mut self, ctx: &mut EffectNodeContext<'_, '_>, w: u32, h: u32) {
        if self.tex_width == w && self.tex_height == h && self.slice_texture.is_some() {
            return;
        }
        let device = ctx.gpu_encoder().device;
        let tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Rgba8Unorm,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL
                | manifold_gpu::GpuTextureUsage::CPU_UPLOAD,
            label: "node.image_folder slice",
            mip_levels: 1,
        });
        self.slice_texture = Some(tex);
        self.tex_width = w;
        self.tex_height = h;
    }
}

/// Scan a directory for supported image files and return the sorted
/// absolute paths. Empty when the path doesn't exist or contains no
/// matching files — both are acceptable runtime states (the primitive
/// emits black until the user sets a valid path).
fn scan_folder(dir: &Path) -> Vec<PathBuf> {
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        })
        .collect();
    paths.sort();
    paths
}

/// Decode any supported image file into RGBA8. TIFF routes to the
/// dedicated `tiff` path (preserving the bit-exact u16/f32 grayscale
/// handling that MRI scans rely on); everything else goes through the
/// `image` crate, which already lands RGBA8 for png/jpg/webp/bmp/gif.
fn load_image(path: &Path) -> Result<DecodedSlice, String> {
    let is_tiff = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff"))
        .unwrap_or(false);
    if is_tiff {
        return load_tiff_slice(path);
    }

    let img = image::open(path)
        .map_err(|e| format!("decode {}: {}", path.display(), e))?;
    let rgba = img.to_rgba8();
    Ok(DecodedSlice {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// Decode a TIFF file into RGBA8. Grayscale sources broadcast to all
/// channels (R=G=B=value, A=255). u16/f32 are scaled into the byte
/// range — matches the legacy `mri_volume_loader::load_tiff_slice`
/// behaviour so MRI scans render identically.
fn load_tiff_slice(path: &Path) -> Result<DecodedSlice, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("open {}: {}", path.display(), e))?;
    let mut decoder = tiff::decoder::Decoder::new(file)
        .map_err(|e| format!("TIFF decode {}: {}", path.display(), e))?;

    let (width, height) = decoder
        .dimensions()
        .map_err(|e| format!("TIFF dimensions: {e}"))?;
    let image = decoder
        .read_image()
        .map_err(|e| format!("TIFF read: {e}"))?;

    let mono: Vec<u8> = match image {
        tiff::decoder::DecodingResult::U8(d) => d,
        tiff::decoder::DecodingResult::U16(d) => d.iter().map(|&v| (v >> 8) as u8).collect(),
        tiff::decoder::DecodingResult::F32(d) => d
            .iter()
            .map(|&v| (v.clamp(0.0, 1.0) * 255.0) as u8)
            .collect(),
        _ => return Err("Unsupported TIFF pixel format".into()),
    };

    // Broadcast grayscale → RGBA. 4× memory but trivial for typical
    // MRI slice sizes (256×256×4 ≈ 256 KB) and keeps the downstream
    // sampler / shader format-agnostic.
    let mut rgba = Vec::with_capacity(mono.len() * 4);
    for v in &mono {
        rgba.push(*v);
        rgba.push(*v);
        rgba.push(*v);
        rgba.push(255);
    }

    Ok(DecodedSlice {
        width,
        height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::ports::{PortType, ScalarType};

    #[test]
    fn image_folder_ports_and_params() {
        assert_eq!(ImageFolder::TYPE_ID, "node.image_folder");
        let inputs = ImageFolder::INPUTS;
        assert_eq!(inputs.len(), 4);
        assert_eq!(inputs[0].name, "position");
        assert_eq!(inputs[0].ty, PortType::Scalar(ScalarType::F32));
        assert!(!inputs[0].required);
        assert_eq!(inputs[1].name, "uv_scale");
        assert_eq!(inputs[2].name, "next");
        assert_eq!(inputs[2].ty, PortType::Scalar(ScalarType::F32));
        assert!(!inputs[2].required);
        assert_eq!(inputs[3].name, "prev");
        assert_eq!(inputs[3].ty, PortType::Scalar(ScalarType::F32));
        assert!(!inputs[3].required);
        assert_eq!(ImageFolder::OUTPUTS.len(), 2);
        assert_eq!(ImageFolder::OUTPUTS[0].name, "out");
        assert_eq!(ImageFolder::OUTPUTS[0].ty, PortType::Texture2D);
        assert_eq!(ImageFolder::OUTPUTS[1].name, "trigger_count");
        assert_eq!(ImageFolder::OUTPUTS[1].ty, PortType::Scalar(ScalarType::F32));

        let names: Vec<&str> = ImageFolder::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["folder", "position", "uv_scale", "next", "prev"]);
    }

    #[test]
    fn primitive_registers() {
        let prim = ImageFolder::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.image_folder");
    }

    #[test]
    fn empty_folder_path_scan_returns_empty() {
        let paths = scan_folder(Path::new(""));
        assert!(paths.is_empty());
        let paths = scan_folder(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(paths.is_empty());
    }

    #[test]
    fn scan_matches_supported_formats_any_case_and_sorts() {
        let dir = std::env::temp_dir().join("manifold_image_folder_scan_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A mix of supported (varied case) and unsupported extensions.
        for name in ["b.PNG", "a.jpg", "c.tiff", "d.WebP", "e.txt", "f.mp4"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let paths = scan_folder(&dir);
        let names: Vec<String> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        // Sorted alphabetically; .txt and .mp4 filtered out; case-insensitive.
        assert_eq!(names, vec!["a.jpg", "b.PNG", "c.tiff", "d.WebP"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
