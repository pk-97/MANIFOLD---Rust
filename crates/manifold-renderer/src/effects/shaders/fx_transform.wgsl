// TransformEffect.shader → fx_transform.wgsl
// Unity ref: Assets/Shaders/TransformEffect.shader
//
// UV transform math (same order as Unity's fragment shader):
//   center(-0.5) → aspect-correct(×aspect) → rotate(cos/sin matrix)
//   → un-aspect(÷aspect) → scale(÷max(scale,0.01)) → translate(subtract)
//   → un-center(+0.5) → OOB check(transparent black if outside [0,1])

struct Uniforms {
    translate_x:  f32,  // _TranslateX  — GetParam(0)
    translate_y:  f32,  // _TranslateY  — GetParam(1)
    scale:        f32,  // _Scale       — GetParam(2)
    rotation:     f32,  // _Rotation    — GetParam(3) * Deg2Rad
    aspect_ratio: f32,  // ctx.width / ctx.height
    _pad0:        f32,
    _pad1:        f32,
    _pad2:        f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0)       uv:       vec2<f32>,
}

// Fullscreen triangle — no vertex buffer needed.
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOut {
    var out: VertexOut;
    let x = f32((vi << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(vi & 2u) * 2.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>(x * 0.5 + 0.5, 1.0 - (y * 0.5 + 0.5));
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    // TransformEffect.shader frag — same math, same order
    var uv = in.uv - vec2<f32>(0.5, 0.5);

    uv.x = uv.x * u.aspect_ratio;

    let cos_r = cos(u.rotation);
    let sin_r = sin(u.rotation);
    uv = vec2<f32>(
        uv.x * cos_r - uv.y * sin_r,
        uv.x * sin_r + uv.y * cos_r,
    );

    uv.x = uv.x / u.aspect_ratio;

    uv = uv / max(u.scale, 0.01);
    uv = uv - vec2<f32>(u.translate_x, u.translate_y);
    uv = uv + vec2<f32>(0.5, 0.5);

    // Out-of-bounds → transparent black
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    return textureSample(source_tex, tex_sampler, uv);
}
