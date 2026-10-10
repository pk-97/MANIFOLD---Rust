use crate::command::Command;
use manifold_core::PresetTypeId;
use manifold_core::LayerId;
use manifold_core::LayerClipTrigger;
use manifold_core::layer::Layer;
use manifold_core::project::{EmbeddedPreset, Project};
use manifold_core::session::SessionSlot;
use manifold_core::types::LayerType;
use std::collections::{HashMap, HashSet};

const TRIGGER_PARENT_REQUIRED: &str = "trigger layers require a parent";
const LAYER_PARENT_MISSING: &str = "layer parent does not exist";
const LAYER_PARENT_INCOMPATIBLE: &str = "layer parent cannot own this child type";
const LAYER_PARENT_SELF: &str = "layer cannot parent itself";
const LAYER_ID_DUPLICATE: &str = "layer id already exists";
const LAYER_REORDER_MEMBERSHIP: &str = "layer reorder changes layer membership";
const LAYER_PARENT_CYCLE: &str = "layer parent hierarchy contains a cycle";
const GROUP_SELECTION_EMPTY: &str = "group selection has no non-trigger roots";
const GROUP_SELECTION_MISSING: &str = "group selection contains a missing layer";

fn validate_parent_in_layers(layers: &[Layer], layer: &Layer) -> Result<(), &'static str> {
    let Some(parent_id) = layer.parent_layer_id.as_ref() else {
        return if layer.is_trigger() { Err(TRIGGER_PARENT_REQUIRED) } else { Ok(()) };
    };
    if parent_id == &layer.layer_id {
        return Err(LAYER_PARENT_SELF);
    }
    let Some(parent) = layers.iter().find(|candidate| &candidate.layer_id == parent_id) else {
        return Err(LAYER_PARENT_MISSING);
    };
    if !parent.layer_type.accepts_child(layer.layer_type) {
        return Err(LAYER_PARENT_INCOMPATIBLE);
    }
    let mut ancestor = Some(parent_id);
    for _ in 0..layers.len() {
        let Some(id) = ancestor else { return Ok(()); };
        if id == &layer.layer_id {
            return Err(LAYER_PARENT_CYCLE);
        }
        ancestor = layers.iter().find(|candidate| &candidate.layer_id == id)
            .and_then(|candidate| candidate.parent_layer_id.as_ref());
    }
    if ancestor.is_some() { Err(LAYER_PARENT_CYCLE) } else { Ok(()) }
}

/// Add a new layer to the timeline.
#[derive(Debug)]
pub struct AddLayerCommand {
    layer: Option<Layer>,
    name: String,
    layer_type: LayerType,
    gen_type: PresetTypeId,
    insert_index: usize,
    parent_group_id: Option<LayerId>,
    applied: bool,
    rejection: Option<&'static str>,
}

impl AddLayerCommand {
    pub fn new(
        name: String,
        layer_type: LayerType,
        gen_type: PresetTypeId,
        insert_index: usize,
        parent_group_id: Option<LayerId>,
    ) -> Self {
        Self {
            layer: None,
            name,
            layer_type,
            gen_type,
            insert_index,
            parent_group_id,
            applied: false,
            rejection: None,
        }
    }

    /// Prepare insertion of an existing layer while preserving its stable ID,
    /// parent and generator instance for command/redo composition.
    pub fn from_layer(layer: Layer, insert_index: usize) -> Self {
        Self {
            name: layer.name.clone(),
            layer_type: layer.layer_type,
            gen_type: layer.generator_type().clone(),
            parent_group_id: layer.parent_layer_id.clone(),
            layer: Some(layer),
            insert_index,
            applied: false,
            rejection: None,
        }
    }

    fn validate_candidate(project: &Project, layer: &Layer) -> Result<(), &'static str> {
        if project
            .timeline
            .layers
            .iter()
            .any(|existing| existing.layer_id == layer.layer_id)
        {
            return Err(LAYER_ID_DUPLICATE);
        }

        validate_parent_in_layers(&project.timeline.layers, layer)
    }
}

impl Command for AddLayerCommand {
    fn execute(&mut self, project: &mut Project) {
        self.applied = false;
        self.rejection = None;

        let layer = if let Some(existing) = &self.layer {
            existing.clone()
        } else {
            let mut new_layer = if self.layer_type == LayerType::Generator {
                Layer::new_generator(self.name.clone(), self.gen_type.clone(), 0)
            } else if self.layer_type == LayerType::Dmx {
                // An LED layer is generator-driven (LED_STRIPS_DESIGN.md section
                // 5b D12): seeded exactly like a generator layer so the standard
                // clip workflow plays the LED preset out of the box.
                // `Layer::new_generator` hardcodes `LayerType::Generator` and
                // `gen_params` has no public setter, so the type is flipped here.
                let mut layer =
                    Layer::new_generator(self.name.clone(), self.gen_type.clone(), 0);
                layer.layer_type = LayerType::Dmx;
                layer
            } else {
                Layer::new(self.name.clone(), self.layer_type, 0)
            };
            new_layer.parent_layer_id = self.parent_group_id.clone();
            // `Layer::new` keys layer_color off its index arg, but `insert_layer`
            // overwrites `index` positionally and never recomputes the colour —
            // so passing 0 here gave every added layer index-0's hue (the uniform
            // timeline colour). Seed from the current layer count so each new
            // layer steps to the next maximally-separated golden-ratio hue.
            new_layer.layer_color =
                Layer::generate_layer_color(project.timeline.layers.len());
            new_layer
        };
        self.layer = Some(layer.clone());
        if let Err(reason) = Self::validate_candidate(project, &layer) {
            self.rejection = Some(reason);
            return;
        }
        project.timeline.insert_layer(self.insert_index, layer);
        self.applied = true;
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        // Find the layer we inserted by ID
        if let Some(layer) = &self.layer
            && let Some(idx) = project
                .timeline
                .layers
                .iter()
                .position(|l| l.layer_id == layer.layer_id)
        {
            project.timeline.remove_layer(idx);
        }
        self.applied = false;
    }

    fn description(&self) -> &str {
        "Add Layer"
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection
    }

    fn was_applied(&self) -> bool {
        self.applied
    }
}

/// Add a generator layer for a pre-assembled import graph — the timeline
/// install step of the glTF import wave.
///
/// The model's assembled graph is registered as a project-embedded preset
/// (`origin: Saved`) and the new layer **tracks** it (`gen_params.graph =
/// None`), exactly like a drop from the browser resolves a catalog id.
/// An id that resolves in no catalog is not a
/// representable state: the earlier version stashed the def as a per-instance
/// override on the layer, which left every type-keyed UI surface blind (card
/// params empty, string params invisible, editor catalog-default `None` —
/// BUG-016). Catalog citizenship fixes them as a class.
///
/// The embedded preset carries its own metadata id; the layer's generator
/// type is that same id, so the renderer resolves the def through the
/// overlay-merged catalog exactly like a bundled generator. The caller
/// (`manifold-app`'s file-drop handler) is responsible for minting a
/// project-unique id and installing the catalog overlay before the first
/// frame reads the id — the assembler and this command stay renderer-free.
#[derive(Debug)]
pub struct ImportModelLayerCommand {
    layer: Option<Layer>,
    name: String,
    preset: EmbeddedPreset,
    insert_index: usize,
    parent_group_id: Option<LayerId>,
}

impl ImportModelLayerCommand {
    pub fn new(
        name: String,
        preset: EmbeddedPreset,
        insert_index: usize,
        parent_group_id: Option<LayerId>,
    ) -> Self {
        Self {
            layer: None,
            name,
            preset,
            insert_index,
            parent_group_id,
        }
    }

    /// The `LayerId` of the inserted layer, available after [`Command::execute`]
    /// has run (the id is generated at first execute). `None` before then.
    /// The drop handler reads it to target the same layer with a default
    /// generator clip so the model renders immediately.
    pub fn inserted_layer_id(&self) -> Option<LayerId> {
        self.layer.as_ref().map(|l| l.layer_id.clone())
    }
}

impl Command for ImportModelLayerCommand {
    fn execute(&mut self, project: &mut Project) {
        // Register (idempotent by id) the model's graph as a project-embedded
        // preset, so its id resolves through the catalog overlay just like a
        // bundled generator. Registering here — instead of stashing an
        // override on the layer — is what keeps the card, string params, and
        // editor catalog-default from going blind (BUG-016 / D9). Runs on both
        // the UI and content threads (the command box is dispatched to each),
        // so the embedded preset lands in both projects.
        project.upsert_embedded_preset(self.preset.clone());

        let layer = if let Some(existing) = self.layer.take() {
            existing
        } else {
            // `new_generator` (not `new`) stamps `kind: Generator` so the
            // instance serializes through the generator path — see the note on
            // `Layer::new_generator`. It seeds the tracking preset id and
            // (because the overlay is installed before this runs, per the
            // caller contract) `init_defaults` seeds the curated card values.
            let preset_type = self.preset.id().cloned().unwrap_or(PresetTypeId::NONE);
            let mut new_layer = Layer::new_generator(self.name.clone(), preset_type, 0);
            new_layer.parent_layer_id = self.parent_group_id.clone();
            // Match `AddLayerCommand`: seed a distinct hue from the current
            // layer count rather than index-0's colour.
            new_layer.layer_color = Layer::generate_layer_color(project.timeline.layers.len());
            // No override graph: the instance keeps `graph: None` and TRACKS
            // the embedded preset by id (mechanism A). A definition edit later
            // bakes a private copy on first touch — that's the one divergence
            // rule, not the import default.
            new_layer
        };
        self.layer = Some(layer.clone());
        project.timeline.insert_layer(self.insert_index, layer);
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(layer) = &self.layer
            && let Some(idx) = project
                .timeline
                .layers
                .iter()
                .position(|l| l.layer_id == layer.layer_id)
        {
            project.timeline.remove_layer(idx);
        }
        // Remove the embedded preset too, so undo is symmetric — the id was
        // minted project-unique per drop, so no other layer tracks it.
        if let Some(id) = self.preset.id().cloned() {
            project.remove_embedded_preset(&id);
        }
    }

    fn description(&self) -> &str {
        "Import 3D Model"
    }
}

/// Delete a layer from the timeline.
/// If the deleted layer is a group, its children's parent_layer_id is cleared
/// (matching Unity's behavior where children become root layers).
///
/// Grid integrity (`docs/SESSION_MODE_DESIGN.md` section 7): a `LayerId` with no
/// resolving layer must never be left behind in `Project.session.slots` —
/// deleting a layer removes that layer's session slots in the same command,
/// restored on undo.
#[derive(Debug)]
pub struct DeleteLayerCommand {
    layer: Option<Layer>,
    layer_id: LayerId,
    /// Remembered during execute so undo re-inserts at the same position.
    deleted_at_index: usize,
    /// Children whose parent_layer_id was cleared when a group was deleted.
    orphaned_children: Vec<(LayerId, Option<LayerId>)>,
    /// Direct trigger children removed with the owner, with their original
    /// timeline positions so undo restores the layer order exactly.
    removed_trigger_children: Vec<(usize, Layer)>,
    /// `None` detaches every ordinary child (DeleteLayerCommand's behavior);
    /// `Some` limits detachment for UngroupLayersCommand's existing API.
    detach_child_ids: Option<Vec<LayerId>>,
    /// This layer's session slots, removed alongside the layer itself.
    removed_slots: Vec<(usize, SessionSlot)>,
}

impl DeleteLayerCommand {
    pub fn new(layer: Layer) -> Self {
        let layer_id = layer.layer_id.clone();
        Self {
            layer: Some(layer),
            layer_id,
            deleted_at_index: 0,
            orphaned_children: Vec::new(),
            removed_trigger_children: Vec::new(),
            detach_child_ids: None,
            removed_slots: Vec::new(),
        }
    }

    fn for_ungroup(layer: Layer, child_layer_ids: Vec<LayerId>) -> Self {
        let mut command = Self::new(layer);
        command.detach_child_ids = Some(child_layer_ids);
        command
    }
}

impl Command for DeleteLayerCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(idx) = project.timeline.find_layer_index_by_id(&self.layer_id) {
            self.deleted_at_index = idx;

            // Trigger children are owned by their parent and disappear with
            // it. Capture their original positions for an exact undo.
            self.removed_trigger_children.clear();
            let trigger_indices: Vec<usize> = project
                .timeline
                .layers
                .iter()
                .enumerate()
                .filter(|(_, layer)| {
                    layer.parent_layer_id.as_ref() == Some(&self.layer_id) && layer.is_trigger()
                })
                .map(|(index, _)| index)
                .collect();
            for trigger_index in trigger_indices.into_iter().rev() {
                let trigger = project.timeline.layers.remove(trigger_index);
                self.removed_trigger_children
                    .push((trigger_index, trigger));
            }
            self.removed_trigger_children
                .sort_by_key(|(index, _)| *index);

            // Clear parent_layer_id on ordinary children referencing this
            // layer. Ungrouping retains its existing selected-child scope.
            self.orphaned_children.clear();
            for layer in &mut project.timeline.layers {
                let should_detach = self
                    .detach_child_ids
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&layer.layer_id));
                if should_detach
                    && layer.parent_layer_id.as_ref() == Some(&self.layer_id)
                {
                    self.orphaned_children
                        .push((layer.layer_id.clone(), layer.parent_layer_id.clone()));
                    layer.parent_layer_id = None;
                }
            }

            let owner_index = project
                .timeline
                .find_layer_index_by_id(&self.layer_id)
                .expect("owner layer remains after trigger children are removed");
            self.layer = project.timeline.remove_layer(owner_index);

            // Grid integrity: remove this layer and its owned trigger lanes'
            // session slots together.
            self.removed_slots.clear();
            let mut removed_layer_ids = vec![self.layer_id.clone()];
            removed_layer_ids.extend(
                self.removed_trigger_children
                    .iter()
                    .map(|(_, layer)| layer.layer_id.clone()),
            );
            let mut i = 0;
            while i < project.session.slots.len() {
                if removed_layer_ids.contains(&project.session.slots[i].layer_id) {
                    // `i` is relative to the shrinking vector; account for
                    // previously removed slots so undo can restore original
                    // positions even when unrelated slots are interleaved.
                    let original_index = i + self.removed_slots.len();
                    self.removed_slots
                        .push((original_index, project.session.slots.remove(i)));
                } else {
                    i += 1;
                }
            }
            if !self.removed_slots.is_empty() {
                project.session.mark_slot_lookup_dirty();
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(layer) = self.layer.take() {
            let restored_owner = layer.clone();
            let mut restored_layers = vec![(self.deleted_at_index, layer)];
            restored_layers.extend(self.removed_trigger_children.iter().cloned());
            restored_layers.sort_by_key(|(index, _)| *index);
            for (index, restored_layer) in restored_layers {
                let idx = index.min(project.timeline.layers.len());
                project.timeline.layers.insert(idx, restored_layer);
            }
            self.layer = Some(restored_owner);

            // Restore parent_layer_id on previously orphaned children
            for (child_id, old_parent) in &self.orphaned_children {
                if let Some((_, child)) = project.timeline.find_layer_by_id_mut(child_id) {
                    child.parent_layer_id = old_parent.clone();
                }
            }
            let restored_order = project.timeline.layers.clone();
            project.timeline.replace_layer_order(restored_order);

            for (index, slot) in self.removed_slots.drain(..) {
                let insert_at = index.min(project.session.slots.len());
                project.session.slots.insert(insert_at, slot);
            }
            project.session.mark_slot_lookup_dirty();
        }

        debug_assert!(
            project
                .session
                .slots
                .iter()
                .all(|s| project.timeline.find_layer_index_by_id(&s.layer_id).is_some()),
            "session slot references a LayerId that no longer resolves"
        );
    }

    fn description(&self) -> &str {
        "Delete Layer"
    }
}

/// Reorder layers atomically.
#[derive(Debug)]
pub struct ReorderLayerCommand {
    old_order: Vec<LayerId>,
    new_order: Vec<LayerId>,
    old_parent_ids: HashMap<LayerId, Option<LayerId>>,
    new_parent_ids: HashMap<LayerId, Option<LayerId>>,
    applied: bool,
    rejection: Option<&'static str>,
}

impl ReorderLayerCommand {
    pub fn new(
        old_order: Vec<Layer>,
        new_order: Vec<Layer>,
        old_parent_ids: HashMap<LayerId, Option<LayerId>>,
        new_parent_ids: HashMap<LayerId, Option<LayerId>>,
    ) -> Self {
        Self {
            old_order: old_order.into_iter().map(|layer| layer.layer_id).collect(),
            new_order: new_order.into_iter().map(|layer| layer.layer_id).collect(),
            old_parent_ids,
            new_parent_ids,
            applied: false,
            rejection: None,
        }
    }

    fn validate_membership(current_order: &[Layer], new_order: &[LayerId]) -> Result<(), &'static str> {
        let current_ids: HashSet<_> = current_order.iter().map(|layer| &layer.layer_id).collect();
        let new_ids: HashSet<_> = new_order.iter().collect();
        if current_ids.len() != current_order.len()
            || new_ids.len() != new_order.len()
            || current_ids != new_ids
        {
            return Err(LAYER_REORDER_MEMBERSHIP);
        }
        Ok(())
    }

    fn validate_changed_parents(layers: &[Layer], original: &[Layer]) -> Result<(), &'static str> {
        for layer in layers.iter().filter(|layer| !layer.is_trigger()) {
            let old_parent = original.iter().find(|old| old.layer_id == layer.layer_id)
                .and_then(|old| old.parent_layer_id.as_ref());
            if old_parent == layer.parent_layer_id.as_ref() {
                continue;
            }
            validate_parent_in_layers(layers, layer)?;
        }
        Ok(())
    }

    fn resolve_live_order(
        project: &Project,
        order: &[LayerId],
        parent_ids: &HashMap<LayerId, Option<LayerId>>,
    ) -> Option<Vec<Layer>> {
        let mut resolved = Vec::with_capacity(order.len());
        for layer_id in order {
            let layer = project
                .timeline
                .layers
                .iter()
                .find(|layer| &layer.layer_id == layer_id)?;
            let mut live = layer.clone();
            if !live.is_trigger()
                && let Some(parent_id) = parent_ids.get(layer_id)
            {
                live.parent_layer_id = parent_id.clone();
            }
            resolved.push(live);
        }
        Some(resolved)
    }
}

impl Command for ReorderLayerCommand {
    fn execute(&mut self, project: &mut Project) {
        self.applied = false;
        self.rejection = None;
        if let Err(reason) = Self::validate_membership(&project.timeline.layers, &self.new_order) {
            self.rejection = Some(reason);
            return;
        }
        let Some(new_order) = Self::resolve_live_order(project, &self.new_order, &self.new_parent_ids) else {
            self.rejection = Some(LAYER_REORDER_MEMBERSHIP);
            return;
        };
        if let Err(reason) = Self::validate_changed_parents(&new_order, &project.timeline.layers) {
            self.rejection = Some(reason);
            return;
        }
        project.timeline.replace_layer_order(new_order);
        self.applied = true;
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        self.rejection = None;
        if let Err(reason) = Self::validate_membership(&project.timeline.layers, &self.old_order) {
            self.rejection = Some(reason);
            return;
        }
        let Some(old_order) = Self::resolve_live_order(project, &self.old_order, &self.old_parent_ids) else {
            self.rejection = Some(LAYER_REORDER_MEMBERSHIP);
            return;
        };
        if let Err(reason) = Self::validate_changed_parents(&old_order, &project.timeline.layers) {
            self.rejection = Some(reason);
            return;
        }
        project.timeline.replace_layer_order(old_order);
        self.applied = false;
    }

    fn description(&self) -> &str {
        "Reorder Layers"
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection
    }

    fn was_applied(&self) -> bool {
        self.applied
    }
}

/// Group selected layers into a new group layer.
#[derive(Debug)]
pub struct GroupLayersCommand {
    selected_layer_ids: Vec<LayerId>,
    group_layer: Option<Layer>,
    /// Full layer list captured before grouping (excludes the group layer,
    /// which `execute` creates). `undo` restores it verbatim so sibling order
    /// survives the round-trip.
    original_order: Vec<Layer>,
    applied: bool,
    rejection: Option<&'static str>,
}

impl GroupLayersCommand {
    pub fn new(selected_layer_ids: Vec<LayerId>, original_order: Vec<Layer>) -> Self {
        Self {
            selected_layer_ids,
            group_layer: None,
            original_order,
            applied: false,
            rejection: None,
        }
    }
}

impl Command for GroupLayersCommand {
    fn execute(&mut self, project: &mut Project) {
        self.applied = false;
        self.rejection = None;
        if self.selected_layer_ids.is_empty()
            || self.selected_layer_ids.iter().any(|id| {
                !project.timeline.layers.iter().any(|layer| &layer.layer_id == id)
            })
        {
            self.rejection = Some(if self.selected_layer_ids.is_empty() {
                GROUP_SELECTION_EMPTY
            } else {
                GROUP_SELECTION_MISSING
            });
            return;
        }
        let selected: HashSet<_> = self.selected_layer_ids.iter().cloned().collect();
        let roots: Vec<LayerId> = project
            .timeline
            .layers
            .iter()
            .filter(|layer| selected.contains(&layer.layer_id) && !layer.is_trigger())
            .filter(|layer| {
                let mut parent = layer.parent_layer_id.as_ref();
                for _ in 0..project.timeline.layers.len() {
                    let Some(parent_id) = parent else { break; };
                    if selected.contains(parent_id) {
                        return false;
                    }
                    parent = project
                        .timeline
                        .layers
                        .iter()
                        .find(|candidate| &candidate.layer_id == parent_id)
                        .and_then(|candidate| candidate.parent_layer_id.as_ref());
                }
                true
            })
            .map(|layer| layer.layer_id.clone())
            .collect();
        if roots.is_empty() {
            self.rejection = Some(GROUP_SELECTION_EMPTY);
            return;
        }
        // Create group layer on first execute
        let group = if let Some(existing) = &self.group_layer {
            existing.clone()
        } else {
            let g = Layer::new("Group".to_string(), LayerType::Group, 0);
            self.group_layer = Some(g.clone());
            g
        };

        let group_id = group.layer_id.clone();

        // Find insertion point (before first selected)
        let insert_idx = project
            .timeline
            .layers
            .iter()
            .position(|l| roots.contains(&l.layer_id))
            .unwrap_or(0);

        // Insert group layer
        project.timeline.insert_layer(insert_idx, group);

        // Reparent selected layers
        for layer in &mut project.timeline.layers {
            if roots.contains(&layer.layer_id) {
                layer.parent_layer_id = Some(group_id.clone());
            }
        }
        project.timeline.enforce_tree_order();
        self.applied = true;
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        // Restore the pre-group snapshot verbatim: the group layer is gone
        // (it isn't in `original_order`), parents are back, and sibling order
        // is reproduced exactly — same restore path as `UngroupLayersCommand`.
        project
            .timeline
            .replace_layer_order(self.original_order.clone());
        self.applied = false;
    }

    fn description(&self) -> &str {
        "Group Layers"
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection
    }

    fn was_applied(&self) -> bool {
        self.applied
    }
}

/// Rename a layer (undoable).
#[derive(Debug)]
pub struct RenameLayerCommand {
    layer_id: LayerId,
    old_name: String,
    new_name: String,
}

impl RenameLayerCommand {
    pub fn new(layer_id: LayerId, old_name: String, new_name: String) -> Self {
        Self {
            layer_id,
            old_name,
            new_name,
        }
    }
}

impl Command for RenameLayerCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.name = self.new_name.clone();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.name = self.old_name.clone();
        }
    }

    fn description(&self) -> &str {
        "Rename Layer"
    }
}

/// Ungroup a group layer, dissolving it.
#[derive(Debug)]
pub struct UngroupLayersCommand {
    group_layer: Option<Layer>,
    child_layer_ids: Vec<LayerId>,
    original_order: Vec<Layer>,
    delete_command: Option<DeleteLayerCommand>,
}

impl UngroupLayersCommand {
    pub fn new(
        group_layer: Layer,
        child_layer_ids: Vec<LayerId>,
        original_order: Vec<Layer>,
    ) -> Self {
        Self {
            group_layer: Some(group_layer),
            child_layer_ids,
            original_order,
            delete_command: None,
        }
    }
}

impl Command for UngroupLayersCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(group) = &self.group_layer {
            if self.delete_command.is_none() {
                self.delete_command = Some(DeleteLayerCommand::for_ungroup(
                    group.clone(),
                    self.child_layer_ids.clone(),
                ));
            }
            if let Some(delete) = &mut self.delete_command {
                delete.execute(project);
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(delete) = &mut self.delete_command {
            delete.undo(project);
        }
        // Restore original order (includes group and all owned trigger lanes).
        project.timeline.replace_layer_order(self.original_order.clone());
    }

    fn description(&self) -> &str {
        "Ungroup Layers"
    }
}

/// Duplicate one or more layers (with full deep copy of all nested IDs).
/// The pre-cloned layers are stored in the command so redo works correctly.
#[derive(Debug)]
pub struct DuplicateLayersCommand {
    /// Pre-built clones (with fresh IDs) ready to insert, in insertion order.
    new_layers: Vec<Layer>,
    /// Index in the timeline Vec to start inserting at.
    insert_after_index: usize,
}

impl DuplicateLayersCommand {
    pub fn new(new_layers: Vec<Layer>, insert_after_index: usize) -> Self {
        Self {
            new_layers,
            insert_after_index,
        }
    }
}

impl Command for DuplicateLayersCommand {
    fn execute(&mut self, project: &mut Project) {
        for (i, layer) in self.new_layers.iter().cloned().enumerate() {
            project
                .timeline
                .insert_layer(self.insert_after_index + i, layer);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        // Remove in reverse insertion order (highest index first) by ID for robustness.
        for layer in self.new_layers.iter().rev() {
            if let Some(idx) = project.timeline.find_layer_index_by_id(&layer.layer_id) {
                project.timeline.remove_layer(idx);
            }
        }
    }

    fn description(&self) -> &str {
        "Duplicate Layers"
    }
}

/// Set an audio layer's output gain (decibels). The track fader for an audio
/// layer; applied to its kira playback handle. See `docs/AUDIO_LAYER_DESIGN.md`.
#[derive(Debug)]
pub struct SetLayerAudioGainCommand {
    layer_id: LayerId,
    old_db: f32,
    new_db: f32,
}

impl SetLayerAudioGainCommand {
    pub fn new(layer_id: LayerId, old_db: f32, new_db: f32) -> Self {
        Self { layer_id, old_db, new_db }
    }
}

impl Command for SetLayerAudioGainCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(layer) = project
            .timeline
            .layers
            .iter_mut()
            .find(|l| l.layer_id == self.layer_id)
        {
            layer.audio_gain_db = self.new_db;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(layer) = project
            .timeline
            .layers
            .iter_mut()
            .find(|l| l.layer_id == self.layer_id)
        {
            layer.audio_gain_db = self.old_db;
        }
    }

    fn description(&self) -> &str {
        "Set Audio Layer Gain"
    }
}

/// Toggle an audio layer's **analysis-only** output state: silent to the master
/// mix but still feeding its send (the third state beside Live and Muted). Mute
/// still wins. See `docs/AUDIO_LAYER_DESIGN.md` section 5 / `LAYER_CONTROLS_DESIGN.md` section 5.3.
#[derive(Debug)]
pub struct SetLayerAnalysisOnlyCommand {
    layer_id: LayerId,
    old_value: bool,
    new_value: bool,
}

impl SetLayerAnalysisOnlyCommand {
    pub fn new(layer_id: LayerId, new_value: bool) -> Self {
        Self { layer_id, old_value: false, new_value }
    }
}

impl Command for SetLayerAnalysisOnlyCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(layer) = project
            .timeline
            .layers
            .iter_mut()
            .find(|l| l.layer_id == self.layer_id)
        {
            self.old_value = layer.analysis_only;
            layer.analysis_only = self.new_value;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(layer) = project
            .timeline
            .layers
            .iter_mut()
            .find(|l| l.layer_id == self.layer_id)
        {
            layer.analysis_only = self.old_value;
        }
    }

    fn description(&self) -> &str {
        "Set Audio Layer Analysis-Only"
    }
}

// ─── LayerClipTrigger (P2) ─────────────────────────────────────────────
//
// The one authorable clip-trigger shape (`docs/AUDIO_SETUP_DOCK_AND_TRIGGER_
// UNIFICATION_DESIGN.md` section 3.1/D2) lives on `Layer.clip_triggers: Vec<LayerClipTrigger>`.
// No `DriverTarget` here — that enum addresses effect/generator-param drivers,
// not a layer's own field — so these commands address by `LayerId` directly,
// exactly like `SetLayerAudioGainCommand`/`SetLayerAnalysisOnlyCommand` above.
// Add/remove mirror `AddAudioModCommand`/`RemoveAudioModCommand`
// (`commands/audio_mod.rs`) — the audio-mod command family's `Vec<T>`-mutation
// shape; `SetLayerClipTriggerCommand` mirrors `SetAudioModTriggerModeCommand`'s
// whole-field old/new capture, generalized to the whole config so every P3
// drawer row (Source/Feature/Band/Shape fields/Length) can share one command
// rather than growing one setter per field.

/// Append a new [`LayerClipTrigger`] to a layer's `clip_triggers`.
#[derive(Debug)]
pub struct AddLayerClipTriggerCommand {
    layer_id: LayerId,
    trigger: LayerClipTrigger,
    /// Length of `clip_triggers` before this command's push — undo truncates
    /// back to it (the `AddAudioModCommand` shape doesn't need this because
    /// audio mods dedupe by `param_id`; clip triggers have no such key).
    len_before: usize,
}

impl AddLayerClipTriggerCommand {
    pub fn new(layer_id: LayerId, trigger: LayerClipTrigger) -> Self {
        Self { layer_id, trigger, len_before: 0 }
    }
}

impl Command for AddLayerClipTriggerCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            self.len_before = layer.clip_triggers.len();
            layer.clip_triggers.push(self.trigger.clone());
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id) {
            layer.clip_triggers.truncate(self.len_before);
        }
    }

    fn description(&self) -> &str {
        "Add Clip Trigger"
    }
}

/// Remove the [`LayerClipTrigger`] at `index` from a layer's `clip_triggers`.
/// Captures the removed config for undo (the `RemoveAudioModCommand` shape).
#[derive(Debug)]
pub struct RemoveLayerClipTriggerCommand {
    layer_id: LayerId,
    index: usize,
    removed: Option<LayerClipTrigger>,
}

impl RemoveLayerClipTriggerCommand {
    pub fn new(layer_id: LayerId, index: usize) -> Self {
        Self { layer_id, index, removed: None }
    }
}

impl Command for RemoveLayerClipTriggerCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id)
            && self.index < layer.clip_triggers.len()
        {
            self.removed = Some(layer.clip_triggers.remove(self.index));
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(trigger) = self.removed.take()
            && let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id)
        {
            let at = self.index.min(layer.clip_triggers.len());
            layer.clip_triggers.insert(at, trigger);
        }
    }

    fn description(&self) -> &str {
        "Remove Clip Trigger"
    }
}

/// Replace the [`LayerClipTrigger`] at `index` wholesale — the P3 drawer's one
/// command for every field edit (enabled/source/shape/one_shot_beats), whole-
/// value old/new capture like [`crate::commands::audio_mod::SetAudioModTriggerModeCommand`].
#[derive(Debug)]
pub struct SetLayerClipTriggerCommand {
    layer_id: LayerId,
    index: usize,
    old: LayerClipTrigger,
    new: LayerClipTrigger,
}

impl SetLayerClipTriggerCommand {
    pub fn new(layer_id: LayerId, index: usize, old: LayerClipTrigger, new: LayerClipTrigger) -> Self {
        Self { layer_id, index, old, new }
    }
}

impl Command for SetLayerClipTriggerCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id)
            && let Some(slot) = layer.clip_triggers.get_mut(self.index)
        {
            *slot = self.new.clone();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&self.layer_id)
            && let Some(slot) = layer.clip_triggers.get_mut(self.index)
        {
            *slot = self.old.clone();
        }
    }

    fn description(&self) -> &str {
        "Edit Clip Trigger"
    }
}

#[cfg(test)]
mod clip_trigger_command_tests {
    use super::*;
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, AudioModSource};
    use manifold_core::id::AudioSendId;

    fn project_with_one_layer() -> (Project, LayerId) {
        let mut project = Project::default();
        let layer = Layer::new("L".to_string(), LayerType::Video, 0);
        let layer_id = layer.layer_id.clone();
        project.timeline.layers.push(layer);
        (project, layer_id)
    }

    fn trigger(sensitivity: f32) -> LayerClipTrigger {
        let mut cfg = LayerClipTrigger::new(AudioModSource {
            send_id: AudioSendId::new("send-a"),
            feature: AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Low),
        });
        cfg.enabled = true;
        cfg.shape.sensitivity = sensitivity;
        cfg
    }

    #[test]
    fn add_pushes_and_undo_truncates() {
        let (mut project, layer_id) = project_with_one_layer();
        let mut cmd = AddLayerClipTriggerCommand::new(layer_id.clone(), trigger(0.5));
        cmd.execute(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        assert_eq!(layer.clip_triggers.len(), 1);

        cmd.undo(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        assert!(layer.clip_triggers.is_empty());
    }

    #[test]
    fn remove_captures_and_undo_reinserts_at_the_same_index() {
        let (mut project, layer_id) = project_with_one_layer();
        {
            let (_, layer) = project.timeline.find_layer_by_id_mut(&layer_id).unwrap();
            layer.clip_triggers.push(trigger(0.1));
            layer.clip_triggers.push(trigger(0.9));
        }

        let mut cmd = RemoveLayerClipTriggerCommand::new(layer_id.clone(), 0);
        cmd.execute(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        assert_eq!(layer.clip_triggers.len(), 1);
        assert_eq!(layer.clip_triggers[0].shape.sensitivity, 0.9);

        cmd.undo(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        assert_eq!(layer.clip_triggers.len(), 2);
        assert_eq!(layer.clip_triggers[0].shape.sensitivity, 0.1);
        assert_eq!(layer.clip_triggers[1].shape.sensitivity, 0.9);
    }

    #[test]
    fn set_replaces_wholesale_and_undo_restores_the_old_value() {
        let (mut project, layer_id) = project_with_one_layer();
        {
            let (_, layer) = project.timeline.find_layer_by_id_mut(&layer_id).unwrap();
            layer.clip_triggers.push(trigger(0.2));
        }
        let old = trigger(0.2);
        let new = trigger(0.8);

        let mut cmd = SetLayerClipTriggerCommand::new(layer_id.clone(), 0, old, new);
        cmd.execute(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        assert_eq!(layer.clip_triggers[0].shape.sensitivity, 0.8);

        cmd.undo(&mut project);
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        assert_eq!(layer.clip_triggers[0].shape.sensitivity, 0.2);
    }
}

#[cfg(test)]
mod import_model_tests {
    use super::*;
    use manifold_core::effect_graph_def::{
        EFFECT_GRAPH_VERSION, EffectGraphDef, PresetMetadata,
    };
    use manifold_core::preset_def::PresetKind;

    /// A minimal self-contained embedded preset stands in for a real assembled
    /// import graph. The command registers this as a project preset and tracks
    /// it from the layer; the graph's internal shape is irrelevant here (the
    /// renderer crate's own tests cover assembly + rendering). What matters is
    /// that the id resolves through the overlay — carried in `preset_metadata`.
    fn stub_embedded_preset(id: &str) -> EmbeddedPreset {
        let meta = PresetMetadata {
            id: PresetTypeId::from_string(id.to_string()),
            display_name: "Azalea".to_string(),
            category: "Geometry".to_string(),
            osc_prefix: id.to_string(),
            legacy_discriminant: None,
            scene_modifier: None,
            scene_bounds: None,
            available: true,
            is_line_based: false,
                layer_types: None,
            params: Vec::new(),
            bindings: Vec::new(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        };
        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: Some("Azalea".to_string()),
            description: None,
            preset_metadata: Some(meta),
            scene_modifiers: Vec::new(),
            nodes: Vec::new(),
            wires: Vec::new(),
        };
        EmbeddedPreset {
            kind: PresetKind::Generator,
            def,
            origin: manifold_core::project::EmbeddedOrigin::Saved,
        }
    }

    #[test]
    fn import_model_registers_embedded_preset_and_tracks_it() {
        let mut project = Project::default();
        let before = project.timeline.layers.len();
        let preset_id = PresetTypeId::new("azalea");

        let mut cmd = ImportModelLayerCommand::new(
            "Azalea".to_string(),
            stub_embedded_preset("azalea"),
            before,
            None,
        );
        cmd.execute(&mut project);

        assert_eq!(project.timeline.layers.len(), before + 1);
        let layer = &project.timeline.layers[before];
        assert_eq!(
            layer.layer_type,
            LayerType::Generator,
            "imported model must be a generator layer"
        );
        assert_eq!(
            layer.generator_type(),
            &preset_id,
            "generator type must be the embedded preset id (so it resolves via the overlay)"
        );
        assert!(
            layer.generator_graph().is_none(),
            "the layer must TRACK the embedded preset (graph: None), not carry an \
             override — that is the D9 fix for BUG-016"
        );
        assert!(
            project.embedded_preset(&preset_id).is_some(),
            "the model's graph must be registered as a project-embedded preset so its \
             id resolves in the catalog overlay"
        );
    }

    #[test]
    fn import_model_undo_removes_layer_and_embedded_preset() {
        let mut project = Project::default();
        let before = project.timeline.layers.len();
        let preset_id = PresetTypeId::new("azalea");

        let mut cmd = ImportModelLayerCommand::new(
            "Azalea".to_string(),
            stub_embedded_preset("azalea"),
            before,
            None,
        );
        cmd.execute(&mut project);
        assert_eq!(project.timeline.layers.len(), before + 1);
        assert!(project.embedded_preset(&preset_id).is_some());

        cmd.undo(&mut project);
        assert_eq!(
            project.timeline.layers.len(),
            before,
            "undo must remove exactly the imported layer"
        );
        assert!(
            project.embedded_preset(&preset_id).is_none(),
            "undo must also remove the embedded preset — symmetric with execute"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(name: &str, index: i32) -> Layer {
        Layer::new(name.to_string(), LayerType::Video, index)
    }

    /// MVP-P1b default-preset contract (LED_STRIPS_DESIGN.md section 5b D12):
    /// creating an LED layer seeds `gen_params` with the bundled LED Fill
    /// preset id — the id is the contract; MVP-P2 grows the graph underneath
    /// it. A missing gen_params here is the "cleared generator" black-render
    /// trap, and a wrong id means the layer silently falls back to no preset.
    #[test]
    fn add_led_layer_seeds_led_fill_gen_params() {
        let mut project = Project::default();
        project.timeline.layers.push(video("A", 0));

        let mut cmd = AddLayerCommand::new(
            "LED 2".to_string(),
            LayerType::Dmx,
            PresetTypeId::new("LED Fill"),
            1,
            None,
        );
        cmd.execute(&mut project);

        let layer = &project.timeline.layers[1];
        assert_eq!(layer.layer_type, LayerType::Dmx);
        assert!(layer.is_dmx());
        let genp = layer
            .gen_params()
            .expect("LED layer must carry gen_params (D12)");
        assert_eq!(
            genp.generator_type(),
            &PresetTypeId::new("LED Fill"),
            "new LED layers default to the LED Fill preset",
        );
    }

    // ── MVP-P3a: gen-carrying predicate (LED_STRIPS_DESIGN.md section 5b D16) ──

    const TEST_LED_GEN_A: PresetTypeId = PresetTypeId::new("TestLedGenA");
    const TEST_LED_GEN_B: PresetTypeId = PresetTypeId::new("TestLedGenB");

    inventory::submit! {
        manifold_core::generator_registration::GeneratorMetadata {
            id: PresetTypeId::new("TestLedGenA"),
            display_name: "Test Led Gen A",
            is_line_based: false,
            available: true,
            osc_prefix: "testLedGenA",
            legacy_discriminant: None,
            params: &[manifold_core::generator_registration::ParamSpec::continuous("speed", "Speed", 0.0, 10.0, 0.0, "F2", "")],
        }
    }
    inventory::submit! {
        manifold_core::generator_registration::GeneratorMetadata {
            id: PresetTypeId::new("TestLedGenB"),
            display_name: "Test Led Gen B",
            is_line_based: false,
            available: true,
            osc_prefix: "testLedGenB",
            legacy_discriminant: None,
            params: &[manifold_core::generator_registration::ParamSpec::continuous("speed", "Speed", 0.0, 10.0, 0.0, "F2", "")],
        }
    }

    /// BUG-ev8u characterization (LED_STRIPS_DESIGN.md section 5b D16): the
    /// generator picker's Change button was a silent no-op on LED lanes
    /// because `change_generator_type` gated on `!= LayerType::Generator`.
    /// Post-fix the change lands on the LED lane; undo restores the previous
    /// type. Registered test types (not "LED Fill") because undo re-seeds
    /// through the registry.
    #[test]
    fn change_generator_type_on_led_layer_assigns_and_undo_restores() {
        use crate::commands::settings::ChangeGeneratorTypeCommand;

        let mut project = Project::default();
        let mut add = AddLayerCommand::new(
            "LED 1".to_string(),
            LayerType::Dmx,
            TEST_LED_GEN_A.clone(),
            0,
            None,
        );
        add.execute(&mut project);
        let layer_id = project.timeline.layers[0].layer_id.clone();

        let (old_type, old_params, old_drivers, old_envelopes) = {
            let gp = project.timeline.layers[0].gen_params().unwrap();
            (
                gp.generator_type().clone(),
                gp.snapshot_params(),
                gp.snapshot_drivers(),
                gp.snapshot_envelopes(),
            )
        };

        let mut cmd = ChangeGeneratorTypeCommand::new(
            layer_id,
            old_type,
            TEST_LED_GEN_B.clone(),
            old_params,
            old_drivers,
            old_envelopes,
        );
        cmd.execute(&mut project);
        assert_eq!(
            project.timeline.layers[0]
                .gen_params()
                .unwrap()
                .generator_type(),
            &TEST_LED_GEN_B,
            "the picker assignment must land on an LED lane"
        );

        cmd.undo(&mut project);
        assert_eq!(
            project.timeline.layers[0]
                .gen_params()
                .unwrap()
                .generator_type(),
            &TEST_LED_GEN_A,
            "undo restores the pre-change generator type on an LED lane"
        );
    }

    /// Grouping a non-contiguous selection then undoing must restore the exact
    /// original layer order and clear every reparent — `undo` restores the
    /// pre-group snapshot verbatim rather than re-deriving order.
    #[test]
    fn group_then_undo_restores_exact_sibling_order() {
        let mut project = Project::default();
        for (i, name) in ["A", "B", "C", "D"].iter().enumerate() {
            project.timeline.layers.push(video(name, i as i32));
        }
        let original = project.timeline.layers.clone();
        let original_ids: Vec<LayerId> = original.iter().map(|l| l.layer_id.clone()).collect();
        // Group B and D — non-contiguous, so the naive restore path shuffles order.
        let selected = vec![original_ids[1].clone(), original_ids[3].clone()];

        let mut cmd = GroupLayersCommand::new(selected.clone(), original.clone());
        cmd.execute(&mut project);

        // Grouping happened: a Group layer exists and both selections are parented under it.
        let group_id = project
            .timeline
            .layers
            .iter()
            .find(|l| l.layer_type == LayerType::Group)
            .map(|l| l.layer_id.clone())
            .expect("group layer created");
        for id in &selected {
            let parent = project
                .timeline
                .layers
                .iter()
                .find(|l| &l.layer_id == id)
                .and_then(|l| l.parent_layer_id.clone());
            assert_eq!(parent.as_ref(), Some(&group_id));
        }

        cmd.undo(&mut project);

        let restored_ids: Vec<LayerId> =
            project.timeline.layers.iter().map(|l| l.layer_id.clone()).collect();
        assert_eq!(restored_ids, original_ids, "undo must restore exact order");
        assert!(
            project.timeline.layers.iter().all(|l| l.parent_layer_id.is_none()),
            "undo must clear all reparenting"
        );
    }
}
