use std::borrow::Cow;

use super::compute_dual_blit_helper::ComputeDualBlitHelper;
use crate::effect::{EffectContext, PostProcessEffect};
use crate::effects::registration::EffectFactory;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::primitives::Infrared;
use crate::node_graph::{
    ChainSpec, Graph, NodeInstanceId, ParamConvert, Routing, SkipMode, SpliceResult,
};
use manifold_core::EffectTypeId;
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::effects::EffectInstance;
use manifold_core::generator_registration::ParamSpec;

inventory::submit! {
    EffectMetadata {
        id: EffectTypeId::INFRARED,
        display_name: "Infrared",
        category: "Surveillance",
        available: true,
        osc_prefix: "infrared",
        legacy_discriminant: Some(37),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
            ParamSpec::whole_labels("palette", "Palette", 0.0, 9.0, 0.0, &["White Hot", "Black Hot", "Green NV", "Iron Bow", "Rainbow", "Lava", "Arctic", "Magenta", "Electric", "Toxic"], "Palette"),
            ParamSpec::continuous("contrast", "Contrast", 0.5, 3.0, 1.0, "F2", "Contrast"),
        ],
    }
}
inventory::submit! {
    EffectFactory {
        id: EffectTypeId::INFRARED,
        create: |device| Box::new(InfraredFX::new(device)),
    }
}

fn splice_infrared(graph: &mut Graph, source: (NodeInstanceId, &'static str)) -> SpliceResult {
    let node = graph.add_node(Box::new(Infrared::new()));
    graph.connect(source, (node, "in")).expect("wire source → Infrared.in");
    SpliceResult {
        output: (node, "out"),
        handles: vec![(Cow::Borrowed("infrared"), node)],
    }
}

inventory::submit! {
    ChainSpec {
        type_id: EffectTypeId::INFRARED,
        splice: splice_infrared,
        routings: &[
            Routing { param_id: "amount", target_handle: "infrared", target_param: "amount", convert: ParamConvert::Float },
            Routing { param_id: "palette", target_handle: "infrared", target_param: "palette", convert: ParamConvert::EnumRound },
            Routing { param_id: "contrast", target_handle: "infrared", target_param: "contrast", convert: ParamConvert::Float },
        ],
        skip: SkipMode::OnZero { param_id: "amount" },
    }
}

/// LUT resolution — 512 entries covering [0, 2] range.
/// First 256 entries cover [0, 1] (normal palette), last 256 cover [1, 2]
/// (HDR extrapolation using the palette functions' natural gradient extension).
const LUT_SIZE: u32 = 512;

/// Maximum luminance value baked into the LUT.
const LUT_MAX_LUM: f32 = 2.0;

/// Number of built-in palettes.
const PALETTE_COUNT: usize = 10;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InfraredUniforms {
    amount: f32,
    contrast: f32,
    _pad0: f32,
    _pad1: f32,
}

/// Infrared / thermal vision effect — LUT-based palette mapping.
/// Pre-bakes 10 palette gradients into 256×1 textures at init. The shader
/// replaces all branching and ALU with a single texture sample.
pub struct InfraredFX {
    helper: ComputeDualBlitHelper,
    /// Pre-baked palette LUT textures (256×1 Rgba16Float each).
    luts: [manifold_gpu::GpuTexture; PALETTE_COUNT],
}

impl InfraredFX {
    pub fn new(device: &manifold_gpu::GpuDevice) -> Self {
        let helper = ComputeDualBlitHelper::new(
            device,
            include_str!("shaders/fx_infrared.wgsl"),
            "Infrared",
        );

        // Bake all 10 palette LUTs and upload via a temporary encoder.
        let mut enc = device.create_encoder("Infrared LUT Upload");
        let luts = std::array::from_fn(|i| {
            let pixels = bake_palette(i);
            let tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
                width: LUT_SIZE,
                height: 1,
                depth: 1,
                format: manifold_gpu::GpuTextureFormat::Rgba16Float,
                dimension: manifold_gpu::GpuTextureDimension::D2,
                usage: manifold_gpu::GpuTextureUsage::SHADER_READ
                    | manifold_gpu::GpuTextureUsage::CPU_UPLOAD,
                label: "Infrared LUT",
                mip_levels: 1,
            });
            let f16_data = pixels_to_f16(&pixels);
            enc.upload_texture(&tex, LUT_SIZE, 1, 1, &f16_data);
            tex
        });
        enc.commit();

        Self { helper, luts }
    }
}

impl PostProcessEffect for InfraredFX {
    fn effect_type(&self) -> &EffectTypeId {
        &EffectTypeId::INFRARED
    }

    fn should_skip(&self, fx: &EffectInstance) -> bool {
        fx.param_values.first().map(|p| p.value).unwrap_or(0.0) <= 0.0
    }

    fn apply(
        &mut self,
        gpu: &mut GpuEncoder,
        source: &manifold_gpu::GpuTexture,
        target: &manifold_gpu::GpuTexture,
        fx: &EffectInstance,
        ctx: &EffectContext,
    ) {
        let p = &fx.param_values;
        let palette_idx = p.get(1).map(|pv| pv.value).unwrap_or(0.0).round() as usize;
        let lut = &self.luts[palette_idx.min(PALETTE_COUNT - 1)];

        let uniforms = InfraredUniforms {
            amount: p.first().map(|pv| pv.value).unwrap_or(0.0),
            contrast: p.get(2).map(|pv| pv.value).unwrap_or(1.0),
            _pad0: 0.0,
            _pad1: 0.0,
        };

        self.helper.dispatch(
            gpu,
            source,
            lut,
            target,
            bytemuck::bytes_of(&uniforms),
            "Infrared Pass",
            ctx.width,
            ctx.height,
        );
    }
}

// ─── Palette baking ─────────────────────────────────────────────────

/// Bake a palette into 512 RGBA pixels (f32) covering [0, 2] luminance range.
/// The palette functions naturally extrapolate beyond t=1.0 (gradient extension),
/// producing the HDR blowout effect that makes Infrared look gorgeous on HDR content.
fn bake_palette(palette_idx: usize) -> Vec<[f32; 4]> {
    (0..LUT_SIZE)
        .map(|i| {
            let t = i as f32 / (LUT_SIZE - 1) as f32 * LUT_MAX_LUM;
            let rgb = match palette_idx {
                0 => palette_white_hot(t),
                1 => palette_black_hot(t),
                2 => palette_green_nv(t),
                3 => palette_iron_bow(t),
                4 => palette_rainbow(t),
                5 => palette_lava(t),
                6 => palette_arctic(t),
                7 => palette_magenta(t),
                8 => palette_electric(t),
                _ => palette_toxic(t),
            };
            [rgb[0], rgb[1], rgb[2], 1.0]
        })
        .collect()
}

/// Convert f32 RGBA pixels to f16 RGBA bytes for Rgba16Float upload.
fn pixels_to_f16(pixels: &[[f32; 4]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels.len() * 8);
    for px in pixels {
        for &ch in px {
            out.extend_from_slice(&half::f16::from_f32(ch).to_le_bytes());
        }
    }
    out
}

// ─── Palette functions (match the original WGSL exactly) ────────────

fn palette_white_hot(t: f32) -> [f32; 3] {
    [t, t, t]
}

fn palette_black_hot(t: f32) -> [f32; 3] {
    let v = 1.0 - t;
    [v, v, v]
}

fn palette_green_nv(t: f32) -> [f32; 3] {
    [t * 0.15, t, t * 0.1]
}

fn palette_iron_bow(t: f32) -> [f32; 3] {
    gradient(
        &[
            (0.0, [0.0, 0.0, 0.0]),
            (0.2, [0.15, 0.0, 0.3]),
            (0.4, [0.7, 0.05, 0.1]),
            (0.6, [0.95, 0.4, 0.0]),
            (0.8, [1.0, 0.85, 0.2]),
            (1.0, [1.0, 1.0, 0.9]),
        ],
        t,
    )
}

fn palette_rainbow(t: f32) -> [f32; 3] {
    let r = ((t + 0.0).fract() * 6.0 - 3.0).abs() - 1.0;
    let g = ((t + 0.333).fract() * 6.0 - 3.0).abs() - 1.0;
    let b = ((t + 0.667).fract() * 6.0 - 3.0).abs() - 1.0;
    [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
}

fn palette_lava(t: f32) -> [f32; 3] {
    gradient(
        &[
            (0.0, [0.0, 0.0, 0.0]),
            (0.25, [0.4, 0.02, 0.0]),
            (0.5, [0.85, 0.15, 0.0]),
            (0.75, [1.0, 0.55, 0.0]),
            (1.0, [1.0, 0.9, 0.2]),
        ],
        t,
    )
}

fn palette_arctic(t: f32) -> [f32; 3] {
    gradient(
        &[
            (0.0, [0.0, 0.0, 0.0]),
            (0.3, [0.0, 0.05, 0.35]),
            (0.6, [0.1, 0.55, 0.8]),
            (0.85, [0.6, 0.9, 1.0]),
            (1.0, [1.0, 1.0, 1.0]),
        ],
        t,
    )
}

fn palette_magenta(t: f32) -> [f32; 3] {
    gradient(
        &[
            (0.0, [0.0, 0.0, 0.0]),
            (0.3, [0.3, 0.0, 0.35]),
            (0.6, [0.9, 0.1, 0.5]),
            (0.85, [1.0, 0.5, 0.7]),
            (1.0, [1.0, 0.95, 1.0]),
        ],
        t,
    )
}

fn palette_electric(t: f32) -> [f32; 3] {
    gradient(
        &[
            (0.0, [0.0, 0.0, 0.0]),
            (0.25, [0.15, 0.0, 0.4]),
            (0.5, [0.1, 0.2, 0.9]),
            (0.75, [0.0, 0.7, 1.0]),
            (1.0, [0.7, 1.0, 1.0]),
        ],
        t,
    )
}

fn palette_toxic(t: f32) -> [f32; 3] {
    gradient(
        &[
            (0.0, [0.0, 0.0, 0.0]),
            (0.3, [0.0, 0.2, 0.05]),
            (0.6, [0.3, 0.75, 0.0]),
            (0.85, [0.7, 1.0, 0.1]),
            (1.0, [1.0, 1.0, 0.3]),
        ],
        t,
    )
}

/// Evaluate a piecewise-linear gradient at position t.
/// For t > last stop, extrapolates the last segment's gradient direction —
/// this produces the HDR blowout colors (e.g., arctic's golden highlights).
fn gradient(stops: &[(f32, [f32; 3])], t: f32) -> [f32; 3] {
    if t <= stops[0].0 {
        return stops[0].1;
    }
    for i in 1..stops.len() {
        if t <= stops[i].0 {
            let s = (t - stops[i - 1].0) / (stops[i].0 - stops[i - 1].0);
            let a = stops[i - 1].1;
            let b = stops[i].1;
            return [
                a[0] + (b[0] - a[0]) * s,
                a[1] + (b[1] - a[1]) * s,
                a[2] + (b[2] - a[2]) * s,
            ];
        }
    }
    // Extrapolate beyond the last stop using the last segment's direction.
    // Matches the original WGSL mix() behavior where s > 1.0 overshoots.
    let n = stops.len();
    let s = (t - stops[n - 2].0) / (stops[n - 1].0 - stops[n - 2].0);
    let a = stops[n - 2].1;
    let b = stops[n - 1].1;
    [
        a[0] + (b[0] - a[0]) * s,
        a[1] + (b[1] - a[1]) * s,
        a[2] + (b[2] - a[2]) * s,
    ]
}
