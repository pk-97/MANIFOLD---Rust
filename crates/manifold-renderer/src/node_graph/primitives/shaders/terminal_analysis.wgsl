// Fixed 64×36 terminal image analysis.
//
// Each output cell uses nine evenly spaced integer texel loads. RGB is the
// average linear colour and alpha stores the local Rec.709 luma contrast
// (max luma minus min luma). Integer coordinates are clamped explicitly so
// narrow sources and edge cells never address outside the source texture.

@group(0) @binding(0) var sourceTexture2D: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> outputSamples: array<vec4<f32>>;

const GRID: vec2<f32> = vec2<f32>(64.0, 36.0);
const LUMA: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);

fn finite_or_zero(value: f32) -> f32 {
    // Comparisons reject both NaN (`value != value`) and infinities while
    // leaving the normal HDR range untouched.
    if value != value || value < -3.4e38 || value > 3.4e38 {
        return 0.0;
    }
    return value;
}

fn load_tap(cell: vec2<f32>, tap: vec2<f32>, dims: vec2<u32>) -> vec3<f32> {
    let max_coord = vec2<i32>(i32(dims.x) - 1, i32(dims.y) - 1);
    let uv = (cell + tap) / GRID;
    let coord = clamp(
        vec2<i32>(i32(uv.x * f32(dims.x)), i32(uv.y * f32(dims.y))),
        vec2<i32>(0, 0),
        max_coord,
    );
    let raw = textureLoad(sourceTexture2D, coord, 0).rgb;
    return vec3<f32>(
        finite_or_zero(raw.r),
        finite_or_zero(raw.g),
        finite_or_zero(raw.b),
    );
}

@compute @workgroup_size(8, 8)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= 64u || id.y >= 36u {
        return;
    }

    let dims = textureDimensions(sourceTexture2D);
    let cell = vec2<f32>(f32(id.x), f32(id.y));
    // One sixth, one half, and five sixths are evenly spaced inside the cell.
    let c00 = load_tap(cell, vec2<f32>(0.16666667, 0.16666667), dims);
    let c10 = load_tap(cell, vec2<f32>(0.5, 0.16666667), dims);
    let c20 = load_tap(cell, vec2<f32>(0.83333333, 0.16666667), dims);
    let c01 = load_tap(cell, vec2<f32>(0.16666667, 0.5), dims);
    let c11 = load_tap(cell, vec2<f32>(0.5, 0.5), dims);
    let c21 = load_tap(cell, vec2<f32>(0.83333333, 0.5), dims);
    let c02 = load_tap(cell, vec2<f32>(0.16666667, 0.83333333), dims);
    let c12 = load_tap(cell, vec2<f32>(0.5, 0.83333333), dims);
    let c22 = load_tap(cell, vec2<f32>(0.83333333, 0.83333333), dims);

    let sum = c00 + c10 + c20 + c01 + c11 + c21 + c02 + c12 + c22;
    let average = sum / 9.0;
    let l00 = dot(c00, LUMA);
    let l10 = dot(c10, LUMA);
    let l20 = dot(c20, LUMA);
    let l01 = dot(c01, LUMA);
    let l11 = dot(c11, LUMA);
    let l21 = dot(c21, LUMA);
    let l02 = dot(c02, LUMA);
    let l12 = dot(c12, LUMA);
    let l22 = dot(c22, LUMA);
    let minimum = min(min(min(l00, l10), min(l20, l01)), min(min(l11, l21), min(l02, min(l12, l22))));
    let maximum = max(max(max(l00, l10), max(l20, l01)), max(max(l11, l21), max(l02, max(l12, l22))));
    let contrast = finite_or_zero(maximum - minimum);

    outputSamples[id.y * 64u + id.x] = vec4<f32>(
        finite_or_zero(average.r),
        finite_or_zero(average.g),
        finite_or_zero(average.b),
        contrast,
    );
}
