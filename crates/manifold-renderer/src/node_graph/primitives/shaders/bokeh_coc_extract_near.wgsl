// node.bokeh_gather — internal near-field CoC extraction (BOKEH_LAYERED_DOF_DESIGN.md P3).
//
// Reads the signed CoC field (`width`): where the sign flag in G is 1.0,
// copies the magnitude from R into the output and clears G/B so the EXISTING
// far-field dilation helper (`bokeh_coc_dilate_wide.wgsl`) can be reused
// unchanged — that helper reads R only where G == 0, and after this kernel
// every valid near pixel has G == 0. Output A = 1 for padding.
//
// Bindings: src(0), samp(1), dst(2, rgba16float write).

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims_i = textureDimensions(dst);
    if id.x >= dims_i.x || id.y >= dims_i.y {
        return;
    }

    let dims = vec2<f32>(f32(dims_i.x), f32(dims_i.y));
    let uv = (vec2<f32>(f32(id.x), f32(id.y)) + vec2<f32>(0.5)) / dims;

    let sample = textureSampleLevel(src, samp, uv, 0.0);
    // Near side: sign flag G == 1.0, magnitude in R. Far/in-focus pixels become 0.
    let near_coc = select(0.0, sample.r, sample.g == 1.0);
    textureStore(dst, vec2<i32>(id.xy), vec4<f32>(near_coc, 0.0, 0.0, 1.0));
}
