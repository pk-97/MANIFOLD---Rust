//! WATER_SIMULATION_DESIGN.md section 9, row "Reset/load/export semantics"
//! (`water_preset_roundtrip_modulates`).
//!
//! A card poke on an exposed WaterPrototype param must survive the real
//! save → reload path and still drive its inner node afterwards:
//!   (a) the reloaded manifest holds the poked value,
//!   (b) the preset binding still routes the manifest value into the node
//!       after reload — poke again post-reload and the node param follows,
//!   (c) a simReset integer bump after reload still reaches the reset
//!       source node through its binding.
//!
//! Mock backend throughout (no GPU): the runtime is built through the real
//! `PresetRuntime::from_json_str` path and params flow through the same
//! `apply_param_values` entry `render()` calls per frame.
//!
//! What the mock backend canNOT do here, stated exactly:
//! - Run frames: the full water plan declares GpuEncoder + StateStore
//!   requirements, and every no-GPU executor entry panics at entry by
//!   design (`execution.rs` `Executor::execute_frame` assert) — water is
//!   GPU-only, unlike the Lissajous graph `water_lifecycle` runs. So no
//!   `execute_frame` loop appears in this test; the frame-driven contract
//!   (bindings applied per frame) is the `apply_param_values` routing
//!   asserted below.
//! - Observe `water_state`'s reset edge re-seeding the particle buffer:
//!   that path needs the GpuEncoder and the seeded array buffer, neither
//!   of which exists on mock (`water_state`'s `run` early-returns before
//!   reading `reset_trigger`). So (c) asserts the binding layer — manifest
//!   → binding → `resetSrc` node param — which is the full contract that
//!   survives reload; the edge-detection consumption below that is
//!   `water_state`'s own unit-tested logic.

use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::preset_def::PresetKind;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::{Beats, NodeId, PresetTypeId};
use manifold_renderer::node_graph::{ParamValue, PrimitiveRegistry};
use manifold_renderer::preset_runtime::PresetRuntime;

const WATER_JSON: &str = include_str!("../assets/generator-presets/WaterPrototype.json");
const WATER_BASELINE_JSON: &str = include_str!("../assets/generator-presets/WaterPrototypeBaseline.json");

#[test]
fn water_presets_keep_stationary_camera_and_solver_geometry() {
    for source in [WATER_JSON, WATER_BASELINE_JSON] {
        let value: serde_json::Value = serde_json::from_str(source).expect("water preset JSON");
        let nodes = value.get("nodes").and_then(serde_json::Value::as_array).expect("top-level nodes");
        if source == WATER_JSON {
            let water = nodes.iter().find(|node| node.get("nodeId") == Some(&serde_json::Value::String("water".into()))).expect("water group");
            let water_nodes = water["group"]["nodes"].as_array().expect("water nodes");
            let emit = water_nodes.iter().find(|node| node.get("nodeId") == Some(&serde_json::Value::String("emit".into()))).expect("emit node");
            assert_eq!(emit["params"]["rate"]["value"].as_f64(), None, "rate is wired from the exposed pour control");
            assert_eq!(emit["params"]["repeat"]["value"].as_f64(), Some(1.0));
            assert_eq!(emit["params"]["velocity_y"]["value"].as_f64(), Some(-1.0));
        }
        let scene = nodes.iter().find(|node| node.get("nodeId") == Some(&serde_json::Value::String("scene".into()))).expect("scene group");
        let group_nodes = scene.get("group").and_then(|g| g.get("nodes")).and_then(serde_json::Value::as_array).expect("scene nodes");
        assert!(group_nodes.iter().all(|node| node.get("nodeId") != Some(&serde_json::Value::String("orbitSweep".into()))));
        let camera = group_nodes.iter().find(|node| node.get("nodeId") == Some(&serde_json::Value::String("camera".into()))).expect("camera node");
        let expected_orbit = if source == WATER_JSON { -2.9670597 } else { 0.698132 };
        assert_eq!(camera["params"]["orbit"]["value"].as_f64(), Some(expected_orbit));
        if source == WATER_JSON {
            for (node_id, expected) in [("wallSObj", 0.0), ("wallWObj", 0.0), ("wallNObj", 1.0), ("wallEObj", 1.0)] {
                let node = group_nodes.iter().find(|node| node.get("nodeId") == Some(&serde_json::Value::String(node_id.into()))).expect("basin wall object");
                assert_eq!(node["params"].get("visible").and_then(serde_json::Value::as_object).and_then(|p| p.get("value")).and_then(serde_json::Value::as_f64).unwrap_or(1.0), expected);
            }
        }
        for (id, x) in [("strip0Xform", -0.9375), ("strip1Xform", -0.5625), ("strip2Xform", -0.1875), ("strip3Xform", 0.1875), ("strip4Xform", 0.5625), ("strip5Xform", 0.9375)] {
            let node = group_nodes.iter().find(|node| node.get("nodeId") == Some(&serde_json::Value::String(id.into()))).expect("floor transform");
            assert_eq!(node["params"]["pos_x"]["value"].as_f64(), Some(x));
            assert_eq!(node["params"]["pos_y"]["value"].as_f64(), Some(0.245));
        }
    }
}

/// The one-layer/one-clip generator project, in the exact shape
/// `manifold-app`'s `generator_editor_fixture` builds it.
fn water_project() -> (Project, manifold_core::LayerId) {
    let pid = PresetTypeId::from_string("WaterPrototype".to_string());
    assert!(
        manifold_renderer::node_graph::bundled_preset_type_ids(PresetKind::Generator)
            .any(|id| id == pid),
        "WaterPrototype must be a bundled generator preset"
    );

    let mut layer = Layer::new("WaterPrototype".into(), LayerType::Generator, 0);
    let layer_id = layer.layer_id.clone();
    layer.change_generator_type(pid);
    layer.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(48.0)));

    let mut project = Project::default();
    project.timeline.layers = vec![layer];
    (project, layer_id)
}

/// Read a node's `value` param from the compiled runtime graph, addressed
/// by the stable `nodeId` (survives the group flatten).
fn node_value(runtime: &PresetRuntime, node_id: &str) -> Option<f32> {
    let instance = runtime
        .graph
        .instance_by_node_id(&NodeId::new(node_id))
        .unwrap_or_else(|| panic!("node '{node_id}' must exist in the compiled water graph"));
    match runtime.graph.get_node(instance).unwrap().params.get("value") {
        Some(ParamValue::Float(v)) => Some(*v),
        other => panic!("node '{node_id}' must carry a Float 'value' param, got {other:?}"),
    }
}

#[test]
fn water_preset_roundtrip_modulates() {
    let registry = PrimitiveRegistry::with_builtin();

    // --- poke pourRate the way a card poke does: the set_base_param funnel
    // (the same PresetInstance write manifold-editing's graph param command
    // uses), then the per-frame path render() takes. ---
    let (mut project, layer_id) = water_project();
    {
        let layer = project.timeline.layers.iter_mut().find(|l| l.layer_id == layer_id).unwrap();
        let poked = layer
            .gen_params_mut()
            .expect("change_generator_type seeds the manifest")
            .set_base_param("pourRate", 800.0);
        assert!(poked, "pourRate must be a manifest param of the seeded layer");
    }

    let mut runtime = PresetRuntime::from_json_str(WATER_JSON, &registry)
        .expect("WaterPrototype must build on the mock backend");
    let manifest = project.timeline.layers[0]
        .gen_params()
        .map(|gp| gp.params.clone())
        .expect("manifest present");
    runtime.apply_param_values(&manifest);
    assert!(
        (node_value(&runtime, "pourRateSrc").unwrap() - 800.0).abs() < 1e-5,
        "binding must route the poked manifest value into the pourRate source node"
    );

    // --- the real save path: V1 project JSON, reload, re-check ---
    let path = std::env::temp_dir().join(format!(
        "manifold_water_roundtrip_{}_{}.manifold",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    manifold_io::saver::save_project_v1(&project, &path).expect("save v1");
    let reloaded = manifold_io::loader::load_project(&path);
    let _ = std::fs::remove_file(&path);
    let mut reloaded = reloaded.expect("reload");

    // (a) the poked value survived the round trip.
    let reloaded_layer = reloaded
        .timeline
        .layers
        .iter_mut()
        .find(|l| l.layer_id == layer_id)
        .expect("layer survives reload");
    let reloaded_manifest = &reloaded_layer.gen_params().expect("gen_params survive reload").params;
    let pour = reloaded_manifest
        .get("pourRate")
        .unwrap_or_else(|| panic!("pourRate must survive reload in the manifest"));
    assert!(
        (pour.value - 800.0).abs() < 1e-5,
        "reloaded manifest must hold the poked pourRate, got {}",
        pour.value
    );

    // (b) the binding still drives the node after reload: rebuild the
    // runtime the from-disk path builds and poke again post-reload.
    let mut runtime = PresetRuntime::from_json_str(WATER_JSON, &registry)
        .expect("WaterPrototype must rebuild on the mock backend");
    runtime.apply_param_values(reloaded_manifest);
    assert!(
        (node_value(&runtime, "pourRateSrc").unwrap() - 800.0).abs() < 1e-5,
        "post-reload binding must still route the manifest value into the node"
    );
    let poked_again = reloaded_layer
        .gen_params_mut()
        .unwrap()
        .set_base_param("pourRate", 1500.0);
    assert!(poked_again, "second poke must land");
    runtime.apply_param_values(&reloaded_layer.gen_params().unwrap().params);
    assert!(
        (node_value(&runtime, "pourRateSrc").unwrap() - 1500.0).abs() < 1e-5,
        "post-reload poke must follow through the binding into the node"
    );

    // (c) simReset integer bump after reload reaches the reset source node
    // through its binding. What is NOT asserted here: water_state actually
    // re-seeding — the reset edge's consumption needs the GpuEncoder +
    // seeded array buffer (absent on mock; water_state early-returns before
    // reading reset_trigger). See the module doc.
    let reset = reloaded_layer
        .gen_params_mut()
        .unwrap()
        .set_base_param("simReset", 1.0);
    assert!(reset, "simReset must be a manifest param after reload");
    runtime.apply_param_values(&reloaded_layer.gen_params().unwrap().params);
    assert!(
        (node_value(&runtime, "resetSrc").unwrap() - 1.0).abs() < 1e-5,
        "simReset bump must reach the reset source node through the binding"
    );
}
