use crate::command::Command;
use manifold_core::LayerId;
use manifold_core::project::Project;
use manifold_core::layer::Layer;
use manifold_core::types::{LayerType, GeneratorType};
use std::collections::HashMap;

/// Add a new layer to the timeline.
#[derive(Debug)]
pub struct AddLayerCommand {
    layer: Option<Layer>,
    name: String,
    layer_type: LayerType,
    gen_type: GeneratorType,
    insert_index: usize,
    parent_group_id: Option<LayerId>,
}

impl AddLayerCommand {
    pub fn new(
        name: String,
        layer_type: LayerType,
        gen_type: GeneratorType,
        insert_index: usize,
        parent_group_id: Option<LayerId>,
    ) -> Self {
        Self { layer: None, name, layer_type, gen_type, insert_index, parent_group_id }
    }
}

impl Command for AddLayerCommand {
    fn execute(&mut self, project: &mut Project) {
        let layer = if let Some(existing) = self.layer.take() {
            existing
        } else {
            let mut new_layer = Layer::new(self.name.clone(), self.layer_type, 0);
            new_layer.parent_layer_id = self.parent_group_id.clone();
            if self.layer_type == LayerType::Generator {
                let gp = new_layer.gen_params.get_or_insert_with(Default::default);
                gp.generator_type = self.gen_type;
            }
            new_layer
        };
        self.layer = Some(layer.clone());
        project.timeline.insert_layer(self.insert_index, layer);
    }

    fn undo(&mut self, project: &mut Project) {
        // Find the layer we inserted by ID
        if let Some(layer) = &self.layer
            && let Some(idx) = project.timeline.layers.iter().position(|l| l.layer_id == layer.layer_id) {
                project.timeline.remove_layer(idx);
            }
    }

    fn description(&self) -> &str { "Add Layer" }
}

/// Delete a layer from the timeline.
#[derive(Debug)]
pub struct DeleteLayerCommand {
    layer: Option<Layer>,
    layer_index: usize,
}

impl DeleteLayerCommand {
    pub fn new(layer: Layer, layer_index: usize) -> Self {
        Self { layer: Some(layer), layer_index }
    }
}

impl Command for DeleteLayerCommand {
    fn execute(&mut self, project: &mut Project) {
        if self.layer_index < project.timeline.layers.len() {
            self.layer = project.timeline.remove_layer(self.layer_index);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(layer) = self.layer.take() {
            let idx = self.layer_index.min(project.timeline.layers.len());
            project.timeline.insert_layer(idx, layer.clone());
            self.layer = Some(layer);
        }
    }

    fn description(&self) -> &str { "Delete Layer" }
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
        Self { old_order, new_order, old_parent_ids, new_parent_ids }
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

    fn description(&self) -> &str { "Reorder Layers" }
}

/// Group selected layers into a new group layer.
#[derive(Debug)]
pub struct GroupLayersCommand {
    selected_layer_ids: Vec<LayerId>,
    group_layer: Option<Layer>,
    #[allow(dead_code)]
    original_order: Vec<Layer>,
    original_parent_ids: HashMap<LayerId, Option<LayerId>>,
}

impl GroupLayersCommand {
    pub fn new(
        selected_layer_ids: Vec<LayerId>,
        original_order: Vec<Layer>,
    ) -> Self {
        let original_parent_ids = original_order.iter()
            .map(|l| (l.layer_id.clone(), l.parent_layer_id.clone()))
            .collect();
        Self { selected_layer_ids, group_layer: None, original_order, original_parent_ids }
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
        let insert_idx = project.timeline.layers.iter()
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
    }

    fn undo(&mut self, project: &mut Project) {
        // Remove group layer
        if let Some(group) = &self.group_layer
            && let Some(idx) = project.timeline.layers.iter().position(|l| l.layer_id == group.layer_id) {
                project.timeline.remove_layer(idx);
            }
        // Restore parent IDs
        for layer in &mut project.timeline.layers {
            if let Some(parent_id) = self.original_parent_ids.get(&layer.layer_id) {
                layer.parent_layer_id = parent_id.clone();
            }
        }
    }

    fn description(&self) -> &str { "Group Layers" }
}

/// Rename a layer (undoable).
#[derive(Debug)]
pub struct RenameLayerCommand {
    layer_index: usize,
    old_name: String,
    new_name: String,
}

impl RenameLayerCommand {
    pub fn new(layer_index: usize, old_name: String, new_name: String) -> Self {
        Self { layer_index, old_name, new_name }
    }
}

impl Command for RenameLayerCommand {
    fn execute(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index) {
            layer.name = self.new_name.clone();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(layer) = project.timeline.layers.get_mut(self.layer_index) {
            layer.name = self.old_name.clone();
        }
    }

    fn description(&self) -> &str { "Rename Layer" }
}

/// Ungroup a group layer, dissolving it.
#[derive(Debug)]
pub struct UngroupLayersCommand {
    group_layer: Option<Layer>,
    #[allow(dead_code)]
    group_index: usize,
    child_layer_ids: Vec<LayerId>,
    original_order: Vec<Layer>,
}

impl UngroupLayersCommand {
    pub fn new(
        group_layer: Layer,
        group_index: usize,
        child_layer_ids: Vec<LayerId>,
        original_order: Vec<Layer>,
    ) -> Self {
        Self { group_layer: Some(group_layer), group_index, child_layer_ids, original_order }
    }
}

impl Command for UngroupLayersCommand {
    fn execute(&mut self, project: &mut Project) {
        // Clear parent IDs on children
        if let Some(group) = &self.group_layer {
            for layer in &mut project.timeline.layers {
                if self.child_layer_ids.contains(&layer.layer_id) && layer.parent_layer_id.as_ref() == Some(&group.layer_id) {
                    layer.parent_layer_id = None;
                }
            }
            // Remove group layer
            if let Some(idx) = project.timeline.layers.iter().position(|l| l.layer_id == group.layer_id) {
                project.timeline.remove_layer(idx);
            }
        }
    }

    fn undo(&mut self, project: &mut Project) {
        // Restore original order (includes group layer)
        project.timeline.replace_layer_order(self.original_order.clone());
    }

    fn description(&self) -> &str { "Ungroup Layers" }
}
