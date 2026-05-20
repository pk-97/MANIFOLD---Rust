// node.checkerboard — alternating black/white squares at a
// configurable scale.
//
// Pure generator. Output is binary {0, 1} broadcast to RGB,
// A = 1. Composes with node.compose / node.lut1d to colorize
// the cells.

struct Uniforms {
    scale:    f32,   // squares per UV-unit (default 8 = 8×8 grid)
    offset_x: f32,
    offset_y: f32,
    _pad:     f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let p = uv * u.scale + vec2<f32>(u.offset_x, u.offset_y);
    let ix = i32(floor(p.x));
    let iy = i32(floor(p.y));
    let on = ((ix + iy) & 1) == 0;
    let v = select(0.0, 1.0, on);
    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(v, v, v, 1.0));
}
