// Auto Gain — apply pass with character coloration.
// Reads precomputed gain from CPU-side envelope follower, applies gain
// with optional analog-style coloration and HDR-aware processing.

struct Uniforms {
    gain: f32,
    character: u32,     // 0=clean, 1=warm, 2=film, 3=vivid, 4=grit
    color_push: f32,
    hdr_retention: f32,
    gain_delta: f32,    // gain - 1.0, for coloration intensity
    amount: f32,        // wet/dry mix (parallel compression)
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

const LUMA: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);

// ── Character modes ────────────────────────────────────────────────────

fn apply_clean(rgb: vec3<f32>, gain: f32) -> vec3<f32> {
    return rgb * gain;
}

fn apply_warm(rgb: vec3<f32>, gain: f32, drive: f32) -> vec3<f32> {
    // Tube-style soft saturation: blend linear→tanh by drive.
    // tanh(x) ≈ x for small x, so dark values stay dark.
    let gained = rgb * gain;
    let saturated = tanh(gained);
    let blended = mix(gained, saturated, drive);
    // Multiplicative warm/cool tint — preserves black point
    let lum = clamp(dot(blended, LUMA), 0.0, 1.0);
    let warm_tint = vec3<f32>(1.0 + 0.04 * drive, 1.0 + 0.02 * drive, 1.0 - 0.04 * drive);
    let cool_tint = vec3<f32>(1.0 - 0.02 * drive, 1.0, 1.0 + 0.04 * drive);
    return blended * mix(warm_tint, cool_tint, lum);
}

fn apply_film(rgb: vec3<f32>, gain: f32, drive: f32) -> vec3<f32> {
    let gained = rgb * gain;
    // Reinhard filmic shoulder: f(0) = 0, f(x) ≈ x for small x,
    // f(x) → white_point for large x. Preserves blacks exactly.
    let white_point = 2.0 - drive; // more drive = earlier rolloff
    let wp2 = white_point * white_point;
    let shoulder = gained * (1.0 + gained / wp2) / (1.0 + gained);
    // Blend linear→shoulder by drive
    let compressed = mix(gained, shoulder, drive);
    // Subtle highlight desaturation (film stock characteristic)
    let lum = dot(compressed, LUMA);
    let desat = smoothstep(0.7, 1.0, lum) * drive * 0.3;
    return mix(compressed, vec3<f32>(lum), desat);
}

fn apply_vivid(rgb: vec3<f32>, gain: f32, drive: f32) -> vec3<f32> {
    // Per-channel gain with slight R/B divergence
    let result = rgb * vec3<f32>(
        gain * (1.0 + 0.05 * drive),
        gain,
        gain * (1.0 - 0.05 * drive),
    );
    let lum = dot(result, LUMA);
    // Saturation boost proportional to compression activity.
    // Capped at 0.3 to prevent minority channels going negative in HDR.
    let sat_boost = min(abs(gain - 1.0) * drive * 0.5, 0.3);
    return mix(vec3<f32>(lum), result, 1.0 + sat_boost);
}

fn apply_grit(rgb: vec3<f32>, gain: f32, drive: f32) -> vec3<f32> {
    // FET/tape-style per-channel soft clipping.
    // Blend linear→clipped by drive so blacks stay black.
    let gained = rgb * gain;
    let clipped = sign(gained) * (1.0 - exp(-abs(gained)));
    return mix(gained, clipped, drive);
}

// ── Main ───────────────────────────────────────────────────────────────

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y { return; }

    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let color = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);

    let gain = uniforms.gain;
    let drive = abs(uniforms.gain_delta);

    // Apply character-specific gain coloration
    let clean = color.rgb * gain;
    var compressed: vec3<f32>;
    if uniforms.character == 0u {
        compressed = clean;
    } else if uniforms.character == 1u {
        compressed = apply_warm(color.rgb, gain, drive);
    } else if uniforms.character == 2u {
        compressed = apply_film(color.rgb, gain, drive);
    } else if uniforms.character == 3u {
        compressed = apply_vivid(color.rgb, gain, drive);
    } else {
        compressed = apply_grit(color.rgb, gain, drive);
    }

    // Clamp to non-negative — character curves can produce negative channels
    // when saturation boost or per-channel divergence exceeds the minority
    // channel's headroom (especially with HDR-range input).
    compressed = max(compressed, vec3<f32>(0.0));

    // Energy-preserve: character curves shape color/tone but must not
    // change overall brightness. Rescale to match the clean gain path's
    // luminance so the compressor stays in control of brightness.
    if uniforms.character != 0u {
        let clean_lum = dot(clean, LUMA);
        let char_lum = dot(compressed, LUMA);
        if char_lum > 0.001 {
            compressed = compressed * (clean_lum / char_lum);
        }
    }

    // HDR retention: preserve above-1.0 energy based on retention param.
    // SDR portion uses the fully compressed signal.
    // HDR portion blends between compressed and original based on retention.
    let original_hdr = max(color.rgb - vec3<f32>(1.0), vec3<f32>(0.0));
    let compressed_hdr = max(compressed - vec3<f32>(1.0), vec3<f32>(0.0));
    let retained_hdr = mix(compressed_hdr, original_hdr, uniforms.hdr_retention);
    let sdr = min(compressed, vec3<f32>(1.0));
    var result = sdr + retained_hdr;

    // Color push: saturation shift proportional to gain delta.
    // Positive push + lifting (gain>1) = more saturation.
    // Positive push + taming (gain<1) = less saturation.
    let lum = dot(result, LUMA);
    let sat_shift = uniforms.gain_delta * uniforms.color_push;
    result = mix(vec3<f32>(lum), result, 1.0 + sat_shift);

    // Wet/dry mix — parallel compression
    result = mix(color.rgb, result, uniforms.amount);

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(result, color.a));
}
