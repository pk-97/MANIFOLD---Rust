//! Where a scene item sits in its render scene right now.
//!
//! Panel selections, gizmo owners, scene actions and commands name an object
//! or a light by its [`SceneNodeRef`]. The render slot (`object_k`, `light_k`)
//! is only order: this module turns an address into the slot it fills, at the
//! moment an edit runs against the live graph, and refuses with a reason when
//! the address no longer names anything. Scenes are root-level, as the panel
//! shows them.

use crate::effect_graph_def::{EffectGraphDef, EffectGraphWire, RENDER_SCENE_TYPE_ID};
use crate::scene_modifier_preset::{SceneNodeRef, SceneNodeRefExt};
use crate::scene_object_model::{SceneObjectModel, scene_object_models};

/// The `render_scene` node (document id) and the slot an item fills in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneSlot {
    pub render_id: u32,
    pub index: u32,
}

const GONE_OBJECT: &str = "That object is no longer in this scene";
const GONE_LIGHT: &str = "That light is no longer in this scene";

fn render_scenes(def: &EffectGraphDef) -> impl Iterator<Item = (u32, SceneNodeRef)> + '_ {
    def.nodes.iter().filter(|node| node.type_id == RENDER_SCENE_TYPE_ID).filter_map(|node| {
        SceneNodeRef::locate(def, &node.node_id).map(|reference| (node.id, reference))
    })
}

/// The slot `object` fills. A compound model's parent row has no slot of its
/// own: it answers with its first part's.
pub fn object_slot(def: &EffectGraphDef, object: &SceneNodeRef) -> Result<SceneSlot, String> {
    for (render_id, scene) in render_scenes(def) {
        let Ok(models) = scene_object_models(def, &scene) else { continue };
        let found = models.iter().find(|model| &model.object == object);
        let index = match found {
            Some(model) => model.slot.or_else(|| first_part_slot(&models, object)),
            None => None,
        };
        if let Some(index) = index {
            return Ok(SceneSlot { render_id, index });
        }
    }
    Err(GONE_OBJECT.into())
}

fn first_part_slot(models: &[SceneObjectModel], parent: &SceneNodeRef) -> Option<u32> {
    models.iter().filter(|model| model.parent.as_ref() == Some(parent)).filter_map(|model| model.slot).min()
}

/// The slot `light` fills.
pub fn light_slot(def: &EffectGraphDef, light: &SceneNodeRef) -> Result<SceneSlot, String> {
    if !light.scope.is_empty() {
        return Err(GONE_LIGHT.into());
    }
    let node = light.resolve(def).ok_or(GONE_LIGHT)?;
    for (render_id, _) in render_scenes(def) {
        let wire = def.wires.iter().find(|wire| wire.from_node == node.id && wire.to_node == render_id && slot_of(wire, "light_").is_some());
        if let Some(index) = wire.and_then(|wire| slot_of(wire, "light_")) {
            return Ok(SceneSlot { render_id, index });
        }
    }
    Err(GONE_LIGHT.into())
}

fn slot_of(wire: &EffectGraphWire, prefix: &str) -> Option<u32> {
    wire.to_port.strip_prefix(prefix)?.parse().ok()
}

/// The object row filling `render_id`'s slot `index`, as an address: what a
/// caller holding only a slot number (an importer's fresh scene, a test)
/// hands to the address-taking commands.
pub fn object_at_slot(def: &EffectGraphDef, render_id: u32, index: u32) -> Option<SceneNodeRef> {
    let render = def.nodes.iter().find(|node| node.id == render_id)?;
    let scene = SceneNodeRef::locate(def, &render.node_id)?;
    scene_object_models(def, &scene).ok()?.into_iter().find(|model| model.slot == Some(index)).map(|model| model.object)
}

/// The light wired to `render_id`'s slot `index`, as an address.
pub fn light_at_slot(def: &EffectGraphDef, render_id: u32, index: u32) -> Option<SceneNodeRef> {
    let wire = def.wires.iter().find(|wire| wire.to_node == render_id && slot_of(wire, "light_") == Some(index))?;
    let node = def.nodes.iter().find(|node| node.id == wire.from_node)?;
    SceneNodeRef::locate(def, &node.node_id)
}
