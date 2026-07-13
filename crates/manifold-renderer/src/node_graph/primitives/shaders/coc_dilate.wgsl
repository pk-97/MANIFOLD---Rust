// node.coc_dilate — hand parity oracle for the generated standalone kernel
// (docs/BUG_BACKLOG.md BUG-137). Same fixed 3x3 neighborhood-max as
// coc_dilate_body.wgsl — kept independent (not sharing Rust source) so the
// gpu_tests parity check is a real cross-check, not a tautology.
//
// Paramless atom: no uniform buffer (matches the generated layout for a
// Gather-only, params-free primitive — bindings start at tex(0)).
//
// Bindings: source_tex(0), tex_sampler(1), output_tex(2, rgba16float storage).

@group(0) @binding(0) var source_tex: texture_2d<f32>;
@group(0) @binding(1) var tex_sampler: sampler;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

fn fetch(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims_i = textureDimensions(output_tex);
    if id.x >= u32(dims_i.x) || id.y >= u32(dims_i.y) {
        return;
    }

    let dims = vec2<f32>(f32(dims_i.x), f32(dims_i.y));
    let texel = vec2<f32>(1.0) / dims;
    let uv = (vec2<f32>(f32(id.x), f32(id.y)) + vec2<f32>(0.5)) / dims;

    var m: f32 = fetch(uv).r;
    m = max(m, fetch(uv + vec2<f32>(-texel.x, -texel.y)).r);
    m = max(m, fetch(uv + vec2<f32>(0.0,      -texel.y)).r);
    m = max(m, fetch(uv + vec2<f32>( texel.x, -texel.y)).r);
    m = max(m, fetch(uv + vec2<f32>(-texel.x, 0.0     )).r);
    m = max(m, fetch(uv + vec2<f32>( texel.x, 0.0     )).r);
    m = max(m, fetch(uv + vec2<f32>(-texel.x,  texel.y)).r);
    m = max(m, fetch(uv + vec2<f32>(0.0,       texel.y)).r);
    m = max(m, fetch(uv + vec2<f32>( texel.x,  texel.y)).r);

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(m, m, m, 1.0));
}
