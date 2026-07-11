// node.mesh_ramp — per-vertex growth-mask weights from a spatial axis
// sweep over an Array<MeshVertex>. One thread per vertex.
//
// axis: 0=X, 1=Y, 2=Z, 3=Radial XZ, 4=Distance
//   m = measure(pos - origin) per axis
//   t = clamp((m - bound_min) / (bound_max - bound_min), 0, 1)
//   w = 1 - smoothstep(phase, phase + feather, t)
//   invert -> 1 - w

struct Uniforms {
    axis:       u32,
    origin_x:   f32,
    origin_y:   f32,
    origin_z:   f32,
    phase:      f32,
    feather:    f32,
    bound_min:  f32,
    bound_max:  f32,
    invert:     u32,
    count:      u32,
    _pad0:      u32,
    _pad1:      u32,
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    _pad2: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> src: array<MeshVertex>;
@group(0) @binding(2) var<storage, read_write> weights: array<f32>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.count { return; }

    let pos = src[idx].position;
    let origin = vec3<f32>(u.origin_x, u.origin_y, u.origin_z);
    let d = pos - origin;

    var m: f32;
    if u.axis == 0u {
        m = d.x;
    } else if u.axis == 1u {
        m = d.y;
    } else if u.axis == 2u {
        m = d.z;
    } else if u.axis == 3u {
        m = length(vec2<f32>(d.x, d.z));
    } else {
        m = length(d);
    }

    let denom = max(u.bound_max - u.bound_min, 1e-6);
    let t = clamp((m - u.bound_min) / denom, 0.0, 1.0);
    let edge0 = u.phase;
    let edge1 = u.phase + max(u.feather, 1e-6);
    var w = 1.0 - smoothstep(edge0, edge1, t);
    w = select(w, 1.0 - w, u.invert == 1u);

    weights[idx] = w;
}
