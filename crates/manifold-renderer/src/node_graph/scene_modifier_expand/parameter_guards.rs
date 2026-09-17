//! Prepared source selectors cannot change underneath saved calibration or
//! allocated buffers. Scene targets remain valid while ordinary controls stay live.

use super::SceneModifierExpandError;
use crate::node_graph::Graph;
use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
use manifold_core::scene_modifier_preset::SceneEndpoint;
use manifold_core::NodeId;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct PreparedModifierParameterGuards {
    sources: Vec<(NodeId, BTreeMap<String, SerializedParamValue>)>,
    scenes: Vec<NodeId>,
}

fn invalid(path: impl Into<String>, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::UnsupportedCoordinateFrame {
        path: path.into(),
        detail: detail.into(),
    }
}

fn source_stat(name: &str) -> bool {
    matches!(name, "source_vertex_count" | "source_bbox_radius")
}

impl PreparedModifierParameterGuards {
    pub(crate) fn prepare(owner: &EffectGraphDef) -> Result<Self, SceneModifierExpandError> {
        let ids: BTreeSet<_> = owner
            .scene_modifiers
            .iter()
            .flat_map(|modifier| {
                modifier
                    .mesh_frames
                    .iter()
                    .map(|frame| frame.source.node.as_str())
            })
            .collect();
        let mut sources = Vec::with_capacity(ids.len());
        // Read the calibrated host before expansion plants card defaults.
        // Otherwise a selector-changing default could become its own baseline.
        let index = super::index::FlatSceneIndex::build(owner)?;
        for id in ids {
            let source = index
                .flat
                .nodes
                .iter()
                .find(|node| node.node_id.as_str() == id)
                .ok_or_else(|| invalid(id, "prepared source is absent"))?;
            let mut params =
                manifold_core::scene_source_identity::effective_source_params(owner, source)
                    .map_err(|error| invalid(id, error.to_string()))?;
            if let Some(capacity) = source.params.get("max_capacity") {
                params.insert("max_capacity".into(), capacity.clone());
            }
            sources.push((source.node_id.clone(), params));
        }
        let mut scenes: Vec<NodeId> = owner
            .scene_modifiers
            .iter()
            .filter(|modifier| {
                modifier
                    .graph
                    .preset_metadata
                    .as_ref()
                    .and_then(|meta| meta.scene_modifier.as_ref())
                    .is_some_and(|recipe| {
                        recipe.stages.iter().any(|stage| {
                            stage
                                .outputs
                                .iter()
                                .any(|output| output.endpoint == SceneEndpoint::Vertices)
                        })
                    })
            })
            .map(|modifier| modifier.scene.node.clone())
            .collect();
        // Legacy v2 graphs have no scene-modifier metadata. Their fragment
        // stages survive inside flattened groups, so discover every render
        // scene reachable downstream from those stages and retain target
        // existence validation for the prepared runtime.
        let fragment_ids: BTreeSet<u32> = index
            .flat
            .nodes
            .iter()
            .filter(|node| {
                matches!(
                    node.type_id.as_str(),
                    "node.ordered_recon_mesh" | "node.transform_mesh_patches"
                )
            })
            .map(|node| node.id)
            .collect();
        let mut pending: Vec<u32> = fragment_ids.iter().copied().collect();
        let mut visited = BTreeSet::new();
        while let Some(from_node) = pending.pop() {
            for wire in index
                .flat
                .wires
                .iter()
                .filter(|wire| wire.from_node == from_node)
            {
                if !visited.insert(wire.to_node) {
                    continue;
                }
                let Some(target) = index.flat.nodes.iter().find(|node| node.id == wire.to_node)
                else {
                    continue;
                };
                if target.type_id == "node.render_scene" {
                    if !target.node_id.is_empty() && !scenes.contains(&target.node_id) {
                        scenes.push(target.node_id.clone());
                    }
                } else {
                    pending.push(target.id);
                }
            }
        }
        Ok(Self {
            sources,
            scenes,
        })
    }

    pub(crate) fn install(self, graph: &mut Graph) -> Result<(), SceneModifierExpandError> {
        // Validate all initial live values before installing any constraints.
        for (source, expected) in &self.sources {
            let node = graph
                .instance_by_node_id(source)
                .and_then(|id| graph.get_node(id))
                .ok_or_else(|| invalid(source.to_string(), "runtime source is absent"))?;
            let mut expected_params: BTreeMap<_, _> = node
                .node
                .parameters()
                .iter()
                .map(|param| (param.name.to_string(), param.default.clone()))
                .collect();
            expected_params.extend(
                expected
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone().into())),
            );
            for (name, expected) in &expected_params {
                if source_stat(name) {
                    continue;
                }
                if node.params.get(name.as_str()) != Some(expected) {
                    return Err(invalid(
                        source.to_string(),
                        format!(
                            "live source selector {name} differs from its saved calibration; restore it or reapply the modifier"
                        ),
                    ));
                }
            }
        }
        for scene in &self.scenes {
            graph
                .instance_by_node_id(scene)
                .and_then(|id| graph.get_node(id))
                .ok_or_else(|| invalid(scene.to_string(), "runtime scene is absent"))?;
        }
        for (source, _) in self.sources {
            let id = graph
                .instance_by_node_id(&source)
                .expect("source validated above");
            let names: Vec<_> = graph
                .get_node(id)
                .expect("source validated above")
                .params
                .keys()
                .filter(|name| !source_stat(name))
                .map(|name| name.to_string())
                .collect();
            for name in names {
                graph
                    .protect_prepared_param(id, &name)
                    .map_err(|error| invalid(source.to_string(), error.to_string()))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::persistence::EffectGraphDefExt;

    const LEGACY_SURFACE_PEEL: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/scene-modifiers/surface_peel_applied_v2.json"
    ));

    #[test]
    fn legacy_fragment_accepts_rt_toggles_and_validates_target() {
        let owner: EffectGraphDef =
            serde_json::from_str(LEGACY_SURFACE_PEEL).expect("legacy fixture parses");
        let guards = PreparedModifierParameterGuards::prepare(&owner)
            .expect("legacy fragment graph should prepare");
        assert_eq!(
            guards.scenes,
            vec![NodeId::new("scan_render")],
            "the legacy downstream render scene remains a valid target"
        );
        assert!(guards.sources.is_empty());

        let mut graph = owner
            .into_graph(
                &crate::node_graph::persistence::PrimitiveRegistry::with_builtin(),
                &crate::node_graph::mesh_change::PreparedMeshRules::default(),
            )
            .expect("legacy fixture graph builds");
        let scene = graph
            .instance_by_node_id(&NodeId::new("scan_render"))
            .expect("legacy render scene");
        graph.set_param_unchecked(
            scene,
            "rt_enabled",
            crate::node_graph::ParamValue::Bool(true),
        );
        guards
            .install(&mut graph)
            .expect("RT-enabled legacy scene remains admissible");
        assert!(graph
            .set_param(scene, "rt_enabled", crate::node_graph::ParamValue::Bool(false))
            .is_ok());
        assert!(graph
            .set_param(scene, "rt_enabled", crate::node_graph::ParamValue::Bool(true))
            .is_ok());
    }
}
