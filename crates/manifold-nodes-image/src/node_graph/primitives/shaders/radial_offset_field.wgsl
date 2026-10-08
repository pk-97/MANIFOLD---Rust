// node.radial_offset_field — directional displacement field generator.
//
// Radial mode: dir = normalize(uv - 0.5), scaled by a center→edge falloff
//   mask. Linear mode: dir = (cos(angle), sin(angle)), uniform across frame.
// Output: R = dir.x, G = dir.y, B = 0, A = 1. The field is SIGNED (RG can be
// negative — the left/bottom half points the opposite way from the right/top).
//
// The reusable direction field behind the radial-warp family. Feed it as the
// velocity/displacement field to node.chromatic_displace (chromatic
// aberration), node.uv_displace_by_flow (lens / zoom warp), node.texture_advect,
// etc. The offset MAGNITUDE and ± SIGN are applied by the consumer (scale its
// `amount`/`weight`); this node only emits the unit-ish direction (|dir| <= 1).
//
// The radial branch is a verbatim port of the legacy fx_chromatic_aberration
// direction math (smoothstep(0, 0.707, dist), faded by 1 - falloff, with the
// near-center (1,0) fallback), so chromatic aberration decomposes to
// radial_offset_field -> chromatic_displace -> mix without changing the look.
//
// Bindings:
//   @binding(0) uniforms (16 bytes)
//   @binding(1) output_tex (rgba16float storage)

struct Uniforms {
    mode: u32,    // 0 = Radial, 1 = Linear
    angle: f32,   // degrees — Linear mode only
    falloff: f32, // 0..1 — Radial mode only
    _pad0: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    var dir: vec2<f32>;
    if uniforms.mode == 0u {
        let delta = uv - vec2<f32>(0.5, 0.5);
        let dist = length(delta);
        var radial_mask = smoothstep(0.0, 0.707, dist);
        radial_mask = mix(radial_mask, 1.0, 1.0 - uniforms.falloff);
        if dist > 1e-5 {
            dir = normalize(delta) * radial_mask;
        } else {
            dir = vec2<f32>(1.0, 0.0);
        }
    } else {
        let rad = uniforms.angle * 0.01745329;
        dir = vec2<f32>(cos(rad), sin(rad));
    }

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(dir, 0.0, 1.0));
}
