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

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Fullscreen triangle — 3 vertices, no vertex buffer
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Center and aspect-correct UV
    var uv = in.uv - vec2<f32>(0.5);
    uv.x *= u.aspect_ratio;
    uv *= u.uv_scale;

    // Julia set with c tracing the Mandelbrot cardioid
    let angle = u.time_val * u.anim_speed * 0.15;
    let cx = 0.5 * cos(angle) - 0.25 * cos(2.0 * angle);
    let cy = 0.5 * sin(angle) - 0.25 * sin(2.0 * angle);

    // Scale UV to fractal space
    var z = uv * 3.0;

    var iter: f32 = 0.0;
    let max_iter: i32 = 80;
    var zr = z.x;
    var zi = z.y;
    var dot: f32 = 0.0;

    for (var i: i32 = 0; i < max_iter; i = i + 1) {
        // z = z^2 + c
        let new_zr = zr * zr - zi * zi + cx;
        let new_zi = 2.0 * zr * zi + cy;
        zr = new_zr;
        zi = new_zi;
        dot = zr * zr + zi * zi;
        if (dot > 4.0) {
            break;
        }
        iter = iter + 1.0;
    }

    // Interior is black
    if (dot <= 4.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    // Smooth iteration count
    let smooth_iter = iter + 1.0 - log2(log2(dot));

    // Color from smooth iteration
    let lum = sin(smooth_iter * 0.4) * 0.5 + 0.5;

    return vec4<f32>(lum, lum, lum, lum);
}
