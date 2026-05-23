// node.downsample — integer-factor box-filter downsample.
//
// Reads `factor × factor` source texels per output texel and writes
// their mean. `textureLoad` (no sampler) reads exact source pixels,
// which is what a box filter wants — `textureSampleLevel` with a
// linear sampler would still bilinearly blend at the boundaries
// between sample positions and give a slightly different kernel.

struct Uniforms {
    factor: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var input_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let out_dims = textureDimensions(output_tex);
    if id.x >= out_dims.x || id.y >= out_dims.y {
        return;
    }

    let factor = uniforms.factor;
    let base = vec2<i32>(id.xy) * i32(factor);

    var sum = vec4<f32>(0.0);
    for (var dy: u32 = 0u; dy < factor; dy = dy + 1u) {
        for (var dx: u32 = 0u; dx < factor; dx = dx + 1u) {
            let coord = base + vec2<i32>(i32(dx), i32(dy));
            sum = sum + textureLoad(input_tex, coord, 0);
        }
    }

    let inv = 1.0 / f32(factor * factor);
    textureStore(output_tex, vec2<i32>(id.xy), sum * inv);
}
