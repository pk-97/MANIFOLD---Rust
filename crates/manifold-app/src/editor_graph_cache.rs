//! The graph editor's MODIFIED badge and a gizmo host's scene, keyed on their
//! inputs (docs/UI_INVALIDATION_DESIGN.md section 4): the target, the
//! catalog default the badge compares against, the preset catalog generation,
//! and the project parts that can change the graph. Each gizmo host (the
//! editor viewport, the main preview) holds one; nothing here is rebuilt on a
//! frame whose inputs did not move.

use std::sync::Arc;

use manifold_core::GraphTarget;
use manifold_core::change::{ChangeSet, Part, PartStamps};
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::project::Project;
use manifold_nodes_scene::node_graph::scene_vm::SceneVm;

/// Parts whose writes can change the watched graph or which graph resolves.
/// Graph edits declare Overrides; Values is left out so a slider drag does not
/// re-diff the graph.
const GRAPH_PARTS: ChangeSet = ChangeSet::of(&[Part::Layers, Part::Effects, Part::Overrides, Part::Shapes, Part::EmbeddedPresets]);

#[derive(Default)]
pub(crate) struct EditorGraphCache {
    catalog_default_version: u64,
    badge: Option<(GraphKey, bool)>,
    viewport: Option<(GraphKey, ViewportDef)>,
}

/// (watched target, catalog default version, catalog generation, parts key)
type GraphKey = (Option<GraphTarget>, u64, u64, u64);

/// The viewport's graph: whether the target's graph resolves, and the scene
/// its authored projection shows (`None` when the projection fails or the
/// graph holds no scene).
#[derive(Clone, Default)]
pub(crate) struct ViewportDef {
    pub(crate) resolved: bool,
    pub(crate) scene: Option<Arc<SceneVm>>,
}

impl EditorGraphCache {
    /// Every write of `Application::watched_catalog_default` calls this.
    pub(crate) fn catalog_default_changed(&mut self) {
        self.catalog_default_version += 1;
    }

    fn key(&self, target: Option<&GraphTarget>, parts: &PartStamps, set: ChangeSet) -> GraphKey {
        (
            target.cloned(),
            self.catalog_default_version,
            manifold_node_engine::load::preset_loader::catalog_generation(),
            parts.key(set),
        )
    }

    /// Whether the watched graph diverges from its catalog default beyond
    /// layout. Moving a node materialises the override, so `graph.is_some()`
    /// alone is not a real edit.
    pub(crate) fn has_graph_mod(
        &mut self,
        project: &Project,
        target: Option<&GraphTarget>,
        catalog_default: Option<&EffectGraphDef>,
        parts: &PartStamps,
    ) -> bool {
        let key = self.key(target, parts, GRAPH_PARTS);
        if let Some((cached, has_mod)) = &self.badge
            && *cached == key
        {
            return *has_mod;
        }
        let has_mod = target.is_some_and(|target| {
            let instance_graph = match target {
                GraphTarget::Effect(eid) => project.find_effect_by_id(eid).and_then(|fx| fx.graph.as_ref()),
                GraphTarget::Generator(lid) => {
                    project.timeline.find_layer_by_id(lid).and_then(|(_, l)| l.generator_graph())
                }
                GraphTarget::SceneModifier { .. } => project
                    .graph_target_owner(target)
                    .and_then(|owner| owner.graph.as_ref())
                    .and_then(|graph| target.graph_in(graph)),
            };
            match (instance_graph, catalog_default.and_then(|base| target.graph_in(base))) {
                (Some(g), Some(base)) => g.diverges_ignoring_layout(base),
                // No catalog base to compare against: can't prove the
                // override is layout-only.
                (Some(_), None) => true,
                (None, _) => false,
            }
        });
        self.badge = Some((key, has_mod));
        has_mod
    }

    /// The scene a gizmo host shows. It reads the owner's base values, so it
    /// keys on Values too.
    pub(crate) fn viewport_def(&mut self, project: &Project, target: Option<&GraphTarget>, parts: &PartStamps) -> ViewportDef {
        let key = self.key(target, parts, GRAPH_PARTS.union(ChangeSet::of(&[Part::Values])));
        if let Some((cached, def)) = &self.viewport
            && *cached == key
        {
            return def.clone();
        }
        let def = target.map_or_else(ViewportDef::default, |target| {
            let scene = crate::scene_gizmo::scene_vm_of(project, target).map(Arc::new);
            ViewportDef { resolved: scene.is_some() || crate::graph_target::resolve(project, target).is_some(), scene }
        });
        self.viewport = Some((key, def.clone()));
        def
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effects::PresetInstance;

    fn project_with_override() -> (Project, GraphTarget) {
        let preset = manifold_core::PresetTypeId::from_string("Bloom".to_string());
        let mut fx = PresetInstance::new(preset.clone());
        fx.init_defaults();
        fx.graph = manifold_nodes::bundled_presets::bundled_preset_def(&preset).map(|def| (*def).clone());
        assert!(fx.graph.is_some(), "Bloom is a bundled preset");
        let target = GraphTarget::Effect(fx.id.clone());
        let mut project = Project::default();
        project.settings.master_effects.push(fx);
        (project, target)
    }

    fn bump(parts: &mut PartStamps, part: Part) {
        parts.bump(ChangeSet::of(&[part]));
    }

    #[test]
    fn badge_recomputes_only_when_a_graph_part_moves() {
        let (mut project, target) = project_with_override();
        let mut parts = PartStamps::default();
        let mut cache = EditorGraphCache::default();
        assert!(cache.has_graph_mod(&project, Some(&target), None, &parts));

        project.settings.master_effects[0].graph = None;
        bump(&mut parts, Part::Values);
        assert!(
            cache.has_graph_mod(&project, Some(&target), None, &parts),
            "a Values-only change does not re-diff the graph"
        );
        bump(&mut parts, Part::Overrides);
        assert!(!cache.has_graph_mod(&project, Some(&target), None, &parts));
    }

    #[test]
    fn badge_recomputes_when_the_catalog_default_changes() {
        let (project, target) = project_with_override();
        let parts = PartStamps::default();
        let mut cache = EditorGraphCache::default();
        assert!(cache.has_graph_mod(&project, Some(&target), None, &parts));
        let base = project.settings.master_effects[0].graph.clone();
        cache.catalog_default_changed();
        assert!(!cache.has_graph_mod(&project, Some(&target), base.as_ref(), &parts));
    }

    #[test]
    fn viewport_scene_reuses_until_its_inputs_move() {
        let mut project = Project::default();
        let index = project.timeline.add_layer("Scene", manifold_core::types::LayerType::Generator, manifold_core::PresetTypeId::SCENE);
        let target = GraphTarget::Generator(project.timeline.layers[index].layer_id.clone());
        let mut parts = PartStamps::default();
        let mut cache = EditorGraphCache::default();
        let first = cache.viewport_def(&project, Some(&target), &parts);
        assert!(first.resolved);
        let first = first.scene.expect("a scene layer projects its scene");

        bump(&mut parts, Part::Clips);
        let same = cache.viewport_def(&project, Some(&target), &parts).scene.unwrap();
        assert!(Arc::ptr_eq(&first, &same), "an unrelated part reuses the scene");

        bump(&mut parts, Part::Values);
        let fresh = cache.viewport_def(&project, Some(&target), &parts).scene.unwrap();
        assert!(!Arc::ptr_eq(&first, &fresh), "base values feed the authored projection");

        let none = cache.viewport_def(&project, None, &parts);
        assert!(!none.resolved && none.scene.is_none());
    }
}
