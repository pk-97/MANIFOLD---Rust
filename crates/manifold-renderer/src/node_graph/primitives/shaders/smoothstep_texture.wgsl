// node.smoothstep_texture — per-pixel WGSL smoothstep on RGB.
//
// mode = Range (0): out = smoothstep(low, high, in)
// mode = Bipolar (1): out = smoothstep(-high, high, in)  (low ignored)
//
// Alpha passes through. Hermite polynomial 3t²-2t³ where
// t = clamp((x - lo) / (hi - lo), 0, 1).

struct Uniforms {
    low:  f32,
    high: f32,
    mode: u32,   // 0 = Range, 1 = Bipolar
    _pad0: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let s = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    let lo = select(u.low, -u.high, u.mode == 1u);
    let hi = u.high;
    let out = vec4<f32>(
        smoothstep(lo, hi, s.r),
        smoothstep(lo, hi, s.g),
        smoothstep(lo, hi, s.b),
        s.a,
    );
    textureStore(output_tex, vec2<i32>(id.xy), out);
}
