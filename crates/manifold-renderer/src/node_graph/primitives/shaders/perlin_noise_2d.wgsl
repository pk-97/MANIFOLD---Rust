// node.perlin_noise_2d — classic 2D Perlin gradient noise.
//
// Uses the wang-hash gradient table also used by node.flow_field_noise
// for consistency. Output remapped from raw Perlin range (≈ [-0.7, 0.7])
// to [0, 1] for storage convenience, broadcast to RGB (A = 1).
//
// Aesthetic differs from simplex: square-grid artifacts visible at low
// scales, smoother lobes — same family, different feel.

struct Uniforms {
    scale:    f32,
    offset_x: f32,
    offset_y: f32,
    _pad:     f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
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

fn perlin2_hash(ix: i32, iy: i32) -> u32 {
    let x = u32(ix + 10000) * 73856093u;
    let y = u32(iy + 10000) * 19349663u;
    return wang_hash(x ^ y);
}

fn perlin2_grad(h: u32) -> vec2<f32> {
    let sel = h & 7u;
    switch sel {
        case 0u: { return vec2<f32>( 1.0,  0.0); }
        case 1u: { return vec2<f32>(-1.0,  0.0); }
        case 2u: { return vec2<f32>( 0.0,  1.0); }
        case 3u: { return vec2<f32>( 0.0, -1.0); }
        case 4u: { return vec2<f32>( 0.70710677,  0.70710677); }
        case 5u: { return vec2<f32>(-0.70710677,  0.70710677); }
        case 6u: { return vec2<f32>( 0.70710677, -0.70710677); }
        default: { return vec2<f32>(-0.70710677, -0.70710677); }
    }
}

fn fade(t: f32) -> f32 { return t * t * t * (t * (t * 6.0 - 15.0) + 10.0); }

fn perlin2(p: vec2<f32>) -> f32 {
    let i = vec2<i32>(floor(p));
    let f = p - floor(p);
    let u = vec2<f32>(fade(f.x), fade(f.y));

    let g00 = perlin2_grad(perlin2_hash(i.x,     i.y    ));
    let g10 = perlin2_grad(perlin2_hash(i.x + 1, i.y    ));
    let g01 = perlin2_grad(perlin2_hash(i.x,     i.y + 1));
    let g11 = perlin2_grad(perlin2_hash(i.x + 1, i.y + 1));

    let n00 = dot(g00, f - vec2<f32>(0.0, 0.0));
    let n10 = dot(g10, f - vec2<f32>(1.0, 0.0));
    let n01 = dot(g01, f - vec2<f32>(0.0, 1.0));
    let n11 = dot(g11, f - vec2<f32>(1.0, 1.0));

    let nx0 = mix(n00, n10, u.x);
    let nx1 = mix(n01, n11, u.x);
    return mix(nx0, nx1, u.y);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let p = uv * u.scale + vec2<f32>(u.offset_x, u.offset_y);
    let n = perlin2(p);
    // Raw Perlin range ≈ [-0.707, 0.707]. Multiply by 1/√2 ≈ 0.707
    // before remapping so the histogram fills [0, 1] reasonably.
    let v = clamp(0.5 + n * 0.7071067811865475, 0.0, 1.0);
    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(v, v, v, 1.0));
}
