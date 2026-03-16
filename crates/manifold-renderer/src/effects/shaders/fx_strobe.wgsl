// Strobe effect — beat-synced square wave flash (opacity, white, or gain).

struct Uniforms {
    amount: f32,
    rate: f32,
    mode: u32,      // 0=Opacity(black), 1=White, 2=Gain
    beat: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(source_tex, tex_sampler, in.uv);
    var col = src.rgb;

    // Square wave strobe synced to beat grid
    let phase = fract(uniforms.beat * uniforms.rate);
    let on = step(0.5, phase);

    let strobe = uniforms.amount * on;

    if uniforms.mode == 2u {
        // Mode 2: Gain — normal when off, boosted when on
        col = col * mix(1.0, 3.0, strobe);
    } else if uniforms.mode == 1u {
        // Mode 1: White — flash to white
        col = mix(col, vec3<f32>(1.0, 1.0, 1.0), strobe);
    } else {
        // Mode 0: Opacity — flash to black
        col = col * (1.0 - strobe);
    }

    return vec4<f32>(col, src.a);
}
