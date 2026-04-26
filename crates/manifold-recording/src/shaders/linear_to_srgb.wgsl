// Linear Rgba16Float → sRGB Bgra8Unorm conversion for live recording.
// Reads the compositor output (linear light, post-tonemap) and writes
// sRGB-gamma-corrected output for HEVC Main (8-bit) encoding.
//
// Triangular-PDF dither at ±1 LSB amplitude breaks up the 8-bit
// quantization staircase on smooth gradients. The eye averages the
// noise back to smooth, but the encoder no longer sees hard contour
// boundaries that would otherwise produce visible banding on opacity
// blends, glow falloff, and slow gradients. Spatial-only (no temporal
// component) so static content doesn't shimmer.

@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<bgra8unorm, write>;

// Cheap per-pixel hash → 3 independent uniform [0, 1) values.
fn hash3(p: vec2<u32>, seed: u32) -> vec3<f32> {
    let q1 = (p.x * 1664525u + p.y * 1013904223u + seed) ^ 0x9E3779B9u;
    let q2 = (p.y * 1664525u + p.x * 1013904223u + seed) ^ 0x85EBCA6Bu;
    let q3 = (p.x * 2246822519u + p.y * 3266489917u + seed) ^ 0xCC9E2D51u;
    return vec3<f32>(
        f32(q1) / 4294967296.0,
        f32(q2) / 4294967296.0,
        f32(q3) / 4294967296.0,
    );
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(t_source);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    var c = textureLoad(t_source, vec2<i32>(gid.xy), 0);

    // Linear → sRGB gamma (same as the Obj-C kCopyShaderSDR).
    c = vec4<f32>(
        pow(max(c.r, 0.0), 1.0 / 2.2),
        pow(max(c.g, 0.0), 1.0 / 2.2),
        pow(max(c.b, 0.0), 1.0 / 2.2),
        c.a,
    );

    // Triangular PDF dither (sum of two uniform [0,1) minus 1.0 → triangular
    // in [-1, 1)) at 1/255 amplitude. Applied before bgra8unorm quantization.
    let n1 = hash3(gid.xy, 0u);
    let n2 = hash3(gid.xy, 1u);
    let dither = (n1 + n2 - vec3<f32>(1.0)) / 255.0;
    c = vec4<f32>(c.rgb + dither, c.a);

    textureStore(output_tex, vec2<i32>(gid.xy), c);
}
