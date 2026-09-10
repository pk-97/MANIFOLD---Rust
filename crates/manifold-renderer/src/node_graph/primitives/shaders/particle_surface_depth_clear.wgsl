// node.particle_surface_depth — depth-bits scratch clear (HAND-AUTHORED).
// Fills the u32 depth scratch with the bit pattern of clip depth 1.0
// (empty): atomicMin in the splat kernel then lands any real depth, and
// untouched pixels stay exactly empty = 1.

struct ClearParams {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> params: ClearParams;
@group(0) @binding(1) var<storage, read_write> buf_depth_bits: array<u32>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= params.width * params.height) {
        return;
    }
    buf_depth_bits[idx] = 0x3f800000u; // bitcast<u32>(1.0f)
}
