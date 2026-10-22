//! The Scene tab's edits as commands (SCENE_DOCUMENT_DESIGN.md section 7.2).
//! Each builder reads the project it is given and returns the command a click
//! sends: one undo step, built from the document commands and plain param
//! writes. Nothing here touches the project.

use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_core::scene_doc::{CameraFields, CameraRig, FeatureSetting, ForceFields, ForceType, Item, ItemFields, ItemId, ItemKind, LightType, MaterialFields, SceneDoc, Shape, TextureSource, param_id};
use manifold_core::scene_schema::{self, FieldControl, FieldValue, add_entry, field_for, unique_name};
use manifold_core::{GraphTarget, LayerId};
use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::commands::effects::ChangeGraphParamCommand;
use manifold_editing::commands::scene_doc::{
    AddSceneItemCommand, RemoveSceneItemCommand, RenameSceneItemCommand, ReparentSceneItemCommand, SetSceneItemFieldsCommand,
    add_object_commands, group_commands, new_material, own_material_commands,
    move_modifier,
    remove_modifier,
    ungroup_commands,
};
use manifold_core::scene_surface::FieldPick;
use manifold_ui::param_surface::{MaterialFeature as UiFeature, MaterialLook as UiLook};

/// A built edit: the command and the item the selection should move to.
pub(crate) struct SceneEdit {
    pub command: Box<dyn Command>,
    pub select: Option<ItemId>,
}

fn doc<'a>(project: &'a Project, layer: &LayerId) -> Result<&'a SceneDoc, String> {
    project.scene_of(layer).ok_or_else(|| "This layer has no scene".to_owned())
}

fn instance<'a>(project: &'a Project, layer: &LayerId) -> Option<&'a PresetInstance> {
    project.timeline.find_layer_by_id(layer.as_str())?.1.gen_params()
}

fn base(inst: &PresetInstance, item: &ItemId, property: &str) -> Option<f32> {
    let id = param_id(item, property);
    inst.params.contains(&id).then(|| inst.get_base_param(&id))
}

fn write(layer: &LayerId, item: &ItemId, property: &str, old: f32, new: f32) -> Box<dyn Command> {
    Box::new(ChangeGraphParamCommand::new(GraphTarget::Generator(layer.clone()), param_id(item, property), old, new))
}

fn batch(commands: Vec<Box<dyn Command>>, description: &str, select: Option<ItemId>) -> SceneEdit {
    SceneEdit { command: Box::new(CompositeCommand::new(commands, description.to_owned())), select }
}

fn new_id() -> ItemId {
    ItemId::new(manifold_core::math::short_id())
}

/// Add the entry `key` of the Add menu under `parent` (the root when the
/// parent cannot hold it), named so no other item shares the name.
pub(crate) fn add(project: &Project, layer: &LayerId, key: &str, parent: Option<&ItemId>) -> Result<SceneEdit, String> {
    let doc = doc(project, layer)?;
    let entry = add_entry(key).ok_or_else(|| format!("There is no \"{key}\" to add"))?;
    let parent = parent.and_then(|id| doc.find(id)).filter(|p| manifold_core::scene_doc::can_hold(Some(p.kind()), entry.kind));
    let index = parent.map_or(doc.items.len(), |p| p.children.len());
    let parent = parent.map(|p| p.id.clone());
    let fields = entry.fields.ok_or_else(|| format!("{} opens the file picker", entry.label))?;
    let names = names(doc);
    let name = unique_name(entry.name, &names);
    let id = new_id();
    // An Object comes with its own Material, so its colour is on its card
    // at once.
    let mut commands: Vec<Box<dyn Command>> = if entry.kind == ItemKind::Object {
        let material = new_material(new_id(), unique_name(MATERIAL_NAME, &names));
        add_object_commands(layer, parent.clone(), index, Item::new(id.clone(), name, fields()), material)
    } else {
        vec![
            Box::new(AddSceneItemCommand::new(layer.clone(), entry.kind, parent.clone(), index, None, id.clone())),
            Box::new(SetSceneItemFieldsCommand::new(layer.clone(), id.clone(), fields())),
            Box::new(RenameSceneItemCommand::new(layer.clone(), id.clone(), name)),
        ]
    };
    if entry.kind == ItemKind::Swarm {
        let mut taken = names.iter().map(|taken| (*taken).to_owned()).collect::<Vec<_>>();
        for (offset, (base, force, values)) in SWARM_FORCES.iter().enumerate() {
            let force_id = new_id();
            let name = unique_name(base, &taken.iter().map(String::as_str).collect::<Vec<_>>());
            taken.push(name.clone());
            commands.push(Box::new(AddSceneItemCommand::new(layer.clone(), ItemKind::Force, parent.clone(), index + 1 + offset, None, force_id.clone())));
            commands.push(Box::new(SetSceneItemFieldsCommand::new(
                layer.clone(),
                force_id.clone(),
                ItemFields::Force(ForceFields { force: *force, ..ForceFields::default() }),
            )));
            commands.push(Box::new(RenameSceneItemCommand::new(layer.clone(), force_id.clone(), name)));
            // The force's params exist once it is added; they start at the stock recipe's values.
            commands.extend(values.iter().map(|(property, value)| write(layer, &force_id, property, 0.0, *value)));
        }
    }
    Ok(batch(commands, &format!("Add {}", entry.label), Some(id)))
}

/// What a new Material is called, numbered when taken.
const MATERIAL_NAME: &str = "Material";

fn names(doc: &SceneDoc) -> Vec<&str> {
    let mut names = Vec::new();
    doc.walk(|item, _| names.push(item.name.as_str()));
    names
}

/// A force a new Swarm brings: its name, type and starting values.
type SwarmForce = (&'static str, ForceType, &'static [(&'static str, f32)]);

/// What a new Swarm brings (Houdini's POP Attract in spring mode): an
/// Attractor and a Radial Force at the swarm's centre, and drifting gusts.
/// Beyond the core's radius the Attractor pulls at full strength; inside it
/// the push cancels the pull in proportion, so the pull grows with distance
/// from the centre. The gusts' Air Resistance relaxes every particle toward
/// the gusts, so the cloud breathes and swirls but never leaves, at stock
/// World gravity with no still air. Tuned on the CPU twin (`swarm_hold`) for
/// a 60 s clip. Forces that target everything act in the swarm's own frame.
const SWARM_FORCES: [SwarmForce; 3] = [
    ("Attractor", ForceType::Attractor, &[("strength", 20.0), ("radius", 2.5), ("falloff", 1.0), ("curve", 1.0)]),
    ("Core", ForceType::Radial, &[("strength", 20.0), ("radius", 1.5), ("falloff", 1.0), ("curve", 0.0)]),
    ("Gusts", ForceType::Wind, &[("speed", 0.5), ("turbulence", 5.0), ("turbulence_size", 0.8), ("air_resistance", 3.0)]),
];

pub(crate) fn remove(project: &Project, layer: &LayerId, item: &ItemId) -> Result<SceneEdit, String> {
    let found = doc(project, layer)?.find(item).ok_or("That item is no longer in the scene")?;
    if found.kind() == ItemKind::World {
        return Err("The World cannot be removed".to_owned());
    }
    Ok(SceneEdit { command: Box::new(RemoveSceneItemCommand::new(layer.clone(), item.clone())), select: None })
}

/// Duplicate an item and everything under it, right after the original.
pub(crate) fn duplicate(project: &Project, layer: &LayerId, item: &ItemId) -> Result<SceneEdit, String> {
    let doc = doc(project, layer)?;
    let found = doc.find(item).ok_or("That item is no longer in the scene")?;
    if found.kind() == ItemKind::World {
        return Err("The World cannot be duplicated".to_owned());
    }
    let (parent, index) = doc.locate(item).ok_or("That item is no longer in the scene")?;
    let id = new_id();
    let command = AddSceneItemCommand::new(layer.clone(), found.kind(), parent, index + 1, Some(item.clone()), id.clone());
    Ok(SceneEdit { command: Box::new(command), select: Some(id) })
}

pub(crate) fn group(project: &Project, layer: &LayerId, items: &[ItemId]) -> Result<SceneEdit, String> {
    let doc = doc(project, layer)?;
    let id = new_id();
    let commands = group_commands(layer, doc, items, id.clone());
    if commands.is_empty() {
        return Err("Select the items to group first".to_owned());
    }
    Ok(batch(commands, "Group", Some(id)))
}

pub(crate) fn ungroup(project: &Project, layer: &LayerId, group: &ItemId) -> Result<SceneEdit, String> {
    let commands = ungroup_commands(layer, doc(project, layer)?, group);
    if commands.is_empty() {
        return Err("Select a group to ungroup".to_owned());
    }
    Ok(batch(commands, "Ungroup", None))
}

/// A structural field set to one of its row's values. `None` when the item
/// already holds it.
pub(crate) fn set_field(project: &Project, layer: &LayerId, item: &ItemId, field: &str, pick: &FieldPick) -> Result<Option<SceneEdit>, String> {
    let doc = doc(project, layer)?;
    let found = doc.find(item).ok_or("That item is no longer in the scene")?;
    let row = field_for(&found.fields, field).ok_or_else(|| format!("{field} is not a setting of this item"))?;
    let value = match pick {
        FieldPick::Choice(key) => FieldValue::Choice(key.clone()),
        FieldPick::Item(target) => FieldValue::Item(target.clone()),
        FieldPick::Layer(target) => FieldValue::Layer(target.clone()),
        FieldPick::File(path) => FieldValue::File(path.clone()),
        FieldPick::All => FieldValue::Items(Vec::new()),
        FieldPick::Toggle(target) => {
            let FieldValue::Items(mut set) = (row.read)(&found.fields) else {
                return Err(format!("{field} does not take a set of items"));
            };
            match set.iter().position(|member| member == target) {
                Some(at) => {
                    set.remove(at);
                }
                None => set.push(target.clone()),
            }
            FieldValue::Items(set)
        }
    };
    if matches!(row.control, FieldControl::File { .. }) && !matches!(value, FieldValue::File(_)) {
        return Err(format!("{field} takes a file"));
    }
    let fields = scene_schema::set_field(doc, found, row, &value)?;
    if fields == found.fields {
        return Ok(None);
    }
    if let (ItemFields::Object(old), ItemFields::Object(new)) = (&found.fields, &fields)
        && old.shape != new.shape
    {
        return Ok(Some(batch(shape_change(layer, doc, found, &old.shape, fields.clone()), "Set Shape", None)));
    }
    Ok(Some(SceneEdit { command: Box::new(SetSceneItemFieldsCommand::new(layer.clone(), item.clone(), fields)), select: None }))
}

/// A Shape change, one undo step: the new Shape; a default name follows it
/// (Cube becomes Sphere), a name the user gave stays; and an imported
/// object that takes a built-in drops its file, so the Material it alone
/// draws with stops reading the file's maps.
fn shape_change(layer: &LayerId, doc: &SceneDoc, object: &Item, old: &Shape, fields: ItemFields) -> Vec<Box<dyn Command>> {
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    if let (Shape::Mesh { .. }, ItemFields::Object(new)) = (old, &fields)
        && !matches!(new.shape, Shape::Mesh { .. })
        && let Some(material) = new.material.as_ref().and_then(|id| doc.find(id))
        && let ItemFields::Material(material_fields) = &material.fields
        && matches!(material_fields.texture, TextureSource::Model { .. })
        && material_users(doc, &material.id) == 1
    {
        let plain = ItemFields::Material(MaterialFields { texture: TextureSource::None, ..material_fields.clone() });
        commands.push(Box::new(SetSceneItemFieldsCommand::new(layer.clone(), material.id.clone(), plain)));
    }
    if let ItemFields::Object(new) = &fields
        && scene_schema::has_shape_name(&object.name, old)
    {
        let names: Vec<&str> = names(doc).into_iter().filter(|name| *name != object.name).collect();
        let name = unique_name(&scene_schema::shape_name(&new.shape), &names);
        commands.push(Box::new(RenameSceneItemCommand::new(layer.clone(), object.id.clone(), name)));
    }
    commands.insert(0, Box::new(SetSceneItemFieldsCommand::new(layer.clone(), object.id.clone(), fields)));
    commands
}

/// How many Objects and Groups draw with `material`.
fn material_users(doc: &SceneDoc, material: &ItemId) -> usize {
    let mut users = 0;
    doc.walk(|item, _| {
        if matches!(item.kind(), ItemKind::Object | ItemKind::Group) && doc.effective_material(&item.id).as_ref() == Some(material) {
            users += 1;
        }
    });
    users
}

pub(crate) fn core_feature(feature: UiFeature) -> manifold_core::material_inspector::MaterialFeature {
    use manifold_core::material_inspector::MaterialFeature as Core;
    match feature {
        UiFeature::Coat => Core::Coat,
        UiFeature::Iridescence => Core::Iridescence,
        UiFeature::Emission => Core::Emission,
        UiFeature::Glass => Core::Glass,
        UiFeature::Sheen => Core::Sheen,
        UiFeature::Anisotropy => Core::Anisotropy,
        UiFeature::Translucency => Core::Translucency,
        UiFeature::Subsurface => Core::Subsurface,
    }
}

/// What a newly added feature starts at, so it shows when it appears: the
/// material params its rows edit, set only where the performer has not moved
/// them.
fn feature_seeds(feature: UiFeature) -> &'static [(&'static str, f32)] {
    match feature {
        UiFeature::Coat => &[("clearcoat", 1.0)],
        UiFeature::Iridescence => &[("iridescence", 1.0)],
        UiFeature::Emission => &[("emission_r", 1.0), ("emission_g", 1.0), ("emission_b", 1.0), ("emission_intensity", 1.0)],
        UiFeature::Glass => &[("transmission", 1.0)],
        UiFeature::Sheen => &[("sheen_color_r", 0.5), ("sheen_color_g", 0.5), ("sheen_color_b", 0.5)],
        UiFeature::Anisotropy => &[("anisotropy_strength", 0.5)],
        UiFeature::Translucency => &[("translucency", 0.5)],
        UiFeature::Subsurface => &[("subsurface_weight", 0.5)],
    }
}

/// Add a feature to a material (with its starting values, in the same undo
/// step) or remove it.
pub(crate) fn material_feature(project: &Project, layer: &LayerId, material: &ItemId, feature: UiFeature, add: bool) -> Result<Option<SceneEdit>, String> {
    let doc = doc(project, layer)?;
    let found = doc.find(material).ok_or("That material is no longer in the scene")?;
    let ItemFields::Material(fields) = &found.fields else {
        return Err("That item is not a material".to_owned());
    };
    let core = core_feature(feature);
    let present = fields.features.iter().any(|setting| setting.feature == core);
    if present == add {
        return Ok(None);
    }
    let mut features = fields.features.clone();
    if add {
        features.push(FeatureSetting { feature: core, mode: 2 });
    } else {
        features.retain(|setting| setting.feature != core);
    }
    let edited = ItemFields::Material(MaterialFields { features, ..fields.clone() });
    let mut commands: Vec<Box<dyn Command>> = vec![Box::new(SetSceneItemFieldsCommand::new(layer.clone(), material.clone(), edited))];
    if add && let Some(inst) = instance(project, layer) {
        for &(property, seed) in feature_seeds(feature) {
            let id = param_id(material, property);
            let Some(param) = inst.params.get(&id) else { continue };
            let held = param.base;
            if (held - param.spec.default_value).abs() <= f32::EPSILON && (held - seed).abs() > f32::EPSILON {
                commands.push(write(layer, material, property, held, seed));
            }
        }
    }
    Ok(Some(batch(commands, if add { "Add Material Feature" } else { "Remove Material Feature" }, None)))
}

/// Give `owner` a Material for it alone: a copy of the one it shares with
/// others, so edits to it stop moving them (Make Unique), or a new one when
/// it draws with none (New Material).
pub(crate) fn make_material_unique(project: &Project, layer: &LayerId, owner: &ItemId) -> Result<SceneEdit, String> {
    let doc = doc(project, layer)?;
    let description = if doc.effective_material(owner).is_some() { "Make Material Unique" } else { "New Material" };
    let name = unique_name(MATERIAL_NAME, &names(doc));
    let commands = own_material_commands(layer, doc, owner, new_id(), &name)?;
    Ok(batch(commands, description, None))
}

/// Apply a starting look to a Material: its features become the look's and
/// the look's factors are written, in one undo step. Default puts every
/// factor back and takes every feature off.
pub(crate) fn material_look(project: &Project, layer: &LayerId, material: &ItemId, look: UiLook) -> Result<Option<SceneEdit>, String> {
    let doc = doc(project, layer)?;
    let found = doc.find(material).ok_or("That material is no longer in the scene")?;
    let ItemFields::Material(fields) = &found.fields else {
        return Err("That item is not a material".to_owned());
    };
    let inst = instance(project, layer).ok_or("There is no scene to edit")?;
    let recipe = crate::scene_material_look::recipe(look);
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    let features: Vec<FeatureSetting> =
        recipe.features.iter().map(|feature| FeatureSetting { feature: core_feature(*feature), mode: 2 }).collect();
    if features != fields.features {
        let edited = ItemFields::Material(MaterialFields { features, ..fields.clone() });
        commands.push(Box::new(SetSceneItemFieldsCommand::new(layer.clone(), material.clone(), edited)));
    }
    for (property, value) in crate::scene_material_look::writes(inst, &crate::scene_material_look::param_ids(inst, material), look) {
        let Some(param) = inst.params.get(&param_id(material, &property)) else { continue };
        if (param.base - value).abs() > f32::EPSILON {
            commands.push(write(layer, material, &property, param.base, value));
        }
    }
    if commands.is_empty() {
        return Ok(None);
    }
    Ok(Some(batch(commands, &format!("Material Look: {}", look.label()), None)))
}

pub(crate) fn rename(project: &Project, layer: &LayerId, item: &ItemId, name: &str) -> Result<Option<SceneEdit>, String> {
    let found = doc(project, layer)?.find(item).ok_or("That item is no longer in the scene")?;
    let name = name.trim();
    if name.is_empty() || name == found.name {
        return Ok(None);
    }
    Ok(Some(SceneEdit { command: Box::new(RenameSceneItemCommand::new(layer.clone(), item.clone(), name)), select: None }))
}

/// Point the first Sun at the brightest part of the HDRI `file`, keeping its
/// distance from the point it aims at.
pub(crate) fn aim_sun_at_hdri(project: &Project, layer: &LayerId, file: &str) -> Result<SceneEdit, String> {
    let doc = doc(project, layer)?;
    let inst = instance(project, layer).ok_or("There is no scene to aim")?;
    let mut sun = None;
    doc.walk(|item, _| {
        if sun.is_none() && matches!(&item.fields, ItemFields::Light(light) if light.light == LightType::Sun) {
            sun = Some(item.id.clone());
        }
    });
    let sun = sun.ok_or("This scene has no Sun light to aim")?;
    let toward = manifold_nodes_scene::node_graph::hdri_sun::hdri_sun_direction(std::path::Path::new(file))
        .map_err(|reason| format!("Could not aim the Sun: {reason}"))?;
    let read = |property: &str| base(inst, &sun, property).unwrap_or(0.0);
    let aim = ["aim_x", "aim_y", "aim_z"].map(read);
    let pos = ["pos_x", "pos_y", "pos_z"].map(read);
    let apart = (0..3).map(|k| (pos[k] - aim[k]).powi(2)).sum::<f32>().sqrt();
    let distance = if apart > 1e-3 { apart } else { 5.0 };
    let writes: Vec<Box<dyn Command>> = ["pos_x", "pos_y", "pos_z"]
        .into_iter()
        .enumerate()
        .filter_map(|(k, property)| {
            let (old, new) = (pos[k], aim[k] + toward[k] * distance);
            ((old - new).abs() > f32::EPSILON).then(|| write(layer, &sun, property, old, new))
        })
        .collect();
    if writes.is_empty() {
        return Err("The Sun already points at the brightest part of the HDRI".to_owned());
    }
    Ok(batch(writes, "Aim Sun at HDRI", None))
}

/// The scene's camera: the World's active camera, else the first Camera.
fn camera_of(doc: &SceneDoc) -> Option<(ItemId, CameraFields)> {
    let active = doc.world().and_then(|world| match &world.fields {
        ItemFields::World(fields) => fields.active_camera.clone(),
        _ => None,
    });
    let mut found = None;
    doc.walk(|item, _| {
        if let ItemFields::Camera(camera) = &item.fields
            && (found.is_none() || active.as_ref() == Some(&item.id))
        {
            found = Some((item.id.clone(), camera.clone()));
        }
    });
    found
}

/// Each item's world origin, as the viewport gizmos place it.
fn item_origins(doc: &SceneDoc, inst: &PresetInstance) -> impl Fn(&ItemId) -> Option<[f32; 3]> {
    let values = |id: &str| inst.params.contains(id).then(|| inst.get_base_param(id));
    let vm = manifold_nodes_scene::node_graph::scene_vm::SceneVm::from_doc(doc, &values);
    move |id| manifold_nodes_scene::node_graph::viewport_gizmo::gizmo_targets(&vm).find(|target| target.row.as_ref() == Some(id)).map(|target| target.origin)
}

/// Pull the camera back to show `item`: an Orbit camera lifts its pivot to
/// the item's height and backs off; a Look At camera aims at it.
pub(crate) fn frame(project: &Project, layer: &LayerId, item: &ItemId) -> Result<SceneEdit, String> {
    let doc = doc(project, layer)?;
    let inst = instance(project, layer).ok_or("There is no scene to frame")?;
    let (camera, fields) = camera_of(doc).ok_or("This scene has no camera")?;
    let origin_of = item_origins(doc, inst);
    let at = origin_of(item).ok_or("This item has no position to frame")?;
    // A Look At camera tracking an item aims at its origin plus Target X/Y/Z.
    let tracked = fields.target.as_ref().and_then(origin_of).unwrap_or([0.0; 3]);
    let radius = 1.0f32;
    let read = |property: &str| base(inst, &camera, property).unwrap_or(0.0);
    let mut writes = Vec::new();
    let mut set = |property: &str, new: f32| {
        let old = read(property);
        if (old - new).abs() > f32::EPSILON {
            writes.push(write(layer, &camera, property, old, new));
        }
    };
    match fields.rig {
        CameraRig::Orbit => {
            let offset = (at[0] * at[0] + at[2] * at[2]).sqrt();
            set("look_y", at[1]);
            set("distance", 2.2 * (radius + offset));
        }
        CameraRig::LookAt => {
            let pos = ["pos_x", "pos_y", "pos_z"].map(read);
            let offset = ["target_x", "target_y", "target_z"].map(read);
            let aim: [f32; 3] = std::array::from_fn(|k| tracked[k] + offset[k]);
            let mut dir = [aim[0] - pos[0], aim[1] - pos[1], aim[2] - pos[2]];
            let len = dir.iter().map(|d| d * d).sum::<f32>().sqrt();
            dir = if len > 1e-6 { dir.map(|d| d / len) } else { [0.0, 0.0, -1.0] };
            for (k, property) in ["target_x", "target_y", "target_z"].into_iter().enumerate() {
                set(property, at[k] - tracked[k]);
            }
            for (k, property) in ["pos_x", "pos_y", "pos_z"].into_iter().enumerate() {
                set(property, at[k] - dir[k] * 2.2 * radius);
            }
        }
        CameraRig::Free => return Err("Frame is not supported for a free camera".to_owned()),
    }
    if writes.is_empty() {
        return Err("The camera already frames this item".to_owned());
    }
    Ok(batch(writes, "Frame camera on item", None))
}

/// The camera params that put `camera` at the editor viewport's view
/// (SCENE_SURFACES D14). `None` when the camera already shows it.
pub(crate) fn copy_view(
    project: &Project,
    layer: &LayerId,
    camera: &ItemId,
    view: &manifold_node_engine::scene::viewport_camera::ViewportCamera,
) -> Result<Option<SceneEdit>, String> {
    use manifold_nodes_scene::node_graph::view_copy::{ViewCopyCamera, camera_writes_from_view};
    let doc = doc(project, layer)?;
    let inst = instance(project, layer).ok_or("This layer has no scene")?;
    let kind = match &doc.find(camera).ok_or("That camera is no longer in the scene")?.fields {
        ItemFields::Camera(fields) => match fields.rig {
            CameraRig::Orbit => ViewCopyCamera::Orbit,
            CameraRig::Free => ViewCopyCamera::Free,
            CameraRig::LookAt => {
                let tracked = fields.target.as_ref().and_then(item_origins(doc, inst)).unwrap_or([0.0; 3]);
                ViewCopyCamera::LookAt { tracked }
            }
        },
        _ => return Err("That item is not a camera".to_owned()),
    };
    let look_y = base(inst, camera, "look_y").unwrap_or(0.0);
    let writes: Vec<Box<dyn Command>> = camera_writes_from_view(kind, view, look_y)
        .into_iter()
        .filter_map(|(property, new)| {
            let old = base(inst, camera, property)?;
            ((old - new).abs() > f32::EPSILON).then(|| write(layer, camera, property, old, new))
        })
        .collect();
    if writes.is_empty() {
        return Ok(None);
    }
    Ok(Some(batch(writes, "Use this view", None)))
}

/// Move an item `delta` places among its siblings. `None` at either end.
pub(crate) fn nudge(project: &Project, layer: &LayerId, item: &ItemId, delta: i32) -> Result<Option<SceneEdit>, String> {
    let doc = doc(project, layer)?;
    let (parent, index) = doc.locate(item).ok_or("That item is no longer in the scene")?;
    let siblings = match &parent {
        Some(id) => doc.find(id).map_or(0, |p| p.children.len()),
        None => doc.items.len(),
    };
    let target = (index as i64 + i64::from(delta)).clamp(0, siblings.saturating_sub(1) as i64) as usize;
    if target == index {
        return Ok(None);
    }
    Ok(Some(SceneEdit { command: Box::new(ReparentSceneItemCommand::new(layer.clone(), item.clone(), parent, target)), select: None }))
}

/// Items copied with Cmd+C in the Scene tab. A paste into the same scene
/// duplicates them; the document has no cross-scene paste yet.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SceneItemClipboard {
    pub layer: LayerId,
    pub items: Vec<ItemId>,
}

/// Remove an Object's modifier `key` (`to` is `None`) or move it to `to` in
/// its list: one undo step either way.
pub(crate) fn modifier_edit(project: &Project, layer: &LayerId, object: &ItemId, key: &str, to: Option<usize>) -> Result<SceneEdit, String> {
    let doc = doc(project, layer)?;
    let (command, description) = match to {
        None => (remove_modifier(layer, doc, object, key)?, "Remove Modifier"),
        Some(index) => (move_modifier(layer, doc, object, key, index)?, "Move Modifier"),
    };
    Ok(batch(vec![Box::new(command)], description, Some(object.clone())))
}
