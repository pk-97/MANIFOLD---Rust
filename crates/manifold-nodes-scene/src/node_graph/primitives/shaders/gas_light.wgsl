// The gas renderer's compute passes. Standalone module; `gas_volume.rs` binds
// each entry point with only the bindings it names.
//
//   cs_gas_unpack: the solver's packed fields into the 3D textures the march
//                  samples (hardware trilinear needs textures), and a copy of
//                  the state at half size, each texel the mean of its eight.
//   cs_gas_glow:   the flames' mean over blocks of the half copy, for the glow.
//   cs_gas_light:  the optical depth toward the sun, and straight up, at every
//                  cell of a light volume spanning the gas box, and the glow.
//   cs_gas_noise:  the detail noise's bytes into its 3D texture, once.
//
// Binding numbers differ across entry points so the module never holds two
// resources at one number.

// ---- cs_gas_unpack ---------------------------------------------------------

struct GasUnpackUniforms {
    dims: vec4<u32>,  // xyz cells, w unused
};

@group(0) @binding(4) var<uniform> unpack_params: GasUnpackUniforms;
// Two words a cell in order x + nx * (y + ny * z): half floats (density,
// temperature) then (fuel, flame).
@group(0) @binding(5) var<storage, read> packed_state: array<u32>;
// Two words a cell: half floats (u, v) then (w, 0), metres per second.
@group(0) @binding(6) var<storage, read> packed_velocity: array<u32>;
@group(0) @binding(7) var state_out: texture_storage_3d<rgba16float, write>;
@group(0) @binding(8) var velocity_out: texture_storage_3d<rgba16float, write>;
@group(0) @binding(14) var half_out: texture_storage_3d<rgba16float, write>;

// One thread a half-size texel: its eight cells, then their mean.
@compute @workgroup_size(4, 4, 4)
fn cs_gas_unpack(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = unpack_params.dims.xyz;
    if any(gid >= textureDimensions(half_out)) { return; }
    var sum = vec4<f32>(0.0);
    var count = 0.0;
    for (var child = 0u; child < 8u; child++) {
        let g = 2u * gid + vec3<u32>(child & 1u, (child >> 1u) & 1u, child >> 2u);
        if any(g >= dims) { continue; }
        let word = 2u * (g.x + dims.x * (g.y + dims.y * g.z));
        let state = vec4<f32>(
            unpack2x16float(packed_state[word]),
            unpack2x16float(packed_state[word + 1u]),
        );
        let velocity_uv = unpack2x16float(packed_velocity[word]);
        let velocity_w = unpack2x16float(packed_velocity[word + 1u]);
        let cell = vec3<i32>(g);
        textureStore(state_out, cell, state);
        textureStore(velocity_out, cell, vec4<f32>(velocity_uv, velocity_w.x, 0.0));
        sum += state;
        count += 1.0;
    }
    textureStore(half_out, vec3<i32>(gid), sum / count);
}

// ---- cs_gas_glow -----------------------------------------------------------

struct GasGlowUniforms {
    dims: vec4<u32>,  // xyz glow texels, w half-size texels per glow texel along each axis
};

@group(0) @binding(11) var<uniform> glow_params: GasGlowUniforms;
@group(0) @binding(12) var glow_out: texture_storage_3d<r16float, write>;

// Each linear tap at the corner shared by a 2x2x2 block of half-size texels
// is their mean, so a glow texel takes an eighth of the reads.
@compute @workgroup_size(4, 4, 4)
fn cs_gas_glow(@builtin(global_invocation_id) gid: vec3<u32>) {
    if any(gid >= glow_params.dims.xyz) { return; }
    let half_dims = textureDimensions(state);
    let side = glow_params.dims.w;
    let origin = gid * side;
    let taps = side / 2u;
    var sum = 0.0;
    var count = 0.0;
    for (var z = 0u; z < taps; z++) {
        for (var y = 0u; y < taps; y++) {
            for (var x = 0u; x < taps; x++) {
                let low = origin + 2u * vec3<u32>(x, y, z);
                if any(low >= half_dims) { continue; }
                let uvw = vec3<f32>(low + 1u) / vec3<f32>(half_dims);
                sum += textureSampleLevel(state, state_sampler, uvw, 0.0).w;
                count += 1.0;
            }
        }
    }
    textureStore(glow_out, vec3<i32>(gid), vec4<f32>(sum / max(count, 1.0), 0.0, 0.0, 1.0));
}

// ---- cs_gas_light ----------------------------------------------------------

struct GasLightUniforms {
    dims: vec4<u32>,     // xyz light volume cells, w the most steps a ray takes
    box_min: vec4<f32>,  // xyz box min corner, w extinction per metre at density 1
    box_max: vec4<f32>,  // xyz box max corner, w 1 when a sun lights the gas
    sun: vec4<f32>,      // xyz unit direction toward the sun, w 1 when the flames glow
};

@group(0) @binding(0) var<uniform> light: GasLightUniforms;
// The half-size state; cs_gas_glow reads it too.
@group(0) @binding(1) var state: texture_3d<f32>;
@group(0) @binding(2) var state_sampler: sampler;
@group(0) @binding(3) var light_out: texture_storage_3d<rgba16float, write>;
@group(0) @binding(13) var glow: texture_3d<f32>;

// The distance along `dir` from `p`, inside the box, to the box's far face.
fn box_exit(p: vec3<f32>, dir: vec3<f32>) -> f32 {
    let small = abs(dir) < vec3<f32>(1e-9);
    let safe = select(dir, vec3<f32>(1e-9), small);
    let to_face = select(light.box_min.xyz - p, light.box_max.xyz - p, safe > vec3<f32>(0.0)) / safe;
    return max(min(to_face.x, min(to_face.y, to_face.z)), 0.0);
}

// The extinction integral from `p` along `dir` to the box's far face, in
// equal midpoint samples about one light cell apart, at most `most`. Fewer
// samples than cells along the ray land on different smoke from one cell to
// the next and stripe the light.
fn optical_depth(p: vec3<f32>, dir: vec3<f32>, most: u32) -> f32 {
    let reach = box_exit(p, dir);
    let size = light.box_max.xyz - light.box_min.xyz;
    let cell = size / vec3<f32>(light.dims.xyz);
    let step_length = min(cell.x, min(cell.y, cell.z));
    let steps = clamp(u32(ceil(reach / step_length)), 1u, most);
    let ds = reach / f32(steps);
    var sum = 0.0;
    for (var i = 0u; i < steps; i++) {
        let q = p + dir * ((f32(i) + 0.5) * ds);
        let uvw = (q - light.box_min.xyz) / size;
        sum += textureSampleLevel(state, state_sampler, uvw, 0.0).x;
    }
    return sum * ds * light.box_min.w;
}

// A cubic B-spline read of `tex` from eight trilinear taps (Sigg and Hadwiger
// 2005), for the glow's coarse texels: trilinear there shows their blocks,
// since its slope jumps at every texel wall.
fn sample_smooth(tex: texture_3d<f32>, uvw: vec3<f32>) -> vec4<f32> {
    let size = vec3<f32>(textureDimensions(tex));
    let coord = uvw * size - 0.5;
    let index = floor(coord);
    let f = coord - index;
    let f2 = f * f;
    let f3 = f2 * f;
    let rest = 1.0 - f;
    let w0 = rest * rest * rest / 6.0;
    let w1 = (4.0 - 6.0 * f2 + 3.0 * f3) / 6.0;
    let w3 = f3 / 6.0;
    let g0 = w0 + w1;
    let g1 = 1.0 - g0;
    // Each pair of B-spline weights becomes one linear tap between the
    // pair's texel centres.
    let lo = (index - 0.5 + w1 / g0) / size;
    let hi = (index + 1.5 + w3 / g1) / size;
    let a = mix(
        textureSampleLevel(tex, state_sampler, vec3<f32>(lo.x, lo.y, lo.z), 0.0),
        textureSampleLevel(tex, state_sampler, vec3<f32>(hi.x, lo.y, lo.z), 0.0),
        g1.x,
    );
    let b = mix(
        textureSampleLevel(tex, state_sampler, vec3<f32>(lo.x, hi.y, lo.z), 0.0),
        textureSampleLevel(tex, state_sampler, vec3<f32>(hi.x, hi.y, lo.z), 0.0),
        g1.x,
    );
    let c = mix(
        textureSampleLevel(tex, state_sampler, vec3<f32>(lo.x, lo.y, hi.z), 0.0),
        textureSampleLevel(tex, state_sampler, vec3<f32>(hi.x, lo.y, hi.z), 0.0),
        g1.x,
    );
    let d = mix(
        textureSampleLevel(tex, state_sampler, vec3<f32>(lo.x, hi.y, hi.z), 0.0),
        textureSampleLevel(tex, state_sampler, vec3<f32>(hi.x, hi.y, hi.z), 0.0),
        g1.x,
    );
    return mix(mix(a, b, g1.y), mix(c, d, g1.y), g1.z);
}

@compute @workgroup_size(4, 4, 4)
fn cs_gas_light(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = light.dims.xyz;
    if any(gid >= dims) { return; }
    let size = light.box_max.xyz - light.box_min.xyz;
    let uvw = (vec3<f32>(gid) + 0.5) / vec3<f32>(dims);
    let p = light.box_min.xyz + uvw * size;
    let most = max(light.dims.w, 1u);
    let tau_up = optical_depth(p, vec3<f32>(0.0, 1.0, 0.0), most);
    var tau_sun = tau_up;
    if light.box_max.w > 0.5 {
        tau_sun = optical_depth(p, light.sun.xyz, most);
    }
    var flames = 0.0;
    if light.sun.w > 0.5 {
        flames = sample_smooth(glow, uvw).x;
    }
    textureStore(light_out, vec3<i32>(gid), vec4<f32>(tau_sun, tau_up, flames, 0.0));
}

// ---- cs_gas_noise ----------------------------------------------------------

// One byte a texel in order x + nx * (y + ny * z), four to a word.
@group(0) @binding(9) var<storage, read> noise_bytes: array<u32>;
@group(0) @binding(10) var noise_out: texture_storage_3d<r8unorm, write>;

@compute @workgroup_size(4, 4, 4)
fn cs_gas_noise(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(noise_out);
    if any(gid >= dims) { return; }
    let index = gid.x + dims.x * (gid.y + dims.y * gid.z);
    let byte = (noise_bytes[index >> 2u] >> ((index & 3u) * 8u)) & 255u;
    textureStore(noise_out, vec3<i32>(gid), vec4<f32>(f32(byte) / 255.0, 0.0, 0.0, 1.0));
}
