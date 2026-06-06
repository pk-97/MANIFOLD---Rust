//! Pixel-exact parity test for `primitive.invert` vs the legacy
//! `InvertColorsFX` effect. First production migration under §6.1 of
//! `docs/PRIMITIVE_LIBRARY_DESIGN.md`.
//!
//! The legacy effect is one compute pass. The primitive replaces it 1:1
//! — same WGSL math, same uniform layout, same workgroup shape, same
//! binding indices, same dispatch shape. The graph runtime introduces
//! one additional element relative to the legacy path: a
//! GPU-side `copy_texture_to_texture` from the Shared-storage fixture
//! into a Private-storage `RenderTarget` so the executor sees the
//! same memory mode production graphs use. RGBA16Float → RGBA16Float
//! copies are bit-preserving, so parity holds.
//!
//! If this test ever flakes:
//!
//! 1. The WGSL drifted — diff `effects/shaders/invert_colors.wgsl` vs
//!    `node_graph/primitives/shaders/invert.wgsl`. They must remain
//!    byte-identical (modulo the leading comment block) for parity.
//! 2. The uniform layout drifted — `InvertUniforms` in both files
//!    must have `intensity: f32` followed by 12 bytes of padding.
//! 3. The dispatch shape drifted — both must compute `width.div_ceil(16)
//!    × height.div_ceil(16) × 1` workgroups.
//! 4. The graph runtime's copy semantics changed — investigate
//!    `MetalBackend::pre_bind_texture_2d` and the per-frame source
//!    blit in `Executor::execute_frame_with_gpu`.


use manifold_core::PresetTypeId;
use manifold_renderer::node_graph::ParamValue;
use manifold_renderer::node_graph::primitives::Invert;
use crate::harness::{self, Fixture, assert_bytewise_equal, default_ctx, make_default_effect};

/// Six representative intensity values per fixture: min, max, default,
/// and three mid-range values that exercise different mix coefficients.
/// 6 × 4 fixtures = 24 parity assertions per `cargo test`.
const INTENSITY_SWEEP: &[f32] = &[0.0, 0.25, 0.5, 0.75, 1.0, 0.42];

#[test]
fn invert_is_pixel_exact_across_fixtures_and_intensities() {
    let h = harness::shared();
    let ctx = default_ctx(h.width, h.height);

    for &fixture in Fixture::all() {
        let input = fixture.build(h);

        for &intensity in INTENSITY_SWEEP {
            let mut fx = make_default_effect(PresetTypeId::INVERT_COLORS);
            // The legacy effect reads `param_values[0].value` as intensity.
            // Patch the slot rather than going through a command, so the
            // test stays close to the data shape modulation/UI both use.
            fx.param_values[0].value = intensity;

            let legacy = h.run_legacy(&fx, &input, &ctx);
            let decomposed =
                h.run_primitive_graph(Box::new(Invert::new()), &input, &ctx, |graph, prim_id| {
                    graph
                        .set_param(prim_id, "intensity", ParamValue::Float(intensity))
                        .expect("node.invert must accept `intensity` param");
                });

            assert_bytewise_equal(
                &format!(
                    "invert/{:?}/intensity={intensity}: legacy vs primitive.invert",
                    fixture
                ),
                &legacy,
                &decomposed,
            );
        }
    }
}

/// Identity case — `intensity = 0` must produce the input unchanged
/// through both paths. Catches dispatch shape / boundary-pixel bugs
/// that the full sweep would also catch but reports them more clearly
/// when this single case fails.
#[test]
fn invert_at_zero_intensity_is_passthrough() {
    let h = harness::shared();
    let input = Fixture::Gradient.build(h);
    let ctx = default_ctx(h.width, h.height);

    let mut fx = make_default_effect(PresetTypeId::INVERT_COLORS);
    fx.param_values[0].value = 0.0;

    let legacy = h.run_legacy(&fx, &input, &ctx);
    let decomposed =
        h.run_primitive_graph(Box::new(Invert::new()), &input, &ctx, |graph, prim_id| {
            graph
                .set_param(prim_id, "intensity", ParamValue::Float(0.0))
                .unwrap();
        });

    assert_bytewise_equal("invert/passthrough", &legacy, &decomposed);
}

/// Full-strength case — `intensity = 1` is the most aggressive
/// transformation. Asserts the inverted output is also bit-identical
/// across paths, ruling out shader-divergence at the math extreme.
#[test]
fn invert_at_full_intensity_matches_legacy() {
    let h = harness::shared();
    let input = Fixture::Swatches.build(h);
    let ctx = default_ctx(h.width, h.height);

    let mut fx = make_default_effect(PresetTypeId::INVERT_COLORS);
    fx.param_values[0].value = 1.0;

    let legacy = h.run_legacy(&fx, &input, &ctx);
    let decomposed =
        h.run_primitive_graph(Box::new(Invert::new()), &input, &ctx, |graph, prim_id| {
            graph
                .set_param(prim_id, "intensity", ParamValue::Float(1.0))
                .unwrap();
        });

    assert_bytewise_equal("invert/full-strength", &legacy, &decomposed);
}
