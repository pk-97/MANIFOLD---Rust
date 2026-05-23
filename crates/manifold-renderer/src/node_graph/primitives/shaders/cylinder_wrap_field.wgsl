// node.cylinder_wrap_field — lift an Array<vec2<f32>> of UVs onto a
// cylindrical surface, emit Array<InstanceTransform>.
//
// For each idx:
//   uv = uv_in[idx]
//   theta = uv.x * TAU
//   r = base_radius * pow(1 - uv.y, taper) + radius_disp[idx]
//   y = (uv.y - 0.5) * height_scale
//   pos = vec3(r * cos(theta), y, r * sin(theta))
//   out[idx] = { pos_scale: vec4(pos, scale), rot_pad: vec4(0) }
//
// `radius_disp` is optional (host binds an aliased buffer + 0
// stride-disp flag when absent). The taper curve pow(1 - uv.y, taper)
// narrows the radius toward uv.y = 1 — produces stem / tube / cone /
// vase shapes depending on taper exponent.

struct Uniforms {
    count:        u32,
    base_radius:  f32,
    height_scale: f32,
    taper:        f32,
    instance_scale: f32,
    has_radius_disp: u32,
    _pad0:        u32,
    _pad1:        u32,
};

struct InstanceTransform {
    pos_scale: vec4<f32>,
    rot_pad:   vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read>       uv_in:       array<vec2<f32>>;
@group(0) @binding(2) var<storage, read>       radius_disp: array<f32>;
@group(0) @binding(3) var<storage, read_write> out:         array<InstanceTransform>;

const TAU: f32 = 6.283185307;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.count { return; }

    let uv = uv_in[idx];
    let theta = uv.x * TAU;
    let taper_factor = pow(max(1.0 - uv.y, 0.0), u.taper);
    let disp = select(0.0, radius_disp[idx], u.has_radius_disp == 1u);
    let r = u.base_radius * taper_factor + disp;
    let y = (uv.y - 0.5) * u.height_scale;
    let pos = vec3<f32>(r * cos(theta), y, r * sin(theta));

    out[idx] = InstanceTransform(
        vec4<f32>(pos, u.instance_scale),
        vec4<f32>(0.0, 0.0, 0.0, 0.0),
    );
}
