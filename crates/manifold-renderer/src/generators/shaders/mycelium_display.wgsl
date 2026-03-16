// Mycelium display shader: reads trail concentration and applies
// HSV coloring with warm-white bloom and glow boost.

struct DisplayUniforms {
    hue: f32,
    glow: f32,
    uv_scale: f32,
    time: f32,
};

@group(0) @binding(0) var<uniform> params: DisplayUniforms;
@group(0) @binding(1) var t_trail: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> vec3<f32> {
    let c = v * s;
    let hh = (h % 1.0 + 1.0) % 1.0 * 6.0;
    let x = c * (1.0 - abs(hh % 2.0 - 1.0));
    var rgb: vec3<f32>;
    if hh < 1.0 { rgb = vec3<f32>(c, x, 0.0); }
    else if hh < 2.0 { rgb = vec3<f32>(x, c, 0.0); }
    else if hh < 3.0 { rgb = vec3<f32>(0.0, c, x); }
    else if hh < 4.0 { rgb = vec3<f32>(0.0, x, c); }
    else if hh < 5.0 { rgb = vec3<f32>(x, 0.0, c); }
    else { rgb = vec3<f32>(c, 0.0, x); }
    let m = v - c;
    return rgb + m;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv / params.uv_scale;
    let t = clamp(textureSample(t_trail, s, uv).r, 0.0, 1.0);

    let hue = params.hue - params.time * 0.02;
    let sat = mix(0.6, 0.12, t * t);
    let val = pow(t, 0.55);
    var rgb = hsv_to_rgb(hue, sat, val);

    // Dense core bloom toward warm white
    let warm = vec3<f32>(1.0, 0.95, 0.85) * val;
    rgb = mix(rgb, warm, smoothstep(0.35, 0.9, t) * 0.6);

    // Glow boost
    rgb *= 1.0 + params.glow * t;

    let lum = dot(rgb, vec3<f32>(0.299, 0.587, 0.114));
    return vec4<f32>(rgb, lum);
}
