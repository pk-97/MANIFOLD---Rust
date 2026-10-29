// node.matter_state — removes the points whose position, velocity, volume
// ratio, affine matrix, deformation or stress is not finite. One thread per point. A removed point
// is a dead one (id 0, which every matter pass skips) with every value reset
// to finite numbers; its volume ratio is 1 because a dead point with ratio 0
// is how node.matter_stats counts a point whose tile got no pool slot.
// The exponent-bits test is used because fast math may fold isfinite().

struct MatterPoint {
    position: vec3<f32>,
    id: u32,
    velocity: vec3<f32>,
    volume_ratio: f32,
    affine_x: vec4<f32>,
    affine_y: vec4<f32>,
    affine_z: vec4<f32>,
    f_x: vec4<f32>,
    f_y: vec4<f32>,
    f_z: vec4<f32>,
    stress_diag: vec4<f32>,
    stress_off: vec4<f32>,
}

struct Params { count: u32, pad0: u32, pad1: u32, pad2: u32 }

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read_write> points: array<MatterPoint>;

fn finite3(v: vec3<f32>) -> bool {
    let e = vec3<u32>(bitcast<u32>(v.x), bitcast<u32>(v.y), bitcast<u32>(v.z)) & vec3<u32>(0x7f800000u);
    return all(e != vec3<u32>(0x7f800000u));
}

fn finite4(v: vec4<f32>) -> bool {
    return finite3(v.xyz) && finite3(vec3<f32>(v.w));
}

@compute @workgroup_size(256)
fn remove_nonfinite(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= min(params.count, arrayLength(&points)) { return; }
    let p = points[gid.x];
    if p.id == 0u { return; }
    if finite3(p.position) && finite3(p.velocity) && finite(p.volume_ratio)
        && finite4(p.affine_x) && finite4(p.affine_y) && finite4(p.affine_z)
        && finite4(p.f_x) && finite4(p.f_y) && finite4(p.f_z)
        && finite4(p.stress_diag) && finite4(p.stress_off) {
        return;
    }
    points[gid.x] = MatterPoint(
        vec3<f32>(0.0), 0u, vec3<f32>(0.0), 1.0,
        vec4<f32>(0.0), vec4<f32>(0.0), vec4<f32>(0.0),
        vec4<f32>(1.0, 0.0, 0.0, 0.0), vec4<f32>(0.0, 1.0, 0.0, 0.0), vec4<f32>(0.0, 0.0, 1.0, 0.0),
        vec4<f32>(0.0), vec4<f32>(0.0));
}

fn finite(x: f32) -> bool {
    return finite3(vec3<f32>(x));
}
