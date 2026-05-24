// node.downsample — integer-factor box-filter downsample.
//
// Reads `factor × factor` source texels per output texel and writes
// their mean. `textureLoad` (no sampler) reads exact source pixels,
// which is what a box filter wants — `textureSampleLevel` with a
// linear sampler would still bilinearly blend at the boundaries
// between sample positions and give a slightly different kernel.
//
// The effective factor is derived from the input/output dim ratio
// at dispatch time rather than read straight from the uniform: this
// makes the shader correct under any input/output ratio the executor
// might allocate. The previous version assumed
// `output_dims == input_dims / uniforms.factor` and strided by the
// uniform factor unconditionally — if the executor ever landed the
// output at full canvas (e.g. because the producer's `output_dims`
// returned None and the canvas-scale fallback wasn't yet wired),
// `id.xy * factor` overshot input bounds, `textureLoad` returned
// zero for the OOB reads, and everything past the top-left 1/factor²
// of the output became zero-poisoned. With the dim-ratio derivation
// here, a degenerate same-size allocation degrades gracefully to a
// 1×1 box (identity copy) instead of zero-poisoning.
//
// The `uniforms.factor` is kept for diagnostics / future use; the
// shader's actual scale is `input_dims / output_dims`.

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

    let in_dims = textureDimensions(input_tex);
    // Effective factor = input / output, clamped to ≥1 so a same-size
    // or upsampling allocation never reads zero pixels.
    let fx = max(1u, in_dims.x / out_dims.x);
    let fy = max(1u, in_dims.y / out_dims.y);
    let base = vec2<i32>(id.xy * vec2<u32>(fx, fy));

    var sum = vec4<f32>(0.0);
    var taps: u32 = 0u;
    for (var dy: u32 = 0u; dy < fy; dy = dy + 1u) {
        for (var dx: u32 = 0u; dx < fx; dx = dx + 1u) {
            let coord = base + vec2<i32>(i32(dx), i32(dy));
            // Guard against the last tile when out_dims doesn't
            // divide in_dims evenly — taps past the right/bottom
            // edge would still go OOB and return zero, biasing the
            // mean. Excluding them keeps the average correct.
            if (coord.x < i32(in_dims.x) && coord.y < i32(in_dims.y)) {
                sum = sum + textureLoad(input_tex, coord, 0);
                taps = taps + 1u;
            }
        }
    }

    let inv = 1.0 / f32(max(taps, 1u));
    textureStore(output_tex, vec2<i32>(id.xy), sum * inv);
}
