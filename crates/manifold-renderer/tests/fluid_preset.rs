//! Structural and persistence contract for the CPU FLIP Water Basin scene.
//!
//! This goes through the production preset loader so the scene cannot pass as
//! JSON alone. The graph is intentionally small: the fluid surface, emitter,
//! moving obstacle, and open cutaway basin are all checked by their authored
//! node and wire identities.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, SerializedParamValue,
};
use manifold_core::effects::ParamConvert;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_runtime::PresetRuntime;

const WATER_BASIN_JSON: &str = include_str!("../assets/generator-presets/WaterBasin.json");
const WATER_DAM_BREAK_JSON: &str = include_str!("../assets/generator-presets/WaterDamBreak.json");

#[test]
fn water_dam_break_compiles_with_separate_whitewater_populations() {
    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_json_str(WATER_DAM_BREAK_JSON, &registry)
        .expect("WaterDamBreak must compile through the production loader");
    let def: EffectGraphDef = serde_json::from_str(WATER_DAM_BREAK_JSON).unwrap();
    let nodes = nodes_by_id(&def);
    let fluid = nodes["fluid_surface"];
    assert_eq!(float_param(fluid, "whitewater"), 1.0);
    for (population, count, object) in [
        ("foam", "foam_count", "foam_object"),
        ("bubbles", "bubble_count", "bubble_object"),
        ("spray", "spray_count", "spray_object"),
    ] {
        assert!(has_wire(
            &def,
            fluid.id,
            population,
            nodes[object].id,
            "instances"
        ));
        assert!(has_wire(
            &def,
            fluid.id,
            count,
            nodes[object].id,
            "instance_count"
        ));
    }
    assert_eq!(float_param(nodes["environment_select"], "selector"), 0.0);
    let metadata = def.preset_metadata.as_ref().unwrap();
    assert!(
        metadata
            .string_params
            .iter()
            .all(|param| param.default_value.is_empty())
    );
}

fn parse() -> EffectGraphDef {
    serde_json::from_str(WATER_BASIN_JSON).expect("WaterBasin preset must parse")
}

fn nodes_by_id(def: &EffectGraphDef) -> BTreeMap<&str, &EffectGraphNode> {
    def.nodes
        .iter()
        .map(|node| (node.node_id.as_str(), node))
        .collect()
}

fn float_param(node: &EffectGraphNode, name: &str) -> f32 {
    match node.params.get(name) {
        Some(SerializedParamValue::Float { value }) => *value,
        Some(SerializedParamValue::Int { value }) => *value as f32,
        Some(other) => panic!("{}.{} must be numeric, got {other:?}", node.node_id, name),
        None => panic!("{}.{} is required", node.node_id, name),
    }
}

fn int_param(node: &EffectGraphNode, name: &str) -> i32 {
    match node.params.get(name) {
        Some(SerializedParamValue::Int { value }) => *value,
        Some(other) => panic!("{}.{} must be an Int, got {other:?}", node.node_id, name),
        None => panic!("{}.{} is required", node.node_id, name),
    }
}

fn has_wire(def: &EffectGraphDef, from: u32, from_port: &str, to: u32, to_port: &str) -> bool {
    def.wires.iter().any(|wire| {
        wire.from_node == from
            && wire.from_port == from_port
            && wire.to_node == to
            && wire.to_port == to_port
    })
}

#[test]
fn water_basin_compiles_with_fluid_scene_and_stable_card_bindings() {
    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_json_str(WATER_BASIN_JSON, &registry)
        .expect("WaterBasin must compile through the production loader");

    let def = parse();
    assert_eq!(def.version, 2);
    assert_eq!(def.name.as_deref(), Some("Water Basin"));
    assert!(
        def.description
            .as_deref()
            .is_some_and(|text| text.contains("CPU FLIP") && text.contains("lag"))
    );

    let metadata = def.preset_metadata.as_ref().expect("WaterBasin metadata");
    assert_eq!(metadata.id.as_str(), "WaterBasin");
    assert_eq!(metadata.display_name, "Water Basin (CPU)");
    let param_ids: BTreeSet<_> = metadata
        .params
        .iter()
        .map(|param| param.id.as_str())
        .collect();
    assert_eq!(
        param_ids,
        BTreeSet::from([
            "resolution",
            "pour",
            "flow",
            "speed",
            "reset",
            "surface_detail"
        ])
    );

    let nodes = nodes_by_id(&def);
    let fluid = nodes
        .get("fluid_surface")
        .expect("fluid_surface node is authored");
    assert_eq!(fluid.type_id, "node.fluid_surface");
    assert_eq!(int_param(fluid, "resolution"), 24);
    assert_eq!(float_param(fluid, "domain_size"), 4.0);
    assert_eq!(float_param(fluid, "fill_height"), 0.4);
    assert_eq!(float_param(fluid, "gravity"), -9.81);
    assert_eq!(float_param(fluid, "emission"), 1.0);
    assert_eq!(float_param(fluid, "inflow_speed"), 1.5);
    assert_eq!(float_param(fluid, "speed"), 1.0);
    assert_eq!(int_param(fluid, "surface_subdivisions"), 0);
    assert_eq!(int_param(fluid, "max_capacity"), 786_432);
    assert_eq!(
        fluid.params.get("transfer"),
        Some(&SerializedParamValue::Enum { value: 0 }),
        "the reference scene uses FLIP transfer"
    );
    assert_eq!(int_param(nodes["scene"], "objects"), 6);
    assert_eq!(int_param(nodes["scene"], "lights"), 1);

    // The obstacle is simulated from the animated input transform, while the
    // rendered object consumes the solver's accepted pose to avoid running
    // ahead when the CPU preview has pending ticks.
    for (from, from_port, to, to_port) in [
        (5, "transform", 4, "emitter"),
        (6, "out", 7, "pos_x"),
        (7, "transform", 4, "obstacle"),
        (4, "vertices", 9, "vertices"),
        (4, "obstacle_pose", 12, "transform"),
        (12, "object", 30, "object_1"),
    ] {
        assert!(
            has_wire(&def, from, from_port, to, to_port),
            "missing wire {from}.{from_port} -> {to}.{to_port}"
        );
    }
    assert_eq!(
        nodes["obstacle_lfo"].params.get("rate_mode"),
        Some(&SerializedParamValue::Enum { value: 1 }),
        "obstacle motion must be free-time"
    );
    assert_eq!(float_param(nodes["obstacle_lfo"], "angular_rate"), 1.5);
    assert_eq!(float_param(nodes["obstacle_lfo"], "min"), -0.25);
    assert_eq!(float_param(nodes["obstacle_lfo"], "max"), 1.0);

    // Every card control targets the live fluid node. Reset is a primitive
    // trigger and therefore has no persisted scalar value in the node map.
    for (id, param, convert) in [
        ("resolution", "resolution", ParamConvert::IntRound),
        ("pour", "emission", ParamConvert::Float),
        ("flow", "inflow_speed", ParamConvert::Float),
        ("speed", "speed", ParamConvert::Float),
        ("reset", "reset", ParamConvert::Trigger),
        (
            "surface_detail",
            "surface_subdivisions",
            ParamConvert::IntRound,
        ),
    ] {
        let binding = metadata
            .bindings
            .iter()
            .find(|binding| binding.id == id)
            .unwrap_or_else(|| panic!("missing {id} binding"));
        assert_eq!(binding.convert, convert, "{id} conversion drifted");
        let BindingTarget::Node {
            node_id,
            param: target_param,
        } = &binding.target
        else {
            panic!("{id} must target a node parameter");
        };
        assert_eq!(node_id.as_str(), "fluid_surface");
        assert_eq!(target_param, param);
    }

    let roundtrip: EffectGraphDef =
        serde_json::from_str(&serde_json::to_string(&def).expect("WaterBasin serializes"))
            .expect("serialized WaterBasin reloads");
    assert_eq!(
        roundtrip, def,
        "metadata and graph wires must round-trip exactly"
    );
}
