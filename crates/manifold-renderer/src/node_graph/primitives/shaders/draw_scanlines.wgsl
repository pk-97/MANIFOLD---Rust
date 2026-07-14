// Hand-authored parity oracle for `node.draw_scanlines` (D3, BUG-114). NOT
// the runtime kernel — `run()` builds its pipeline from `standalone_for_spec`
// (see `shaders/draw_scanlines_body.wgsl`). Kept byte-for-byte identical to
// the pre-conversion kernel so the generated-vs-hand parity test proves the
// codegen path reproduces this exactly.

struct U {
    color: vec3<f32>,
    alpha: f32,
    period_px: f32,
    intensity: f32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var src_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= dims.x || gid.y >= dims.y { return; }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    var src = textureSampleLevel(source_tex, src_sampler, uv, 0.0);

    let scanline = abs(fract(uv.y * f32(dims.y) / u.period_px) - 0.5) * 2.0;
    let scan_alpha = (1.0 - smoothstep(0.4, 0.5, scanline)) * u.intensity;

    let add = scan_alpha * u.alpha;
    src = vec4<f32>(src.rgb + u.color * add, src.a);
    textureStore(output_tex, vec2<i32>(gid.xy), src);
}
