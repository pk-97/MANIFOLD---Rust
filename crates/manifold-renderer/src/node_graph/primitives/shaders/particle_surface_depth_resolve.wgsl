// node.particle_surface_depth — resolve kernel (HAND-AUTHORED). Copies the
// splat scratch into the node's real outputs: raw clip depth into the
// R32Float depth texture (empty = 1) and 0/1 into the R8Unorm coverage
// texture (0 empty, 1 occupied).

struct ResolveParams {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> params: ResolveParams;
@group(0) @binding(1) var<storage, read> buf_depth_bits: array<u32>;
@group(0) @binding(2) var<storage, read> buf_coverage: array<u32>;
@group(0) @binding(3) var depth_out: texture_storage_2d<r32float, write>;
@group(0) @binding(4) var coverage_out: texture_storage_2d<r8unorm, write>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= params.width * params.height) {
        return;
    }
    let px = vec2<i32>(i32(idx % params.width), i32(idx / params.width));
    let raw = bitcast<f32>(buf_depth_bits[idx]);
    let cov = f32(buf_coverage[idx]);
    textureStore(depth_out, px, vec4<f32>(raw, 0.0, 0.0, 1.0));
    textureStore(coverage_out, px, vec4<f32>(cov, 0.0, 0.0, 1.0));
}
