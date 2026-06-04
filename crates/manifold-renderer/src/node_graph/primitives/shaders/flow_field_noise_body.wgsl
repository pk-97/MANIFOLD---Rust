// node.flow_field_noise — fusable body (freeze §12), SOURCE. 2D flow vector field
// from domain-warped fBM 3D Perlin noise (R=flow_x, B=flow_y, G=0, A=1, the
// Watercolor flow-map convention). z = time * z_scale evolves it; warp_scale=0
// skips the domain warp. The `resolution` param controls the output SIZE (handled
// Rust-side, not in the shader), so the body ignores it. Helpers verbatim from
// flow_field_noise.wgsl; ffn_flow_noise takes warp_scale as an arg (no global u).
// PARAMS: [time, z_scale, warp_scale, resolution (Enum->u32, ignored here)].
fn ffn_wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn ffn_perlin3_hash(ix: i32, iy: i32, iz: i32) -> u32 {
    let x = u32(ix + 10000) * 73856093u;
    let y = u32(iy + 10000) * 19349663u;
    let z = u32(iz + 10000) * 83492791u;
    return ffn_wang_hash(x ^ y ^ z);
}

fn ffn_perlin3_grad(h: u32) -> vec3<f32> {
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

fn ffn_fade(t: f32) -> f32 { return t * t * t * (t * (t * 6.0 - 15.0) + 10.0); }

fn ffn_perlin3(p: vec3<f32>) -> f32 {
    let i = vec3<i32>(floor(p));
    let f = p - floor(p);
    let u = vec3<f32>(ffn_fade(f.x), ffn_fade(f.y), ffn_fade(f.z));

    let g000 = ffn_perlin3_grad(ffn_perlin3_hash(i.x,     i.y,     i.z    ));
    let g100 = ffn_perlin3_grad(ffn_perlin3_hash(i.x + 1, i.y,     i.z    ));
    let g010 = ffn_perlin3_grad(ffn_perlin3_hash(i.x,     i.y + 1, i.z    ));
    let g110 = ffn_perlin3_grad(ffn_perlin3_hash(i.x + 1, i.y + 1, i.z    ));
    let g001 = ffn_perlin3_grad(ffn_perlin3_hash(i.x,     i.y,     i.z + 1));
    let g101 = ffn_perlin3_grad(ffn_perlin3_hash(i.x + 1, i.y,     i.z + 1));
    let g011 = ffn_perlin3_grad(ffn_perlin3_hash(i.x,     i.y + 1, i.z + 1));
    let g111 = ffn_perlin3_grad(ffn_perlin3_hash(i.x + 1, i.y + 1, i.z + 1));

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

fn ffn_fbm(p: vec3<f32>) -> f32 {
    var total = 0.0;
    var amplitude = 1.0;
    var freq = 1.0;
    for (var i = 0; i < 4; i++) {
        total += ffn_perlin3(p * freq) * amplitude;
        amplitude *= 0.5;
        freq *= 2.0;
    }
    return total;
}

fn ffn_flow_noise(uv: vec2<f32>, z: f32, warp_scale: f32) -> vec2<f32> {
    let p = vec3<f32>(uv * 4.0, z);
    var pw = p;
    if (warp_scale != 0.0) {
        let warp = vec2<f32>(ffn_fbm(p), ffn_fbm(p + vec3<f32>(5.2, 1.3, 0.0))) * warp_scale;
        pw = p + vec3<f32>(warp, 0.0);
    }
    return vec2<f32>(
        ffn_fbm(pw + vec3<f32>(1.7, 9.2, 0.0)),
        ffn_fbm(pw + vec3<f32>(8.3, 2.8, 0.0)),
    );
}

fn body(uv: vec2<f32>, dims: vec2<f32>, time: f32, z_scale: f32, warp_scale: f32, resolution: u32) -> vec4<f32> {
    let z = time * z_scale;
    let flow = ffn_flow_noise(uv, z, warp_scale);
    return vec4<f32>(flow.x, 0.0, flow.y, 1.0);
}
