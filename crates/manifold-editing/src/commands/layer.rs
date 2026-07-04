use crate::command::Command;
use manifold_core::PresetTypeId;
use manifold_core::LayerId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::session::SessionSlot;
use manifold_core::types::LayerType;
use std::collections::HashMap;

/// Add a new layer to the timeline.
#[derive(Debug)]
pub struct AddLayerCommand {
    layer: Option<Layer>,
    name: String,
    layer_type: LayerType,
    gen_type: PresetTypeId,
    insert_index: usize,
    parent_group_id: Option<LayerId>,
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
        }
    }
}

impl Command for AddLayerCommand {
    fn execute(&mut self, project: &mut Project) {
        let layer = if let Some(existing) = self.layer.take() {
            existing
        } else {
            let mut new_layer = if self.layer_type == LayerType::Generator {
                Layer::new_generator(self.name.clone(), self.gen_type.clone(), 0)
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
        project.timeline.insert_layer(self.insert_index, layer);
    }

    fn undo(&mut self, project: &mut Project) {
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
    }

    fn description(&self) -> &str {
        "Add Layer"
    }
}

/// Add a generator layer whose per-instance graph is a pre-assembled import
/// graph — the timeline install step of the glTF import wave.
///
/// Unlike [`AddLayerCommand`] (which spawns a *bundled-preset* generator that
/// resolves its graph from the catalog by `PresetTypeId`), this installs a
/// self-contained override graph directly onto the new layer's
/// `gen_params.graph`. The renderer builds it via
/// `GeneratorRegistry::create_with_override`'s `from_def` path, so the
/// imported preset id never has to exist in the bundled catalog — the layer
/// carries its own definition and survives save/reload with no registry work.
///
/// The `graph` is produced upstream by
/// `manifold_renderer::node_graph::gltf_import::assemble_import_graph` (which
/// lives in the renderer crate, so it cannot be called from here — this
/// command takes the already-assembled [`EffectGraphDef`], a `manifold-core`
/// type, keeping the editing crate free of any renderer dependency).
#[derive(Debug)]
pub struct ImportModelLayerCommand {
    layer: Option<Layer>,
    name: String,
    preset_type: PresetTypeId,
    graph: EffectGraphDef,
    insert_index: usize,
    parent_group_id: Option<LayerId>,
}

impl ImportModelLayerCommand {
    pub fn new(
        name: String,
        preset_type: PresetTypeId,
        graph: EffectGraphDef,
        insert_index: usize,
        parent_group_id: Option<LayerId>,
    ) -> Self {
        Self {
            layer: None,
            name,
            preset_type,
            graph,
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
        let layer = if let Some(existing) = self.layer.take() {
            existing
        } else {
            // `new_generator` (not `new`) stamps `kind: Generator` so the
            // instance serializes through the generator path — see the note on
            // `Layer::new_generator`. It also seeds the preset id, which the
            // override graph carries independently; keeping them in sync lets
            // the layer card name the model even before the graph is read.
            let mut new_layer =
                Layer::new_generator(self.name.clone(), self.preset_type.clone(), 0);
            new_layer.parent_layer_id = self.parent_group_id.clone();
            // Match `AddLayerCommand`: seed a distinct hue from the current
            // layer count rather than index-0's colour.
            new_layer.layer_color =
                Layer::generate_layer_color(project.timeline.layers.len());
            // Install the assembled import graph as the layer's per-instance
            // override, then bump the *structure* version so the generator
            // renderer treats it as an authored graph and builds its chain
            // against this def (mirrors how the first graph edit lifts 0 → 1).
            let gp = new_layer.gen_params_or_init();
            *gp.graph_def_mut() = Some(self.graph.clone());
            gp.bump_graph_structure_version();
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
    }

    fn description(&self) -> &str {
        "Import 3D Model"
    }
}

/// Delete a layer from the timeline.
/// If the deleted layer is a group, its children's parent_layer_id is cleared
/// (matching Unity's behavior where children become root layers).
///
/// Grid integrity (`docs/SESSION_MODE_DESIGN.md` §7): a `LayerId` with no
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
    /// This layer's session slots, removed alongside the layer itself.
    removed_slots: Vec<SessionSlot>,
}

impl DeleteLayerCommand {
    pub fn new(layer: Layer) -> Self {
        let layer_id = layer.layer_id.clone();
        Self {
            layer: Some(layer),
            layer_id,
            deleted_at_index: 0,
            orphaned_children: Vec::new(),
            removed_slots: Vec::new(),
        }
    }
}

impl Command for DeleteLayerCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(idx) = project.timeline.find_layer_index_by_id(&self.layer_id) {
            self.deleted_at_index = idx;

            // Clear parent_layer_id on children referencing this layer
            self.orphaned_children.clear();
            for layer in &mut project.timeline.layers {
                if layer.parent_layer_id.as_ref() == Some(&self.layer_id) {
                    self.orphaned_children
                        .push((layer.layer_id.clone(), layer.parent_layer_id.clone()));
                    layer.parent_layer_id = None;
                }
            }

            self.layer = project.timeline.remove_layer(idx);

            // Grid integrity: remove this layer's session slots too.
            self.removed_slots.clear();
            let layer_id = self.layer_id.clone();
            let mut i = 0;
            while i < project.session.slots.len() {
                if project.session.slots[i].layer_id == layer_id {
                    self.removed_slots.push(project.session.slots.remove(i));
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
            let idx = self.deleted_at_index.min(project.timeline.layers.len());
            project.timeline.insert_layer(idx, layer.clone());
            self.layer = Some(layer);

            // Restore parent_layer_id on previously orphaned children
            for (child_id, old_parent) in &self.orphaned_children {
                if let Some((_, child)) = project.timeline.find_layer_by_id_mut(child_id) {
                    child.parent_layer_id = old_parent.clone();
                }
            }
            project.timeline.enforce_tree_order();

            for slot in self.removed_slots.drain(..) {
                project.session.slots.push(slot);
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
    old_order: Vec<Layer>,
    new_order: Vec<Layer>,
    old_parent_ids: HashMap<LayerId, Option<LayerId>>,
    new_parent_ids: HashMap<LayerId, Option<LayerId>>,
}

impl ReorderLayerCommand {
    pub fn new(
        old_order: Vec<Layer>,
        new_order: Vec<Layer>,
        old_parent_ids: HashMap<LayerId, Option<LayerId>>,
        new_parent_ids: HashMap<LayerId, Option<LayerId>>,
    ) -> Self {
        Self {
            old_order,
            new_order,
            old_parent_ids,
            new_parent_ids,
        }
    }

    fn apply_parent_ids(layers: &mut [Layer], parent_ids: &HashMap<LayerId, Option<LayerId>>) {
        for layer in layers {
            if let Some(parent_id) = parent_ids.get(&layer.layer_id) {
                layer.parent_layer_id = parent_id.clone();
            }
        }
    }
}

impl Command for ReorderLayerCommand {
    fn execute(&mut self, project: &mut Project) {
        let mut new_order = self.new_order.clone();
        Self::apply_parent_ids(&mut new_order, &self.new_parent_ids);
        project.timeline.replace_layer_order(new_order);
    }

    fn undo(&mut self, project: &mut Project) {
        let mut old_order = self.old_order.clone();
        Self::apply_parent_ids(&mut old_order, &self.old_parent_ids);
        project.timeline.replace_layer_order(old_order);
    }

    fn description(&self) -> &str {
        "Reorder Layers"
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
}

impl GroupLayersCommand {
    pub fn new(selected_layer_ids: Vec<LayerId>, original_order: Vec<Layer>) -> Self {
        Self {
            selected_layer_ids,
            group_layer: None,
            original_order,
        }
    }
}

impl Command for GroupLayersCommand {
    fn execute(&mut self, project: &mut Project) {
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
            .position(|l| self.selected_layer_ids.contains(&l.layer_id))
            .unwrap_or(0);

        // Insert group layer
        project.timeline.insert_layer(insert_idx, group);

        // Reparent selected layers
        for layer in &mut project.timeline.layers {
            if self.selected_layer_ids.contains(&layer.layer_id) {
                layer.parent_layer_id = Some(group_id.clone());
            }
        }
        project.timeline.enforce_tree_order();
    }

    fn undo(&mut self, project: &mut Project) {
        // Restore the pre-group snapshot verbatim: the group layer is gone
        // (it isn't in `original_order`), parents are back, and sibling order
        // is reproduced exactly — same restore path as `UngroupLayersCommand`.
        project
            .timeline
            .replace_layer_order(self.original_order.clone());
    }

    fn description(&self) -> &str {
        "Group Layers"
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
        }
    }
}

impl Command for UngroupLayersCommand {
    fn execute(&mut self, project: &mut Project) {
        // Clear parent IDs on children
        if let Some(group) = &self.group_layer {
            for layer in &mut project.timeline.layers {
                if self.child_layer_ids.contains(&layer.layer_id)
                    && layer.parent_layer_id.as_ref() == Some(&group.layer_id)
                {
                    layer.parent_layer_id = None;
                }
            }
            // Remove group layer
            if let Some(idx) = project
                .timeline
                .layers
                .iter()
                .position(|l| l.layer_id == group.layer_id)
            {
                project.timeline.remove_layer(idx);
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        // Restore original order (includes group layer)
        project
            .timeline
            .replace_layer_order(self.original_order.clone());
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
/// still wins. See `docs/AUDIO_LAYER_DESIGN.md` §5 / `LAYER_CONTROLS_DESIGN.md` §5.3.
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

#[cfg(test)]
mod import_model_tests {
    use super::*;
    use manifold_core::effect_graph_def::{EFFECT_GRAPH_VERSION, EffectGraphDef};

    /// A minimal self-contained def stands in for a real assembled import
    /// graph: this command only stores the def and installs the layer, so the
    /// graph's internal shape is irrelevant to what it proves (the renderer
    /// crate's own tests cover assembly + rendering). What matters here is the
    /// *layer* it produces.
    fn stub_import_graph() -> EffectGraphDef {
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: Some("Azalea".to_string()),
            description: None,
            preset_metadata: None,
            nodes: Vec::new(),
            wires: Vec::new(),
        }
    }

    #[test]
    fn import_model_installs_generator_layer_with_override_graph() {
        let mut project = Project::default();
        let before = project.timeline.layers.len();
        let preset = PresetTypeId::new("azalea");

        let mut cmd = ImportModelLayerCommand::new(
            "Azalea".to_string(),
            preset.clone(),
            stub_import_graph(),
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
            &preset,
            "generator type must be the imported preset id (so the card names it)"
        );
        assert!(
            layer.generator_graph().is_some(),
            "the override graph must be installed on gen_params.graph — the renderer \
             reads it via create_with_override's from_def path"
        );
        assert_eq!(layer.generator_graph().unwrap().name.as_deref(), Some("Azalea"));
        assert!(
            layer.generator_graph_structure_version() >= 1,
            "structure version must be bumped so the generator renderer rebuilds \
             its chain against the authored graph"
        );
    }

    #[test]
    fn import_model_undo_removes_the_layer() {
        let mut project = Project::default();
        let before = project.timeline.layers.len();

        let mut cmd = ImportModelLayerCommand::new(
            "Azalea".to_string(),
            PresetTypeId::new("azalea"),
            stub_import_graph(),
            before,
            None,
        );
        cmd.execute(&mut project);
        assert_eq!(project.timeline.layers.len(), before + 1);

        cmd.undo(&mut project);
        assert_eq!(
            project.timeline.layers.len(),
            before,
            "undo must remove exactly the imported layer"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(name: &str, index: i32) -> Layer {
        Layer::new(name.to_string(), LayerType::Video, index)
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
