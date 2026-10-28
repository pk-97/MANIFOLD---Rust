// node.motion_blur — shared by its tile, neighbour and gather passes. The
// Rust side prepends depth_common.wgsl and this file to each pass.

// Tile edge in output pixels; `TILE` in motion_blur.rs matches.
const TILE: u32 = 16u;

struct Uniforms {
    // Output size in pixels.
    dims: vec2<f32>,
    // shutter_angle / 360: the fraction of the frame the shutter is open.
    shutter_scale: f32,
    // Longest smear in pixels.
    max_blur_px: f32,
    near: f32,
    far: f32,
    // Neighbour search radius in tiles, from max_blur_px.
    reach_tiles: u32,
    _pad: u32,
}

fn own_texel(uv: vec2<f32>, tex_dims: vec2<u32>) -> vec2<i32> {
    return clamp(vec2<i32>(floor(uv * vec2<f32>(tex_dims))), vec2<i32>(0), vec2<i32>(tex_dims) - vec2<i32>(1));
}

// Half the shutter's smear in output pixels, texture space (y down).
// `ndc` is the scene's move since last frame in NDC (y up). The smear is
// centred on the pixel, so it reaches half its length each way. Its full
// length is capped at max_blur_px without bending its direction.
fn spread(ndc: vec2<f32>, dims: vec2<f32>, shutter_scale: f32, max_blur_px: f32) -> vec2<f32> {
    let smear = vec2<f32>(ndc.x, -ndc.y) * 0.5 * dims * shutter_scale;
    let len = length(smear);
    let capped = select(smear, smear * (max_blur_px / len), len > max_blur_px);
    return capped * 0.5;
}
