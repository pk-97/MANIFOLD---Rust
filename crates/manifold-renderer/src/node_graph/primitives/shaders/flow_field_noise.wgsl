// node.flow_field_noise — generate a 2D flow vector field from
// domain-warped fBM Perlin noise. Output is RG packed (or RB —
// Watercolor uses RB for displacement, this primitive matches by
// writing R = flow_x, B = flow_y, G/A = 0/1 for compatibility).
//
// Bit-exact extract of the Watercolor mode-2 (Flow Map Generation)
// pass. Pure generator — zero inputs.

struct FlowFieldUniforms {
    time:       f32,    // seconds (drives slow noise evolution)
    z_scale:    f32,    // multiplier for noise Z (default 0.01)
    warp_scale: f32,    // domain-warp amplitude (default 0.5)
    _pad0:      f32,
};

@group(0) @binding(0) var<uniform> u: FlowFieldUniforms;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

// 3D Perlin noise (adapted from particle_common / Watercolor — same
// gradient table, same quintic fade).

fn wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn perlin3_hash(ix: i32, iy: i32, iz: i32) -> u32 {
    let x = u32(ix + 10000) * 73856093u;
    let y = u32(iy + 10000) * 19349663u;
    let z = u32(iz + 10000) * 83492791u;
    return wang_hash(x ^ y ^ z);
}

fn perlin3_grad(h: u32) -> vec3<f32> {
    let sel = h & 15u;
    switch sel {
        case 0u:  { return vec3<f32>( 1.0,  1.0,  0.0); }
        case 1u:  { return vec3<f32>(-1.0,  1.0,  0.0); }
        case 2u:  { return vec3<f32>( 1.0, -1.0,  0.0); }
        case 3u:  { return vec3<f32>(-1.0, -1.0,  0.0); }
        case 4u:  { return vec3<f32>( 1.0,  0.0,  1.0); }
        case 5u:  { return vec3<f32>(-1.0,  0.0,  1.0); }
        case 6u:  { return vec3<f32>( 1.0,  0.0, -1.0); }
        case 7u:  { return vec3<f32>(-1.0,  0.0, -1.0); }
        case 8u:  { return vec3<f32>( 0.0,  1.0,  1.0); }
        case 9u:  { return vec3<f32>( 0.0, -1.0,  1.0); }
        case 10u: { return vec3<f32>( 0.0,  1.0, -1.0); }
        case 11u: { return vec3<f32>( 0.0, -1.0, -1.0); }
        case 12u: { return vec3<f32>( 1.0,  1.0,  0.0); }
        case 13u: { return vec3<f32>(-1.0,  1.0,  0.0); }
        case 14u: { return vec3<f32>( 0.0, -1.0,  1.0); }
        default:  { return vec3<f32>( 0.0, -1.0, -1.0); }
    }
}

fn fade(t: f32) -> f32 { return t * t * t * (t * (t * 6.0 - 15.0) + 10.0); }

fn perlin3(p: vec3<f32>) -> f32 {
    let i = vec3<i32>(floor(p));
    let f = p - floor(p);
    let u = vec3<f32>(fade(f.x), fade(f.y), fade(f.z));

    let g000 = perlin3_grad(perlin3_hash(i.x,     i.y,     i.z    ));
    let g100 = perlin3_grad(perlin3_hash(i.x + 1, i.y,     i.z    ));
    let g010 = perlin3_grad(perlin3_hash(i.x,     i.y + 1, i.z    ));
    let g110 = perlin3_grad(perlin3_hash(i.x + 1, i.y + 1, i.z    ));
    let g001 = perlin3_grad(perlin3_hash(i.x,     i.y,     i.z + 1));
    let g101 = perlin3_grad(perlin3_hash(i.x + 1, i.y,     i.z + 1));
    let g011 = perlin3_grad(perlin3_hash(i.x,     i.y + 1, i.z + 1));
    let g111 = perlin3_grad(perlin3_hash(i.x + 1, i.y + 1, i.z + 1));

    let n000 = dot(g000, f - vec3<f32>(0.0, 0.0, 0.0));
    let n100 = dot(g100, f - vec3<f32>(1.0, 0.0, 0.0));
    let n010 = dot(g010, f - vec3<f32>(0.0, 1.0, 0.0));
    let n110 = dot(g110, f - vec3<f32>(1.0, 1.0, 0.0));
    let n001 = dot(g001, f - vec3<f32>(0.0, 0.0, 1.0));
    let n101 = dot(g101, f - vec3<f32>(1.0, 0.0, 1.0));
    let n011 = dot(g011, f - vec3<f32>(0.0, 1.0, 1.0));
    let n111 = dot(g111, f - vec3<f32>(1.0, 1.0, 1.0));

    let nx00 = mix(n000, n100, u.x);
    let nx10 = mix(n010, n110, u.x);
    let nx01 = mix(n001, n101, u.x);
    let nx11 = mix(n011, n111, u.x);
    let nxy0 = mix(nx00, nx10, u.y);
    let nxy1 = mix(nx01, nx11, u.y);
    return mix(nxy0, nxy1, u.z);
}

fn fbm(p: vec3<f32>) -> f32 {
    var total = 0.0;
    var amplitude = 1.0;
    var freq = 1.0;
    for (var i = 0; i < 4; i++) {
        total += perlin3(p * freq) * amplitude;
        amplitude *= 0.5;
        freq *= 2.0;
    }
    return total;
}

fn flow_noise(uv: vec2<f32>, z: f32) -> vec2<f32> {
    let p = vec3<f32>(uv * 4.0, z);
    // Domain warp: shift noise input by another noise sample.
    let warp = vec2<f32>(fbm(p), fbm(p + vec3<f32>(5.2, 1.3, 0.0))) * u.warp_scale;
    let pw = p + vec3<f32>(warp, 0.0);
    return vec2<f32>(
        fbm(pw + vec3<f32>(1.7, 9.2, 0.0)),
        fbm(pw + vec3<f32>(8.3, 2.8, 0.0)),
    );
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let z = u.time * u.z_scale;
    let flow = flow_noise(uv, z);
    // R = x flow, G = 0, B = y flow, A = 1.0 — matches Watercolor's
    // packed-RB convention so it composes with anything reading
    // .rb as a flow vector.
    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(flow.x, 0.0, flow.y, 1.0));
}
