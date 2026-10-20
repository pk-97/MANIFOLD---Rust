//! The one undoable scene insertion: every Add menu entry that places a group
//! in a render scene goes through [`InsertSceneTemplateCommand`].

use manifold_core::GraphTarget;
use manifold_core::change::ChangeSet;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::project::Project;
use manifold_core::scene_graph_edit::SceneGraphEditError;
use manifold_core::scene_modifier_preset::SceneNodeRef;
use manifold_core::scene_template::{SceneTemplate, insert_scene_template};

use crate::command::Command;
use crate::scene_transaction::{SceneGraphRejection, SceneGraphTransaction};

/// Insert one prepared template into a render scene. The candidate graph is
/// built when the command is made, so a refusal surfaces before submission
/// and redo reuses the same ids.
#[derive(Debug)]
pub struct InsertSceneTemplateCommand {
    transaction: SceneGraphTransaction,
}

impl InsertSceneTemplateCommand {
    pub fn new(
        project: &Project,
        target: GraphTarget,
        scene: SceneNodeRef,
        template: SceneTemplate,
        catalog_default: &EffectGraphDef,
    ) -> Result<Self, SceneGraphEditError> {
        Ok(Self {
            transaction: SceneGraphTransaction::prepare(project, target, catalog_default, "Add to Scene", |graph| {
                insert_scene_template(graph, &scene, &template)
            })?,
        })
    }

    pub fn prepared_graph(&self) -> &EffectGraphDef {
        self.transaction.prepared_graph()
    }
}

impl Command for InsertSceneTemplateCommand {
    fn touches(&self) -> ChangeSet {
        self.transaction.touches()
    }
    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(self.transaction.owner.clone());
    }
    fn do_execute(&mut self, project: &mut Project) {
        self.transaction.execute(project);
    }
    fn do_undo(&mut self, project: &mut Project) {
        self.transaction.undo(project);
    }
    fn description(&self) -> &str {
        self.transaction.description
    }
    fn was_applied(&self) -> bool {
        self.transaction.was_applied()
    }
    fn rejection_reason(&self) -> Option<&str> {
        match self.transaction.error()? {
            SceneGraphRejection::MissingOwner => Some("Scene owner is no longer present"),
            SceneGraphRejection::PresetTypeChanged => Some("Scene type changed while preparing this edit"),
            SceneGraphRejection::StaleOwner => Some("Scene graph changed while preparing this edit"),
            SceneGraphRejection::Noop => None,
        }
    }
}
