//! Read-only choices for both trigger assignment entry points.

use manifold_core::params::ClipTriggerSource;
use manifold_core::project::Project;
use manifold_core::effects::ParamId;
use manifold_core::{GraphTarget, LayerId};
use manifold_ui::param_surface::{ClipTriggerRow, ParamSurface};
use manifold_ui::view::{UiClipTriggerSource, UiGraphTarget};

#[derive(Default)]
pub(crate) struct TriggerRoutingCatalog {
    pub sources: Vec<TriggerSourceChoice>,
    pub targets: Vec<TriggerTargetChoice>,
}

pub(crate) struct TriggerSourceChoice {
    pub id: LayerId,
    pub label: String,
}

pub(crate) struct TriggerTargetChoice {
    pub owner: LayerId,
    pub target: UiGraphTarget,
    pub param_id: ParamId,
    pub label: String,
    pub group_label: String,
    pub source: UiClipTriggerSource,
    pub has_main_source: bool,
    pub eligible_sources: Vec<LayerId>,
}

pub(crate) fn to_ui_source(source: &ClipTriggerSource) -> UiClipTriggerSource {
    match source {
        ClipTriggerSource::OwnLayer => UiClipTriggerSource::Main,
        ClipTriggerSource::Disabled => UiClipTriggerSource::Disabled,
        ClipTriggerSource::Lane { layer_id } => UiClipTriggerSource::Lane(layer_id.clone()),
    }
}

pub(crate) fn to_core_source(source: &UiClipTriggerSource) -> ClipTriggerSource {
    match source {
        UiClipTriggerSource::Main => ClipTriggerSource::OwnLayer,
        UiClipTriggerSource::Disabled => ClipTriggerSource::Disabled,
        UiClipTriggerSource::Lane(layer_id) => ClipTriggerSource::Lane { layer_id: layer_id.clone() },
    }
}

/// Resolve navigation against the same curated generator surface the inspector
/// builds. Scene-owned properties and forces open in Scene Setup instead.
pub(crate) fn response_uses_scene_panel(project: &Project, target: &GraphTarget, param_id: &str) -> bool {
    let GraphTarget::Generator(layer_id) = target else { return false; };
    let Some(instance) = project.graph_target_owner(target) else { return false; };
    let bundled = instance.graph.is_none()
        .then(|| manifold_nodes::bundled_presets::bundled_preset_def(instance.generator_type())).flatten();
    let Some(graph) = instance.graph.as_ref().or(bundled.as_deref()) else { return false; };
    if manifold_nodes_scene::node_graph::scene_vm::SceneVm::from_def(graph).is_none() {
        return false;
    }
    if let Some(modifier_id) = graph.preset_metadata.as_ref().and_then(|metadata| {
        metadata.bindings.iter().find_map(|binding| match &binding.target {
            manifold_core::effect_graph_def::BindingTarget::SceneModifier { modifier_id, .. }
                if binding.id == param_id => Some(modifier_id),
            _ => None,
        })
    }) {
        return graph.scene_modifiers.iter().any(|modifier| modifier.id == *modifier_id
            && manifold_core::scene_modifier_preset::is_force_recipe(&modifier.graph));
    }
    let surface = super::cards::gen_params_to_surface(instance, layer_id.as_str(), None, &[],
        super::cards::SurfaceVisibility::CuratedCard, (project.settings.bpm, project.settings.frame_rate));
    !surface.rows.iter().any(|row| row.id == param_id)
}

/// Scene modifier rows retain the host generator address. Binding metadata is
/// still enough to give those shared host parameters their friendly card label.
fn modifier_label_for_param(
    instance: &manifold_core::effects::PresetInstance,
    param_id: &str,
) -> Option<String> {
    let graph = instance.graph.as_ref()?;
    let bindings = graph.preset_metadata.as_ref()?.bindings.as_slice();
    let modifier_id = bindings.iter().find_map(|binding| {
        if binding.id != param_id {
            return None;
        }
        match &binding.target {
            manifold_core::effect_graph_def::BindingTarget::SceneModifier { modifier_id, .. } => Some(modifier_id),
            _ => None,
        }
    })?;
    graph
        .scene_modifiers
        .iter()
        .find(|modifier| modifier.id == *modifier_id)
        .and_then(|modifier| modifier.graph.preset_metadata.as_ref())
        .map(|metadata| metadata.display_name.clone())
}

fn source_label(project: &Project, target: &GraphTarget, param_id: &str, source: &ClipTriggerSource) -> String {
    match source {
        ClipTriggerSource::OwnLayer if project
            .clip_trigger_target_layer(target)
            .is_some_and(|owner| owner.is_group()) => "No clip source".into(),
        ClipTriggerSource::OwnLayer => "Main lane".into(),
        ClipTriggerSource::Disabled => "No source".into(),
        source @ ClipTriggerSource::Lane { layer_id } => match project.timeline.find_layer_by_id(layer_id) {
            Some((_, lane)) if project.can_assign_clip_trigger_source(target, param_id, source) => lane.name.clone(),
            Some((_, lane)) => format!("Unavailable: {}", lane.name),
            None => "Missing source".into(),
        },
    }
}

impl TriggerRoutingCatalog {
    pub(crate) fn project(project: &Project) -> Self {
        let mut catalog = Self::default();
        for layer in &project.timeline.layers {
            if layer.is_trigger() {
                let parent = layer.parent_layer_id.as_ref()
                    .and_then(|id| project.timeline.find_layer_by_id(id))
                    .map(|(_, parent)| parent.name.as_str()).unwrap_or("Missing owner");
                catalog.sources.push(TriggerSourceChoice {
                    id: layer.layer_id.clone(), label: format!("{parent} / {}", layer.name),
                });
                continue;
            }
            for instance in layer.effects.iter().flatten() {
                catalog.add_instance(project, layer, instance, GraphTarget::Effect(instance.id.clone()));
            }
            if let Some(instance) = layer.gen_params() {
                catalog.add_instance(project, layer, instance, GraphTarget::Generator(layer.layer_id.clone()));
            }
        }
        catalog
    }

    fn add_instance(
        &mut self,
        project: &Project,
        layer: &manifold_core::layer::Layer,
        instance: &manifold_core::effects::PresetInstance,
        target: GraphTarget,
    ) {
        let name = manifold_core::preset_type_registry::display_name(instance.effect_type());
        for param in instance.params.iter() {
            let group_label = modifier_label_for_param(instance, param.id())
                .map(|modifier| format!("{} / {modifier}", layer.name))
                .unwrap_or_else(|| format!("{} / {name}", layer.name));
            self.add_param(project, layer, &target, param, group_label);
        }
    }

    fn add_param(
        &mut self,
        project: &Project,
        layer: &manifold_core::layer::Layer,
        target: &GraphTarget,
        param: &manifold_core::params::Param,
        group_label: String,
    ) {
        if param.spec.is_trigger_gate
            || !project.can_assign_clip_trigger_source(target, param.id(), &ClipTriggerSource::Disabled)
            || crate::scene_modifier_edit::macro_parameter_lock_reason(project, target, param.id()).is_some()
        {
            return;
        }
        self.targets.push(TriggerTargetChoice {
            owner: layer.layer_id.clone(),
            target: crate::editing_host::to_ui_graph_target(target),
            param_id: param.id().to_owned().into(),
            label: format!("{group_label} / {}", param.spec.name),
            group_label,
            source: to_ui_source(&param.clip_trigger_source),
            has_main_source: project
                .clip_trigger_target_layer(target)
                .is_some_and(|owner| !owner.is_group()),
            eligible_sources: project.clip_trigger_source_options(target),
        });
    }

    pub(crate) fn targets_label(&self, source: &LayerId) -> String {
        let mut targets = self.targets.iter().filter(|target| {
            target.source == UiClipTriggerSource::Lane(source.clone())
                && target.eligible_sources.contains(source)
        });
        let Some(first) = targets.next() else { return "Assign…".into(); };
        let others = targets.count();
        if others == 0 { first.label.clone() } else { format!("{} +{others}", first.label) }
    }
}

/// Extend the existing card source projection. All card hosts use this same
/// function; modifier rows keep their manifest parameter identity.
pub(crate) fn attach_trigger_sources(configs: &mut [ParamSurface], project: &Project) {
    for config in configs {
        let target = if let Some(modifier) = &config.modifier {
            GraphTarget::Generator(modifier.layer_id.clone())
        } else if let Some(layer) = &config.layer_id {
            GraphTarget::Generator(layer.clone())
        } else {
            GraphTarget::Effect(config.effect_id.clone())
        };
        let Some(instance) = project.graph_target_owner(&target) else { continue; };
        for row in &mut config.rows {
            row.clip_trigger = None;
            let Some(param) = instance.params.get(row.id.as_ref()) else { continue; };
            if param.spec.is_trigger_gate
                || !project.can_assign_clip_trigger_source(&target, param.id(), &ClipTriggerSource::Disabled)
                || crate::scene_modifier_edit::macro_parameter_lock_reason(project, &target, param.id()).is_some()
                || row.spec.disabled.is_some()
            { continue; }
            let source_label = source_label(project, &target, param.id(), &param.clip_trigger_source);
            row.clip_trigger = Some(ClipTriggerRow {
                target: crate::editing_host::to_ui_graph_target(&target), source_label,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::ParamSpecDef;
    use manifold_core::effects::PresetInstance;
    use manifold_core::layer::Layer;
    use manifold_core::params::{Param, ParamManifest};
    use manifold_core::types::LayerType;
    use manifold_core::{EffectId, PresetTypeId};

    fn effect_layer(id: &str, kind: LayerType, parent: Option<&str>) -> Layer {
        let mut layer = Layer::new(id.into(), kind, 0);
        layer.layer_id = LayerId::new(id);
        layer.parent_layer_id = parent.map(LayerId::new);
        let mut effect = PresetInstance::new(PresetTypeId::BLOOM);
        effect.id = EffectId::new(format!("{id}-effect"));
        effect.params = ParamManifest::from_params(vec![Param::user_added(ParamSpecDef {
            id: "amount".into(),
            name: "Amount".into(),
            ..Default::default()
        })]);
        layer.effects = Some(vec![effect]);
        layer
    }

    fn trigger_layer(id: &str, parent: &str) -> Layer {
        let mut layer = Layer::new_trigger(id.into(), LayerId::new(parent), 0);
        layer.layer_id = LayerId::new(id);
        layer
    }

    #[test]
    fn catalog_has_one_host_entry_and_scoped_sources() {
        let mut project = Project::default();
        project.timeline.layers = vec![
            effect_layer("group", LayerType::Group, None),
            trigger_layer("group-lane", "group"),
            effect_layer("owner", LayerType::Video, Some("group")),
            trigger_layer("owner-lane", "owner"),
            trigger_layer("unrelated", "missing"),
        ];
        let catalog = TriggerRoutingCatalog::project(&project);
        let target = catalog.targets.iter().find(|target| target.owner == LayerId::new("owner")).unwrap();
        assert_eq!(catalog.targets.iter().filter(|entry| entry.owner == LayerId::new("owner")).count(), 1);
        assert_eq!(target.eligible_sources, vec![LayerId::new("group-lane"), LayerId::new("owner-lane")]);
        assert!(target.has_main_source);
        assert_eq!(catalog.targets_label(&LayerId::new("owner-lane")), "Assign…");
    }

    #[test]
    fn group_main_source_and_missing_lane_are_explicit() {
        let mut project = Project::default();
        project.timeline.layers = vec![effect_layer("group", LayerType::Group, None)];
        let target = GraphTarget::Effect(EffectId::new("group-effect"));
        let catalog = TriggerRoutingCatalog::project(&project);
        let choice = catalog.targets.first().unwrap();
        assert!(!choice.has_main_source);
        assert_eq!(source_label(&project, &target, "amount", &ClipTriggerSource::OwnLayer), "No clip source");
        assert_eq!(source_label(&project, &target, "amount", &ClipTriggerSource::Lane { layer_id: LayerId::new("gone") }), "Missing source");
    }

    #[test]
    fn assigned_target_label_and_drawer_source_share_scope() {
        let mut project = Project::default();
        let mut owner = effect_layer("owner", LayerType::Video, None);
        owner.effects.as_mut().unwrap()[0].params.get_mut("amount").unwrap().clip_trigger_source =
            ClipTriggerSource::Lane { layer_id: LayerId::new("lane") };
        project.timeline.layers = vec![owner, trigger_layer("lane", "owner")];
        let catalog = TriggerRoutingCatalog::project(&project);
        let target = catalog.targets.first().unwrap();
        assert_eq!(target.source, UiClipTriggerSource::Lane(LayerId::new("lane")));
        assert_eq!(catalog.targets_label(&LayerId::new("lane")), target.label);
        assert_eq!(source_label(&project, &GraphTarget::Effect(EffectId::new("owner-effect")), "amount", &ClipTriggerSource::Lane { layer_id: LayerId::new("lane") }), "lane");
    }

    #[test]
    fn generator_projection_has_source_identity_before_inspector_configuration() {
        let mut project = Project::default();
        let owner = Layer::new_generator("Scene".into(), PresetTypeId::PLASMA, 0);
        let owner_id = owner.layer_id.clone();
        project.timeline.layers.push(owner);
        let instance = project.timeline.layers[0].gen_params().unwrap();
        let mut surface = super::super::cards::gen_params_to_surface(
            instance, owner_id.as_str(), None, &[],
            super::super::cards::SurfaceVisibility::CuratedCard,
            (manifold_core::Bpm(120.0), 0.0),
        );
        assert_eq!(surface.layer_id.as_ref(), Some(&owner_id));
        attach_trigger_sources(std::slice::from_mut(&mut surface), &project);
        let sources: Vec<_> = surface.rows.iter().filter_map(|row| row.clip_trigger.as_ref()).collect();
        assert!(!sources.is_empty(), "the generator card exposes compatible numeric parameters");
        assert!(sources.iter().all(|source| source.target == UiGraphTarget::Generator(owner_id.clone())));
    }
}
