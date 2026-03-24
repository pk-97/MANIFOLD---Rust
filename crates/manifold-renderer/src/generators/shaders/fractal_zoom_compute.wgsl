struct Uniforms {
    time_val: f32,
    beat: f32,
    aspect_ratio: f32,
    anim_speed: f32,
    uv_scale: f32,
    trigger_count: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output);
    if id.x >= dims.x || id.y >= dims.y { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    // Center and aspect-correct UV
    var p_uv = uv - vec2<f32>(0.5);
    p_uv.x *= u.aspect_ratio;
    p_uv *= u.uv_scale;

    // Julia set with c tracing the Mandelbrot cardioid
    let angle = u.time_val * u.anim_speed * 0.15;
    let cx = 0.5 * cos(angle) - 0.25 * cos(2.0 * angle);
    let cy = 0.5 * sin(angle) - 0.25 * sin(2.0 * angle);

    // Scale UV to fractal space
    var z = p_uv * 3.0;

    var iter: f32 = 0.0;
    let max_iter: i32 = 80;
    var zr = z.x;
    var zi = z.y;
    var dot_val: f32 = 0.0;

    for (var i: i32 = 0; i < max_iter; i = i + 1) {
        // z = z^2 + c
        let new_zr = zr * zr - zi * zi + cx;
        let new_zi = 2.0 * zr * zi + cy;
        zr = new_zr;
        zi = new_zi;
        dot_val = zr * zr + zi * zi;
        if (dot_val > 4.0) {
            break;
        }
        iter = iter + 1.0;
    }

    // Interior is black
    if (dot_val <= 4.0) {
        textureStore(output, vec2<i32>(id.xy), vec4<f32>(0.0, 0.0, 0.0, 1.0));
        return;
    }

    // Smooth iteration count
    let smooth_iter = iter + 1.0 - log2(log2(dot_val));

    // Color from smooth iteration
    let lum = sin(smooth_iter * 0.4) * 0.5 + 0.5;

    textureStore(output, vec2<i32>(id.xy), vec4<f32>(lum, lum, lum, lum));
}
