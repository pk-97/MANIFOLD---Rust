// 3D gradient + curl force field generation from blurred density volume.
// 6-tap central differences on density, cross with rotating reference axis.

const PI: f32 = 3.14159265;

struct GradientCurl3DUniforms {
    vol_res: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
    flow: f32,
    curl_angle: f32,
    time_val: f32,
    _pad3: f32,
};

@group(0) @binding(0) var<uniform> params: GradientCurl3DUniforms;
@group(0) @binding(1) var density: texture_3d<f32>;
@group(0) @binding(2) var vector_volume: texture_storage_3d<rgba16float, write>;

@compute @workgroup_size(8, 8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let vr = params.vol_res;
    if id.x >= vr || id.y >= vr || id.z >= vr {
        return;
    }

    let coord = vec3<i32>(i32(id.x), i32(id.y), i32(id.z));
    let ivr = i32(vr);

    // 6-tap central differences with toroidal wrap
    let dR = textureLoad(density, vec3<i32>((coord.x + 1) % ivr, coord.y, coord.z), 0).r;
    let dL = textureLoad(density, vec3<i32>((coord.x - 1 + ivr) % ivr, coord.y, coord.z), 0).r;
    let dU = textureLoad(density, vec3<i32>(coord.x, (coord.y + 1) % ivr, coord.z), 0).r;
    let dD = textureLoad(density, vec3<i32>(coord.x, (coord.y - 1 + ivr) % ivr, coord.z), 0).r;
    let dF = textureLoad(density, vec3<i32>(coord.x, coord.y, (coord.z + 1) % ivr), 0).r;
    let dB = textureLoad(density, vec3<i32>(coord.x, coord.y, (coord.z - 1 + ivr) % ivr), 0).r;

    let texel = 1.0 / f32(vr);
    let gradient = vec3<f32>(dR - dL, dU - dD, dF - dB) / (2.0 * texel);

    // Rotating reference axis (time-driven)
    let t = params.time_val;
    let ref_axis = normalize(vec3<f32>(sin(t), cos(0.7 * t), sin(0.5 * t)));

    // Curl = cross(gradient, reference)
    let curl_force = cross(gradient, ref_axis);

    // Decompose from curl angle
    let curl_angle_rad = params.curl_angle * PI / 180.0;
    let curl_strength = params.flow * 500.0 * sin(curl_angle_rad);
    let slope_strength = params.flow * 500.0 * cos(curl_angle_rad);

    let force = curl_force * curl_strength + gradient * slope_strength;

    textureStore(vector_volume, coord, vec4<f32>(force, 0.0));
}
