// Infrared / thermal vision effect — LUT-based palette mapping.
// Replaces per-pixel branching with a single texture lookup into a
// pre-baked 256×1 palette LUT. Zero ALU for palette math, zero branching.

struct Uniforms {
    amount: f32,
    contrast: f32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var lut_tex: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let src = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);

    // Extract luminance (BT.601 weights)
    let lum_raw = dot(src.rgb, vec3<f32>(0.299, 0.587, 0.114));

    // Apply contrast (pivot at 0.5); clamp negative only so HDR values
    // above 1 extrapolate the palette instead of collapsing to hottest slot
    let lum = max(0.0, (lum_raw - 0.5) * uniforms.contrast + 0.5);

    // Sample the pre-baked palette LUT (512×1 covering [0, 2] range).
    // HDR extrapolation is baked into the LUT — the palette functions naturally
    // extend their gradient beyond t=1.0, producing gorgeous blown-out highlights.
    let lut_coord = clamp(lum * 0.5, 0.0, 1.0);
    let thermal = textureSampleLevel(lut_tex, tex_sampler, vec2<f32>(lut_coord, 0.5), 0.0).rgb;

    let result = mix(src.rgb, thermal, uniforms.amount);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(result, src.a));
}
