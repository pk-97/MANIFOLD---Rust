// node.lerp_instance_fields — elementwise lerp of two
// Array<InstanceTransform>s: out = (1 - t) * a + t * b.
//
// Both pos_scale (xyz position + .w scale) and rot_pad (xyz Euler
// rotation + .w pad) are lerped component-wise. Pair with
// node.cylinder_wrap_field / node.torus_wrap_field to morph
// continuously between two topology-derived fields — what
// node.mux_array can't do (it selects discretely).

struct Uniforms {
    count: u32,
    t:     f32,
    _pad0: u32,
    _pad1: u32,
};

struct InstanceTransform {
    pos_scale: vec4<f32>,
    rot_pad:   vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read>       a:   array<InstanceTransform>;
@group(0) @binding(2) var<storage, read>       b:   array<InstanceTransform>;
@group(0) @binding(3) var<storage, read_write> out: array<InstanceTransform>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.count { return; }
    let inv_t = 1.0 - u.t;
    let av = a[idx];
    let bv = b[idx];
    out[idx] = InstanceTransform(
        av.pos_scale * inv_t + bv.pos_scale * u.t,
        av.rot_pad   * inv_t + bv.rot_pad   * u.t,
    );
}
