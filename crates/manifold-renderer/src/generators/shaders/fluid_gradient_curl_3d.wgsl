// 3D gradient + curl force field generation from blurred density volume.
// Line-by-line translation of FluidGradientCurl3D.compute.
//
// 6-tap central differences on density, cross with rotating reference axis.
// gradient = float3(dx, dy, dz) * 0.5  (integer voxel space, matches Unity line 43)
// ref axis and curl/slope strengths are precomputed on CPU (C# DispatchGradientCurl).

struct GradientCurl3DUniforms {
    vol_res:   u32,
    vol_depth: u32,
    _pad0:     u32,
    _pad1:     u32,
    curl_strength:  f32,  // flow * 500 * sin(curl_angle_rad)  — precomputed C#-side
    slope_strength: f32,  // flow * 500 * cos(curl_angle_rad)  — precomputed C#-side
    ref_axis_x:     f32,  // normalized rotating reference axis, C#-side: ctx.Time * 0.3
    ref_axis_y:     f32,
    ref_axis_z:     f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<uniform> params: GradientCurl3DUniforms;
@group(0) @binding(1) var density: texture_3d<f32>;
@group(0) @binding(2) var vector_volume: texture_storage_3d<rgba16float, write>;

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

    // 6-tap central differences with toroidal wrap (XY use vol_res, Z uses vol_depth)
    // matches FluidGradientCurl3D.compute lines 36-41
    let dx = textureLoad(density, vec3<i32>(((c.x + 1) % ivr + ivr) % ivr, c.y, c.z), 0).r
           - textureLoad(density, vec3<i32>(((c.x - 1) % ivr + ivr) % ivr, c.y, c.z), 0).r;
    let dy = textureLoad(density, vec3<i32>(c.x, ((c.y + 1) % ivr + ivr) % ivr, c.z), 0).r
           - textureLoad(density, vec3<i32>(c.x, ((c.y - 1) % ivr + ivr) % ivr, c.z), 0).r;
    let dz = textureLoad(density, vec3<i32>(c.x, c.y, ((c.z + 1) % idp + idp) % idp), 0).r
           - textureLoad(density, vec3<i32>(c.x, c.y, ((c.z - 1) % idp + idp) % idp), 0).r;

    // gradient = float3(dx, dy, dz) * 0.5  — integer voxel space central difference scale
    // Unity FluidGradientCurl3D.compute line 43: float3 gradient = float3(dx, dy, dz) * 0.5;
    let gradient = vec3<f32>(dx, dy, dz) * 0.5;

    // ref_axis precomputed on CPU from ctx.Time * 0.3 (DIFF-8)
    let ref_axis = vec3<f32>(params.ref_axis_x, params.ref_axis_y, params.ref_axis_z);

    // Curl: cross(gradient, ref_axis) — tangential flow around density peaks
    let curl_force = cross(gradient, ref_axis);

    // Combined force: curl (tangential orbit) + slope (radial push/pull)
    // curl_strength and slope_strength precomputed on CPU from flow * FORCE_SCALE * sin/cos
    let force = curl_force * params.curl_strength + gradient * params.slope_strength;

    textureStore(vector_volume, vec3<i32>(c), vec4<f32>(force, 0.0));
}
