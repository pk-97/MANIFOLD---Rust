// node.nest_copies — copy k = i * inner_count + j places inner copy j at
// outer copy i: position P + s1 * R1 * p2, rotation R1 * R2 (back to the
// renderer's Euler XYZ, R = Rz * Ry * Rx), scale s1 * s2. A zero-scale copy
// on either side stays a hole.

struct Params {
    inner_count: u32,
    total: u32,
    _pad0: u32,
    _pad1: u32,
};

struct Inst {
    pos_scale: vec4<f32>,
    rot_pad: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> outer: array<Inst>;
@group(0) @binding(2) var<storage, read> inner: array<Inst>;
@group(0) @binding(3) var<storage, read_write> nested: array<Inst>;

fn euler_xyz(angles: vec3<f32>) -> mat3x3<f32> {
    let cx = cos(angles.x);
    let sx = sin(angles.x);
    let cy = cos(angles.y);
    let sy = sin(angles.y);
    let cz = cos(angles.z);
    let sz = sin(angles.z);
    let rx = mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, cx, sx), vec3<f32>(0.0, -sx, cx));
    let ry = mat3x3<f32>(vec3<f32>(cy, 0.0, -sy), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(sy, 0.0, cy));
    let rz = mat3x3<f32>(vec3<f32>(cz, sz, 0.0), vec3<f32>(-sz, cz, 0.0), vec3<f32>(0.0, 0.0, 1.0));
    return rz * ry * rx;
}

// The inverse of euler_xyz, as `rotation_to_render_scene_euler` reads it
// (m[col][row]); at the poles z is pinned to 0.
fn xyz_euler(m: mat3x3<f32>) -> vec3<f32> {
    let r20 = clamp(m[0][2], -1.0, 1.0);
    if 1.0 - abs(r20) < 1e-6 {
        let ry = select(1.5707963267948966, -1.5707963267948966, r20 > 0.0);
        let rx = select(atan2(-m[1][0], m[1][1]), atan2(m[1][0], m[1][1]), r20 < 0.0);
        return vec3<f32>(rx, ry, 0.0);
    }
    return vec3<f32>(atan2(m[1][2], m[2][2]), asin(-r20), atan2(m[0][1], m[0][0]));
}

@compute @workgroup_size(256, 1, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let k = id.x;
    if k >= params.total || k >= arrayLength(&nested) {
        return;
    }
    let i = k / params.inner_count;
    let j = k % params.inner_count;
    if i >= arrayLength(&outer) || j >= arrayLength(&inner) {
        nested[k] = Inst(vec4<f32>(0.0), vec4<f32>(0.0));
        return;
    }
    let a = outer[i];
    let b = inner[j];
    let scale = a.pos_scale.w * b.pos_scale.w;
    if scale == 0.0 {
        nested[k] = Inst(vec4<f32>(0.0), vec4<f32>(0.0));
        return;
    }
    let ra = euler_xyz(a.rot_pad.xyz);
    let position = a.pos_scale.xyz + a.pos_scale.w * (ra * b.pos_scale.xyz);
    let rotation = xyz_euler(ra * euler_xyz(b.rot_pad.xyz));
    nested[k] = Inst(vec4<f32>(position, scale), vec4<f32>(rotation, 0.0));
}
