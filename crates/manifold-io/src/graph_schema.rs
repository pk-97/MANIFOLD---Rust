//! Graph schema checks before project catalog registration or file publication.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::project::Project;
use manifold_core::scene_modifier_preset::{
    SceneModifierSchemaError, has_scene_modifier_data, validate_scene_modifier_schema,
};

#[derive(Debug)]
pub(crate) struct ProjectGraphSchemaError {
    graph: String,
    source: SceneModifierSchemaError,
}

impl std::fmt::Display for ProjectGraphSchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.graph, self.source)
    }
}

pub(crate) fn validate_project_graphs(project: &Project) -> Result<(), ProjectGraphSchemaError> {
    fn check(
        graph: &EffectGraphDef,
        owner: String,
        instance_override: bool,
    ) -> Result<(), ProjectGraphSchemaError> {
        // Existing user-binding overrides (including migrate::fold_user_param_bindings)
        // store metadata in an empty v0 stub. The renderer supplies its preset topology.
        // This exception is only for instance metadata, never executable/embedded graphs
        // or any scene-modifier data.
        if instance_override
            && graph.version == 0
            && graph.nodes.is_empty()
            && graph.wires.is_empty()
            && graph.preset_metadata.is_some()
            && !has_scene_modifier_data(graph)
        {
            return Ok(());
        }
        validate_scene_modifier_schema(graph).map_err(|source| ProjectGraphSchemaError {
            graph: owner,
            source,
        })
    }

    for (index, preset) in project.embedded_presets.iter().enumerate() {
        let has_recipe = preset
            .def
            .preset_metadata
            .as_ref()
            .is_some_and(|meta| meta.scene_modifier.is_some());
        if has_recipe != preset.kind.is_scene_modifier() {
            return Err(ProjectGraphSchemaError {
                graph: format!("embedded preset {index}"),
                source: SceneModifierSchemaError::InvalidRecipe {
                    path: "presetMetadata.sceneModifier".into(),
                    detail: "sceneModifier preset kind and recipe metadata must agree".into(),
                },
            });
        }
        check(&preset.def, format!("embedded preset {index}"), false)?;
    }
    for effect in &project.settings.master_effects {
        if let Some(graph) = &effect.graph {
            check(graph, format!("master effect {}", effect.id), true)?;
        }
    }
    for layer in &project.timeline.layers {
        if let Some(graph) = layer.generator_graph() {
            check(graph, format!("generator {}", layer.layer_id), true)?;
        }
        if let Some(effects) = &layer.effects {
            for effect in effects {
                if let Some(graph) = &effect.graph {
                    check(
                        graph,
                        format!("layer {} effect {}", layer.layer_id, effect.id),
                        true,
                    )?;
                }
            }
        }
        for clip in &layer.clips {
            for effect in &clip.effects {
                if let Some(graph) = &effect.graph {
                    check(
                        graph,
                        format!("clip {} effect {}", clip.id, effect.id),
                        true,
                    )?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata_stub() -> EffectGraphDef {
        serde_json::from_value(serde_json::json!({
            "version": 0, "nodes": [], "wires": [],
            "presetMetadata": {
                "id": "", "displayName": "", "category": "", "oscPrefix": "",
                "params": [], "bindings": []
            }
        }))
        .unwrap()
    }

    #[test]
    fn legacy_instance_metadata_stub_is_allowed_but_not_executable_graphs() {
        let mut project = Project::default();
        let mut effect =
            manifold_core::effects::PresetInstance::new(manifold_core::PresetTypeId::MIRROR);
        effect.graph = Some(metadata_stub());
        project.settings.master_effects.push(effect);
        assert!(validate_project_graphs(&project).is_ok());

        let graph = project.settings.master_effects[0].graph.as_mut().unwrap();
        graph.wires.push(
            serde_json::from_value(serde_json::json!({
                "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in"
            }))
            .unwrap(),
        );
        assert!(validate_project_graphs(&project).is_err());

        project.settings.master_effects[0].graph = Some(metadata_stub());
        project.settings.master_effects[0]
            .graph
            .as_mut()
            .unwrap()
            .version = 4;
        assert!(validate_project_graphs(&project).is_err());
    }

    #[test]
    fn metadata_stub_cannot_hide_modifier_metadata_or_be_embedded() {
        let mut raw = serde_json::to_value(Project::default()).unwrap();
        raw["embeddedPresets"] = serde_json::json!([{
            "kind": "effect", "def": metadata_stub(), "origin": "Saved"
        }]);
        let project: Project = serde_json::from_value(raw).unwrap();
        assert!(validate_project_graphs(&project).is_err());

        let mut graph = metadata_stub();
        graph.preset_metadata.as_mut().unwrap().scene_modifier = Some(
            serde_json::from_value(serde_json::json!({
                "schemaVersion": 1, "singleton": false, "enabledParam": "enabled", "stages": []
            }))
            .unwrap(),
        );
        let mut project = Project::default();
        let mut effect =
            manifold_core::effects::PresetInstance::new(manifold_core::PresetTypeId::MIRROR);
        effect.graph = Some(graph);
        project.settings.master_effects.push(effect);
        assert!(validate_project_graphs(&project).is_err());
    }
}
