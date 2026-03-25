// Compute variant of mycelium_display.wgsl.
// Identical math — only I/O mechanism changes:
//   - textureSampleLevel instead of textureSample
//   - textureStore to output storage texture instead of fragment return
//   - @compute @workgroup_size(16,16) instead of vertex+fragment

struct DisplayUniforms {
    hue: f32,
    glow: f32,
    uv_scale: f32,
    time: f32,
};

@group(0) @binding(0) var<uniform> params: DisplayUniforms;
@group(0) @binding(1) var t_trail: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

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

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if (gid.x >= u32(dims.x) || gid.y >= u32(dims.y)) {
        return;
    }

    let uv_raw = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let uv = uv_raw / params.uv_scale;
    let t = clamp(textureSampleLevel(t_trail, s, uv, 0.0).r, 0.0, 1.0);

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
    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(rgb, lum));
}
