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
    // Tube-style soft saturation
    let saturated = tanh(rgb * gain * (1.0 + drive));
    let lum = dot(saturated, LUMA);
    // Warm shadows, cool highlights
    let warm = vec3<f32>(0.02, 0.01, -0.02) * (1.0 - lum) * drive;
    let cool = vec3<f32>(-0.01, 0.0, 0.02) * lum * drive;
    return saturated + warm + cool;
}

fn apply_film(rgb: vec3<f32>, gain: f32, drive: f32) -> vec3<f32> {
    var compressed = rgb * gain;
    // Lifted blacks — never true zero
    compressed = max(compressed, vec3<f32>(0.02 * drive));
    // Highlight shoulder (log rolloff)
    compressed = 1.0 - exp(-compressed * (1.0 + drive));
    // Desaturate highlights (film stock characteristic)
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
    // Saturation boost proportional to compression activity
    let sat_boost = abs(gain - 1.0) * drive * 0.5;
    return mix(vec3<f32>(lum), result, 1.0 + sat_boost);
}

fn apply_grit(rgb: vec3<f32>, gain: f32, drive: f32) -> vec3<f32> {
    // FET/tape-style per-channel soft clipping
    let driven = rgb * gain * (1.0 + drive * 2.0);
    return sign(driven) * (1.0 - exp(-abs(driven)));
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
    var compressed: vec3<f32>;
    if uniforms.character == 0u {
        compressed = apply_clean(color.rgb, gain);
    } else if uniforms.character == 1u {
        compressed = apply_warm(color.rgb, gain, drive);
    } else if uniforms.character == 2u {
        compressed = apply_film(color.rgb, gain, drive);
    } else if uniforms.character == 3u {
        compressed = apply_vivid(color.rgb, gain, drive);
    } else {
        compressed = apply_grit(color.rgb, gain, drive);
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
