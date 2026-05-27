// node.channel_mix — per-pixel 4x4 RGBA matrix transform.
//
//   out = M · in
//
// where M is the 4x4 matrix whose rows are the four Vec4 params:
//
//   M = | row0.r row0.g row0.b row0.a |     out.r = dot(row0, in)
//       | row1.r row1.g row1.b row1.a |     out.g = dot(row1, in)
//       | row2.r row2.g row2.b row2.a |     out.b = dot(row2, in)
//       | row3.r row3.g row3.b row3.a |     out.a = dot(row3, in)
//
// Identity matrix is the param default — output = input.
// Common useful matrices:
//   - Swap A → R:  row0 = (0,0,0,1), row1 = (0,1,0,0), row2 = (0,0,1,0), row3 = (0,0,0,1)
//   - Luma drop:   row0 = (0.2126, 0.7152, 0.0722, 0), and same for rows 1/2; row3 = (0,0,0,1)
//   - Halation tint: scale R, kill G/B; row0 = (1,0,0,0), row1 = (0,0,0,0), row2 = (0,0,0,0)
//   - Isolate B:   row0 = row1 = row2 = (0,0,1,0); row3 = (0,0,0,1)

struct Uniforms {
    row0: vec4<f32>,
    row1: vec4<f32>,
    row2: vec4<f32>,
    row3: vec4<f32>,
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
    let out = vec4<f32>(
        dot(u.row0, s),
        dot(u.row1, s),
        dot(u.row2, s),
        dot(u.row3, s),
    );
    textureStore(output_tex, vec2<i32>(id.xy), out);
}
