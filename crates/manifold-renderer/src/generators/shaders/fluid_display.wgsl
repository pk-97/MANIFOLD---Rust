// Display pass: extended Reinhard tone mapping of density field
// with 6 color palette modes and alpha from luminance.

struct DisplayUniforms {
    intensity: f32,
    contrast: f32,
    invert: f32,
    color_mode: f32,
    color_bright: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> params: DisplayUniforms;
@group(0) @binding(1) var t_density: texture_2d<f32>;
@group(0) @binding(2) var s_density: sampler;

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

fn hsv_to_rgb(h: f32, sat: f32, v: f32) -> vec3<f32> {
    let c = v * sat;
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

fn apply_palette(lum: f32, mode: f32) -> vec3<f32> {
    let mode_int = i32(mode + 0.5);

    if mode_int == 1 {
        // Blush
        return mix(vec3<f32>(0.1, 0.05, 0.08), vec3<f32>(1.0, 0.85, 0.9), lum);
    } else if mode_int == 2 {
        // Sunset: dark->mid->bright with orange mid push
        let base = mix(vec3<f32>(0.1, 0.02, 0.05), vec3<f32>(1.0, 0.6, 0.2), lum);
        let mid_push = vec3<f32>(1.0, 0.3, 0.1);
        let mid_weight = smoothstep(0.0, 0.5, lum) * smoothstep(1.0, 0.5, lum) * 2.0;
        return mix(base, mid_push, mid_weight * 0.4);
    } else if mode_int == 3 {
        // Ocean
        let base = mix(vec3<f32>(0.02, 0.05, 0.1), vec3<f32>(0.3, 0.8, 1.0), lum);
        let mid_push = vec3<f32>(0.1, 0.4, 0.8);
        let mid_weight = smoothstep(0.0, 0.5, lum) * smoothstep(1.0, 0.5, lum) * 2.0;
        return mix(base, mid_push, mid_weight * 0.4);
    } else if mode_int == 4 {
        // Vivid: HSV rainbow
        return hsv_to_rgb(lum * 0.8 + 0.6, 0.8, lum);
    } else if mode_int == 5 {
        // White: same as mono, no color_bright scaling (handled in caller)
        return vec3<f32>(lum);
    }

    // Mode 0 or default: Mono
    return vec3<f32>(lum);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let density = textureSample(t_density, s_density, in.uv).r;

    // Extended Reinhard tone mapping (WHITE_POINT = 3.0)
    let x = density * params.intensity * params.contrast;
    var lum = x * (1.0 + x / 9.0) / (1.0 + x);

    if params.invert > 0.5 {
        lum = 1.0 - lum;
    }

    let mode_int = i32(params.color_mode + 0.5);
    var color = apply_palette(lum, params.color_mode);

    // Apply color_bright (except for White mode which is unscaled)
    if mode_int != 5 {
        color *= params.color_bright;
    }

    let alpha = dot(color, vec3<f32>(0.299, 0.587, 0.114));
    return vec4<f32>(color, alpha);
}
