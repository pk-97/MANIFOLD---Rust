// Black Hole — Polar Resolve
//
// Converts atomic accumulator → RGBA density texture + self-clear.

struct ResolveUniforms {
    tex_w: u32,
    tex_h: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<storage, read_write> accum: array<atomic<u32>>;
@group(0) @binding(1) var density_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> params: ResolveUniforms;

@compute @workgroup_size(16, 16)
fn resolve(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.tex_w || gid.y >= params.tex_h {
        return;
    }

    let idx = gid.y * params.tex_w + gid.x;
    let raw = atomicLoad(&accum[idx]);
    let density = f32(raw) / 4096.0;

    textureStore(density_out, gid.xy, vec4<f32>(density, 0.0, 0.0, 1.0));

    atomicStore(&accum[idx], 0u);
}
