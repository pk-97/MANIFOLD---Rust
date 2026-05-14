//! Pixel-exact parity test for `node.color_lut` vs the legacy
//! `InfraredFX` effect. Eighth §6.1 migration and the first
//! multi-input primitive parity test.
//!
//! The legacy effect owns 10 pre-baked palette LUTs (512×1
//! Rgba16Float each). The primitive accepts the LUT as a port. This
//! test replicates the legacy palette baking inline (`bake_palette`
//! / `gradient` / `palette_*` functions) so the LUT bytes are
//! bit-identical to what `InfraredFX::new` uploads — then routes
//! that LUT through the primitive via the harness's
//! `run_primitive_graph_with_aux_inputs` helper.
//!
//! When this test ever flakes the cause is one of:
//!
//! 1. Palette baking math drifted — diff against
//!    `effects/infrared.rs:174-271`. Stop coordinates and gradient
//!    extrapolation must remain byte-identical.
//! 2. `pixels_to_f16` rounding differs — both paths use
//!    `half::f16::from_f32` so the bit pattern is canonical.
//! 3. LUT upload-into-RT copy introduces drift — RGBA16F → RGBA16F
//!    is bit-preserving.

mod parity;

use manifold_core::EffectTypeId;
use manifold_renderer::node_graph::primitives::ColorLut;
use manifold_renderer::node_graph::ParamValue;
use parity::{
    assert_bytewise_equal, default_ctx, make_default_effect, Fixture, ParityHarness,
};

const LUT_SIZE: u32 = 512;
const LUT_MAX_LUM: f32 = 2.0;

// ─── Palettes — mirror of effects/infrared.rs:174-271. The two
// copies must stay byte-identical. Future refactor: lift the
// palette functions into a shared `manifold-renderer::palettes`
// module, but for the parity gate keeping them duplicated forces
// us to notice drift at test time.

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

fn p_white_hot(t: f32) -> [f32; 3] { [t, t, t] }
fn p_black_hot(t: f32) -> [f32; 3] { let v = 1.0 - t; [v, v, v] }
fn p_green_nv(t: f32) -> [f32; 3] { [t * 0.15, t, t * 0.1] }
fn p_iron_bow(t: f32) -> [f32; 3] {
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
fn p_rainbow(t: f32) -> [f32; 3] {
    let r = ((t + 0.0).fract() * 6.0 - 3.0).abs() - 1.0;
    let g = ((t + 0.333).fract() * 6.0 - 3.0).abs() - 1.0;
    let b = ((t + 0.667).fract() * 6.0 - 3.0).abs() - 1.0;
    [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
}
fn p_lava(t: f32) -> [f32; 3] {
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
fn p_arctic(t: f32) -> [f32; 3] {
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
fn p_magenta(t: f32) -> [f32; 3] {
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
fn p_electric(t: f32) -> [f32; 3] {
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
fn p_toxic(t: f32) -> [f32; 3] {
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

fn bake_palette_f16(idx: usize) -> Vec<half::f16> {
    let mut out = Vec::with_capacity((LUT_SIZE * 4) as usize);
    for i in 0..LUT_SIZE {
        let t = i as f32 / (LUT_SIZE - 1) as f32 * LUT_MAX_LUM;
        let rgb = match idx {
            0 => p_white_hot(t),
            1 => p_black_hot(t),
            2 => p_green_nv(t),
            3 => p_iron_bow(t),
            4 => p_rainbow(t),
            5 => p_lava(t),
            6 => p_arctic(t),
            7 => p_magenta(t),
            8 => p_electric(t),
            _ => p_toxic(t),
        };
        out.push(half::f16::from_f32(rgb[0]));
        out.push(half::f16::from_f32(rgb[1]));
        out.push(half::f16::from_f32(rgb[2]));
        out.push(half::f16::from_f32(1.0));
    }
    out
}

// LUT textures are 512×1, but the harness's `upload_f16_rgba`
// expects the parity dimensions (128×128). Build a wider helper that
// uploads at arbitrary dimensions.
fn upload_lut(h: &ParityHarness, idx: usize) -> manifold_gpu::GpuTexture {
    use manifold_gpu::{GpuTextureDesc, GpuTextureDimension, GpuTextureUsage};

    let pixels = bake_palette_f16(idx);
    let texture = h.device.create_texture(&GpuTextureDesc {
        width: LUT_SIZE,
        height: 1,
        depth: 1,
        format: h.format,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD
            | GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::COPY_SRC,
        label: "parity-infrared-lut",
        mip_levels: 1,
    });
    let bytes = unsafe {
        std::slice::from_raw_parts(
            pixels.as_ptr().cast::<u8>(),
            std::mem::size_of_val(pixels.as_slice()),
        )
    };
    h.device.upload_texture(&texture, bytes);
    texture
}

const PALETTES: &[(usize, &str)] = &[
    (0, "white_hot"),
    (1, "black_hot"),
    (2, "green_nv"),
    (3, "iron_bow"),
    (4, "rainbow"),
    (5, "lava"),
    (6, "arctic"),
    (7, "magenta"),
    (8, "electric"),
    (9, "toxic"),
];

const SETUPS: &[(f32, f32, &str)] = &[
    (1.0, 1.0, "full_neutral"),
    (0.5, 1.0, "half_neutral"),
    (1.0, 0.5, "low_contrast"),
    (1.0, 2.0, "high_contrast"),
];

#[test]
fn lut1d_is_pixel_exact_across_fixtures_palettes_setups() {
    let mut h = ParityHarness::new();
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(&h);

        for &(palette_idx, palette_label) in PALETTES {
            let lut_tex = upload_lut(&h, palette_idx);

            for &(amount, contrast, setup_label) in SETUPS {
                let mut fx = make_default_effect(EffectTypeId::INFRARED);
                fx.param_values[0].value = amount;
                fx.param_values[1].value = palette_idx as f32;
                fx.param_values[2].value = contrast;

                let legacy = h.run_legacy(&fx, &input, &ctx);
                let decomposed = h.run_primitive_graph_with_aux_inputs(
                    Box::new(ColorLut::new()),
                    &input,
                    &[("lut", &lut_tex)],
                    &ctx,
                    |graph, prim_id| {
                        graph
                            .set_param(prim_id, "amount", ParamValue::Float(amount))
                            .unwrap();
                        graph
                            .set_param(prim_id, "contrast", ParamValue::Float(contrast))
                            .unwrap();
                    },
                );

                assert_bytewise_equal(
                    &format!(
                        "lut1d/{:?}/palette={palette_label}/setup={setup_label}: legacy vs node.color_lut",
                        fixture
                    ),
                    &legacy,
                    &decomposed,
                );
            }
        }
    }
}
