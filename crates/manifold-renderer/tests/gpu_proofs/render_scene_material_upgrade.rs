//! Existing-import material upgrade proof.
//!
//! A saved graph from before vertex-colour activation must render like a fresh
//! import after load-time upgrade, while the unupgraded graph remains visibly
//! different. The graph is also serialized and reloaded to exercise the real
//! saved-project boundary and the upgrade's idempotence contract.

use std::path::{Path, PathBuf};

use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, SerializedParamValue};
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::headless_readback::{
    encode_rgba8_png, non_black_fraction, readback_tonemapped_rgba8,
};
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::node_graph::gltf_import::{
    MaterialUpgradeCache, assemble_import_graph, upgrade_material_graph,
};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

fn box_vertex_colors_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/khronos/BoxVertexColors.glb")
}

fn remove_vertex_color_flags(nodes: &mut [EffectGraphNode]) {
    for node in nodes {
        if matches!(
            node.type_id.as_str(),
            "node.gltf_mesh_source" | "node.gltf_skinned_mesh_source"
        ) {
            node.params.remove("vertex_colors");
        }
        if let Some(group) = node.group.as_mut() {
            remove_vertex_color_flags(&mut group.nodes);
        }
    }
}

fn find_node(nodes: &[EffectGraphNode], id: u32) -> Option<&EffectGraphNode> {
    nodes.iter().find_map(|node| {
        if node.id == id {
            Some(node)
        } else {
            node.group
                .as_ref()
                .and_then(|group| find_node(&group.nodes, id))
        }
    })
}

fn has_vertex_color_opt_in(nodes: &[EffectGraphNode]) -> bool {
    nodes.iter().any(|node| {
        node.params.get("vertex_colors") == Some(&SerializedParamValue::Bool { value: true })
            || node
                .group
                .as_ref()
                .is_some_and(|group| has_vertex_color_opt_in(&group.nodes))
    })
}

fn assert_stable_authored_nodes(before: &EffectGraphDef, after: &EffectGraphDef) {
    fn visit(nodes: &[EffectGraphNode], after: &EffectGraphDef) {
        for node in nodes {
            let upgraded =
                find_node(&after.nodes, node.id).expect("upgrade must retain authored node IDs");
            assert_eq!(
                upgraded.node_id, node.node_id,
                "node_id changed for node {}",
                node.id
            );
            assert_eq!(
                upgraded.type_id, node.type_id,
                "node type changed for node {}",
                node.id
            );
            if node.type_id == "node.transform_3d" || node.type_id.contains("animation") {
                assert_eq!(
                    upgraded.params, node.params,
                    "transform/animation params changed for node {}",
                    node.id
                );
            }
            if let Some(group) = node.group.as_ref() {
                visit(&group.nodes, after);
            }
        }
    }
    visit(&before.nodes, after);
}

fn render_graph(def: EffectGraphDef, label: &str) -> Vec<u8> {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_def_with_device(
        def,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|error| panic!("{label} graph must build: {error:?}"));
    let target = h.make_target(label);
    let mut previous = None;
    let mut stable = 0u32;
    for frame in 0..180i64 {
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
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
        let mut enc = h.device.create_encoder(label);
        {
            let mut gpu = manifold_renderer::gpu_encoder::GpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &manifold_core::params::ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();
        let pixels = readback_tonemapped_rgba8(&h.device, &target.texture, h.width, h.height);
        let is_nonblank = non_black_fraction(&pixels) > 0.01;
        if is_nonblank
            && !runtime.warmup_pending()
            && !runtime.io_pending()
            && previous.as_ref() == Some(&pixels)
        {
            stable += 1;
            if stable >= 3 {
                return pixels;
            }
        } else {
            stable = 0;
        }
        previous = Some(pixels);
        if runtime.warmup_pending() || runtime.io_pending() {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    panic!("{label} graph did not produce a stable nonblank frame");
}

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(left, right)| f64::from(left.abs_diff(*right)) / 255.0)
        .sum::<f64>()
        / a.len() as f64
}

fn colorful_fraction(rgba: &[u8]) -> f64 {
    let colorful = rgba
        .chunks_exact(4)
        .filter(|pixel| {
            let min = pixel[0].min(pixel[1]).min(pixel[2]);
            let max = pixel[0].max(pixel[1]).max(pixel[2]);
            max > 12 && max - min > 8
        })
        .count();
    colorful as f64 / (rgba.len() / 4).max(1) as f64
}

fn maybe_capture(name: &str, rgba: &[u8], width: u32, height: u32) {
    let Ok(dir) = std::env::var("MANIFOLD_MATERIAL_UPGRADE_CAPTURE_DIR") else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("create material-upgrade capture directory");
    let path = Path::new(&dir).join(format!("{name}.png"));
    std::fs::write(&path, encode_rgba8_png(rgba, width, height))
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

#[test]
fn legacy_import_upgrade_matches_fresh_render_and_is_idempotent() {
    let path = box_vertex_colors_fixture();
    assert!(
        path.exists(),
        "required colored fixture missing: {}",
        path.display()
    );

    let (fresh, _report) =
        assemble_import_graph(&path).expect("BoxVertexColors import must assemble");
    let mut legacy = fresh.clone();
    remove_vertex_color_flags(&mut legacy.nodes);
    let saved_legacy = serde_json::to_string(&legacy).expect("legacy graph must serialize");
    let mut reloaded_legacy: EffectGraphDef =
        serde_json::from_str(&saved_legacy).expect("saved legacy graph must reload");
    assert_stable_authored_nodes(&legacy, &reloaded_legacy);

    let mut cache = MaterialUpgradeCache::default();
    let upgrade = upgrade_material_graph(&mut reloaded_legacy, &mut cache);
    assert!(
        upgrade.changed,
        "legacy graph must receive a material upgrade"
    );
    assert!(
        upgrade.notices.is_empty(),
        "unexpected upgrade notices: {:?}",
        upgrade.notices
    );
    assert!(
        has_vertex_color_opt_in(&reloaded_legacy.nodes),
        "varying COLOR_0 source must opt into authored vertex colors"
    );
    assert_stable_authored_nodes(&legacy, &reloaded_legacy);

    let saved_upgraded =
        serde_json::to_string(&reloaded_legacy).expect("upgraded graph must serialize");
    let mut reloaded_upgraded: EffectGraphDef =
        serde_json::from_str(&saved_upgraded).expect("saved upgraded graph must reload");
    let second = upgrade_material_graph(&mut reloaded_upgraded, &mut cache);
    assert!(!second.changed, "second upgrade must be idempotent");
    assert!(
        second.notices.is_empty(),
        "completed upgrade must stay quiet"
    );
    assert_eq!(
        reloaded_legacy, reloaded_upgraded,
        "upgrade must round-trip without drift"
    );

    let legacy_pixels = render_graph(legacy, "material-upgrade-legacy");
    let upgraded_pixels = render_graph(reloaded_upgraded, "material-upgrade-upgraded");
    let fresh_pixels = render_graph(fresh, "material-upgrade-fresh");
    let h = harness::shared();
    for (name, pixels) in [
        ("legacy", &legacy_pixels),
        ("upgraded", &upgraded_pixels),
        ("fresh", &fresh_pixels),
    ] {
        assert!(
            non_black_fraction(pixels) > 0.01,
            "{name} render must be nonblank"
        );
        if name != "legacy" {
            assert!(
                colorful_fraction(pixels) > 0.001,
                "{name} render must contain colorful pixels"
            );
        }
        maybe_capture(name, pixels, h.width, h.height);
    }
    let upgraded_error = mean_abs_diff(&upgraded_pixels, &fresh_pixels);
    let legacy_error = mean_abs_diff(&legacy_pixels, &fresh_pixels);
    eprintln!(
        "material upgrade render diff: legacy={legacy_error:.6}, upgraded={upgraded_error:.6}"
    );
    assert!(
        upgraded_error < 0.01,
        "upgraded render must match fresh import: {upgraded_error:.6}"
    );
    assert!(
        legacy_error > 0.001,
        "legacy white-colour render must differ from fresh import: {legacy_error:.6}"
    );
    assert!(
        upgraded_error < legacy_error * 0.25,
        "upgrade must materially improve parity: upgraded={upgraded_error:.6}, legacy={legacy_error:.6}"
    );
}
