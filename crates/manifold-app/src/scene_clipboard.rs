//! The Scene tab's clipboard (SCENE_DOCUMENT_DESIGN.md section 7.2): Cmd+C
//! captures the selected items with everything under them, the Materials
//! they draw with and every param value, so Cmd+V pastes a working copy into
//! the same scene or another scene layer, as Blender copies objects between
//! files. A paste is Add with a captured source, one undo step.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::LayerId;
use manifold_core::project::Project;
use manifold_core::scene_doc::{Item, ItemFields, ItemId, ItemKind, SceneDoc, param_id, split_param_id};
use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::commands::scene_doc::AddSceneItemCommand;

use crate::scene_edit::SceneEdit;

/// What Cmd+C captured.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SceneItemClipboard {
    /// The layer the items were copied from: only there do their ids name
    /// the same items.
    source: LayerId,
    /// The copied items with their subtrees, in selection order.
    items: Vec<Item>,
    /// The Materials the copied items draw with that sit outside them. A
    /// paste back into the source layer brings one along only when it is
    /// gone, so the copy shares it, as Duplicate does; a paste into another
    /// layer always brings it, under a fresh id.
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
    Some(SceneItemClipboard { source: layer.clone(), items, materials, values })
}

/// Paste `clipboard` at the top of `layer`'s scene with fresh ids: the
/// Materials it brings first, then the items in copy order, each with its
/// copied values. Every pasted id is fresh and the copy's references point
/// inside the copy, as Blender's paste makes `.001` copies. A reference to
/// an item left behind holds only in the source layer, where it names the
/// same item; in another layer it falls back to its default, since an id
/// there (every base scene's "cube") names a different item. One undo step;
/// the first pasted item is selected.
pub(crate) fn paste(project: &Project, layer: &LayerId, clipboard: &SceneItemClipboard) -> Result<SceneEdit, String> {
    let doc = project.scene_of(layer).ok_or("This layer has no scene")?;
    let home = *layer == clipboard.source;
    let materials: Vec<&Item> =
        clipboard.materials.iter().filter(|material| !home || doc.find(&material.id).is_none()).collect();
    let mut map = BTreeMap::new();
    for root in clipboard.items.iter().chain(materials.iter().copied()) {
        fresh_ids(root, &mut map);
    }
    let values: Vec<(String, f32, f32)> = clipboard
        .values
        .iter()
        .filter_map(|(item, property, value, base)| Some((param_id(map.get(item)?, property), *value, *base)))
        .collect();
    let mut pasted = SceneDoc { items: materials.into_iter().chain(&clipboard.items).map(|item| item.remapped(&map)).collect() };
    let fresh: BTreeSet<&ItemId> = map.values().collect();
    pasted.retain_references(|id| fresh.contains(id) || (home && doc.find(id).is_some()));
    let select = clipboard.items.first().and_then(|item| map.get(&item.id)).cloned();
    let last = pasted.items.len().saturating_sub(1);
    let commands: Vec<Box<dyn Command>> = pasted
        .items
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


#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::scene_doc::{BASE_CUBE, BASE_CUBE_MATERIAL, CameraFields, CameraRig};
    use manifold_core::types::LayerType;
    use manifold_editing::undo::UndoRedoManager;

    /// Two scene layers, each holding the base scene.
    fn two_scenes() -> (Project, LayerId, LayerId) {
        let mut project = Project::default();
        let scene = PresetTypeId::from_string("Scene".to_string());
        let a = project.timeline.add_layer("A", LayerType::Generator, scene.clone());
        let b = project.timeline.add_layer("B", LayerType::Generator, scene);
        let (a, b) = (project.timeline.layers[a].layer_id.clone(), project.timeline.layers[b].layer_id.clone());
        (project, a, b)
    }

    fn instance<'p>(project: &'p mut Project, layer: &LayerId) -> &'p mut manifold_core::effects::PresetInstance {
        project.timeline.find_layer_by_id_mut(layer.as_str()).unwrap().1.gen_params_mut().unwrap()
    }

    fn run(project: &mut Project, edit: SceneEdit) {
        assert!(UndoRedoManager::new().execute(edit.command, project), "the paste applies");
    }

    /// A cube copied from one layer into another brings its own Material
    /// with its colour: it never binds the destination's "cube-material".
    #[test]
    fn a_cross_layer_paste_brings_its_material_under_a_fresh_id() {
        let (mut project, a, b) = two_scenes();
        let colour = instance(&mut project, &a)
            .params
            .iter()
            .map(|p| p.id().to_owned())
            .find(|id| split_param_id(id).is_some_and(|(item, _)| item == BASE_CUBE_MATERIAL))
            .expect("the cube's Material has a param");
        assert!(instance(&mut project, &a).set_base_param(&colour, 0.123));
        let b_before = instance(&mut project, &b).get_base_param(&colour);
        let clipboard = copy(&project, &a, &[ItemId::new(BASE_CUBE)]).expect("the cube copies");
        let edit = paste(&project, &b, &clipboard).unwrap();
        let pasted = edit.select.clone().expect("the paste selects its cube");
        run(&mut project, edit);
        let doc = project.scene_of(&b).unwrap();
        let material = doc.find(&pasted).unwrap().fields.material().cloned().expect("the pasted cube draws with a Material");
        assert_ne!(material.as_str(), BASE_CUBE_MATERIAL, "the paste bound the destination's own Material");
        assert!(doc.find(&material).is_some(), "the copied Material came along");
        let (_, property) = split_param_id(&colour).unwrap();
        let inst = instance(&mut project, &b);
        assert_eq!(inst.get_base_param(&param_id(&material, property)), 0.123, "the copied colour came along");
        assert_eq!(inst.get_base_param(&colour), b_before, "the destination's own Material is untouched");
    }

    /// A Look At camera pasted into another layer without its target aims at
    /// its point, not at that layer's own "cube"; with its target it aims at
    /// the pasted copy.
    #[test]
    fn a_cross_layer_paste_never_links_to_the_destinations_items() {
        let (mut project, a, b) = two_scenes();
        let camera = project.scene_of(&a).unwrap().cameras()[0].clone();
        let mut doc = project.scene_of(&a).unwrap().clone();
        doc.find_mut(&camera).unwrap().fields =
            ItemFields::Camera(CameraFields { rig: CameraRig::LookAt, target: Some(ItemId::new(BASE_CUBE)) });
        instance(&mut project, &a).install_scene_document(doc).unwrap();
        let target_of = |project: &Project, id: &ItemId| match &project.scene_of(&b).unwrap().find(id).unwrap().fields {
            ItemFields::Camera(fields) => fields.target.clone(),
            other => panic!("not a camera: {other:?}"),
        };

        let alone = copy(&project, &a, std::slice::from_ref(&camera)).unwrap();
        let edit = paste(&project, &b, &alone).unwrap();
        let pasted = edit.select.clone().unwrap();
        run(&mut project, edit);
        assert_eq!(target_of(&project, &pasted), None, "the camera aims at another layer's cube");

        let both = copy(&project, &a, &[camera.clone(), ItemId::new(BASE_CUBE)]).unwrap();
        let edit = paste(&project, &b, &both).unwrap();
        let pasted = edit.select.clone().unwrap();
        run(&mut project, edit);
        let target = target_of(&project, &pasted).expect("the camera keeps a target");
        assert_ne!(target.as_str(), BASE_CUBE, "the camera aims at the destination's own cube");
        assert!(project.scene_of(&b).unwrap().find(&target).is_some());
    }
}
