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
const INNER_X0: u32 = 2;
const INNER_X1: u32 = 28;
const INNER_Y0: u32 = 2;
const INNER_Y1: u32 = 46;
const INNER_WIDTH: usize = (INNER_X1 - INNER_X0) as usize;
const INNER_HEIGHT: usize = (INNER_Y1 - INNER_Y0) as usize;

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

#[derive(Clone, Copy)]
struct RankPixel {
    x: usize,
    y: usize,
    coverage: u8,
    distance: u16,
    tie: u32,
}

fn dispersed_tie(x: usize, y: usize) -> u32 {
    let mut value =
        (x as u32 + 1).wrapping_mul(0x9e37_79b9) ^ (y as u32 + 1).wrapping_mul(0x85eb_ca6b);
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value
}

fn chamfer_distances(coverage: &[u8]) -> Vec<u16> {
    let mut distances = vec![u16::MAX; coverage.len()];
    for (distance, &value) in distances.iter_mut().zip(coverage) {
        if value > 0 {
            *distance = 0;
        }
    }

    for y in 0..INNER_HEIGHT {
        for x in 0..INNER_WIDTH {
            let index = y * INNER_WIDTH + x;
            let mut best = distances[index];
            if x > 0 {
                best = best.min(distances[index - 1].saturating_add(1));
            }
            if y > 0 {
                best = best.min(distances[index - INNER_WIDTH].saturating_add(1));
            }
            distances[index] = best;
        }
    }
    for y in (0..INNER_HEIGHT).rev() {
        for x in (0..INNER_WIDTH).rev() {
            let index = y * INNER_WIDTH + x;
            let mut best = distances[index];
            if x + 1 < INNER_WIDTH {
                best = best.min(distances[index + 1].saturating_add(1));
            }
            if y + 1 < INNER_HEIGHT {
                best = best.min(distances[index + INNER_WIDTH].saturating_add(1));
            }
            distances[index] = best;
        }
    }
    distances
}

/// Build the per-glyph threshold atlas from the already-rasterized coverage.
/// Each inner rectangle is a deterministic 0..255 permutation: strong ink is
/// enabled first, then pixels proceed outward from the ink. The two-pixel
/// border is clamped to the inner rectangle so bilinear atlas sampling never
/// sees an uninitialized threshold.
pub(crate) fn build_threshold_atlas_pixels(coverage: &[u8]) -> Vec<u8> {
    assert_eq!(
        coverage.len(),
        (ATLAS_WIDTH * ATLAS_HEIGHT) as usize,
        "glyph coverage atlas has unexpected dimensions"
    );
    let mut thresholds = vec![0u8; coverage.len()];

    for slot in 0..ASCII_SLOT_COUNT {
        let tile_x = (slot % ATLAS_COLUMNS) * ATLAS_TILE_WIDTH;
        let tile_y = (slot / ATLAS_COLUMNS) * ATLAS_TILE_HEIGHT;
        let inner_coverage: Vec<u8> = (0..INNER_HEIGHT)
            .flat_map(|y| {
                let start = ((tile_y + INNER_Y0) * ATLAS_WIDTH + tile_x + INNER_X0) as usize
                    + y * ATLAS_WIDTH as usize;
                coverage[start..start + INNER_WIDTH].iter().copied()
            })
            .collect();
        let distances = chamfer_distances(&inner_coverage);
        let mut ranked = Vec::with_capacity(INNER_WIDTH * INNER_HEIGHT);
        for y in 0..INNER_HEIGHT {
            for x in 0..INNER_WIDTH {
                let index = y * INNER_WIDTH + x;
                ranked.push(RankPixel {
                    x,
                    y,
                    coverage: inner_coverage[index],
                    distance: distances[index],
                    tie: dispersed_tie(x, y),
                });
            }
        }
        ranked.sort_unstable_by(|a, b| {
            b.coverage
                .cmp(&a.coverage)
                .then_with(|| a.distance.cmp(&b.distance))
                .then_with(|| a.tie.cmp(&b.tie))
                .then_with(|| a.y.cmp(&b.y))
                .then_with(|| a.x.cmp(&b.x))
        });

        let last = ranked.len() - 1;
        for (order, pixel) in ranked.into_iter().enumerate() {
            let rank = ((last - order) * 256 / (last + 1)) as u8;
            let index = ((tile_y + INNER_Y0 + pixel.y as u32) * ATLAS_WIDTH
                + tile_x
                + INNER_X0
                + pixel.x as u32) as usize;
            thresholds[index] = rank;
        }

        for y in 0..ATLAS_TILE_HEIGHT {
            let source_y = (y.clamp(INNER_Y0, INNER_Y1 - 1) - INNER_Y0) as usize;
            for x in 0..ATLAS_TILE_WIDTH {
                let source_x = (x.clamp(INNER_X0, INNER_X1 - 1) - INNER_X0) as usize;
                let dst = ((tile_y + y) * ATLAS_WIDTH + tile_x + x) as usize;
                let src = ((tile_y + INNER_Y0 + source_y as u32) * ATLAS_WIDTH
                    + tile_x
                    + INNER_X0
                    + source_x as u32) as usize;
                thresholds[dst] = thresholds[src];
            }
        }
    }

    thresholds
}

crate::primitive! {
    name: GlyphAtlas,
    type_id: "node.glyph_atlas",
    purpose: "Build the immutable 32 px Menlo ASCII coverage atlas for node.render_glyph_grid. Slots 0..94 contain printable ASCII 32..126, slot 95 is the solid inset cursor for codepoint 127, and the coverage space slot is blank. The threshold output ranks pixels around each glyph for tone-preserving code dithering.",
    inputs: {},
    outputs: {
        out: Texture2D,
        threshold: Texture2D,
    },
    params: [],
    depth_rule: SourceHeight,
    composition_notes: "A zero-input IoBridge source. The texture is generated once during warmup/run and retained; the fixed 32 px tile geometry is consumed by node.render_glyph_grid. Use out for coverage, or threshold through render_glyph_grid into node.dither.pattern; threshold ranks expand ink with luminance and use dispersed ranks for spaces.",
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
        published_threshold_texture: Option<GpuTexture> = None,
    },
}

impl Primitive for GlyphAtlas {
    fn provides_texture_output(&self, port: &str) -> bool {
        matches!(port, "out" | "threshold")
    }

    fn provided_texture_output(&self, port: &str) -> Option<&GpuTexture> {
        match port {
            "out" => self.published_texture.as_ref(),
            "threshold" => self.published_threshold_texture.as_ref(),
            _ => None,
        }
    }

    fn output_dims(
        &self,
        port: &str,
        _canvas_dims: (u32, u32),
        _input_dims: &[(&str, (u32, u32))],
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        matches!(port, "out" | "threshold").then_some((ATLAS_WIDTH, ATLAS_HEIGHT))
    }

    fn output_format(&self, port: &str) -> Option<GpuTextureFormat> {
        matches!(port, "out" | "threshold").then_some(GpuTextureFormat::R8Unorm)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let out_desc = ctx.outputs.provided_texture_descriptor("out");
        let threshold_desc = ctx.outputs.provided_texture_descriptor("threshold");
        if out_desc.is_none() && threshold_desc.is_none() {
            // A host-prebound path is not part of the source contract. Keep
            // the CPU build lazy and let the normal graph ownership path
            // publish the immutable texture on the next planned evaluation.
            return;
        }

        let out_ready = out_desc.is_none() || self.published_texture.is_some();
        let threshold_ready =
            threshold_desc.is_none() || self.published_threshold_texture.is_some();
        if out_ready && threshold_ready {
            ctx.mark_outputs_unchanged();
            return;
        }

        let pixels = build_atlas_pixels(&mut self.rasterizer);
        let threshold_pixels = build_threshold_atlas_pixels(&pixels);
        let gpu = ctx.gpu_encoder();
        if let Some(desc) = out_desc
            && self.published_texture.is_none()
        {
            let output = gpu.device.create_texture(&GpuTextureDesc {
                usage: desc.usage | GpuTextureUsage::CPU_UPLOAD,
                ..desc
            });
            gpu.device.upload_texture(&output, &pixels);
            self.published_texture = Some(output);
        }
        if let Some(desc) = threshold_desc
            && self.published_threshold_texture.is_none()
        {
            let output = gpu.device.create_texture(&GpuTextureDesc {
                usage: desc.usage | GpuTextureUsage::CPU_UPLOAD,
                ..desc
            });
            gpu.device.upload_texture(&output, &threshold_pixels);
            self.published_threshold_texture = Some(output);
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

    fn inner_tile(pixels: &[u8], code: u8) -> Vec<u8> {
        let slot = u32::from(code) - ASCII_FIRST;
        let x = slot % ATLAS_COLUMNS * ATLAS_TILE_WIDTH + INNER_X0;
        let y = slot / ATLAS_COLUMNS * ATLAS_TILE_HEIGHT + INNER_Y0;
        (0..INNER_HEIGHT)
            .flat_map(|row| {
                let start = (y as usize + row) * ATLAS_WIDTH as usize + x as usize;
                pixels[start..start + INNER_WIDTH].iter().copied()
            })
            .collect()
    }

    fn inner_coverage(coverage: &[u8], code: u8) -> Vec<u8> {
        inner_tile(coverage, code)
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

    #[test]
    fn threshold_ranks_are_uniform_and_ink_precedes_background() {
        let coverage = build_atlas_pixels(&mut TextRasterizer::new());
        let thresholds = build_threshold_atlas_pixels(&coverage);
        for code in [b' ', b'A', b'g', b'_'] {
            let ranks = inner_tile(&thresholds, code);
            assert_eq!(ranks.iter().min(), Some(&0));
            assert_eq!(ranks.iter().max(), Some(&255));
            let buckets = ranks.iter().fold([0usize; 256], |mut buckets, &rank| {
                buckets[rank as usize] += 1;
                buckets
            });
            let min_bucket = *buckets.iter().min().unwrap();
            let max_bucket = *buckets.iter().max().unwrap();
            assert!(
                max_bucket - min_bucket <= 1,
                "rank distribution is not uniform for {code:?}: {min_bucket}..{max_bucket}"
            );

            let ink = inner_coverage(&coverage, code);
            if let (Some(min_ink), Some(max_background)) = (
                ranks
                    .iter()
                    .zip(&ink)
                    .filter_map(|(&rank, &coverage)| (coverage > 0).then_some(rank))
                    .min(),
                ranks
                    .iter()
                    .zip(&ink)
                    .filter_map(|(&rank, &coverage)| (coverage == 0).then_some(rank))
                    .max(),
            ) {
                assert!(
                    min_ink >= max_background,
                    "ink must rank above background for {code:?}"
                );
            }
        }
    }

    #[test]
    fn threshold_ranks_follow_coverage_and_clamp_tile_edges() {
        let coverage = build_atlas_pixels(&mut TextRasterizer::new());
        let thresholds = build_threshold_atlas_pixels(&coverage);
        let ink = inner_coverage(&coverage, b'A');
        let ranks = inner_tile(&thresholds, b'A');
        let mut ranked: Vec<(u8, u8)> = ranks.into_iter().zip(ink).collect();
        ranked.sort_unstable_by_key(|&(rank, _)| std::cmp::Reverse(rank));
        for window in ranked.windows(2) {
            assert!(
                window[0].0 == window[1].0 || window[0].1 >= window[1].1,
                "coverage must be monotonic in descending threshold rank"
            );
        }

        let slot = u32::from(b'A') - ASCII_FIRST;
        let tile_x = slot % ATLAS_COLUMNS * ATLAS_TILE_WIDTH;
        let tile_y = slot / ATLAS_COLUMNS * ATLAS_TILE_HEIGHT;
        for y in 0..ATLAS_TILE_HEIGHT {
            let row = ((tile_y + y) * ATLAS_WIDTH + tile_x) as usize;
            assert_eq!(thresholds[row], thresholds[row + INNER_X0 as usize]);
            assert_eq!(
                thresholds[row + (ATLAS_TILE_WIDTH - 1) as usize],
                thresholds[row + (INNER_X1 - 1) as usize]
            );
        }
    }
}
