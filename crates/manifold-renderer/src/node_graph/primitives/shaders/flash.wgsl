// node.flash — modulate an image's brightness by a scalar `amount`
// (typically a gate/LFO/envelope), in one of three modes. Verbatim
// port of the apply block in fx_strobe / node.strobe, with the gate
// computation factored out to node.beat_gate (or any scalar source).
//   Opacity (0): col * (1 - amount)        — flash toward black
//   White   (1): mix(col, white, amount)   — flash toward white
//   Gain    (2): col * mix(1, 3, amount)   — brighten (3x at amount=1)

struct Uniforms {
    amount: f32,
    mode: u32,
    _pad0: f32,
    _pad1: f32,
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

    let amount = uniforms.amount;
    if uniforms.mode == 2u {
        col = col * mix(1.0, 3.0, amount);
    } else if uniforms.mode == 1u {
        col = mix(col, vec3<f32>(1.0, 1.0, 1.0), amount);
    } else {
        col = col * (1.0 - amount);
    }

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(col, src.a));
}
