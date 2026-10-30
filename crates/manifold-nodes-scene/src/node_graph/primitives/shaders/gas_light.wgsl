// The gas renderer's compute passes. Standalone module; `gas_volume.rs` binds
// each entry point with only the bindings it names.
//
//   cs_gas_unpack: the solver's packed fields into the 3D textures the march
//                  samples (hardware trilinear needs textures).
//   cs_gas_light:  the optical depth toward the sun, and straight up, at every
//                  cell of a light volume spanning the gas box.
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

@compute @workgroup_size(4, 4, 4)
fn cs_gas_unpack(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = unpack_params.dims.xyz;
    if any(gid >= dims) { return; }
    let word = 2u * (gid.x + dims.x * (gid.y + dims.y * gid.z));
    let density_temperature = unpack2x16float(packed_state[word]);
    let fuel_flame = unpack2x16float(packed_state[word + 1u]);
    let velocity_uv = unpack2x16float(packed_velocity[word]);
    let velocity_w = unpack2x16float(packed_velocity[word + 1u]);
    let cell = vec3<i32>(gid);
    textureStore(state_out, cell, vec4<f32>(density_temperature, fuel_flame));
    textureStore(velocity_out, cell, vec4<f32>(velocity_uv, velocity_w.x, 0.0));
}

// ---- cs_gas_light ----------------------------------------------------------

struct GasLightUniforms {
    dims: vec4<u32>,     // xyz light volume cells, w the most steps a ray takes
    box_min: vec4<f32>,  // xyz box min corner, w extinction per metre at density 1
    box_max: vec4<f32>,  // xyz box max corner, w 1 when a sun lights the gas
    sun: vec4<f32>,      // xyz unit direction toward the sun, w the state mip one cell across
};

@group(0) @binding(0) var<uniform> light: GasLightUniforms;
@group(0) @binding(1) var state: texture_3d<f32>;
@group(0) @binding(2) var state_sampler: sampler;
@group(0) @binding(3) var light_out: texture_storage_3d<rgba16float, write>;

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
        sum += textureSampleLevel(state, state_sampler, uvw, light.sun.w).x;
    }
    return sum * ds * light.box_min.w;
}

@compute @workgroup_size(4, 4, 4)
fn cs_gas_light(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = light.dims.xyz;
    if any(gid >= dims) { return; }
    let size = light.box_max.xyz - light.box_min.xyz;
    let p = light.box_min.xyz + (vec3<f32>(gid) + 0.5) / vec3<f32>(dims) * size;
    let most = max(light.dims.w, 1u);
    let tau_up = optical_depth(p, vec3<f32>(0.0, 1.0, 0.0), most);
    var tau_sun = tau_up;
    if light.box_max.w > 0.5 {
        tau_sun = optical_depth(p, light.sun.xyz, most);
    }
    textureStore(light_out, vec3<i32>(gid), vec4<f32>(tau_sun, tau_up, 0.0, 0.0));
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
