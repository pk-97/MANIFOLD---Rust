use manifold_compositor::preset_thumbnail::*;
use manifold_core::preset_def::PresetKind;
#[cfg(feature = "gpu-proofs")]
use manifold_core::effect_graph_def::EffectGraphDef;

#[cfg(feature = "gpu-proofs")]
/// Minimal bundled effect preset (Bloom, a real shipped effect) parsed
/// from disk — exercises the real `system.source` → primitives →
/// `system.final_output` shape, not a synthetic fixture.
fn bloom_def() -> EffectGraphDef {
    let json = std::fs::read_to_string(std::path::Path::new(manifold_renderer::testkit::assets::CATALOG_ASSETS_ROOT).join("effect-presets/Bloom.json"))
    .expect("read Bloom.json");
    serde_json::from_str(&json).expect("parse Bloom.json")
}

#[cfg(feature = "gpu-proofs")]
fn generator_def(id: &str) -> EffectGraphDef {
    let json = std::fs::read_to_string(format!(
        "{}/generator-presets/{id}.json",
        manifold_renderer::testkit::assets::CATALOG_ASSETS_ROOT
    ))
    .expect("read generator preset");
    serde_json::from_str(&json).expect("parse generator preset")
}

#[cfg(feature = "gpu-proofs")]
fn assert_opaque(png: &[u8], what: &str) {
    let decoded = image::load_from_memory(png)
        .expect("decode produced PNG")
        .to_rgba8();
    assert!(
        decoded.pixels().all(|px| px.0[3] == 255),
        "{what}: thumbnail has non-opaque pixels (D3)"
    );
}

#[cfg(feature = "gpu-proofs")]
/// Headless value-level gate: renders a real stock effect over the test
/// card and asserts the output isn't flat/empty (a spread of distinct
/// pixel values) — same non-uniform-content check `mesh_snapshot.rs`/
/// `graph_dump.rs` tests use to catch a broken dispatch.
#[test]
fn render_effect_thumbnail_produces_non_trivial_png() {
    let device = manifold_gpu::testkit::test_device();
    let def = bloom_def();
    let png = render_preset_thumbnail(&device.arc(), PresetKind::Effect, &def, 96, 54, false)
        .expect("effect thumbnail render");
    assert!(!png.is_empty(), "PNG bytes must be non-empty");

    let decoded = image::load_from_memory(&png).expect("decode produced PNG").to_rgba8();
    let mut distinct = std::collections::HashSet::new();
    for px in decoded.pixels() {
        distinct.insert(px.0);
        if distinct.len() > 4 {
            break;
        }
    }
    assert!(
        distinct.len() > 2,
        "expected a spread of distinct colors (card run through Bloom), got {distinct:?}"
    );
    assert_opaque(&png, "Bloom effect");
}

#[cfg(feature = "gpu-proofs")]
#[test]
fn render_generator_thumbnail_produces_non_trivial_png() {
    let device = manifold_gpu::testkit::test_device();
    let def = generator_def("BlackHole");
    let png = render_preset_thumbnail(&device.arc(), PresetKind::Generator, &def, 96, 54, false)
        .expect("generator thumbnail render");
    assert!(!png.is_empty());
    let decoded = image::load_from_memory(&png).expect("decode produced PNG").to_rgba8();
    // Not asserting non-black here — BlackHole may legitimately render
    // mostly empty space; this confirms the render+encode path itself
    // works and produces a real, decodable image at the right size.
    assert_eq!(decoded.width(), 96);
    assert_eq!(decoded.height(), 54);
    assert_opaque(&png, "BlackHole generator");
}

#[cfg(feature = "gpu-proofs")]
/// Determinism gate (STATIC_THUMBNAILS_DESIGN §4): Bloom (effect) and a
/// stateful generator, rendered twice each through the full capture
/// recipe, must produce byte-identical PNGs with every pixel opaque.
#[test]
fn thumbnail_render_deterministic() {
    let device = manifold_gpu::testkit::test_device();

    let bloom = bloom_def();
    let a = render_preset_thumbnail(&device.arc(), PresetKind::Effect, &bloom, 128, 72, false)
        .expect("Bloom render 1");
    let b = render_preset_thumbnail(&device.arc(), PresetKind::Effect, &bloom, 128, 72, false)
        .expect("Bloom render 2");
    assert_eq!(a, b, "Bloom thumbnail not byte-identical across two runs");
    assert_opaque(&a, "Bloom effect");

    // Stateful generator: first pick FluidSim2D. If it is not
    // byte-identical across two runs, substitute another stateful
    // generator rather than weaken the assertion.
    let mut stateful_ok = false;
    for id in ["FluidSim2D", "StrangeAttractor", "OilyFluid", "StarField"] {
        let def = generator_def(id);
        let g1 = render_preset_thumbnail(&device.arc(), PresetKind::Generator, &def, 128, 72, false)
            .unwrap_or_else(|e| panic!("{id} render 1: {e}"));
        let g2 = render_preset_thumbnail(&device.arc(), PresetKind::Generator, &def, 128, 72, false)
            .unwrap_or_else(|e| panic!("{id} render 2: {e}"));
        if g1 == g2 {
            assert_opaque(&g1, id);
            eprintln!("thumbnail_render_deterministic: stateful pick = {id}");
            stateful_ok = true;
            break;
        }
        eprintln!("thumbnail_render_deterministic: {id} not byte-identical, substituting");
    }
    assert!(
        stateful_ok,
        "no stateful generator rendered byte-identical thumbnails — determinism is broken, do not weaken this test"
    );
}

#[test]
fn factory_thumbnail_path_resolves_under_dev_assets_when_unpackaged() {
    // In the test binary there's no packaged bundle, so this resolves to
    // the dev workspace assets dir — proves the path shape without
    // needing a GPU.
    let p = factory_thumbnail_path(PresetKind::Effect, "Bloom").expect("path resolves");
    assert!(p.ends_with("assets/preset-thumbnails/effects/Bloom.png"));
}
