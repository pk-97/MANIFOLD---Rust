// node.hash_noise_field_2d — high-frequency white-ish noise from a
// per-pixel wang_hash on quantised UV coordinates. R channel = noise
// value in [0, 1]; GBA = (0, 0, 1).
//
// Output is uncorrelated per pixel (NOT a smooth noise like
// simplex / Perlin). Useful for film grain, dust, dither sources, LIC
// ink for line-integral-convolution renders, hash-based dithering.
//
// `scale` controls cell density (higher = finer grain). Offsets shift
// the seed; animate them with an LFO for a flowing dither.
//
// Bindings:
//   @binding(0) uniforms (16 bytes)
//   @binding(1) output_tex (rgba16float storage)

struct Uniforms {
    scale: f32,
    offset_x: f32,
    offset_y: f32,
    _pad0: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

fn wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let inv = vec2<f32>(1.0) / vec2<f32>(dims);
    let uv = (vec2<f32>(id.xy) + 0.5) * inv;

    let qx = u32(uv.x * uniforms.scale + uniforms.offset_x);
    let qy = u32(uv.y * uniforms.scale + uniforms.offset_y);
    let h = wang_hash(qx * 73856093u ^ qy * 19349663u);
    let n = f32(h) / 4294967296.0;
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(n, 0.0, 0.0, 1.0));
}
