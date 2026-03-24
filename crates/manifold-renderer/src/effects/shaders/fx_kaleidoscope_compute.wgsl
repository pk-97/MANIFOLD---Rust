// Compute variant of fx_kaleidoscope.wgsl — same math, no TBDR tile overhead.
// Polar-coordinate segment mirroring.

struct Uniforms {
    amount: f32,
    segments: f32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

const TAU: f32 = 6.28318530718;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }

    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let src = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    let original = src.rgb;

    // Center UV at origin
    let centered = uv - 0.5;

    // Convert to polar coordinates
    let angle = atan2(centered.y, centered.x);
    let radius = length(centered);

    // Slice angle into segments and mirror alternating slices
    let segment_angle = TAU / uniforms.segments;
    let slice_index = floor(angle / segment_angle);
    var local_angle = angle - slice_index * segment_angle;

    // Mirror odd slices for seamless reflection
    if (abs(slice_index) % 2.0) > 0.5 {
        local_angle = segment_angle - local_angle;
    }

    // Convert back to cartesian
    var kaleid_uv: vec2<f32>;
    kaleid_uv.x = cos(local_angle) * radius + 0.5;
    kaleid_uv.y = sin(local_angle) * radius + 0.5;

    // Clamp to valid UV range
    kaleid_uv = clamp(kaleid_uv, vec2<f32>(0.0), vec2<f32>(1.0));

    let kaleid_sample = textureSampleLevel(source_tex, tex_sampler, kaleid_uv, 0.0);
    let result = mix(original, kaleid_sample.rgb, uniforms.amount);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(result, mix(src.a, kaleid_sample.a, uniforms.amount)));
}
