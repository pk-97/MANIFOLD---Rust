//! `node.glyph_atlas` — the fixed ASCII coverage atlas used by the terminal
//! reconstruction.  CoreText is deliberately only involved while the atlas
//! is first built; the published texture is immutable for the lifetime of the
//! primitive.

use manifold_gpu::{GpuTexture, GpuTextureDesc, GpuTextureFormat, GpuTextureUsage};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;
use crate::text_rasterizer::{HAlign, RasterizeOptions, TextRasterizer};

pub(crate) const ATLAS_TILE_WIDTH: u32 = 32;
pub(crate) const ATLAS_TILE_HEIGHT: u32 = 48;
pub(crate) const ATLAS_COLUMNS: u32 = 16;
pub(crate) const ATLAS_ROWS: u32 = 6;
pub(crate) const ATLAS_WIDTH: u32 = ATLAS_TILE_WIDTH * ATLAS_COLUMNS;
pub(crate) const ATLAS_HEIGHT: u32 = ATLAS_TILE_HEIGHT * ATLAS_ROWS;
pub(crate) const ASCII_FIRST: u32 = 32;
pub(crate) const ASCII_LAST: u32 = 127;
pub(crate) const ASCII_SLOT_COUNT: u32 = ASCII_LAST - ASCII_FIRST + 1;

fn rasterize_options() -> RasterizeOptions<'static> {
    RasterizeOptions {
        font_family: Some("Menlo"),
        h_align: HAlign::Left,
        letter_spacing: 0.0,
        line_spacing: 1.0,
        stroke_width: 0.0,
    }
}

/// Build the complete atlas in row-major tile order.  Each single-character
/// bitmap has the same CoreText ascent/descent geometry, so copying its
/// padded bitmap at the fixed tile origin preserves the baseline.  In
/// particular, this does not find and re-center individual ink bounds.
pub(crate) fn build_atlas_pixels(rasterizer: &mut TextRasterizer) -> Vec<u8> {
    let mut pixels = vec![0u8; (ATLAS_WIDTH * ATLAS_HEIGHT) as usize];
    let options = rasterize_options();

    for slot in 0..ASCII_SLOT_COUNT {
        let codepoint = ASCII_FIRST + slot;
        let tile_x = (slot % ATLAS_COLUMNS) * ATLAS_TILE_WIDTH;
        let tile_y = (slot / ATLAS_COLUMNS) * ATLAS_TILE_HEIGHT;

        // Space is intentionally an empty tile. TextRasterizer also returns
        // None for whitespace-only strings, but keeping this explicit makes
        // the terminal wire contract independent of CoreText's whitespace
        // handling.
        if codepoint == b' ' as u32 {
            continue;
        }

        // Slot 95 is the terminal cursor (DEL / 127), represented by a solid
        // inset block so it remains visible at small cell sizes.
        if codepoint == ASCII_LAST {
            let inset = 2;
            for y in inset..(ATLAS_TILE_HEIGHT - inset) {
                let row = ((tile_y + y) * ATLAS_WIDTH + tile_x + inset) as usize;
                pixels[row..row + (ATLAS_TILE_WIDTH - inset * 2) as usize].fill(255);
            }
            continue;
        }

        let ch = char::from_u32(codepoint).expect("printable ASCII codepoint");
        let bitmap = rasterizer
            .rasterize(&ch.to_string(), 32.0, &options)
            .expect("printable ASCII has a rasterized glyph");

        // The fixed origin is the important part: the rasterizer's padding,
        // advance, and baseline stay intact. Reject unexpected font metrics
        // rather than silently clipping characters.
        assert!(
            bitmap.width <= ATLAS_TILE_WIDTH && bitmap.height <= ATLAS_TILE_HEIGHT,
            "Menlo 32px glyph does not fit the terminal atlas tile"
        );
        let copy_width = bitmap.width;
        let copy_height = bitmap.height;
        for y in 0..copy_height {
            let src = (y * bitmap.width) as usize;
            let dst = ((tile_y + y) * ATLAS_WIDTH + tile_x) as usize;
            pixels[dst..dst + copy_width as usize]
                .copy_from_slice(&bitmap.fill[src..src + copy_width as usize]);
        }
    }

    pixels
}

crate::primitive! {
    name: GlyphAtlas,
    type_id: "node.glyph_atlas",
    purpose: "Build the immutable 32 px Menlo ASCII coverage atlas for node.render_glyph_grid. Slots 0..94 contain printable ASCII 32..126, slot 95 is the solid inset cursor for codepoint 127, and the space slot is blank.",
    inputs: {},
    outputs: {
        out: Texture2D,
    },
    params: [],
    depth_rule: SourceHeight,
    composition_notes: "A zero-input IoBridge source. The texture is generated once during warmup/run and retained; the fixed 32 px tile geometry is consumed by node.render_glyph_grid.",
    examples: [],
    picker: { label: "Glyph Atlas", category: Atom },
    summary: "Provides the fixed ASCII coverage atlas used by terminal glyph rendering.",
    category: Generate,
    role: Source,
    aliases: ["glyph atlas", "ascii atlas", "terminal atlas"],
    boundary_reason: IoBridge,
    extra_fields: {
        rasterizer: TextRasterizer = TextRasterizer::new(),
        published_texture: Option<GpuTexture> = None,
    },
}

impl Primitive for GlyphAtlas {
    fn provides_texture_output(&self, port: &str) -> bool {
        port == "out"
    }

    fn provided_texture_output(&self, port: &str) -> Option<&GpuTexture> {
        (port == "out")
            .then_some(self.published_texture.as_ref())
            .flatten()
    }

    fn output_dims(
        &self,
        port: &str,
        _canvas_dims: (u32, u32),
        _input_dims: &[(&str, (u32, u32))],
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        (port == "out").then_some((ATLAS_WIDTH, ATLAS_HEIGHT))
    }

    fn output_format(&self, port: &str) -> Option<GpuTextureFormat> {
        (port == "out").then_some(GpuTextureFormat::R8Unorm)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(desc) = ctx.outputs.provided_texture_descriptor("out") else {
            // A host-prebound path is not part of the source contract. Keep
            // the CPU build lazy and let the normal graph ownership path
            // publish the immutable texture on the next planned evaluation.
            return;
        };

        if self.published_texture.is_none() {
            let pixels = build_atlas_pixels(&mut self.rasterizer);
            let gpu = ctx.gpu_encoder();
            let output = gpu.device.create_texture(&GpuTextureDesc {
                usage: desc.usage | GpuTextureUsage::CPU_UPLOAD,
                ..desc
            });
            gpu.device.upload_texture(&output, &pixels);
            self.published_texture = Some(output);
        } else {
            ctx.mark_outputs_unchanged();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(pixels: &[u8], code: u8) -> Vec<u8> {
        let slot = u32::from(code) - ASCII_FIRST;
        let x = slot % ATLAS_COLUMNS * ATLAS_TILE_WIDTH;
        let y = slot / ATLAS_COLUMNS * ATLAS_TILE_HEIGHT;
        (0..ATLAS_TILE_HEIGHT)
            .flat_map(|row| {
                let start = ((y + row) * ATLAS_WIDTH + x) as usize;
                pixels[start..start + ATLAS_TILE_WIDTH as usize]
                    .iter()
                    .copied()
            })
            .collect()
    }

    #[test]
    fn coretext_atlas_has_all_printable_glyphs_blank_space_and_aligned_punctuation() {
        let pixels = build_atlas_pixels(&mut TextRasterizer::new());
        assert!(tile(&pixels, b' ').iter().all(|&p| p == 0));
        for code in 33..=127 {
            assert!(
                tile(&pixels, code).iter().any(|&p| p > 0),
                "missing glyph {code}"
            );
        }
        let cursor = tile(&pixels, 127);
        assert_eq!(cursor[0], 0);
        assert_eq!(
            cursor[(ATLAS_TILE_WIDTH * ATLAS_TILE_HEIGHT / 2 + ATLAS_TILE_WIDTH / 2) as usize],
            255
        );
        let ink_row = |code| {
            let coverage = tile(&pixels, code);
            let weight: f64 = coverage.iter().map(|&v| f64::from(v)).sum();
            coverage
                .iter()
                .enumerate()
                .map(|(i, &v)| (i / ATLAS_TILE_WIDTH as usize) as f64 * f64::from(v))
                .sum::<f64>()
                / weight
        };
        assert!(
            ink_row(b'_') > ink_row(b'-') + 4.0,
            "punctuation must retain its baseline position"
        );
    }
}
