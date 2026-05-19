// node.resolve_accumulator — read u32 fixed-point accumulator,
// divide by FIXED_POINT_SCALE, write as float density into the
// output Rgba16Float texture. Phase A.7 of BUFFER_PORT_PLAN.
//
// Pairs with node.scatter_particles: scatter emits u32 atomic-add
// accumulations; resolve normalises and surfaces as a sampleable
// 2D texture for downstream Mix / Blur / display chains.

struct ResolveUniforms {
    width: u32,
    height: u32,
    inv_scale: f32, // 1.0 / FIXED_POINT_SCALE
    _pad: f32,
};

@group(0) @binding(0) var<uniform> params: ResolveUniforms;
@group(0) @binding(1) var<storage, read> accum: array<u32>;
@group(0) @binding(2) var density_out: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.width || id.y >= params.height {
        return;
    }

    let idx = id.y * params.width + id.x;
    let density = f32(accum[idx]) * params.inv_scale;

    textureStore(
        density_out,
        vec2<i32>(i32(id.x), i32(id.y)),
        vec4<f32>(density, density, density, 1.0),
    );
}
