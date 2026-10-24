//! Construction-time graph installation owned by the water family.

use ahash::AHashMap;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;

use manifold_node_engine::exec::effect_node::NodeInstanceId;
use manifold_node_engine::graph::Graph;
use manifold_node_engine::load::expand::SceneModifierExpandError;
use manifold_node_engine::load::graph_loader::GraphBuildError;
use manifold_node_engine::load::instantiation::GraphInstantiationHook;
use manifold_node_engine::persistence::PrimitiveRegistry;

pub mod coupling;

/// The runtime node a prepared stable id names in this instantiation.
fn runtime_node(
    def: &EffectGraphDef,
    id_map: &AHashMap<u32, NodeInstanceId>,
    node_id: &NodeId,
) -> Result<NodeInstanceId, GraphBuildError> {
    let missing = |detail: &str| {
        GraphBuildError::SceneModifier(SceneModifierExpandError::MissingTarget {
            path: node_id.to_string(),
            detail: detail.into(),
        })
    };
    let doc = def
        .nodes
        .iter()
        .find(|node| node.node_id == *node_id)
        .ok_or_else(|| missing("prepared coupled node is missing from the instantiated def"))?;
    id_map
        .get(&doc.id)
        .copied()
        .ok_or_else(|| missing("prepared coupled node has no runtime mapping"))
}

fn install_coupled_scenes(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    id_map: &AHashMap<u32, NodeInstanceId>,
    graph: &mut Graph,
) -> Result<(), GraphBuildError> {
    // Coupled physics is graph-owned runtime metadata. Resolve stable scene
    // identities against this exact def and this instantiation's id_map;
    // the graph-wide stable-id lookup is not valid for effect splices.
    let has_fluid = def
        .nodes
        .iter()
        .any(|node| manifold_core::liquid_domain::is_liquid_domain(&node.type_id));
    let has_rigid = def
        .nodes
        .iter()
        .any(|node| node.type_id == "node.physics_world");
    if has_fluid && has_rigid {
        let bindings = coupling::prepare_coupled_scenes(def, registry)
            .map_err(GraphBuildError::SceneModifier)?;
        let failed = |error| GraphBuildError::InvalidWire {
            wire_index: usize::MAX,
            reason: format!("failed to register coupled scene: {error}"),
        };
        for binding in bindings {
            let fluid = runtime_node(def, id_map, &binding.fluid)?;
            let rigid = runtime_node(def, id_map, &binding.rigid)?;
            crate::physics_scene::add_coupled_scene(graph, fluid, rigid, binding.colliders)
                .map_err(failed)?;
            for observer in &binding.observers {
                let observer = runtime_node(def, id_map, observer)?;
                crate::physics_scene::add_shared_solids(graph, fluid, observer).map_err(failed)?;
            }
        }
    }
    Ok(())
}

inventory::submit! {
    GraphInstantiationHook {
        name: "water.coupled-scenes",
        apply: install_coupled_scenes,
    }
}
