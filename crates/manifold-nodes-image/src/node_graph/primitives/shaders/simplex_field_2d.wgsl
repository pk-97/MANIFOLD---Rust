// node.simplex_field_2d — 3D Perlin-style simplex noise sampled at
// `(uv * scale + offset, z)`. Output is the raw signed noise value in
// approximately [-1, +1], written to the R channel; GBA = (0, 0, 1).
//
// Distinct from `node.noise` (Simplex):
//   - This is 3D: the `z` axis lets a single static node sample an
//     evolving noise field (animate `z` over time = turbulent shimmer
//     in place, vs `node.noise`'s offset_x/y which pans through
//     a static field).
//   - Output is SIGNED — caller scales/biases with downstream gain /
//     scale_offset_texture as needed. `simplex_noise_2d` is remapped to
//     [0, 1] and broadcast to RGB for direct visual use.
//
// The implementation is a direct copy of `simplex_noise_3d()` from
// `generators/shaders/particle_common.wgsl` so the primitive is
// self-contained (no shared-header include).
//
// Bindings:
//   @binding(0) uniforms (32 bytes)
//   @binding(1) output_tex (rgba16float storage)

struct Uniforms {
    scale_x: f32,
    scale_y: f32,
    offset_x: f32,
    offset_y: f32,
    z: f32,
    output_channel: u32,   // 0=R, 1=G, 2=B, 3=A — which channel of the
                            // output texture receives the noise value;
                            // other channels are 0.
    _pad0: f32,
    _pad1: f32,
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

fn perlin3_hash(ix: i32, iy: i32, iz: i32) -> u32 {
    let x = u32(ix + 10000) * 73856093u;
    let y = u32(iy + 10000) * 19349663u;
    let z = u32(iz + 10000) * 83492791u;
    return wang_hash(x ^ y ^ z);
}

fn perlin3_grad(h: u32) -> vec3<f32> {
    // 16 gradient directions on cube edges (12 unique + 4 repeated for
    // power-of-two hash). Ken Perlin's "improved noise" table.
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
    // Perlin quintic fade: 6t^5 - 15t^4 + 10t^3
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

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let p = vec3<f32>(
        uv.x * uniforms.scale_x + uniforms.offset_x,
        uv.y * uniforms.scale_y + uniforms.offset_y,
        uniforms.z,
    );
    let n = simplex_noise_3d(p);
    var out_col = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    switch uniforms.output_channel {
        case 0u: { out_col.r = n; }
        case 1u: { out_col.g = n; }
        case 2u: { out_col.b = n; }
        default: { out_col.a = n; }
    }
    textureStore(output_tex, vec2<i32>(id.xy), out_col);
}
