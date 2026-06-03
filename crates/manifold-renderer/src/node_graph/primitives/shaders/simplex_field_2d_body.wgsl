// node.simplex_field_2d — fusable body (freeze §12), SOURCE. 3D Perlin-style
// simplex noise at (uv*scale + offset, z); the signed value is written to the
// selected output channel. Helpers verbatim from simplex_field_2d.wgsl. PARAMS:
// [scale_x, scale_y, offset_x, offset_y, z, output_channel (Enum -> u32)].
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

fn perlin3_grad_dot(ix: i32, iy: i32, iz: i32, fx: f32, fy: f32, fz: f32) -> f32 {
    let h = perlin3_hash(ix, iy, iz);
    let g = perlin3_grad(h);
    return g.x * fx + g.y * fy + g.z * fz;
}

fn simplex_noise_3d(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);

    let ix = i32(i.x);
    let iy = i32(i.y);
    let iz = i32(i.z);

    let n000 = perlin3_grad_dot(ix,     iy,     iz,     f.x,       f.y,       f.z);
    let n100 = perlin3_grad_dot(ix + 1, iy,     iz,     f.x - 1.0, f.y,       f.z);
    let n010 = perlin3_grad_dot(ix,     iy + 1, iz,     f.x,       f.y - 1.0, f.z);
    let n110 = perlin3_grad_dot(ix + 1, iy + 1, iz,     f.x - 1.0, f.y - 1.0, f.z);
    let n001 = perlin3_grad_dot(ix,     iy,     iz + 1, f.x,       f.y,       f.z - 1.0);
    let n101 = perlin3_grad_dot(ix + 1, iy,     iz + 1, f.x - 1.0, f.y,       f.z - 1.0);
    let n011 = perlin3_grad_dot(ix,     iy + 1, iz + 1, f.x,       f.y - 1.0, f.z - 1.0);
    let n111 = perlin3_grad_dot(ix + 1, iy + 1, iz + 1, f.x - 1.0, f.y - 1.0, f.z - 1.0);

    let nx00 = mix(n000, n100, u.x);
    let nx10 = mix(n010, n110, u.x);
    let nx01 = mix(n001, n101, u.x);
    let nx11 = mix(n011, n111, u.x);
    let nxy0 = mix(nx00, nx10, u.y);
    let nxy1 = mix(nx01, nx11, u.y);
    return mix(nxy0, nxy1, u.z);
}

fn body(uv: vec2<f32>, dims: vec2<f32>, scale_x: f32, scale_y: f32, offset_x: f32, offset_y: f32, z: f32, output_channel: u32) -> vec4<f32> {
    let p = vec3<f32>(
        uv.x * scale_x + offset_x,
        uv.y * scale_y + offset_y,
        z,
    );
    let n = simplex_noise_3d(p);
    var out_col = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    switch output_channel {
        case 0u: { out_col.r = n; }
        case 1u: { out_col.g = n; }
        case 2u: { out_col.b = n; }
        default: { out_col.a = n; }
    }
    return out_col;
}
