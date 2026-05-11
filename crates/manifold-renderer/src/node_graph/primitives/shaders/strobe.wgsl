// primitive.strobe — pixel-exact replacement for legacy
// `effects/shaders/fx_strobe.wgsl`. Beat-synced square wave flash.
// Fused composite — atomic BeatGate + Mix would introduce fp16
// rounding on the gate write that breaks bit-exact parity vs the
// single-pass legacy.

struct Uniforms {
    amount: f32,
    rate: f32,
    mode: u32,
    beat: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let src = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    var col = src.rgb;

    let phase = fract(uniforms.beat * uniforms.rate);
    let on = step(0.5, phase);

    let strobe = uniforms.amount * on;

    if uniforms.mode == 2u {
        col = col * mix(1.0, 3.0, strobe);
    } else if uniforms.mode == 1u {
        col = mix(col, vec3<f32>(1.0, 1.0, 1.0), strobe);
    } else {
        col = col * (1.0 - strobe);
    }

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(col, src.a));
}
