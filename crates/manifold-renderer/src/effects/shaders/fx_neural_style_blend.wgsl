// Neural Style blend shader.
// Mixes the original source with the stylized result from AdaIN inference.
//
// Uses ComputeDualBlitHelper bindings:
//   @binding(0) uniforms
//   @binding(1) source_a = original frame
//   @binding(2) source_b = stylized result (from ONNX inference)
//   @binding(3) sampler
//   @binding(4) output (storage write)

struct Uniforms {
    strength: f32,
    has_result: u32,
    _pad: vec2<f32>,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var source_a: texture_2d<f32>;
@group(0) @binding(2) var source_b: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y { return; }

    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let original = textureSampleLevel(source_a, tex_sampler, uv, 0.0);

    if u.has_result == 0u {
        // No inference result yet — pass through original.
        textureStore(output_tex, id.xy, original);
        return;
    }

    let styled = textureSampleLevel(source_b, tex_sampler, uv, 0.0);
    // Preserve original alpha, blend RGB only.
    let blended = vec4<f32>(
        mix(original.rgb, styled.rgb, u.strength),
        original.a,
    );
    textureStore(output_tex, id.xy, blended);
}
