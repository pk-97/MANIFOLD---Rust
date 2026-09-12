//! Graph schema checks before project catalog registration or file publication.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::project::Project;
use manifold_core::scene_modifier_preset::{
    SceneModifierSchemaError, validate_scene_modifier_schema,
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
    fn check(graph: &EffectGraphDef, owner: String) -> Result<(), ProjectGraphSchemaError> {
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
        check(&preset.def, format!("embedded preset {index}"))?;
    }
    for effect in &project.settings.master_effects {
        if let Some(graph) = &effect.graph {
            check(graph, format!("master effect {}", effect.id))?;
        }
    }
    for layer in &project.timeline.layers {
        if let Some(graph) = layer.generator_graph() {
            check(graph, format!("generator {}", layer.layer_id))?;
        }
        if let Some(effects) = &layer.effects {
            for effect in effects {
                if let Some(graph) = &effect.graph {
                    check(
                        graph,
                        format!("layer {} effect {}", layer.layer_id, effect.id),
                    )?;
                }
            }
        }
        for clip in &layer.clips {
            for effect in &clip.effects {
                if let Some(graph) = &effect.graph {
                    check(graph, format!("clip {} effect {}", clip.id, effect.id))?;
                }
            }
        }
    }
    Ok(())
}
