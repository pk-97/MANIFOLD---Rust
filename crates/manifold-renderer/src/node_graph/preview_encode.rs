//! Shared semantic preview encoder — turns a captured node-output texture into
//! a displayable colour image according to its [`PreviewEncoding`].
//!
//! The same six encodings the editor shows in its node-output pane (a vector
//! field as an optical-flow colour wheel, a scalar as a black-floor lift, a
//! signed field as a diverging ramp, a normal map decoded, a depth buffer as a
//! turbo ramp, a colour image raw). This is the single source of truth for that
//! visualisation: the editor's display path ([`content_pipeline`]) and any
//! offline tool (the preset guidance report) both build one of these and call
//! [`PreviewEncoder::encode`], so the picture is identical wherever it appears.
//!
//! The shaders carry **no per-frame statistics** — every curve is fixed — so the
//! output is stable across frames and outliers (a data-derived window flickers
//! and crushes to black). `asinh` is the shared lift curve: ~linear near 0
//! (faithful for small values), log-compressed far out (no clipping). K = 0.05
//! (inv_k = 20) sets the linear→log knee around sim-scale data.

use manifold_gpu::{
    GpuBinding, GpuDevice, GpuEncoder, GpuFilterMode, GpuRenderPipeline, GpuSampler,
    GpuSamplerDesc, GpuTexture, GpuTextureFormat,
};

use crate::node_graph::PreviewEncoding;

/// Fullscreen-triangle vertex stage + the shared `asinh` lift, prepended to
/// every fragment below so they share one vertex entry point.
const PREVIEW_VS: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}
// asinh(x) = log(x + sqrt(x*x + 1)); arg is always > 0, valid for negative x.
fn asinh_approx(x: f32) -> f32 {
    return log(x + sqrt(x * x + 1.0));
}
"#;

/// Raw passthrough — colour images, composites, final outputs.
const RAW_FRAG: &str = r#"
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(t_source, s_source, in.uv);
}
"#;

/// Scalar lift: 0 stays black (clamp negatives), dark values raised.
const SCALAR_FRAG: &str = r#"
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
fn lift(c: f32, inv_k: f32, norm: f32) -> f32 {
    return asinh_approx(max(c, 0.0) * inv_k) / norm;
}
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let c = textureSample(t_source, s_source, in.uv).rgb;
    let inv_k = 20.0;
    let norm = asinh_approx(inv_k);     // so c = 1 maps to white
    let rgb = vec3<f32>(
        lift(c.r, inv_k, norm),
        lift(c.g, inv_k, norm),
        lift(c.b, inv_k, norm),
    );
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
"#;

/// Vector field: RG → 2D vector → optical-flow colour wheel
/// (direction = hue, magnitude = brightness). Zero vector → black.
const VECTOR_FRAG: &str = r#"
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
fn hsv2rgb(h: f32, s: f32, v: f32) -> vec3<f32> {
    let p = abs(fract(vec3<f32>(h) + vec3<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0);
    return v * mix(vec3<f32>(1.0), clamp(p - 1.0, vec3<f32>(0.0), vec3<f32>(1.0)), s);
}
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let v = textureSample(t_source, s_source, in.uv).rg;
    let two_pi = 6.2831853;
    let hue = atan2(v.g, v.r) / two_pi + 0.5;
    let inv_k = 20.0;
    let norm = asinh_approx(inv_k);
    let val = clamp(asinh_approx(length(v) * inv_k) / norm, 0.0, 1.0);
    return vec4<f32>(hsv2rgb(hue, 1.0, val), 1.0);
}
"#;

/// Signed scalar: diverging ramp centred at 0. Negative → blue, 0 → black,
/// positive → red, with the asinh lift on |value| so small swings either side
/// of zero are visible.
const SIGNED_FRAG: &str = r#"
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let c = textureSample(t_source, s_source, in.uv).r;
    let inv_k = 20.0;
    let norm = asinh_approx(inv_k);
    let mag = clamp(asinh_approx(abs(c) * inv_k) / norm, 0.0, 1.0);
    let pos = vec3<f32>(0.9, 0.2, 0.15);   // warm red for c > 0
    let neg = vec3<f32>(0.15, 0.35, 0.95); // cool blue for c < 0
    let tint = select(neg, pos, c >= 0.0);
    return vec4<f32>(tint * mag, 1.0);
}
"#;

/// Normal map: decode xyz from [-1,1] to the familiar blue-dominant RGB.
/// Tolerant of already-encoded [0,1] normals (re-normalising is a no-op).
const NORMAL_FRAG: &str = r#"
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let raw = textureSample(t_source, s_source, in.uv).xyz;
    // Treat values already in [0,1] as encoded; those spanning negatives as
    // raw [-1,1]. Decode raw → [0,1] for display.
    let mn = min(min(raw.x, raw.y), raw.z);
    let n = select(raw, raw * 0.5 + vec3<f32>(0.5), mn < 0.0);
    return vec4<f32>(clamp(n, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
"#;

/// Depth: a turbo-like near-far colour ramp. Depth from R; the asinh lift
/// spreads near values (where detail clusters) without a per-frame window.
const DEPTH_FRAG: &str = r#"
@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
// Compact turbo approximation: smooth blue→cyan→green→yellow→red.
fn turbo(t: f32) -> vec3<f32> {
    let x = clamp(t, 0.0, 1.0);
    let r = clamp(1.5 - abs(2.0 * x - 1.5) * 2.0, 0.0, 1.0);
    let g = clamp(1.5 - abs(2.0 * x - 1.0) * 2.0, 0.0, 1.0);
    let b = clamp(1.5 - abs(2.0 * x - 0.5) * 2.0, 0.0, 1.0);
    return vec3<f32>(r, g, b);
}
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let d = textureSample(t_source, s_source, in.uv).r;
    let inv_k = 20.0;
    let norm = asinh_approx(inv_k);
    let t = clamp(asinh_approx(max(d, 0.0) * inv_k) / norm, 0.0, 1.0);
    return vec4<f32>(turbo(t), 1.0);
}
"#;

/// Owns the six fullscreen-triangle pipelines (five semantic encodings plus the
/// raw blit) and a linear sampler, all built for one target colour format.
///
/// Construct one per target format: the editor builds it for `Rgba16Float`
/// (its preview surface); an offline exporter builds it for `Rgba8Unorm` so the
/// encoded result reads back directly as 8-bit PNG pixels.
pub struct PreviewEncoder {
    scalar_lift: GpuRenderPipeline,
    scalar_signed: GpuRenderPipeline,
    vector: GpuRenderPipeline,
    normal: GpuRenderPipeline,
    depth: GpuRenderPipeline,
    raw: GpuRenderPipeline,
    sampler: GpuSampler,
}

impl PreviewEncoder {
    /// Build all six pipelines for the given target colour format.
    pub fn new(device: &GpuDevice, target_format: GpuTextureFormat) -> Self {
        let mk = |frag: &str, label: &str| {
            let src = format!("{PREVIEW_VS}{frag}");
            device.create_render_pipeline(&src, "vs_main", "fs_main", target_format, None, label)
        };
        Self {
            scalar_lift: mk(SCALAR_FRAG, "Node Preview Scalar Lift"),
            scalar_signed: mk(SIGNED_FRAG, "Node Preview Signed Diverging"),
            vector: mk(VECTOR_FRAG, "Node Preview Vector Wheel"),
            normal: mk(NORMAL_FRAG, "Node Preview Normal Decode"),
            depth: mk(DEPTH_FRAG, "Node Preview Depth Ramp"),
            raw: mk(RAW_FRAG, "Preview Blit"),
            sampler: device.create_sampler(&GpuSamplerDesc {
                min_filter: GpuFilterMode::Linear,
                mag_filter: GpuFilterMode::Linear,
                ..Default::default()
            }),
        }
    }

    /// Render `source` into `target`, applying the semantic `encoding`. `smart`
    /// off (or `encoding == Color`) falls through to the raw blit, exactly as the
    /// editor's preview pane does when auto-gain is toggled off.
    pub fn encode(
        &self,
        enc: &mut GpuEncoder,
        source: &GpuTexture,
        target: &GpuTexture,
        encoding: PreviewEncoding,
        smart: bool,
    ) {
        let pipeline = if smart {
            match encoding {
                PreviewEncoding::ScalarLift => Some(&self.scalar_lift),
                PreviewEncoding::ScalarSigned => Some(&self.scalar_signed),
                PreviewEncoding::VectorField => Some(&self.vector),
                PreviewEncoding::Normal => Some(&self.normal),
                PreviewEncoding::Depth => Some(&self.depth),
                PreviewEncoding::Color => None,
            }
        } else {
            None
        };
        match pipeline {
            Some(p) => self.draw(enc, p, source, target, "Node Preview Encoding Blit"),
            None => self.blit(enc, source, target),
        }
    }

    /// Raw passthrough of `source` into `target`. Same-size, same-format copies
    /// take the cheap blit path; a size or format change goes through the raw
    /// fullscreen shader (which also converts the format, e.g. f16 → unorm8).
    pub fn blit(&self, enc: &mut GpuEncoder, source: &GpuTexture, target: &GpuTexture) {
        if target.width == source.width
            && target.height == source.height
            && target.format == source.format
        {
            enc.copy_texture_to_texture(source, target, target.width, target.height, 1);
            return;
        }
        self.draw(enc, &self.raw, source, target, "Preview Blit");
    }

    fn draw(
        &self,
        enc: &mut GpuEncoder,
        pipeline: &GpuRenderPipeline,
        source: &GpuTexture,
        target: &GpuTexture,
        label: &str,
    ) {
        enc.draw_fullscreen(
            pipeline,
            target,
            &[
                GpuBinding::Texture {
                    binding: 0,
                    texture: source,
                },
                GpuBinding::Sampler {
                    binding: 1,
                    sampler: &self.sampler,
                },
            ],
            true,
            true,
            label,
        );
    }
}
