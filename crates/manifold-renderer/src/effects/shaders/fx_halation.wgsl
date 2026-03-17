// Mechanical port of Unity HalationEffect.shader.
// Two textures: main_tex (_MainTex) and halo_tex (_HaloTex).
// Mode 0: fragThreshold — 13-tap blur with threshold extraction + tint (reads main_tex)
// Mode 1: fragBlurWide  — 13-tap blur (reads halo_tex)
// Mode 2: fragComposite — src + halo * amount (reads main_tex + halo_tex)

struct Uniforms {
    mode: u32,
    amount: f32,
    threshold: f32,
    spread: f32,
    tint_r: f32,
    tint_g: f32,
    tint_b: f32,
    main_texel_size_x: f32,
    main_texel_size_y: f32,
    halo_texel_size_x: f32,
    halo_texel_size_y: f32,
    _pad: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var main_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var halo_tex: texture_2d<f32>;

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
    if uniforms.mode == 0u {
        // HalationEffect.shader Pass 0 (ThresholdTintBlur): fragThreshold
        // float2 texel = _MainTex_TexelSize.xy;
        // float r = _Spread * 6.0 + 1.0;
        // float3 tint = float3(_TintR, _TintG, _TintB);
        let texel = vec2<f32>(uniforms.main_texel_size_x, uniforms.main_texel_size_y);
        let r = uniforms.spread * 6.0 + 1.0;
        let tint = vec3<f32>(uniforms.tint_r, uniforms.tint_g, uniforms.tint_b);

        // Port of SAMPLE_THRESH macro (shader lines 76-82):
        // float2 suv = i.uv + float2(ox, oy) * texel * r;
        // float3 col = tex2D(_MainTex, suv).rgb;
        // float lm = dot(col, float3(0.2126, 0.7152, 0.0722));
        // float mk = smoothstep(_Threshold - 0.1, _Threshold + 0.1, lm);
        // acc += col * mk * tint * w;
        var acc = vec3<f32>(0.0);

        // SAMPLE_THRESH( 0,  0, 0.16)
        var suv = in.uv + vec2<f32>( 0.0,  0.0) * texel * r;
        var col = textureSample(main_tex, tex_sampler, suv).rgb;
        var lm = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
        var mk = smoothstep(uniforms.threshold - 0.1, uniforms.threshold + 0.1, lm);
        acc += col * mk * tint * 0.16;

        // SAMPLE_THRESH( 1,  0, 0.10)
        suv = in.uv + vec2<f32>( 1.0,  0.0) * texel * r;
        col = textureSample(main_tex, tex_sampler, suv).rgb;
        lm = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
        mk = smoothstep(uniforms.threshold - 0.1, uniforms.threshold + 0.1, lm);
        acc += col * mk * tint * 0.10;

        // SAMPLE_THRESH(-1,  0, 0.10)
        suv = in.uv + vec2<f32>(-1.0,  0.0) * texel * r;
        col = textureSample(main_tex, tex_sampler, suv).rgb;
        lm = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
        mk = smoothstep(uniforms.threshold - 0.1, uniforms.threshold + 0.1, lm);
        acc += col * mk * tint * 0.10;

        // SAMPLE_THRESH( 0,  1, 0.10)
        suv = in.uv + vec2<f32>( 0.0,  1.0) * texel * r;
        col = textureSample(main_tex, tex_sampler, suv).rgb;
        lm = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
        mk = smoothstep(uniforms.threshold - 0.1, uniforms.threshold + 0.1, lm);
        acc += col * mk * tint * 0.10;

        // SAMPLE_THRESH( 0, -1, 0.10)
        suv = in.uv + vec2<f32>( 0.0, -1.0) * texel * r;
        col = textureSample(main_tex, tex_sampler, suv).rgb;
        lm = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
        mk = smoothstep(uniforms.threshold - 0.1, uniforms.threshold + 0.1, lm);
        acc += col * mk * tint * 0.10;

        // SAMPLE_THRESH( 1,  1, 0.06)
        suv = in.uv + vec2<f32>( 1.0,  1.0) * texel * r;
        col = textureSample(main_tex, tex_sampler, suv).rgb;
        lm = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
        mk = smoothstep(uniforms.threshold - 0.1, uniforms.threshold + 0.1, lm);
        acc += col * mk * tint * 0.06;

        // SAMPLE_THRESH(-1,  1, 0.06)
        suv = in.uv + vec2<f32>(-1.0,  1.0) * texel * r;
        col = textureSample(main_tex, tex_sampler, suv).rgb;
        lm = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
        mk = smoothstep(uniforms.threshold - 0.1, uniforms.threshold + 0.1, lm);
        acc += col * mk * tint * 0.06;

        // SAMPLE_THRESH( 1, -1, 0.06)
        suv = in.uv + vec2<f32>( 1.0, -1.0) * texel * r;
        col = textureSample(main_tex, tex_sampler, suv).rgb;
        lm = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
        mk = smoothstep(uniforms.threshold - 0.1, uniforms.threshold + 0.1, lm);
        acc += col * mk * tint * 0.06;

        // SAMPLE_THRESH(-1, -1, 0.06)
        suv = in.uv + vec2<f32>(-1.0, -1.0) * texel * r;
        col = textureSample(main_tex, tex_sampler, suv).rgb;
        lm = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
        mk = smoothstep(uniforms.threshold - 0.1, uniforms.threshold + 0.1, lm);
        acc += col * mk * tint * 0.06;

        // SAMPLE_THRESH( 2,  0, 0.03)
        suv = in.uv + vec2<f32>( 2.0,  0.0) * texel * r;
        col = textureSample(main_tex, tex_sampler, suv).rgb;
        lm = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
        mk = smoothstep(uniforms.threshold - 0.1, uniforms.threshold + 0.1, lm);
        acc += col * mk * tint * 0.03;

        // SAMPLE_THRESH(-2,  0, 0.03)
        suv = in.uv + vec2<f32>(-2.0,  0.0) * texel * r;
        col = textureSample(main_tex, tex_sampler, suv).rgb;
        lm = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
        mk = smoothstep(uniforms.threshold - 0.1, uniforms.threshold + 0.1, lm);
        acc += col * mk * tint * 0.03;

        // SAMPLE_THRESH( 0,  2, 0.03)
        suv = in.uv + vec2<f32>( 0.0,  2.0) * texel * r;
        col = textureSample(main_tex, tex_sampler, suv).rgb;
        lm = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
        mk = smoothstep(uniforms.threshold - 0.1, uniforms.threshold + 0.1, lm);
        acc += col * mk * tint * 0.03;

        // SAMPLE_THRESH( 0, -2, 0.03)
        suv = in.uv + vec2<f32>( 0.0, -2.0) * texel * r;
        col = textureSample(main_tex, tex_sampler, suv).rgb;
        lm = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
        mk = smoothstep(uniforms.threshold - 0.1, uniforms.threshold + 0.1, lm);
        acc += col * mk * tint * 0.03;

        return vec4<f32>(acc, 1.0);

    } else if uniforms.mode == 1u {
        // HalationEffect.shader Pass 1 (BlurWide): fragBlurWide
        // float2 texel = _HaloTex_TexelSize.xy;
        // float r = _Spread * 8.0 + 2.0;
        let texel = vec2<f32>(uniforms.halo_texel_size_x, uniforms.halo_texel_size_y);
        let r = uniforms.spread * 8.0 + 2.0;

        // Port of SAMPLE_BLUR macro (shader line 125):
        // acc += tex2D(_HaloTex, i.uv + float2(ox, oy) * texel * r).rgb * w;
        var acc = vec3<f32>(0.0);

        acc += textureSample(halo_tex, tex_sampler, in.uv + vec2<f32>( 0.0,  0.0) * texel * r).rgb * 0.16;
        acc += textureSample(halo_tex, tex_sampler, in.uv + vec2<f32>( 1.0,  0.0) * texel * r).rgb * 0.10;
        acc += textureSample(halo_tex, tex_sampler, in.uv + vec2<f32>(-1.0,  0.0) * texel * r).rgb * 0.10;
        acc += textureSample(halo_tex, tex_sampler, in.uv + vec2<f32>( 0.0,  1.0) * texel * r).rgb * 0.10;
        acc += textureSample(halo_tex, tex_sampler, in.uv + vec2<f32>( 0.0, -1.0) * texel * r).rgb * 0.10;
        acc += textureSample(halo_tex, tex_sampler, in.uv + vec2<f32>( 1.0,  1.0) * texel * r).rgb * 0.06;
        acc += textureSample(halo_tex, tex_sampler, in.uv + vec2<f32>(-1.0,  1.0) * texel * r).rgb * 0.06;
        acc += textureSample(halo_tex, tex_sampler, in.uv + vec2<f32>( 1.0, -1.0) * texel * r).rgb * 0.06;
        acc += textureSample(halo_tex, tex_sampler, in.uv + vec2<f32>(-1.0, -1.0) * texel * r).rgb * 0.06;
        acc += textureSample(halo_tex, tex_sampler, in.uv + vec2<f32>( 2.0,  0.0) * texel * r).rgb * 0.03;
        acc += textureSample(halo_tex, tex_sampler, in.uv + vec2<f32>(-2.0,  0.0) * texel * r).rgb * 0.03;
        acc += textureSample(halo_tex, tex_sampler, in.uv + vec2<f32>( 0.0,  2.0) * texel * r).rgb * 0.03;
        acc += textureSample(halo_tex, tex_sampler, in.uv + vec2<f32>( 0.0, -2.0) * texel * r).rgb * 0.03;

        return vec4<f32>(acc, 1.0);

    } else {
        // HalationEffect.shader Pass 2 (Composite): fragComposite
        // fixed4 src = tex2D(_MainTex, i.uv);
        // float3 halo = tex2D(_HaloTex, i.uv).rgb;
        // float3 result = src.rgb + halo * _Amount;
        // return float4(result, src.a);
        let src = textureSample(main_tex, tex_sampler, in.uv);
        let halo = textureSample(halo_tex, tex_sampler, in.uv).rgb;
        let result = src.rgb + halo * uniforms.amount;
        return vec4<f32>(result, src.a);
    }
}
