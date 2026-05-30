// node.gradient_central_diff_3d — 6-tap central-difference gradient of
// a scalar density Texture3D, written as a vec3 Texture3D.
//
// Per voxel: gradient = float3(dx, dy, dz) * 0.5 in integer voxel space,
// with toroidal wrap (XY use vol_res, Z uses vol_depth). Bit-exact with
// the gradient half of the legacy fused fluid_gradient_curl_3d pass —
// the curl + slope combine is the separate node.curl_slope_force_3d atom
// downstream, so the two compose into the FluidSim3D force field.

struct U {
    vol_res:   u32,
    vol_depth: u32,
    _pad0:     u32,
    _pad1:     u32,
};

@group(0) @binding(0) var<uniform> params: U;
@group(0) @binding(1) var density: texture_3d<f32>;
@group(0) @binding(2) var gradient_out: texture_storage_3d<rgba16float, write>;

@compute @workgroup_size(8, 8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let vr    = params.vol_res;
    let depth = params.vol_depth;
    if id.x >= vr || id.y >= vr || id.z >= depth {
        return;
    }

    let c   = vec3<i32>(i32(id.x), i32(id.y), i32(id.z));
    let ivr = i32(vr);
    let idp = i32(depth);

    // 6-tap central differences with toroidal wrap (XY use vol_res, Z
    // uses vol_depth) — matches the legacy fluid_gradient_curl_3d taps.
    let dx = textureLoad(density, vec3<i32>(((c.x + 1) % ivr + ivr) % ivr, c.y, c.z), 0).r
           - textureLoad(density, vec3<i32>(((c.x - 1) % ivr + ivr) % ivr, c.y, c.z), 0).r;
    let dy = textureLoad(density, vec3<i32>(c.x, ((c.y + 1) % ivr + ivr) % ivr, c.z), 0).r
           - textureLoad(density, vec3<i32>(c.x, ((c.y - 1) % ivr + ivr) % ivr, c.z), 0).r;
    let dz = textureLoad(density, vec3<i32>(c.x, c.y, ((c.z + 1) % idp + idp) % idp), 0).r
           - textureLoad(density, vec3<i32>(c.x, c.y, ((c.z - 1) % idp + idp) % idp), 0).r;

    // gradient = float3(dx, dy, dz) * 0.5 — integer voxel-space central
    // difference scale (legacy line 43).
    let gradient = vec3<f32>(dx, dy, dz) * 0.5;

    textureStore(gradient_out, vec3<i32>(c), vec4<f32>(gradient, 0.0));
}
