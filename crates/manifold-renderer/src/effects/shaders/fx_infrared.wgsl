// Infrared / thermal vision effect.
// Unity ref: InfraredEffect.shader

struct Uniforms {
    amount: f32,
    palette: f32,
    contrast: f32,
    noise: f32,
    scanline: f32,
    hot_spot: f32,
    time: f32,
    texel_size_x: f32,  // 1/width
    texel_size_y: f32,  // 1/height
    texel_size_z: f32,  // width
    texel_size_w: f32,  // height
    _pad0: f32,
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

// Hash matching Unity InfraredEffect.shader hash()
fn hash(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
}

// --- Palette functions ---
// Each maps a 0-1 luminance value to an RGB color

// White Hot: black -> white
fn palette_white_hot(t: f32) -> vec3<f32> {
    return vec3<f32>(t, t, t);
}

// Black Hot: white -> black (inverted)
fn palette_black_hot(t: f32) -> vec3<f32> {
    let v = 1.0 - t;
    return vec3<f32>(v, v, v);
}

// Green Night Vision: dark green -> bright green
fn palette_green_nv(t: f32) -> vec3<f32> {
    return vec3<f32>(t * 0.15, t, t * 0.1);
}

// Iron Bow: black -> purple -> red -> orange -> yellow -> white
fn palette_iron_bow(t: f32) -> vec3<f32> {
    var c: vec3<f32>;
    if (t < 0.2) {
        let s = t / 0.2;
        c = mix(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.15, 0.0, 0.3), s);
    } else if (t < 0.4) {
        let s = (t - 0.2) / 0.2;
        c = mix(vec3<f32>(0.15, 0.0, 0.3), vec3<f32>(0.7, 0.05, 0.1), s);
    } else if (t < 0.6) {
        let s = (t - 0.4) / 0.2;
        c = mix(vec3<f32>(0.7, 0.05, 0.1), vec3<f32>(0.95, 0.4, 0.0), s);
    } else if (t < 0.8) {
        let s = (t - 0.6) / 0.2;
        c = mix(vec3<f32>(0.95, 0.4, 0.0), vec3<f32>(1.0, 0.85, 0.2), s);
    } else {
        let s = (t - 0.8) / 0.2;
        c = mix(vec3<f32>(1.0, 0.85, 0.2), vec3<f32>(1.0, 1.0, 0.9), s);
    }
    return c;
}

// Rainbow: full HSV sweep
fn palette_rainbow(t: f32) -> vec3<f32> {
    let c = abs(fract(t + vec3<f32>(0.0, 0.333, 0.667)) * 6.0 - 3.0) - 1.0;
    return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
}

// Lava: black -> deep red -> orange -> yellow
fn palette_lava(t: f32) -> vec3<f32> {
    var c: vec3<f32>;
    if (t < 0.25) {
        let s = t / 0.25;
        c = mix(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.4, 0.02, 0.0), s);
    } else if (t < 0.5) {
        let s = (t - 0.25) / 0.25;
        c = mix(vec3<f32>(0.4, 0.02, 0.0), vec3<f32>(0.85, 0.15, 0.0), s);
    } else if (t < 0.75) {
        let s = (t - 0.5) / 0.25;
        c = mix(vec3<f32>(0.85, 0.15, 0.0), vec3<f32>(1.0, 0.55, 0.0), s);
    } else {
        let s = (t - 0.75) / 0.25;
        c = mix(vec3<f32>(1.0, 0.55, 0.0), vec3<f32>(1.0, 0.9, 0.2), s);
    }
    return c;
}

// Arctic: black -> deep blue -> cyan -> white
fn palette_arctic(t: f32) -> vec3<f32> {
    var c: vec3<f32>;
    if (t < 0.3) {
        let s = t / 0.3;
        c = mix(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.0, 0.05, 0.35), s);
    } else if (t < 0.6) {
        let s = (t - 0.3) / 0.3;
        c = mix(vec3<f32>(0.0, 0.05, 0.35), vec3<f32>(0.1, 0.55, 0.8), s);
    } else if (t < 0.85) {
        let s = (t - 0.6) / 0.25;
        c = mix(vec3<f32>(0.1, 0.55, 0.8), vec3<f32>(0.6, 0.9, 1.0), s);
    } else {
        let s = (t - 0.85) / 0.15;
        c = mix(vec3<f32>(0.6, 0.9, 1.0), vec3<f32>(1.0, 1.0, 1.0), s);
    }
    return c;
}

// Magenta: black -> dark magenta -> hot pink -> white
fn palette_magenta(t: f32) -> vec3<f32> {
    var c: vec3<f32>;
    if (t < 0.3) {
        let s = t / 0.3;
        c = mix(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.3, 0.0, 0.35), s);
    } else if (t < 0.6) {
        let s = (t - 0.3) / 0.3;
        c = mix(vec3<f32>(0.3, 0.0, 0.35), vec3<f32>(0.9, 0.1, 0.5), s);
    } else if (t < 0.85) {
        let s = (t - 0.6) / 0.25;
        c = mix(vec3<f32>(0.9, 0.1, 0.5), vec3<f32>(1.0, 0.5, 0.7), s);
    } else {
        let s = (t - 0.85) / 0.15;
        c = mix(vec3<f32>(1.0, 0.5, 0.7), vec3<f32>(1.0, 0.95, 1.0), s);
    }
    return c;
}

// Electric: black -> indigo -> electric blue -> cyan
fn palette_electric(t: f32) -> vec3<f32> {
    var c: vec3<f32>;
    if (t < 0.25) {
        let s = t / 0.25;
        c = mix(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.15, 0.0, 0.4), s);
    } else if (t < 0.5) {
        let s = (t - 0.25) / 0.25;
        c = mix(vec3<f32>(0.15, 0.0, 0.4), vec3<f32>(0.1, 0.2, 0.9), s);
    } else if (t < 0.75) {
        let s = (t - 0.5) / 0.25;
        c = mix(vec3<f32>(0.1, 0.2, 0.9), vec3<f32>(0.0, 0.7, 1.0), s);
    } else {
        let s = (t - 0.75) / 0.25;
        c = mix(vec3<f32>(0.0, 0.7, 1.0), vec3<f32>(0.7, 1.0, 1.0), s);
    }
    return c;
}

// Toxic: black -> dark green -> lime -> yellow
fn palette_toxic(t: f32) -> vec3<f32> {
    var c: vec3<f32>;
    if (t < 0.3) {
        let s = t / 0.3;
        c = mix(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.0, 0.2, 0.05), s);
    } else if (t < 0.6) {
        let s = (t - 0.3) / 0.3;
        c = mix(vec3<f32>(0.0, 0.2, 0.05), vec3<f32>(0.3, 0.75, 0.0), s);
    } else if (t < 0.85) {
        let s = (t - 0.6) / 0.25;
        c = mix(vec3<f32>(0.3, 0.75, 0.0), vec3<f32>(0.7, 1.0, 0.1), s);
    } else {
        let s = (t - 0.85) / 0.15;
        c = mix(vec3<f32>(0.7, 1.0, 0.1), vec3<f32>(1.0, 1.0, 0.3), s);
    }
    return c;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(source_tex, tex_sampler, in.uv);

    // Extract luminance (BT.601 weights)
    let lum_raw = dot(src.rgb, vec3<f32>(0.299, 0.587, 0.114));

    // Apply contrast (pivot at 0.5); clamp negative only so HDR values
    // above 1 extrapolate the palette instead of collapsing to hottest slot
    let lum = max(0.0, (lum_raw - 0.5) * uniforms.contrast + 0.5);

    // Map through selected palette
    let pal = i32(uniforms.palette);
    var thermal: vec3<f32>;
    if (pal == 0) {
        thermal = palette_white_hot(lum);
    } else if (pal == 1) {
        thermal = palette_black_hot(lum);
    } else if (pal == 2) {
        thermal = palette_green_nv(lum);
    } else if (pal == 3) {
        thermal = palette_iron_bow(lum);
    } else if (pal == 4) {
        thermal = palette_rainbow(lum);
    } else if (pal == 5) {
        thermal = palette_lava(lum);
    } else if (pal == 6) {
        thermal = palette_arctic(lum);
    } else if (pal == 7) {
        thermal = palette_magenta(lum);
    } else if (pal == 8) {
        thermal = palette_electric(lum);
    } else {
        thermal = palette_toxic(lum);
    }

    // Hot spot bloom (bright region glow)
    // hotMask is computed but never used — only hotGlow contributes (matches Unity exactly)
    let hot_mask = smoothstep(0.7, 1.0, lum) * uniforms.hot_spot;
    let _ = hot_mask;
    let texel = vec2<f32>(uniforms.texel_size_x, uniforms.texel_size_y) * 4.0;
    let hot_l = dot(textureSample(source_tex, tex_sampler, in.uv + vec2<f32>(-texel.x, 0.0)).rgb, vec3<f32>(0.299, 0.587, 0.114));
    let hot_r = dot(textureSample(source_tex, tex_sampler, in.uv + vec2<f32>( texel.x, 0.0)).rgb, vec3<f32>(0.299, 0.587, 0.114));
    let hot_u = dot(textureSample(source_tex, tex_sampler, in.uv + vec2<f32>(0.0,  texel.y)).rgb, vec3<f32>(0.299, 0.587, 0.114));
    let hot_d = dot(textureSample(source_tex, tex_sampler, in.uv + vec2<f32>(0.0, -texel.y)).rgb, vec3<f32>(0.299, 0.587, 0.114));
    let hot_avg = (hot_l + hot_r + hot_u + hot_d) * 0.25;
    let hot_glow = smoothstep(0.6, 1.0, hot_avg);
    thermal += hot_glow * uniforms.hot_spot * 0.4;

    // Sensor noise
    let noise = (hash(in.uv * vec2<f32>(uniforms.texel_size_z, uniforms.texel_size_w) + uniforms.time * 137.0) * 2.0 - 1.0) * uniforms.noise * 0.15;
    thermal += noise;

    // Scanline overlay
    let scanline = 1.0 - uniforms.scanline * 0.3 * step(0.5, fract(in.uv.y * uniforms.texel_size_w * 0.5));
    thermal *= scanline;

    let result = mix(src.rgb, thermal, uniforms.amount);
    return vec4<f32>(result, src.a);
}
