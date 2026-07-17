//! `node.render_text` — composite a CPU-rasterized text bitmap into the
//! output.
//!
//! Outer-card use: the host wires `text` and `fontFamily` String params
//! through preset `stringBindings`. The primitive owns a CoreText-backed
//! [`TextRasterizer`] that turns those into an R8Unorm grayscale glyph
//! bitmap, dirty-cached so re-rasterization only happens when the text,
//! size, font, or styling actually change. A compute kernel then samples
//! that bitmap and writes premultiplied white glyphs on a transparent
//! background into the output with the usual position / scale / aspect /
//! alignment math, so the text keys over the layer below.
//!
//! Single-primitive wrap of the legacy `Text` generator — the rasterizer
//! and shader are lifted verbatim. The decomposition gain is that the
//! generator is now JSON-authored (one `system.generator_input →
//! node.render_text → system.final_output` preset) so users can drill in
//! from the editor.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::text_rasterizer::{HAlign, RasterizeOptions, TextRasterizer};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RenderTextUniforms {
    pos_x: f32,
    pos_y: f32,
    display_scale: f32,
    aspect_ratio: f32,
    // -- 16-byte boundary --
    tex_width: f32,
    tex_height: f32,
    output_width: f32,
    output_height: f32,
    // -- 16-byte boundary --
    v_align: f32,    // 0=Top, 1=Center, 2=Bottom
    has_stroke: f32, // 0 = no outline, 1 = blend stroke_color under the fill
    _pad0: f32,
    _pad1: f32,
    // -- 16-byte boundary --
    fill_color: [f32; 4],
    // -- 16-byte boundary --
    stroke_color: [f32; 4],
}

crate::primitive! {
    name: RenderText,
    type_id: "node.render_text",
    purpose: "Render a text string to the output texture. The host wires `text` and `fontFamily` through preset stringBindings; size/position/scale/alignment/spacing/stroke_width are port-shadows-param scalars; fill_color and stroke_color are editor-set Color params. CPU-rasterizes the glyphs via CoreText into internal R8Unorm coverage bitmaps — a fill mask always, plus an outline mask when stroke_width > 0 (both dirty-cached — only re-rasterized when text/font/size/style change). A compute kernel composites them as premultiplied alpha (fill over stroke over transparent) with aspect correction, so the text keys cleanly over the layer below. Single-node text generator: drop it between `system.generator_input` and `system.final_output`.",
    inputs: {
        size: ScalarF32 optional,
        pos_x: ScalarF32 optional,
        pos_y: ScalarF32 optional,
        scale: ScalarF32 optional,
        h_align: ScalarF32 optional,
        v_align: ScalarF32 optional,
        letter_spacing: ScalarF32 optional,
        line_spacing: ScalarF32 optional,
        stroke_width: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("size"),
            label: "Size",
            ty: ParamType::Float,
            default: ParamValue::Float(0.25),
            range: Some((0.02, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("pos_x"),
            label: "Position X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("pos_y"),
            label: "Position Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scale"),
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.1, 5.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("h_align"),
            label: "H Align",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &["Left", "Center", "Right"],
        },
        ParamDef {
            name: Cow::Borrowed("v_align"),
            label: "V Align",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &["Top", "Center", "Bottom"],
        },
        ParamDef {
            name: Cow::Borrowed("letter_spacing"),
            label: "Letter Spacing",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-0.5, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("line_spacing"),
            label: "Line Spacing",
            ty: ParamType::Float,
            default: ParamValue::Float(1.2),
            range: Some((0.5, 3.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("stroke_width"),
            label: "Stroke Width",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 0.5)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("fill_color"),
            label: "Fill Color",
            ty: ParamType::Color,
            default: ParamValue::Color([1.0, 1.0, 1.0, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("stroke_color"),
            label: "Stroke Color",
            ty: ParamType::Color,
            default: ParamValue::Color([0.0, 0.0, 0.0, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("text"),
            label: "Text",
            ty: ParamType::String,
            // String default supplied via stringBindings; this slot is never read.
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("fontFamily"),
            label: "Font",
            ty: ParamType::String,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    // depth_rule: general CoreText raster generator (no texture input) — can serve as real kinetic-typography content, not only a diagnostic readout like render_value_overlay
    depth_rule: SourceHeight,
    composition_notes: "Text + fontFamily come via presetMetadata.stringBindings — wire the JSON-graph generator's outer-card text fields into this primitive's String params. Scalar inputs are port-shadows-param: wire upstream LFOs / envelopes into `size`, `pos_x`, `pos_y`, `scale`, `h_align`, `v_align`, `letter_spacing`, `line_spacing`, `stroke_width` to animate them. `stroke_width` is a fraction of font size; > 0 rasterizes a second outline coverage mask (so width changes re-raster, but the per-frame cost stays the composite dispatch). `fill_color` / `stroke_color` are editor-set Color params blended in the shader — free to change, never re-rasterize, but not modulatable (the binding system is scalar-only). Crispness: the glyphs are CPU-rasterized at their true on-screen pixel footprint (size × scale × output_height), so `scale` is folded into the raster — never a GPU magnify of a fixed bitmap — and the composite samples ~1:1 through a linear sampler. The result is sharp, anti-aliased edges at any size, scale, or output resolution. Oversized layouts scale down to fit the 16384² texture cap (linear-upscaled back to size) instead of cropping. Trade-off: animating size/scale re-rasterizes each frame (sub-ms CoreText for one line); static text is dirty-cached and re-rasterizes only when text, font, footprint, or styling actually change. Output is premultiplied alpha on a transparent background (fill over stroke), so it keys over the layer below instead of painting a black box; with fill=white and no stroke it matches the original white-glyph behaviour.",
    examples: ["assets/generator-presets/Text.json"],
    picker: { label: "Render Text", category: Atom },
    summary: "Draws a text string onto the image with a chosen font, size, and position. Wire the text and font through the card so you can change them live.",
    category: Generate,
    role: Filter,
    aliases: ["text", "render text", "title", "Text TOP"],
    boundary_reason: DrawCall,
    extra_fields: {
        rasterizer: TextRasterizer = TextRasterizer::new(),
        text_texture: Option<manifold_gpu::GpuTexture> = None,
        stroke_texture: Option<manifold_gpu::GpuTexture> = None,
        text_tex_dims: (u32, u32) = (0, 0),
        display_scale: f32 = 1.0,
        cached_text: String = String::new(),
        cached_font_family: String = String::new(),
        cached_pixel_size: f32 = 0.0,
        cached_h_align: f32 = -1.0,
        cached_letter_spacing: f32 = f32::NAN,
        cached_line_spacing: f32 = f32::NAN,
        cached_stroke_width: f32 = f32::NAN,
    },
}

impl Primitive for RenderText {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out.width, out.height);
        if w == 0 || h == 0 {
            return;
        }

        // Read string params (default to empty / Inter fallback when unset).
        let text = ctx
            .params
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let font_family = ctx
            .params
            .get("fontFamily")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();

        // Read scalars via port-shadows-param so upstream wires animate.
        // `scale` is folded into the rasterization size (not a GPU magnify of a
        // fixed bitmap) so the glyphs are always rendered at their true
        // on-screen pixel footprint — crisp at any size/scale/output res.
        let size_frac = ctx.scalar_or_param("size", 0.25).clamp(0.02, 1.0);
        let scale = ctx.scalar_or_param("scale", 1.0).max(0.01);
        let font_size = size_frac * scale * h as f32;
        let pos_x = ctx.scalar_or_param("pos_x", 0.0);
        let pos_y = ctx.scalar_or_param("pos_y", 0.0);
        let h_align = ctx.scalar_or_param("h_align", 1.0);
        let v_align = ctx.scalar_or_param("v_align", 1.0);
        let letter_spacing = ctx.scalar_or_param("letter_spacing", 0.0);
        let line_spacing = ctx.scalar_or_param("line_spacing", 1.2);
        let stroke_width = ctx.scalar_or_param("stroke_width", 0.0).max(0.0);

        // Colours are editor-set params (the binding/modulation system is
        // scalar-only, like every other colour in the app). They feed the
        // composite shader directly — changing them costs nothing extra and
        // never invalidates the glyph bitmap cache.
        let fill_color = match ctx.params.get("fill_color") {
            Some(ParamValue::Color(c)) => *c,
            _ => [1.0, 1.0, 1.0, 1.0],
        };
        let stroke_color = match ctx.params.get("stroke_color") {
            Some(ParamValue::Color(c)) => *c,
            _ => [0.0, 0.0, 0.0, 1.0],
        };

        // Dirty-check the rasterizer cache — only rebuild the glyph bitmap
        // when something it depends on actually changed. NaN sentinels on
        // the spacing caches make the first-frame compare false; the
        // `text_changed` branch carries the initial rasterize.
        let text_changed = text != self.cached_text;
        let font_changed = font_family != self.cached_font_family;
        let size_changed = (font_size - self.cached_pixel_size).abs() > 0.5;
        let style_changed = (h_align - self.cached_h_align).abs() > 0.01
            || (letter_spacing - self.cached_letter_spacing).abs() > 0.001
            || (line_spacing - self.cached_line_spacing).abs() > 0.01
            || (stroke_width - self.cached_stroke_width).abs() > 0.0005;

        if text_changed || font_changed || size_changed || style_changed {
            // Pre-warm font cache on family change so the first rasterize
            // doesn't stall on a CoreText descriptor lookup.
            if font_changed && !font_family.is_empty() {
                self.rasterizer.prewarm_font(&font_family);
            }
            let opts = RasterizeOptions {
                font_family: if font_family.is_empty() {
                    None
                } else {
                    Some(font_family.as_str())
                },
                h_align: HAlign::from_param(h_align),
                letter_spacing,
                line_spacing,
                stroke_width,
            };
            match self.rasterizer.rasterize(&text, font_size, &opts) {
                Some(result) => {
                    // Bitmap may have been capped below the requested size; the
                    // shader upscales by this ratio (1.0 in the common case).
                    self.display_scale = (font_size / result.rendered_font_px.max(1.0)).max(1.0);
                    self.ensure_textures(ctx, result.width, result.height, result.stroke.is_some());
                    if let Some(ref texture) = self.text_texture {
                        ctx.gpu_encoder().native_enc.upload_texture(
                            texture,
                            result.width,
                            result.height,
                            1,
                            &result.fill,
                        );
                    }
                    if let (Some(texture), Some(pixels)) =
                        (self.stroke_texture.as_ref(), result.stroke.as_ref())
                    {
                        ctx.gpu_encoder().native_enc.upload_texture(
                            texture,
                            result.width,
                            result.height,
                            1,
                            pixels,
                        );
                    }
                }
                None => {
                    self.text_texture = None;
                    self.stroke_texture = None;
                    self.text_tex_dims = (0, 0);
                }
            }
            self.cached_text = text;
            self.cached_font_family = font_family;
            self.cached_pixel_size = font_size;
            self.cached_h_align = h_align;
            self.cached_letter_spacing = letter_spacing;
            self.cached_line_spacing = line_spacing;
            self.cached_stroke_width = stroke_width;
        }

        // Reborrow output here — `ensure_texture` and `upload_texture`
        // above borrowed `ctx`, so we re-fetch the texture handle now.
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };

        // No glyph bitmap (empty text, whitespace, or rasterize failed)
        // → clear to fully transparent and bail (premultiplied alpha contract:
        // nothing to draw means the layer below shows through, not a black box).
        let Some(text_tex) = self.text_texture.as_ref() else {
            ctx.gpu_encoder().clear_texture(out, 0.0, 0.0, 0.0, 0.0);
            return;
        };

        // When there is no outline the stroke slot still needs a valid
        // binding — point it at the fill texture and gate the sample with
        // has_stroke = 0 so the shader never reads it.
        let stroke_tex = self.stroke_texture.as_ref().unwrap_or(text_tex);
        let has_stroke = if self.stroke_texture.is_some() {
            1.0
        } else {
            0.0
        };

        let aspect = w as f32 / h as f32;
        let uniforms = RenderTextUniforms {
            pos_x,
            pos_y,
            display_scale: self.display_scale,
            aspect_ratio: aspect,
            tex_width: self.text_tex_dims.0 as f32,
            tex_height: self.text_tex_dims.1 as f32,
            output_width: w as f32,
            output_height: h as f32,
            v_align,
            has_stroke,
            _pad0: 0.0,
            _pad1: 0.0,
            fill_color,
            stroke_color,
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/render_text.wgsl"),
                "cs_main",
                "node.render_text",
            )
        });
        // Linear + clamp-to-edge: keeps CoreText's AA crisp when the bitmap is
        // magnified, and the transparent PADDING border AAs the glyph edges.
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&manifold_gpu::GpuSamplerDesc::default()));

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: text_tex,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: stroke_tex,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: out,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.render_text",
        );
    }
}

impl RenderText {
    /// Ensure the fill texture (always) and the stroke texture (only when
    /// `want_stroke`) exist at `w × h`. Both glyph masks share dimensions, so
    /// a size change re-creates whichever are needed; when the stroke is off,
    /// its texture is dropped so we don't hold a stale outline.
    fn ensure_textures(
        &mut self,
        ctx: &mut EffectNodeContext<'_, '_>,
        w: u32,
        h: u32,
        want_stroke: bool,
    ) {
        let dims_changed = self.text_tex_dims != (w, h);
        let device = ctx.gpu_encoder().device;
        let make = || {
            device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: w,
                height: h,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::R8Unorm,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                    | manifold_gpu::GpuTextureUsage::CPU_UPLOAD,
                label: "node.render_text glyphs",
                mip_levels: 1,
            })
        };
        if dims_changed || self.text_texture.is_none() {
            self.text_texture = Some(make());
        }
        if want_stroke {
            if dims_changed || self.stroke_texture.is_none() {
                self.stroke_texture = Some(make());
            }
        } else {
            self.stroke_texture = None;
        }
        self.text_tex_dims = (w, h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::ports::{PortType, ScalarType};

    #[test]
    fn render_text_ports_and_params() {
        assert_eq!(RenderText::TYPE_ID, "node.render_text");
        assert_eq!(RenderText::INPUTS.len(), 9);
        for input in RenderText::INPUTS {
            assert!(!input.required, "{} should be optional", input.name);
            assert_eq!(input.ty, PortType::Scalar(ScalarType::F32));
        }
        assert_eq!(RenderText::OUTPUTS.len(), 1);
        assert_eq!(RenderText::OUTPUTS[0].ty, PortType::Texture2D);

        let names: Vec<&str> = RenderText::PARAMS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(
            names,
            vec![
                "size",
                "pos_x",
                "pos_y",
                "scale",
                "h_align",
                "v_align",
                "letter_spacing",
                "line_spacing",
                "stroke_width",
                "fill_color",
                "stroke_color",
                "text",
                "fontFamily",
            ]
        );

        // Two String-typed params; two Color; the rest Float.
        let string_params: Vec<&str> = RenderText::PARAMS
            .iter()
            .filter(|p| matches!(p.ty, ParamType::String))
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(string_params, vec!["text", "fontFamily"]);

        let color_params: Vec<&str> = RenderText::PARAMS
            .iter()
            .filter(|p| matches!(p.ty, ParamType::Color))
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(color_params, vec!["fill_color", "stroke_color"]);
    }

    #[test]
    fn primitive_registers() {
        let prim = RenderText::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.render_text");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU smoke test: with text="HELLO" the output texture should
    //! contain some non-black pixels (the glyph silhouette). The
    //! rasterizer's own bit-exact tests live in `text_rasterizer.rs` —
    //! this one is the integration check that the primitive uploads the
    //! bitmap and the composite shader actually writes it through.

    use std::sync::Arc;

    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use super::RenderText;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::Slot;
    use crate::node_graph::{
        Executor, FinalOutput, FrameTime, Graph, MetalBackend, ParamValue, compile,
    };

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    /// With text="HELLO" + Helvetica, the output should land glyph
    /// pixels somewhere — non-black covers more than just one stray
    /// texel, so we require at least 0.1% of the texture to be lit.
    #[test]
    fn hello_writes_glyph_pixels_to_output() {
        let device = crate::test_device();
        let (w, h) = (256u32, 128u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let rt = g.add_node(Box::new(RenderText::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(
            rt,
            "text",
            ParamValue::String(Arc::new("HELLO".to_string())),
        )
        .unwrap();
        // Pin to Helvetica so the test doesn't depend on whichever
        // system default the Inter fallback would resolve to.
        g.set_param(
            rt,
            "fontFamily",
            ParamValue::String(Arc::new("Helvetica".to_string())),
        )
        .unwrap();
        g.connect((rt, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let backend = MetalBackend::new(device.arc(), w, h, format);
        // RenderText's `out` is the first (and only) lazily-allocated
        // Texture2D — capture the slot before exec releases the binding.
        let out_slot = Slot(backend.slot_count());

        let mut native_enc = device.create_encoder("render-text-test");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let out_tex = exec
            .backend()
            .texture_2d(out_slot)
            .expect("output texture retained");
        let bytes_per_row = w * 8; // Rgba16Float = 8 bytes/pixel
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("render-text-readback");
        readback_enc.copy_texture_to_buffer(out_tex, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback_buf.mapped_ptr().expect("shared buffer pointer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        let lit_pixels = (0..(w * h) as usize)
            .filter(|&i| {
                let o = i * 4;
                f16::from_bits(halves[o]).to_f32() > 0.05
            })
            .count();
        let min_lit = ((w * h) as usize) / 1000; // ≥ 0.1 % of pixels.
        assert!(
            lit_pixels > min_lit,
            "expected glyphs to light at least {min_lit} pixels, got {lit_pixels}",
        );

        // Keying guard: text must composite as a real masked layer, not an
        // opaque black box. The background must be fully transparent (alpha 0)
        // and the glyphs must carry alpha (premultiplied: alpha = coverage).
        let transparent_bg = (0..(w * h) as usize)
            .filter(|&i| f16::from_bits(halves[i * 4 + 3]).to_f32() < 0.01)
            .count();
        let glyph_alpha = (0..(w * h) as usize)
            .filter(|&i| f16::from_bits(halves[i * 4 + 3]).to_f32() > 0.5)
            .count();
        assert!(
            transparent_bg > 0,
            "expected a transparent background so text keys over the layer below",
        );
        assert!(
            glyph_alpha > 0,
            "expected glyph pixels to carry alpha (premultiplied coverage)",
        );
    }

    /// Empty text → output cleared to fully transparent (rasterize returns
    /// None and the primitive bails after clearing). Guards the
    /// rasterize-returned-None branch against a regression where it leaves a
    /// stale bitmap — or an opaque black box — on screen.
    #[test]
    fn empty_text_clears_to_transparent() {
        let device = crate::test_device();
        let (w, h) = (32u32, 32u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let rt = g.add_node(Box::new(RenderText::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(rt, "text", ParamValue::String(Arc::new(String::new())))
            .unwrap();
        g.connect((rt, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let backend = MetalBackend::new(device.arc(), w, h, format);
        let out_slot = Slot(backend.slot_count());

        let mut native_enc = device.create_encoder("render-text-empty");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let out_tex = exec.backend().texture_2d(out_slot).unwrap();
        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("render-text-empty-readback");
        readback_enc.copy_texture_to_buffer(out_tex, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback_buf.mapped_ptr().unwrap();
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        for i in 0..(w * h) as usize {
            let r = f16::from_bits(halves[i * 4]).to_f32();
            let a = f16::from_bits(halves[i * 4 + 3]).to_f32();
            assert!(r < 0.01, "expected black rgb for empty text, got {r} at {i}");
            assert!(
                a < 0.01,
                "expected transparent alpha for empty text, got {a} at {i}",
            );
        }
    }
}
