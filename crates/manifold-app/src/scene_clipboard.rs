//! The Scene tab's clipboard (SCENE_DOCUMENT_DESIGN.md section 7.2): Cmd+C
//! captures the selected items with everything under them, the Materials
//! they draw with and every param value, so Cmd+V pastes a working copy into
//! the same scene or another scene layer, as Blender copies objects between
//! files. A paste is Add with a captured source, one undo step.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::LayerId;
use manifold_core::project::Project;
use manifold_core::scene_doc::{Item, ItemFields, ItemId, ItemKind, param_id, split_param_id};
use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::commands::scene_doc::AddSceneItemCommand;

use crate::scene_edit::SceneEdit;

/// What Cmd+C captured.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SceneItemClipboard {
    /// The copied items with their subtrees, in selection order.
    items: Vec<Item>,
    /// The Materials the copied items draw with that sit outside them. A
    /// paste brings one along only where the destination lacks it, so a
    /// paste into the same scene shares it, as Duplicate does.
    materials: Vec<Item>,
    /// Every param of the copied items and those Materials:
    /// `(item, property, value, base)`.
    values: Vec<(ItemId, String, f32, f32)>,
}

/// Capture `ids` (and everything under them) from `layer`'s scene. An item
/// inside another copied item comes with it, not twice. The World stays.
/// `None` when nothing can be copied.
pub(crate) fn copy(project: &Project, layer: &LayerId, ids: &[ItemId]) -> Option<SceneItemClipboard> {
    let doc = project.scene_of(layer)?;
    let mut roots: Vec<&Item> = Vec::new();
    doc.walk(|item, ancestors| {
        let picked = ids.contains(&item.id) && !ancestors.iter().any(|ancestor| ids.contains(&ancestor.id));
        if picked && item.kind() != ItemKind::World {
            roots.push(item);
        }
    });
    roots.sort_by_key(|item| ids.iter().position(|id| *id == item.id));
    if roots.is_empty() {
        return None;
    }
    let mut copied = BTreeSet::new();
    for root in &roots {
        each(root, &mut |item| {
            copied.insert(item.id.clone());
        });
    }
    let mut materials: Vec<Item> = Vec::new();
    let mut items = Vec::with_capacity(roots.len());
    for root in roots {
        let mut item = root.clone();
        // A root drawing with a Material it inherits keeps drawing with it.
        if item.fields.material().is_none()
            && let Some(inherited) = doc.effective_material(&item.id)
        {
            bind(&mut item.fields, inherited);
        }
        each(&item, &mut |inner| {
            let used = if inner.id == item.id { item.fields.material().cloned() } else { doc.effective_material(&inner.id) };
            if let Some(material) = used.filter(|m| !copied.contains(m) && !materials.iter().any(|known| known.id == *m))
                && let Some(found) = doc.find(&material)
            {
                materials.push(found.clone());
            }
        });
        items.push(item);
    }
    for material in &materials {
        copied.insert(material.id.clone());
    }
    let instance = project.timeline.find_layer_by_id(layer.as_str())?.1.gen_params()?;
    let values = instance
        .params
        .iter()
        .filter_map(|p| {
            let (item, property) = split_param_id(p.id())?;
            let item = ItemId::new(item);
            copied.contains(&item).then(|| (item, property.to_owned(), p.value, p.base))
        })
        .collect();
    Some(SceneItemClipboard { items, materials, values })
}

/// Paste `clipboard` at the top of `layer`'s scene with fresh ids: the
/// Materials the scene lacks first, then the items in copy order, each with
/// its copied values. One undo step; the first pasted item is selected.
pub(crate) fn paste(project: &Project, layer: &LayerId, clipboard: &SceneItemClipboard) -> Result<SceneEdit, String> {
    let doc = project.scene_of(layer).ok_or("This layer has no scene")?;
    let materials: Vec<&Item> = clipboard.materials.iter().filter(|material| doc.find(&material.id).is_none()).collect();
    let mut map = BTreeMap::new();
    for root in clipboard.items.iter().chain(materials.iter().copied()) {
        fresh_ids(root, &mut map);
    }
    let values: Vec<(String, f32, f32)> = clipboard
        .values
        .iter()
        .filter_map(|(item, property, value, base)| Some((param_id(map.get(item)?, property), *value, *base)))
        .collect();
    let pasted: Vec<Item> = materials.into_iter().chain(&clipboard.items).map(|item| item.remapped(&map)).collect();
    let select = clipboard.items.first().and_then(|item| map.get(&item.id)).cloned();
    let last = pasted.len().saturating_sub(1);
    let commands: Vec<Box<dyn Command>> = pasted
        .into_iter()
        .enumerate()
        .map(|(k, item)| {
            let add = AddSceneItemCommand::with_subtree(layer.clone(), None, doc.items.len() + k, item);
            // Every pasted item is in by the last add, so it writes the values.
            Box::new(if k == last { add.with_values(values.clone()) } else { add }) as Box<dyn Command>
        })
        .collect();
    if commands.is_empty() {
        return Err("Nothing to paste".to_owned());
    }
    Ok(SceneEdit { command: Box::new(CompositeCommand::new(commands, "Paste Items".to_owned())), select })
}

/// A fresh id for `root` and everything under it, into `map`. An import's
/// parts stay keyed by file node path under the new root's id, as Duplicate
/// keeps them (`AddSceneItemCommand`), so a Model File swap still finds them.
fn fresh_ids(root: &Item, map: &mut BTreeMap<ItemId, ItemId>) {
    let new_root = ItemId::new(manifold_core::math::short_id());
    each(root, &mut |item| {
        let id = if item.id == root.id {
            new_root.clone()
        } else if let Some(rest) = item.id.as_str().strip_prefix(root.id.as_str()).and_then(|rest| rest.strip_prefix('-')) {
            ItemId::new(format!("{new_root}-{rest}"))
        } else {
            ItemId::new(manifold_core::math::short_id())
        };
        map.insert(item.id.clone(), id);
    });
}

fn each(item: &Item, f: &mut impl FnMut(&Item)) {
    f(item);
    for child in &item.children {
        each(child, f);
    }
}

/// Bind `material` on an item that holds a binding.
fn bind(fields: &mut ItemFields, material: ItemId) {
    match fields {
        ItemFields::Object(o) => o.material = Some(material),
        ItemFields::Group(g) => g.material = Some(material),
        ItemFields::Swarm(s) => s.material = Some(material),
        ItemFields::Water(w) => w.material = Some(material),
        _ => {}
    }
}

