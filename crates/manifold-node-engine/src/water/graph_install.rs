//! Construction-time graph installation owned by the water family.

use ahash::AHashMap;
use manifold_core::effect_graph_def::EffectGraphDef;

use crate::exec::effect_node::NodeInstanceId;
use crate::graph::Graph;
use crate::load::graph_loader::GraphBuildError;
use crate::load::instantiation::GraphInstantiationHook;
use crate::persistence::PrimitiveRegistry;

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
        let bindings = crate::load::expand::prepare_coupled_scenes(def, registry)
            .map_err(GraphBuildError::SceneModifier)?;
        for binding in bindings {
            let fluid_doc = def
                .nodes
                .iter()
                .find(|node| node.node_id == binding.fluid)
                .ok_or_else(|| {
                    GraphBuildError::SceneModifier(
                        crate::load::expand::SceneModifierExpandError::MissingTarget {
                            path: binding.fluid.to_string(),
                            detail:
                                "prepared coupled fluid node is missing from the instantiated def"
                                    .into(),
                        },
                    )
                })?;
            let rigid_doc = def
                .nodes
                .iter()
                .find(|node| node.node_id == binding.rigid)
                .ok_or_else(|| {
                    GraphBuildError::SceneModifier(
                        crate::load::expand::SceneModifierExpandError::MissingTarget {
                            path: binding.rigid.to_string(),
                            detail:
                                "prepared coupled rigid node is missing from the instantiated def"
                                    .into(),
                        },
                    )
                })?;
            let fluid = *id_map.get(&fluid_doc.id).ok_or_else(|| {
                GraphBuildError::SceneModifier(
                    crate::load::expand::SceneModifierExpandError::MissingTarget {
                        path: binding.fluid.to_string(),
                        detail: "prepared coupled fluid node has no runtime mapping".into(),
                    },
                )
            })?;
            let rigid = *id_map.get(&rigid_doc.id).ok_or_else(|| {
                GraphBuildError::SceneModifier(
                    crate::load::expand::SceneModifierExpandError::MissingTarget {
                        path: binding.rigid.to_string(),
                        detail: "prepared coupled rigid node has no runtime mapping".into(),
                    },
                )
            })?;
            crate::water::physics_scene::add_coupled_scene(graph, fluid, rigid, binding.colliders)
                .map_err(|error| GraphBuildError::InvalidWire {
                    wire_index: usize::MAX,
                    reason: format!("failed to register coupled scene: {error}"),
                })?;
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
