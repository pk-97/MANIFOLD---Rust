//! Held-out round-trip gate for `migrate_scene_object_wires`
//! (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D5/P2's "load the azalea fixture
//! project ... assert migration fires once, renders correctly, save →
//! reload → migration is a no-op").
//!
//! **Honest substitution, stated per the phase brief's own instruction**:
//! no `.manifold` PROJECT fixture with legacy `render_scene` per-object
//! wiring exists anywhere under `tests/fixtures/` in this repo (the
//! `cc0__oomurasaki_azalea_r._x_pulchrum.glb` fixture the design doc calls
//! "the azalea fixture" is a raw glTF asset, not a saved project — its
//! import graph is proven separately by `gltf_import.rs`'s
//! `assembles_azalea_into_two_object_render_scene_graph` and by
//! `glb_conformance_sweep`, both of which pass an AntiqueCamera.glb-style
//! import through this exact migration). Rather than fabricate a project
//! fixture, this test uses `pre_migration_scene_starter.json` — the actual
//! `SceneStarter.json` bundled preset content as it existed at commit
//! `73f9d7f4` (P1 HEAD, `git show 73f9d7f4:crates/manifold-renderer/assets/
//! generator-presets/SceneStarter.json`, byte-for-byte, before `graph_tool
//! migrate --in-place` ran on it), checked in verbatim as a held-out def
//! this phase's worker did not author. It is a real production def with
//! real legacy `mesh_k`/`material_k`/`base_color_map_k`/`transform_k`
//! wiring, going through the exact same `instantiate_def` choke point any
//! other def would.

use half::f16;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_object_migration::migrate_scene_object_wires;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const PRE_MIGRATION_JSON: &str = include_str!("../fixtures/pre_migration_scene_starter.json");

fn render_readback(json: &str) -> (Vec<u8>, u32, u32) {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        json,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("SceneStarter (pre- or post-migration) must build");

    let target = h.make_target("scene-object-migration-round-trip");
    for frame in 0..2 {
        let ctx = PresetContext {
            time: 0.1,
            beat: 0.2,
            dt: 1.0 / 60.0,
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = h.device.create_encoder("scene-object-migration-round-trip-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &manifold_core::params::ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();
    }
    (h.readback(&target.texture), h.width, h.height)
}

fn write_png(bytes: &[u8], w: u32, h: u32, path: &str) {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for px in bytes.chunks_exact(8) {
        for c in 0..4 {
            let v = f16::from_le_bytes([px[c * 2], px[c * 2 + 1]]).to_f32();
            let mapped = (v / (1.0 + v)).clamp(0.0, 1.0);
            out.push((mapped.powf(1.0 / 2.2) * 255.0).round() as u8);
        }
    }
    image::save_buffer(path, &out, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("write {path}: {e}"));
}

/// Mean absolute per-channel difference between two same-sized Rgba16Float
/// readbacks, in [0,1] tonemapped-and-gamma-corrected units — same metric
/// `glb_conformance.rs`'s golden compare uses.
fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let tonemap = |bytes: &[u8]| -> Vec<f32> {
        bytes
            .chunks_exact(8)
            .flat_map(|px| {
                (0..4).map(move |c| {
                    let v = f16::from_le_bytes([px[c * 2], px[c * 2 + 1]]).to_f32();
                    (v / (1.0 + v)).clamp(0.0, 1.0)
                })
            })
            .collect()
    };
    let ta = tonemap(a);
    let tb = tonemap(b);
    let sum: f64 = ta.iter().zip(&tb).map(|(x, y)| (x - y).abs() as f64).sum();
    sum / ta.len() as f64
}

#[test]
fn pre_migration_scene_starter_migrates_once_renders_and_is_idempotent_on_reload() {
    // ---- Structural half: migration fires exactly once, then is a no-op
    // on the "reload" (the def as it would be re-parsed after a save). ----
    let mut def: EffectGraphDef =
        serde_json::from_str(PRE_MIGRATION_JSON).expect("pre-migration SceneStarter must parse");
    assert!(
        def.wires.iter().any(|w| w.to_port == "mesh_0"),
        "held-out fixture must actually carry legacy per-object wiring \
         (sanity check on the fixture itself, not the migration)"
    );

    let fired = migrate_scene_object_wires(&mut def);
    assert!(fired, "migration must fire on the pre-migration def");
    assert!(
        !def.wires.iter().any(|w| w.to_port == "mesh_0" || w.to_port == "material_0"),
        "no legacy per-object ports may survive migration"
    );

    // "Save → reload": re-serialize and re-parse (byte round-trip through
    // JSON, exactly what a project save/load does), then migrate again.
    let saved = serde_json::to_string(&def).expect("migrated def must serialize");
    let mut reloaded: EffectGraphDef =
        serde_json::from_str(&saved).expect("re-parse of the saved def must succeed");
    let fired_again = migrate_scene_object_wires(&mut reloaded);
    assert!(!fired_again, "migration must be a no-op on an already-migrated, reloaded def");
    assert_eq!(def, reloaded, "reload must round-trip byte-identical (JSON-level)");

    // ---- Visual half: the migrated-at-load-time render of the HELD-OUT
    // pre-migration JSON matches the render of the checked-in, already-
    // migrated SceneStarter.json (graph_tool migrate --in-place's actual
    // output) — same scene, same pixels, migration changed nothing a
    // viewer would see. ----
    let (pre_bytes, w, h) = render_readback(PRE_MIGRATION_JSON);
    let current_json = include_str!(
        "../../assets/generator-presets/SceneStarter.json"
    );
    let (post_bytes, w2, h2) = render_readback(current_json);
    assert_eq!((w, h), (w2, h2));

    write_png(&pre_bytes, w, h, "/tmp/scene_starter_pre_migration.png");
    write_png(&post_bytes, w, h, "/tmp/scene_starter_post_migration.png");

    let diff = mean_abs_diff(&pre_bytes, &post_bytes);
    eprintln!("SceneStarter pre- vs post-migration mean_abs_diff = {diff:.6}");
    assert!(
        diff < 0.01,
        "load-time migration of the held-out pre-migration def must render \
         pixel-comparably to the checked-in migrated def: mean_abs_diff={diff:.6}"
    );
}
