// Infrared / thermal vision effect.
// Unity ref: InfraredEffect.shader

struct Uniforms {
    amount: f32,
    palette: f32,
    contrast: f32,
    _pad0: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

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

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let src = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);

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

    let result = mix(src.rgb, thermal, uniforms.amount);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(result, src.a));
}
