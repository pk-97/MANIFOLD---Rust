// node.fbm_2d — octave-summed Perlin (fractional Brownian motion).
//
// Pure generator. fBM aggregates `octaves` octaves of 2D Perlin
// with frequency *= lacunarity and amplitude *= persistence each
// octave. The result has richer, more natural-looking detail than
// single-octave noise. Output remapped to [0, 1] and broadcast to
// RGB (A = 1).

struct Uniforms {
    scale:       f32,
    offset_x:    f32,
    offset_y:    f32,
    octaves:     i32,
    lacunarity:  f32,
    persistence: f32,
    _pad0:       f32,
    _pad1:       f32,
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
    let uu = vec2<f32>(fade(f.x), fade(f.y));

    let g00 = perlin2_grad(perlin2_hash(i.x,     i.y    ));
    let g10 = perlin2_grad(perlin2_hash(i.x + 1, i.y    ));
    let g01 = perlin2_grad(perlin2_hash(i.x,     i.y + 1));
    let g11 = perlin2_grad(perlin2_hash(i.x + 1, i.y + 1));

    let n00 = dot(g00, f - vec2<f32>(0.0, 0.0));
    let n10 = dot(g10, f - vec2<f32>(1.0, 0.0));
    let n01 = dot(g01, f - vec2<f32>(0.0, 1.0));
    let n11 = dot(g11, f - vec2<f32>(1.0, 1.0));

    let nx0 = mix(n00, n10, uu.x);
    let nx1 = mix(n01, n11, uu.x);
    return mix(nx0, nx1, uu.y);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    var p = uv * u.scale + vec2<f32>(u.offset_x, u.offset_y);

    var total = 0.0;
    var amplitude = 1.0;
    var amp_sum = 0.0;
    let max_octaves = clamp(u.octaves, 1, 8);
    for (var i = 0; i < max_octaves; i = i + 1) {
        total = total + perlin2(p) * amplitude;
        amp_sum = amp_sum + amplitude;
        p = p * u.lacunarity;
        amplitude = amplitude * u.persistence;
    }
    // Normalize raw fBM to [-0.7071, 0.7071] approx, then remap [0,1]
    let raw = total / max(amp_sum, 1e-5);
    let v = clamp(0.5 + raw * 0.7071067811865475, 0.0, 1.0);
    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(v, v, v, 1.0));
}
