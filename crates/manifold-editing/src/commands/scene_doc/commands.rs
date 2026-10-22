//! The five scene document commands.

use std::collections::BTreeMap;

use manifold_core::change::ChangeSet;
use manifold_core::project::Project;
use manifold_core::scene_doc::{Item, ItemFields, ItemId, ItemKind, SceneDoc, SceneEditError, split_param_id};
use manifold_core::{GraphTarget, LayerId};

use super::{Before, apply, restore, scene_instance};
use crate::command::Command;

/// One applied document edit: what to restore, or why it was refused.
#[derive(Debug, Default)]
struct Step {
    before: Option<Before>,
    rejection: Option<String>,
}

impl Step {
    fn run(
        &mut self,
        project: &mut Project,
        layer: &LayerId,
        edit: impl FnOnce(&mut SceneDoc) -> Result<(), SceneEditError>,
    ) -> bool {
        match apply(project, layer, edit) {
            Ok(before) => {
                self.before = Some(before);
                self.rejection = None;
                true
            }
            Err(reason) => {
                self.before = None;
                self.rejection = Some(reason);
                false
            }
        }
    }

    fn undo(&mut self, project: &mut Project, layer: &LayerId) {
        if let Some(before) = self.before.take() {
            restore(project, layer, before);
        }
    }
}

/// The trait items every scene document command shares.
macro_rules! scene_command_common {
    () => {
        fn touches(&self) -> ChangeSet {
            crate::commands::WHOLE_INSTANCES
        }
        fn rejection_reason(&self) -> Option<&str> {
            self.step.rejection.as_deref()
        }
        fn was_applied(&self) -> bool {
            self.step.before.is_some()
        }
        fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
            targets.push(GraphTarget::Generator(self.layer.clone()));
        }
    };
}

/// Add an item of `kind` under `parent` at `index`, with id `id`. With a
/// `source` it duplicates that item and its subtree instead: fresh ids, the
/// copy's references pointing inside the copy, and every param value copied.
#[derive(Debug)]
pub struct AddSceneItemCommand {
    layer: LayerId,
    kind: ItemKind,
    parent: Option<ItemId>,
    index: usize,
    source: Option<ItemId>,
    id: ItemId,
    /// The item as first added, so a redo brings back the same ids.
    prepared: Option<(Item, BTreeMap<ItemId, ItemId>)>,
    step: Step,
}

impl AddSceneItemCommand {
    pub fn new(
        layer: LayerId,
        kind: ItemKind,
        parent: Option<ItemId>,
        index: usize,
        source: Option<ItemId>,
        id: ItemId,
    ) -> Self {
        Self { layer, kind, parent, index, source, id, prepared: None, step: Step::default() }
    }

    fn prepare(&self, doc: &SceneDoc) -> Result<(Item, BTreeMap<ItemId, ItemId>), SceneEditError> {
        let Some(source) = &self.source else {
            let item = Item::new(self.id.clone(), self.kind.default_name(), ItemFields::default_for(self.kind));
            return Ok((item, BTreeMap::new()));
        };
        let mut first = true;
        let root = self.id.clone();
        doc.copy_subtree(source, || {
            if std::mem::take(&mut first) { root.clone() } else { ItemId::new(manifold_core::math::short_id()) }
        })
    }
}

impl Command for AddSceneItemCommand {
    fn do_execute(&mut self, project: &mut Project) {
        if self.prepared.is_none() {
            let prepared = scene_instance(project, &self.layer)
                .map_err(|error| error.to_string())
                .and_then(|instance| self.prepare(instance.scene.as_ref().expect("scene")).map_err(|e| e.to_string()));
            match prepared {
                Ok(prepared) => self.prepared = Some(prepared),
                Err(reason) => {
                    self.step.rejection = Some(reason);
                    return;
                }
            }
        }
        let (item, map) = self.prepared.clone().expect("prepared");
        let (parent, index) = (self.parent.clone(), self.index);
        if self.step.run(project, &self.layer, |doc| doc.insert(parent.as_ref(), index, item)) {
            copy_values(project, &self.layer, &map);
        }
    }

    fn do_undo(&mut self, project: &mut Project) {
        self.step.undo(project, &self.layer);
    }

    fn description(&self) -> &str {
        if self.source.is_some() { "Duplicate Item" } else { "Add Item" }
    }

    scene_command_common!();
}

/// A duplicate starts with its source's values.
fn copy_values(project: &mut Project, layer: &LayerId, map: &BTreeMap<ItemId, ItemId>) {
    let Ok(instance) = scene_instance(project, layer) else {
        return;
    };
    let copies: Vec<(String, f32, f32)> = instance
        .params
        .iter()
        .filter_map(|p| {
            let (item, property) = split_param_id(p.id())?;
            let copy = map.get(item)?;
            Some((manifold_core::scene_doc::param_id(copy, property), p.value, p.base))
        })
        .collect();
    for (id, value, base) in copies {
        if let Some(p) = instance.params.get_mut(&id) {
            p.value = value;
            p.base = base;
        }
    }
}

/// Remove an item and its subtree. References to it fall back to their
/// defaults in the same step.
#[derive(Debug)]
pub struct RemoveSceneItemCommand {
    layer: LayerId,
    id: ItemId,
    step: Step,
}

impl RemoveSceneItemCommand {
    pub fn new(layer: LayerId, id: ItemId) -> Self {
        Self { layer, id, step: Step::default() }
    }
}

impl Command for RemoveSceneItemCommand {
    fn do_execute(&mut self, project: &mut Project) {
        let id = self.id.clone();
        self.step.run(project, &self.layer, |doc| doc.remove(&id).map(|_| ()));
    }

    fn do_undo(&mut self, project: &mut Project) {
        self.step.undo(project, &self.layer);
    }

    fn description(&self) -> &str {
        "Remove Item"
    }

    scene_command_common!();
}

/// Move an item under `parent` at `index`; also reorders.
#[derive(Debug)]
pub struct ReparentSceneItemCommand {
    layer: LayerId,
    id: ItemId,
    parent: Option<ItemId>,
    index: usize,
    step: Step,
}

impl ReparentSceneItemCommand {
    pub fn new(layer: LayerId, id: ItemId, parent: Option<ItemId>, index: usize) -> Self {
        Self { layer, id, parent, index, step: Step::default() }
    }
}

impl Command for ReparentSceneItemCommand {
    fn do_execute(&mut self, project: &mut Project) {
        let (id, parent, index) = (self.id.clone(), self.parent.clone(), self.index);
        self.step.run(project, &self.layer, |doc| doc.reparent(&id, parent.as_ref(), index));
    }

    fn do_undo(&mut self, project: &mut Project) {
        self.step.undo(project, &self.layer);
    }

    fn description(&self) -> &str {
        "Move Item"
    }

    scene_command_common!();
}

#[derive(Debug)]
pub struct RenameSceneItemCommand {
    layer: LayerId,
    id: ItemId,
    name: String,
    step: Step,
}

impl RenameSceneItemCommand {
    pub fn new(layer: LayerId, id: ItemId, name: impl Into<String>) -> Self {
        Self { layer, id, name: name.into(), step: Step::default() }
    }
}

impl Command for RenameSceneItemCommand {
    fn do_execute(&mut self, project: &mut Project) {
        let (id, name) = (self.id.clone(), self.name.clone());
        self.step.run(project, &self.layer, |doc| {
            doc.find_mut(&id).ok_or_else(|| SceneEditError::UnknownItem(id.clone()))?.name = name;
            Ok(())
        });
    }

    fn do_undo(&mut self, project: &mut Project) {
        self.step.undo(project, &self.layer);
    }

    fn description(&self) -> &str {
        "Rename Item"
    }

    scene_command_common!();
}

/// Set an item's structural fields: a structural edit, which recompiles.
/// The fields replace the item's whole set and keep its kind.
#[derive(Debug)]
pub struct SetSceneItemFieldsCommand {
    layer: LayerId,
    id: ItemId,
    fields: ItemFields,
    step: Step,
}

impl SetSceneItemFieldsCommand {
    pub fn new(layer: LayerId, id: ItemId, fields: ItemFields) -> Self {
        Self { layer, id, fields, step: Step::default() }
    }
}

impl Command for SetSceneItemFieldsCommand {
    fn do_execute(&mut self, project: &mut Project) {
        let (id, fields) = (self.id.clone(), self.fields.clone());
        self.step.run(project, &self.layer, |doc| {
            let item = doc.find_mut(&id).ok_or_else(|| SceneEditError::UnknownItem(id.clone()))?;
            if item.kind() != fields.kind() {
                return Err(SceneEditError::KindChange { from: item.kind(), to: fields.kind() });
            }
            item.fields = fields;
            Ok(())
        });
    }

    fn do_undo(&mut self, project: &mut Project) {
        self.step.undo(project, &self.layer);
    }

    fn description(&self) -> &str {
        "Set Item"
    }

    scene_command_common!();
}
