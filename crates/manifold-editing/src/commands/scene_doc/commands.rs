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
        edit: impl FnOnce(&mut SceneDoc, &dyn Fn(&str) -> Option<f32>) -> Result<(), SceneEditError>,
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

/// One scene command's edit of the document, apart from the compile. Alone,
/// a command edits, compiles and installs; a composite of commands on one
/// layer edits the document with each in turn, then compiles and installs
/// once (`CompositeCommand`), so grouping N items or swapping a 300-part
/// file is one compile.
pub trait SceneEdit {
    /// The scene layer the edit writes.
    fn layer(&self) -> &LayerId;
    /// Edit the document only. `values` reads the layer's params as they
    /// were before the edit.
    fn edit_doc(&mut self, doc: &mut SceneDoc, values: &dyn Fn(&str) -> Option<f32>) -> Result<(), SceneEditError>;
    /// Once the edited document is installed: write what needs its params.
    /// Returns each written param's value and base before, for undo.
    fn after_install(&mut self, _project: &mut Project) -> Vec<(String, f32, f32)> {
        Vec::new()
    }
}

/// The trait items every scene document command shares.
macro_rules! scene_command_common {
    () => {
        fn do_execute(&mut self, project: &mut Project) {
            let layer = self.layer.clone();
            let mut step = std::mem::take(&mut self.step);
            let applied = step.run(project, &layer, |doc, values| SceneEdit::edit_doc(self, doc, values));
            self.step = step;
            if applied {
                let written = SceneEdit::after_install(self, project);
                if let Some(before) = self.step.before.as_mut() {
                    before.note_written(written);
                }
            }
        }
        fn do_undo(&mut self, project: &mut Project) {
            self.step.undo(project, &self.layer);
        }
        fn scene_edit(&mut self) -> Option<&mut dyn SceneEdit> {
            Some(self)
        }
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
    /// Param values written once the item is in: `(param id, value, base)`.
    /// A paste carries its copied items' values this way.
    values: Vec<(String, f32, f32)>,
    step: Step,
}

impl AddSceneItemCommand {
    /// Add `item` and everything under it, as built. An import is added this
    /// way: its parts are keyed by file node path, so their ids are chosen,
    /// not made fresh.
    pub fn with_subtree(layer: LayerId, parent: Option<ItemId>, index: usize, item: Item) -> Self {
        let kind = item.kind();
        let id = item.id.clone();
        Self { layer, kind, parent, index, source: None, id, prepared: Some((item, BTreeMap::new())), values: Vec::new(), step: Step::default() }
    }

    /// Also write these param values `(param id, value, base)` once the item
    /// is in, in the same undo step.
    pub fn with_values(mut self, values: Vec<(String, f32, f32)>) -> Self {
        self.values = values;
        self
    }

    pub fn new(
        layer: LayerId,
        kind: ItemKind,
        parent: Option<ItemId>,
        index: usize,
        source: Option<ItemId>,
        id: ItemId,
    ) -> Self {
        Self { layer, kind, parent, index, source, id, prepared: None, values: Vec::new(), step: Step::default() }
    }

    fn prepare(&self, doc: &SceneDoc) -> Result<(Item, BTreeMap<ItemId, ItemId>), SceneEditError> {
        let Some(source) = &self.source else {
            let item = Item::new(self.id.clone(), self.kind.default_name(), ItemFields::default_for(self.kind));
            return Ok((item, BTreeMap::new()));
        };
        let root = self.id.clone();
        doc.copy_subtree(source, |old| {
            if old == source {
                root.clone()
            } else if let Some(rest) = old.as_str().strip_prefix(source.as_str()).and_then(|r| r.strip_prefix('-'))
            {
                // An import's parts stay keyed by file node path under the copy's id.
                ItemId::new(format!("{root}-{rest}"))
            } else {
                ItemId::new(manifold_core::math::short_id())
            }
        })
    }
}

impl SceneEdit for AddSceneItemCommand {
    fn layer(&self) -> &LayerId {
        &self.layer
    }

    fn edit_doc(&mut self, doc: &mut SceneDoc, _: &dyn Fn(&str) -> Option<f32>) -> Result<(), SceneEditError> {
        if self.prepared.is_none() {
            self.prepared = Some(self.prepare(doc)?);
        }
        let (item, _) = self.prepared.clone().expect("prepared");
        doc.insert(self.parent.as_ref(), self.index, item)
    }

    // The new item's params leave with it on undo, so nothing is noted.
    fn after_install(&mut self, project: &mut Project) -> Vec<(String, f32, f32)> {
        if let Some((_, map)) = &self.prepared {
            copy_values(project, &self.layer, map);
        }
        write_values(project, &self.layer, &self.values);
        Vec::new()
    }
}

impl Command for AddSceneItemCommand {
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

/// Write `values` `(param id, value, base)` into the layer's params that hold them.
fn write_values(project: &mut Project, layer: &LayerId, values: &[(String, f32, f32)]) {
    let Ok(instance) = scene_instance(project, layer) else {
        return;
    };
    for (id, value, base) in values {
        if let Some(p) = instance.params.get_mut(id) {
            p.value = *value;
            p.base = *base;
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

impl SceneEdit for RemoveSceneItemCommand {
    fn layer(&self) -> &LayerId {
        &self.layer
    }

    fn edit_doc(&mut self, doc: &mut SceneDoc, _: &dyn Fn(&str) -> Option<f32>) -> Result<(), SceneEditError> {
        doc.remove(&self.id).map(|_| ())
    }
}

impl Command for RemoveSceneItemCommand {

    fn description(&self) -> &str {
        "Remove Item"
    }

    scene_command_common!();
}

/// Move an item under `parent` at `index`; also reorders. The item keeps
/// its place on screen: its transform rows are rewritten into the new
/// parent's frame (Blender's Keep Transform), in the same undo step.
#[derive(Debug)]
pub struct ReparentSceneItemCommand {
    layer: LayerId,
    id: ItemId,
    parent: Option<ItemId>,
    index: usize,
    /// The transform writes that keep the item in place, once installed.
    keep: Vec<(String, f32)>,
    step: Step,
}

impl ReparentSceneItemCommand {
    pub fn new(layer: LayerId, id: ItemId, parent: Option<ItemId>, index: usize) -> Self {
        Self { layer, id, parent, index, keep: Vec::new(), step: Step::default() }
    }
}

impl SceneEdit for ReparentSceneItemCommand {
    fn layer(&self) -> &LayerId {
        &self.layer
    }

    fn edit_doc(&mut self, doc: &mut SceneDoc, values: &dyn Fn(&str) -> Option<f32>) -> Result<(), SceneEditError> {
        use manifold_core::scene_doc::transform::{frame_of, keep_world};
        let (old_parent, _) = doc.locate(&self.id).ok_or_else(|| SceneEditError::UnknownItem(self.id.clone()))?;
        let from = frame_of(doc, old_parent.as_ref(), values);
        doc.reparent(&self.id, self.parent.as_ref(), self.index)?;
        let to = frame_of(doc, self.parent.as_ref(), values);
        self.keep = doc.find(&self.id).map(|item| keep_world(item, from, to, values)).unwrap_or_default();
        Ok(())
    }

    fn after_install(&mut self, project: &mut Project) -> Vec<(String, f32, f32)> {
        let Ok(instance) = scene_instance(project, &self.layer) else {
            return Vec::new();
        };
        let mut before = Vec::new();
        for (id, value) in &self.keep {
            if let Some(param) = instance.params.get_mut(id) {
                before.push((id.clone(), param.value, param.base));
                param.value = *value;
                param.base = *value;
            }
        }
        before
    }
}

impl Command for ReparentSceneItemCommand {

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

impl SceneEdit for RenameSceneItemCommand {
    fn layer(&self) -> &LayerId {
        &self.layer
    }

    fn edit_doc(&mut self, doc: &mut SceneDoc, _: &dyn Fn(&str) -> Option<f32>) -> Result<(), SceneEditError> {
        doc.find_mut(&self.id).ok_or_else(|| SceneEditError::UnknownItem(self.id.clone()))?.name = self.name.clone();
        Ok(())
    }
}

impl Command for RenameSceneItemCommand {

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

impl SceneEdit for SetSceneItemFieldsCommand {
    fn layer(&self) -> &LayerId {
        &self.layer
    }

    fn edit_doc(&mut self, doc: &mut SceneDoc, _: &dyn Fn(&str) -> Option<f32>) -> Result<(), SceneEditError> {
        let item = doc.find_mut(&self.id).ok_or_else(|| SceneEditError::UnknownItem(self.id.clone()))?;
        if item.kind() != self.fields.kind() {
            return Err(SceneEditError::KindChange { from: item.kind(), to: self.fields.kind() });
        }
        item.fields = self.fields.clone();
        match doc.second_fill(&self.id) {
            Some(reason) => Err(SceneEditError::SecondOfType(reason)),
            None => Ok(()),
        }
    }
}

impl Command for SetSceneItemFieldsCommand {

    fn description(&self) -> &str {
        "Set Item"
    }

    scene_command_common!();
}
