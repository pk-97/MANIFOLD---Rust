//! The shipped `Swarm.json` is the Add menu's swarm inserted into its
//! dressing (`swarm_preset_def`), so the preset and an inserted swarm are one
//! graph: one clock wiring, one card, one row.

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, GROUP_TYPE_ID};
use manifold_nodes_scene::node_graph::scene_templates::swarm_preset_def;
use manifold_core::scene_world_controls::{WORLD_INPUTS, WorldControlInput};
use serde_json::Value;

const SWARM_PRESET_PATH: &str = "assets/generator-presets/Swarm.json";

fn canonical(def: &Value) -> Value {
    let mut def = def.clone();
    def["nodes"].as_array_mut().expect("nodes").sort_by_key(|node| node["id"].as_u64());
    def["wires"].as_array_mut().expect("wires").sort_by_key(|wire| {
        let end = |key: &str| wire[key].as_u64().expect("wire end");
        let port = |key: &str| wire[key].as_str().expect("wire port").to_string();
        (end("fromNode"), port("fromPort"), end("toNode"), port("toPort"))
    });
    def
}

fn shipped() -> EffectGraphDef {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SWARM_PRESET_PATH);
    serde_json::from_str(&std::fs::read_to_string(path).expect("the shipped preset reads")).expect("parses")
}

/// `UPDATE_SWARM_PRESET=1` rewrites `Swarm.json` from the builder.
#[test]
fn swarm_shipped_preset_is_the_inserted_template() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SWARM_PRESET_PATH);
    let built = serde_json::to_value(swarm_preset_def()).expect("serialise");
    if std::env::var("UPDATE_SWARM_PRESET").is_ok() {
        let mut json = serde_json::to_string_pretty(&built).expect("serialise");
        json.push('\n');
        std::fs::write(&path, json).expect("write the shipped preset");
    }
    let shipped: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("the shipped preset reads")).expect("parses");
    assert!(
        canonical(&shipped) == canonical(&built),
        "Swarm.json differs from the inserted swarm template; rerun with UPDATE_SWARM_PRESET=1"
    );
}

/// The shipped swarm follows every Physics World control as water does: one
/// source drives each World control and the swarm's matching input, so it
/// follows World Speed (D-107) and falls under World gravity (D-109), and a
/// card change to either reaches it. Reset is a card trigger, which every
/// clip trigger fires. No generator trigger count reaches the swarm.
#[test]
fn swarm_shipped_preset_follows_world_speed_gravity_and_reset() {
    let def = shipped();
    let world = def.nodes.iter().find(|node| node.type_id == "node.physics_world").expect("the preset has a World");
    let group = def.nodes.iter().find(|node| node.type_id == GROUP_TYPE_ID).expect("the swarm is one group");
    let body = group.group.as_deref().expect("group body");
    let forces = body.nodes.iter().find(|node| node.type_id == "node.swarm_forces").expect("forces node");
    let meta = def.preset_metadata.as_ref().expect("card");
    for WorldControlInput { world: world_port, input: port } in WORLD_INPUTS {
        let source = def.wires.iter().find(|wire| wire.to_node == world.id && wire.to_port == world_port).expect("World source");
        assert!(
            def.wires.iter().any(|wire| (wire.from_node, wire.from_port.as_str(), wire.to_node, wire.to_port.as_str())
                == (source.from_node, source.from_port.as_str(), group.id, world_port)),
            "the World's {port} source enters the swarm group"
        );
        let input = body.nodes.iter().find(|node| node.type_id == "system.group_input").expect("group input");
        assert!(
            body.wires.iter().any(|wire| wire.from_node == input.id && wire.from_port == world_port
                && wire.to_node == forces.id && wire.to_port == port),
            "{port} reaches the swarm's forces node"
        );
        let source_node = def.nodes.iter().find(|node| node.id == source.from_node).expect("source node");
        let binding = meta
            .bindings
            .iter()
            .find(|binding| matches!(&binding.target, BindingTarget::Node { node_id, param }
                if node_id == &source_node.node_id && param == "value"))
            .unwrap_or_else(|| panic!("World {port} is on the card"));
        let spec = meta.params.iter().find(|spec| spec.id == binding.id).expect("exposure");
        assert_eq!(spec.is_trigger, port == "reset", "{port} trigger flag");
    }
    let input = def.nodes.iter().find(|node| node.type_id == "system.generator_input").expect("generator input");
    assert!(!def.wires.iter().any(|wire| wire.from_node == input.id), "nothing reads the generator trigger count");
    assert!(
        !meta.bindings.iter().any(|binding| matches!(&binding.target, BindingTarget::Node { node_id, param }
            if node_id == &forces.node_id && param == "speed")),
        "the swarm's own Speed is not a card control: the World drives it"
    );
}
