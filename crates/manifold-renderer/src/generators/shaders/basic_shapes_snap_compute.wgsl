struct Uniforms {
    time_val: f32,
    beat: f32,
    aspect_ratio: f32,
    line_thickness: f32,
    uv_scale: f32,
    trigger_count: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output: texture_storage_2d<rgba16float, write>;

// ── Utility ──

fn ease_out_cubic(t: f32) -> f32 {
    let t1 = 1.0 - t;
    return 1.0 - t1 * t1 * t1;
}

fn rotate2d(p: vec2<f32>, angle: f32) -> vec2<f32> {
    let s = sin(angle);
    let c = cos(angle);
    return vec2<f32>(p.x * c - p.y * s, p.x * s + p.y * c);
}

// ── SDF functions ──

fn sd_square(p: vec2<f32>, size: f32) -> f32 {
    let d = abs(p) - vec2<f32>(size);
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0);
}

fn sd_diamond(p: vec2<f32>, size: f32) -> f32 {
    let ap = abs(p);
    return (ap.x + ap.y - size) / 1.414213562;
}

fn sd_octagon(p_in: vec2<f32>, r: f32) -> f32 {
    let k = vec3<f32>(-0.9238795325, 0.3826834323, 0.4142135623);
    var p = abs(p_in);
    p -= 2.0 * min(dot(vec2<f32>(k.x, k.y), p), 0.0) * vec2<f32>(k.x, k.y);
    p -= 2.0 * min(dot(vec2<f32>(-k.x, k.y), p), 0.0) * vec2<f32>(-k.x, k.y);
    p -= vec2<f32>(clamp(p.x, -k.z * r, k.z * r), r);
    return length(p) * sign(p.y);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output);
    if id.x >= dims.x || id.y >= dims.y { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    var p_uv = uv - vec2<f32>(0.5);
    p_uv.x *= u.aspect_ratio;
    p_uv *= u.uv_scale;

    let beat_frac = fract(u.beat);

    // Cycle shape + fill from trigger count (3 shapes x 2 fill = 6 variants)
    let tc = i32(u.trigger_count);
    let variant = u32(tc) % 6u;
    let shape_idx = variant % 3u;
    let is_wireframe = variant >= 3u;

    // Rotation cycles every 6 triggers (4 angles x 2 directions = 8 steps)
    let DEG45 = 0.78539816; // pi/4
    let rot_step = (u32(tc) / 6u) % 8u;
    let target_angle = f32(rot_step % 4u) * DEG45;
    let rot_direction = select(1.0, -1.0, rot_step >= 4u);

    // Animated rotation: eased arrival at target angle
    let rotation = target_angle * rot_direction * ease_out_cubic(saturate(beat_frac * 4.0));

    // Sharp scale snap: instant appearance at beat 0, fast ease-out
    // Reduced by 10%: 0.35 -> 0.315
    let scale_anim = ease_out_cubic(saturate(beat_frac * 6.0));

    // Transform UV
    var p = p_uv / (0.315 * scale_anim + 0.001);
    p = rotate2d(p, rotation);

    // Evaluate SDF
    var d: f32;
    switch shape_idx {
        case 1u: { d = sd_diamond(p, 1.0); }
        case 2u: { d = sd_octagon(p, 1.0); }
        default: { d = sd_square(p, 1.0); }
    }

    // Anti-aliased edge — compute shaders have no fwidth, use analytical estimate
    let texel_size = 1.0 / vec2<f32>(dims);
    let pw = length(texel_size) * u.uv_scale * (1.0 / (0.315 * scale_anim + 0.001));

    var shape: f32;
    if is_wireframe {
        // Hollow outline only (thinner)
        let thickness = u.line_thickness;
        shape = 1.0 - smoothstep(thickness - pw, thickness + pw, abs(d));
    } else {
        // Solid fill
        shape = 1.0 - smoothstep(-pw, pw, d);
    }

    // Beat flash: bright burst on downbeat
    let flash = smoothstep(0.1, 0.0, beat_frac) * 0.4;

    // Black and white: white shape on black (no fade)
    var lum = shape + flash * shape;
    lum = saturate(lum);

    textureStore(output, vec2<i32>(id.xy), vec4<f32>(lum, lum, lum, lum));
}
