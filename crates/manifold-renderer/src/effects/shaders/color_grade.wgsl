// ColorGrade effect — HSV hue shift, saturation, gain, contrast.

struct Uniforms {
    hue_shift: f32,    // param[0]: -1..1 → -180..180 degrees
    saturation: f32,   // param[1]: 0..2 (1=neutral)
    gain: f32,         // param[2]: 0..3 (1=neutral)
    contrast: f32,     // param[3]: 0..3 (1=neutral)
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

fn rgb_to_hsv(c: vec3<f32>) -> vec3<f32> {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    let d = mx - mn;
    var h = 0.0;
    if d > 0.0001 {
        if mx == c.r {
            h = (c.g - c.b) / d;
            if h < 0.0 { h += 6.0; }
        } else if mx == c.g {
            h = (c.b - c.r) / d + 2.0;
        } else {
            h = (c.r - c.g) / d + 4.0;
        }
        h /= 6.0;
    }
    let s = select(0.0, d / mx, mx > 0.0);
    return vec3<f32>(h, s, mx);
}

fn hsv_to_rgb(hsv: vec3<f32>) -> vec3<f32> {
    let h = hsv.x * 6.0;
    let s = hsv.y;
    let v = hsv.z;
    let c = v * s;
    let x = c * (1.0 - abs(h % 2.0 - 1.0));
    let m = v - c;
    var rgb: vec3<f32>;
    if h < 1.0 { rgb = vec3<f32>(c, x, 0.0); }
    else if h < 2.0 { rgb = vec3<f32>(x, c, 0.0); }
    else if h < 3.0 { rgb = vec3<f32>(0.0, c, x); }
    else if h < 4.0 { rgb = vec3<f32>(0.0, x, c); }
    else if h < 5.0 { rgb = vec3<f32>(x, 0.0, c); }
    else { rgb = vec3<f32>(c, 0.0, x); }
    return rgb + vec3<f32>(m);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(source_tex, tex_sampler, in.uv);

    // Convert to HSV
    var hsv = rgb_to_hsv(color.rgb);

    // Hue shift
    hsv.x = (hsv.x + uniforms.hue_shift + 1.0) % 1.0;

    // Saturation
    hsv.y = clamp(hsv.y * uniforms.saturation, 0.0, 1.0);

    // Convert back to RGB
    var rgb = hsv_to_rgb(hsv);

    // Gain (multiply)
    rgb *= uniforms.gain;

    // Contrast (around 0.5 midpoint)
    rgb = (rgb - 0.5) * uniforms.contrast + 0.5;

    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), color.a);
}
