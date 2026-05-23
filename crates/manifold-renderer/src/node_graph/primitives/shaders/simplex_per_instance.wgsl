// node.simplex_per_instance — sample 3D simplex noise at each UV
// position in an Array<vec2<f32>>, with frequency / xy offset / z
// controls; emit Array<f32>.
//
// noise_common.wgsl is prepended at pipeline creation time (same
// pattern as the legacy DigitalPlants compute pass). It supplies
// the `simplex3d`, `mod289_3`, `mod289_4`, `permute`, and
// `taylor_inv_sqrt` functions used below.

struct Uniforms {
    count:    u32,
    scale:    f32,   // frequency multiplier in UV units
    z:        f32,   // third coordinate — drive from time, beat, or hold static
    offset_x: f32,
    offset_y: f32,
    _pad0:    u32,
    _pad1:    u32,
    _pad2:    u32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read>       uv_in: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read_write> out:    array<f32>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.count { return; }
    let uv = uv_in[idx];
    let p = vec3<f32>(
        uv.x * u.scale + u.offset_x,
        uv.y * u.scale + u.offset_y,
        u.z,
    );
    out[idx] = simplex3d(p);
}
