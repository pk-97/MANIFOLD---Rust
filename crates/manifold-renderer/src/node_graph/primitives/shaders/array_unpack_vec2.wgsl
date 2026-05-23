// array_unpack_vec2 — split an Array<vec2<f32>> into two Array<f32>s
// (x and y). One thread per element; output capacity equals input
// capacity (chain build enforces).

struct UnpackUniforms {
    count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<uniform> u: UnpackUniforms;
@group(0) @binding(1) var<storage, read>       in_vec: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read_write> out_x: array<f32>;
@group(0) @binding(3) var<storage, read_write> out_y: array<f32>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.count { return; }
    let v = in_vec[idx];
    out_x[idx] = v.x;
    out_y[idx] = v.y;
}
