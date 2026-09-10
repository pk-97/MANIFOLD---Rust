// node.particle_thickness — resolve kernel (HAND-AUTHORED). Copies the
// additive f32 thickness scratch into the R16Float thickness output
// (empty = 0). R16Float conversion happens at the store.

struct ResolveParams {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> params: ResolveParams;
@group(0) @binding(1) var<storage, read> buf_thickness: array<u32>;
@group(0) @binding(2) var thickness_out: texture_storage_2d<r16float, write>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= params.width * params.height) {
        return;
    }
    let px = vec2<i32>(i32(idx % params.width), i32(idx / params.width));
    let thickness = bitcast<f32>(buf_thickness[idx]);
    textureStore(thickness_out, px, vec4<f32>(thickness, 0.0, 0.0, 1.0));
}
