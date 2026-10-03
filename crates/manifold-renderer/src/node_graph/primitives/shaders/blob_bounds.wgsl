struct Params { count: u32, a: u32, b: u32, c: u32, }
struct Blob { center_radius: vec4<f32>, shape_diag: vec4<f32>, shape_off: vec4<f32>, }
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> blobs: array<Blob>;
@group(0) @binding(2) var<storage, read_write> bounds: array<f32>;
var<workgroup> partial: array<vec2<f32>, 256>;
@compute @workgroup_size(256)
fn main(@builtin(local_invocation_index) lane: u32) {
    var bound = vec2<f32>(0.0);
    for (var i = lane; i < params.count; i += 256u) {
        let blob = blobs[i];
        let r = blob.center_radius.w;
        if r > 0.0 { bound = max(bound, vec2<f32>(r, 1.5 * r + blob.shape_off.w)); }
    }
    partial[lane] = bound;
    workgroupBarrier();
    for (var stride = 128u; stride > 0u; stride /= 2u) {
        if lane < stride { partial[lane] = max(partial[lane], partial[lane + stride]); }
        workgroupBarrier();
    }
    if lane == 0u { bounds[0] = partial[0].x; bounds[1] = partial[0].y; }
}
