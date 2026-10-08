// node.torus_wrap_field — lift an Array<vec2<f32>> of UVs onto a
// torus surface, emit Array<InstanceTransform>.
//
// For each idx:
//   theta = uv.x * TAU, phi = uv.y * TAU
//   pos = ((R + r·cos φ)·cos θ, r·sin φ, (R + r·cos φ)·sin θ)
//   normal = (cos φ · cos θ, sin φ, cos φ · sin θ)         [outward]
//   pos += normal * normal_disp[idx]    (optional input)
//   pos = rotate_x(pos, fold_angle)
//   out[idx] = { pos_scale: vec4(pos, instance_scale), rot_pad: vec4(0) }
//
// Generic across rings / halos / donuts / flower discs / gateways.

struct Uniforms {
    count:           u32,
    base_radius:     f32,   // r — tube radius
    torus_radius:    f32,   // R — major radius
    fold_angle:      f32,   // radians, X-axis whole-field rotation
    instance_scale:  f32,
    has_normal_disp: u32,
    _pad0:           u32,
    _pad1:           u32,
};

struct InstanceTransform {
    pos_scale: vec4<f32>,
    rot_pad:   vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read>       uv_in:       array<vec2<f32>>;
@group(0) @binding(2) var<storage, read>       normal_disp: array<f32>;
@group(0) @binding(3) var<storage, read_write> out:         array<InstanceTransform>;

const TAU: f32 = 6.283185307;

fn rotate_x(p: vec3<f32>, angle: f32) -> vec3<f32> {
    let c = cos(angle);
    let s = sin(angle);
    return vec3<f32>(p.x, c * p.y - s * p.z, s * p.y + c * p.z);
}

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.count { return; }

    let uv = uv_in[idx];
    let theta = uv.x * TAU;
    let phi = uv.y * TAU;
    let ct = cos(theta);
    let st = sin(theta);
    let cp = cos(phi);
    let sp = sin(phi);

    let R = u.torus_radius;
    let r = u.base_radius;
    var pos = vec3<f32>(
        (R + r * cp) * ct,
        r * sp,
        (R + r * cp) * st,
    );

    let normal_outward = vec3<f32>(cp * ct, sp, cp * st);
    let disp = select(0.0, normal_disp[idx], u.has_normal_disp == 1u);
    pos += normal_outward * disp;

    pos = rotate_x(pos, u.fold_angle);

    out[idx] = InstanceTransform(
        vec4<f32>(pos, u.instance_scale),
        vec4<f32>(0.0, 0.0, 0.0, 0.0),
    );
}
