struct Params { count: u32, groups: u32, _pad1: u32, _pad2: u32, }
struct Blob { center_radius: vec4<f32>, shape_diag: vec4<f32>, shape_off: vec4<f32>, }
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> blobs: array<Blob>;
@group(0) @binding(2) var<storage, read_write> bounds: array<f32>;
@group(0) @binding(3) var<storage, read_write> partial_bounds: array<vec2<f32>>;
var<workgroup> partial: array<vec2<f32>, 256>;

fn blob_bound(i: u32) -> vec2<f32> {
    let blob = blobs[i];
    let r = blob.center_radius.w;
    if r > 0.0 { return vec2<f32>(r, 1.5 * r + blob.shape_off.w); }
    return vec2<f32>(0.0);
}

fn reduce_group(lane: u32, bound: vec2<f32>) -> vec2<f32> {
    partial[lane] = bound;
    workgroupBarrier();
    for (var stride = 128u; stride > 0u; stride /= 2u) {
        if lane < stride { partial[lane] = max(partial[lane], partial[lane + stride]); }
        workgroupBarrier();
    }
    return partial[0];
}

@compute @workgroup_size(256)
fn main(@builtin(local_invocation_index) lane: u32) {
    var bound = vec2<f32>(0.0);
    for (var i = lane; i < params.count; i += 256u) {
        bound = max(bound, blob_bound(i));
    }
    let result = reduce_group(lane, bound);
    if lane == 0u { bounds[0] = result.x; bounds[1] = result.y; }
}

@compute @workgroup_size(256)
fn partial_main(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
) {
    var bound = vec2<f32>(0.0);
    for (var i = gid.x; i < params.count; i += 256u * params.groups) {
        bound = max(bound, blob_bound(i));
    }
    let result = reduce_group(lane, bound);
    if lane == 0u { partial_bounds[group.x] = result; }
}

@compute @workgroup_size(256)
fn finish_main(@builtin(local_invocation_index) lane: u32) {
    var bound = vec2<f32>(0.0);
    if lane < params.groups { bound = partial_bounds[lane]; }
    let result = reduce_group(lane, bound);
    if lane == 0u { bounds[0] = result.x; bounds[1] = result.y; }
}
