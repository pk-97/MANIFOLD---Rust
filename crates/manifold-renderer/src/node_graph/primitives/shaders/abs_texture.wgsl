// node.abs_texture — per-pixel abs(input.rgb), alpha pass-through.
//
// Useful after sin/cos: abs(sin(x)) gives a positive-only humped
// pattern (twice the spatial frequency of sin). After
// scale_offset(2, -1) on a [0, 1] field, abs maps the result back
// to a "V" curve centered at 0.

@group(0) @binding(0) var source_tex: texture_2d<f32>;
@group(0) @binding(1) var tex_sampler: sampler;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let s = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    let out = vec4<f32>(abs(s.r), abs(s.g), abs(s.b), s.a);
    textureStore(output_tex, vec2<i32>(id.xy), out);
}
