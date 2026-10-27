use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::{Param, ParamManifest};
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::{parameters::ParamValue, persistence::PrimitiveRegistry};

fn shipped() -> EffectGraphDef {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/presets/WaterDamBreakGpuFlip.json"
    )))
    .unwrap()
}

#[test]
fn gpu_flip_surface_fresh_defaults_and_live_bindings_reach_nested_nodes() {
    let def = shipped();
    let mut params = ParamManifest::from_params(
        def.preset_metadata
            .as_ref()
            .unwrap()
            .params
            .iter()
            .cloned()
            .map(Param::bundled)
            .collect(),
    );
    let mut runtime =
        PresetRuntime::from_def(def, &PrimitiveRegistry::with_builtin(), None).unwrap();
    let assert_param = |runtime: &PresetRuntime, node: &str, name: &str, value| {
        let id = runtime
            .graph
            .instance_by_node_id(&NodeId::new(node))
            .unwrap();
        assert_eq!(
            runtime.graph.get_node(id).unwrap().params.get(name),
            Some(&ParamValue::Float(value)),
            "{node}.{name}"
        );
    };
    assert_param(&runtime, "surface_tiles", "resolution_scale", 1.0);
    assert_param(&runtime, "liquid_particle_scale", "value", 3.0);
    assert_param(&runtime, "liquid_smoothness", "value", 1.0);
    assert_param(&runtime, "water_ripple_detail", "value", 1.0);
    assert!(runtime.shadowed_def_params().next().is_none());
    for (name, value) in [
        ("surface_detail", 1.0),
        ("surface_particle_scale", 2.2),
        ("surface_smoothness", 1.5),
        ("surface_ripple_detail", 0.5),
    ] {
        let param = params.get_mut(name).unwrap();
        param.value = value;
        param.base = value;
    }
    runtime.apply_param_values(&params);
    assert_param(&runtime, "surface_tiles", "resolution_scale", 2.0);
    assert_param(&runtime, "liquid_particle_scale", "value", 2.2);
    assert_param(&runtime, "liquid_smoothness", "value", 1.5);
    assert_param(&runtime, "water_ripple_detail", "value", 0.5);
}
