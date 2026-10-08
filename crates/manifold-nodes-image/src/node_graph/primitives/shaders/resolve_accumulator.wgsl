// node.resolve_accumulator — read u32 fixed-point accumulator,
// divide by FIXED_POINT_SCALE, write as float density into the
// output Rgba16Float texture. Phase A.7 of BUFFER_PORT_PLAN.
//
// Pairs with node.scatter_particles (2D) and node.scatter_particles_camera
// (3D→2D): scatter emits u32 atomic-add accumulations; resolve normalises,
// surfaces as a sampleable 2D texture, AND self-clears the accumulator
// to zero so the next frame starts fresh. Self-clear in the resolve is
// the same pattern node.resolve_3d_accumulator uses for the 3D volume
// path — collapses the "who zeros the buffer" decision so neither
// scatter has to worry about it.

struct ResolveUniforms {
    width: u32,
    height: u32,
    inv_scale: f32, // 1.0 / FIXED_POINT_SCALE
    _pad: f32,
};

@group(0) @binding(0) var<uniform> params: ResolveUniforms;
@group(0) @binding(1) var<storage, read_write> accum: array<atomic<u32>>;
@group(0) @binding(2) var density_out: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.width || id.y >= params.height {
        return;
    }

    let idx = id.y * params.width + id.x;
    let raw = atomicLoad(&accum[idx]);
    let density = f32(raw) * params.inv_scale;

    textureStore(
        density_out,
        vec2<i32>(i32(id.x), i32(id.y)),
        vec4<f32>(density, density, density, 1.0),
    );

    // Self-clearing so the next frame's scatter starts from zero. Same
    // pattern as resolve_3d's atomicStore(0) self-clear; removes the
    // need for a scatter-side pre-clear pass.
    atomicStore(&accum[idx], 0u);
}
