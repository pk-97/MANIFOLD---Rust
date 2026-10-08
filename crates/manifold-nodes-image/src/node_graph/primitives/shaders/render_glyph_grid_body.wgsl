// `node.render_glyph_grid` fusable body. `atlas` is a Gather input so the
// body computes a dependent atlas coordinate for each terminal cell. `cells`
// is BufferIndex and therefore arrives as the generated `buf_cells` storage
// array of bare u32 elements (single-channel arrays use the canonical value
// scalar representation).

const ATLAS_WIDTH: f32 = 512.0;
const ATLAS_HEIGHT: f32 = 288.0;
const TILE_WIDTH: f32 = 32.0;
const TILE_HEIGHT: f32 = 48.0;
const ATLAS_COLUMNS: u32 = 16u;

fn body(
    atlas: texture_2d<f32>,
    atlas_sampler: sampler,
    uv: vec2<f32>,
    dims: vec2<f32>,
    columns: f32,
    rows: f32,
) -> vec4<f32> {
    // Keep the scalar-to-index conversion bounded even if a live control wire
    // supplies a value outside the editor's nominal parameter range.
    let cols = u32(clamp(round(columns), 1.0, 4096.0));
    let row_count = u32(clamp(round(rows), 1.0, 4096.0));
    let grid = vec2<f32>(f32(cols), f32(row_count));
    let cell = min(floor(uv * grid), grid - vec2<f32>(1.0));
    let cell_index = u32(cell.y) * cols + u32(cell.x);

    let cell_count = arrayLength(&buf_cells);
    if (cell_index >= cell_count) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let codepoint = buf_cells[cell_index];
    if (codepoint < 32u || codepoint > 127u) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let slot = codepoint - 32u;
    let local = fract(uv * grid);
    let tile = vec2<f32>(f32(slot % ATLAS_COLUMNS), f32(slot / ATLAS_COLUMNS));

    // Discard excess atlas padding, preserving every glyph's common baseline.
    // The original full-tile stretch made terminal text look letter-spaced.
    // This inner 26×44 rectangle retains the ink and stays inside the tile.
    let atlas_px = tile * vec2<f32>(TILE_WIDTH, TILE_HEIGHT)
        + vec2<f32>(2.5)
        + local * vec2<f32>(25.0, 43.0);
    let atlas_uv = atlas_px / vec2<f32>(ATLAS_WIDTH, ATLAS_HEIGHT);
    let coverage = textureSampleLevel(atlas, atlas_sampler, atlas_uv, 0.0).r;
    return vec4<f32>(coverage, coverage, coverage, 1.0);
}
