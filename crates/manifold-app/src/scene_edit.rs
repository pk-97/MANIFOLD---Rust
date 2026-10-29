//! The Scene tab's edits as commands (SCENE_DOCUMENT_DESIGN.md section 7.2).
//! Each builder reads the project it is given and returns the command a click
//! sends: one undo step, built from the document commands and plain param
//! writes. Nothing here touches the project.

use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_core::scene_doc::{
    CameraFields, CameraRig, FeatureSetting, ForceFields, ForceTargets, ForceType, Item,
    ItemFields, ItemId, ItemKind, LightType, MaterialFields, MaterialType, SandFields, SceneDoc,
    Shape, SourceRole, TextureFeeds, TextureSource, WaterFields, WaterSourceFields, param_id,
};
use manifold_core::scene_schema::{
    self, FieldControl, FieldValue, add_entry, field_for, unique_name,
};
use manifold_core::scene_surface::FieldPick;
use manifold_core::{GraphTarget, LayerId};
use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::commands::effects::ChangeGraphParamCommand;
use manifold_editing::commands::scene_doc::{
    AddSceneItemCommand, RemoveSceneItemCommand, RenameSceneItemCommand, ReparentSceneItemCommand,
    SetSceneItemFieldsCommand, add_modifier, add_object_commands, group_commands, move_modifier,
    new_material, new_modifier, own_material_commands, ungroup_commands,
};
use manifold_ui::param_surface::{MaterialFeature as UiFeature, MaterialLook as UiLook};

/// A built edit: the command and the item the selection should move to.
pub(crate) struct SceneEdit {
    pub command: Box<dyn Command>,
    pub select: Option<ItemId>,
}

fn doc<'a>(project: &'a Project, layer: &LayerId) -> Result<&'a SceneDoc, String> {
    project
        .scene_of(layer)
        .ok_or_else(|| "This layer has no scene".to_owned())
}

fn instance<'a>(project: &'a Project, layer: &LayerId) -> Option<&'a PresetInstance> {
    project
        .timeline
        .find_layer_by_id(layer.as_str())?
        .1
        .gen_params()
}

fn base(inst: &PresetInstance, item: &ItemId, property: &str) -> Option<f32> {
    let id = param_id(item, property);
    inst.params.contains(&id).then(|| inst.get_base_param(&id))
}

fn write(layer: &LayerId, item: &ItemId, property: &str, old: f32, new: f32) -> Box<dyn Command> {
    Box::new(ChangeGraphParamCommand::new(
        GraphTarget::Generator(layer.clone()),
        param_id(item, property),
        old,
        new,
    ))
}

fn batch(commands: Vec<Box<dyn Command>>, description: &str, select: Option<ItemId>) -> SceneEdit {
    SceneEdit {
        command: Box::new(CompositeCommand::new(commands, description.to_owned())),
        select,
    }
}

fn new_id() -> ItemId {
    ItemId::new(manifold_core::math::short_id())
}

/// Add the entry `key` of the Add menu under `parent` (the root when the
/// parent cannot hold it), named so no other item shares the name. A
/// Modifier card joins the end of the chain, aimed at the `selected` Objects
/// and Groups, or the whole scene when none is selected.
pub(crate) fn add(
    project: &Project,
    layer: &LayerId,
    key: &str,
    parent: Option<&ItemId>,
    selected: &[ItemId],
) -> Result<SceneEdit, String> {
    let doc = doc(project, layer)?;
    let entry = add_entry(key).ok_or_else(|| format!("There is no \"{key}\" to add"))?;
    let parent = parent
        .and_then(|id| doc.find(id))
        .filter(|p| manifold_core::scene_doc::can_hold(Some(p.kind()), entry.kind));
    let index = parent.map_or(doc.items.len(), |p| p.children.len());
    let parent = parent.map(|p| p.id.clone());
    let fields = entry
        .fields
        .ok_or_else(|| format!("{} opens the file picker", entry.label))?;
    if let Some(reason) = manifold_core::scene_schema::add_refused(doc, &fields()) {
        return Err(reason.to_owned());
    }
    let names = names(doc);
    let name = unique_name(entry.name, &names);
    let id = new_id();
    if let ItemFields::Modifier(card) = fields() {
        let mut item = new_modifier(doc, id.clone(), card.modifier, selected);
        if let ItemFields::Modifier(made) = &item.fields
            && manifold_core::scene_schema::acts_on_copies(made.modifier)
            && let Some(reason) = manifold_core::scene_schema::copies_refused(doc, &made.targets)
        {
            return Err(reason.to_owned());
        }
        item.name = name;
        let mut commands: Vec<Box<dyn Command>> = vec![Box::new(add_modifier(layer, doc, item))];
        if key == "modifier.scene_loop"
            && !scene_schema::has_loop_camera(doc)
            && let Some(world) = doc.world().map(|world| world.id.clone())
            && let Some(inst) = instance(project, layer)
        {
            let cam_id = new_id();
            let cam_index = doc
                .items
                .iter()
                .rposition(|item| item.kind() == ItemKind::Camera)
                .map_or(doc.items.len(), |last| last + 1);
            let cut = doc.upcoming_cut();
            let pick = base(inst, &world, manifold_core::scene_doc::CAMERA_PICK).unwrap_or(0.0);
            commands.push(Box::new(AddSceneItemCommand::new(
                layer.clone(),
                ItemKind::Camera,
                None,
                cam_index,
                None,
                cam_id.clone(),
            )));
            commands.push(Box::new(SetSceneItemFieldsCommand::new(
                layer.clone(),
                cam_id.clone(),
                ItemFields::Camera(CameraFields {
                    rig: CameraRig::Loop,
                    ..CameraFields::default()
                }),
            )));
            commands.push(Box::new(RenameSceneItemCommand::new(
                layer.clone(),
                cam_id.clone(),
                unique_name(LOOP_CAMERA_NAME, &names),
            )));
            commands.push(write(
                layer,
                &world,
                manifold_core::scene_doc::CAMERA_PICK,
                pick,
                cut as f32,
            ));
        }
        return Ok(batch(commands, &format!("Add {}", entry.label), Some(id)));
    }
    // An Object comes with its own Material, so its colour is on its card
    // at once.
    let mut commands: Vec<Box<dyn Command>> = if entry.kind == ItemKind::Object {
        let mut material = new_material(new_id(), unique_name(MATERIAL_NAME, &names));
        if key == LAYER_PLANE {
            // The first other layer in the stack; the Texture row re-picks it.
            let layer_source = project
                .timeline
                .layers
                .iter()
                .find(|other| other.layer_id != *layer);
            material.fields = ItemFields::Material(MaterialFields {
                material_type: MaterialType::Unlit,
                texture: layer_source.map_or(TextureSource::None, |other| TextureSource::Layer {
                    layer: other.layer_id.clone(),
                }),
                ..MaterialFields::default()
            });
        }
        let material_id = material.id.clone();
        let mut commands = add_object_commands(
            layer,
            parent.clone(),
            index,
            Item::new(id.clone(), name.clone(), fields()),
            material,
        );
        if key == LAYER_PLANE {
            // Alpha cutout, so a layer's transparent pixels leave the plane clear.
            commands.push(write(layer, &material_id, "alpha_mode", 0.0, ALPHA_MASK));
        }
        commands
    } else {
        vec![
            Box::new(AddSceneItemCommand::new(
                layer.clone(),
                entry.kind,
                parent.clone(),
                index,
                None,
                id.clone(),
            )),
            Box::new(SetSceneItemFieldsCommand::new(
                layer.clone(),
                id.clone(),
                fields(),
            )),
            Box::new(RenameSceneItemCommand::new(
                layer.clone(),
                id.clone(),
                name.clone(),
            )),
        ]
    };
    // The params exist once the item is added; a starting point writes over the recipe's defaults.
    commands.extend(
        scene_schema::entry_starts(key)
            .iter()
            .map(|(property, value)| write(layer, &id, property, 0.0, *value)),
    );
    if entry.kind == ItemKind::Swarm {
        let mut taken = names
            .iter()
            .map(|taken| (*taken).to_owned())
            .collect::<Vec<_>>();
        // Forces sit at the top of the scene: after the swarm when it is
        // there too, else at the end.
        let force_index = if parent.is_none() {
            index + 1
        } else {
            doc.items.len()
        };
        for (offset, (role, force, values)) in SWARM_FORCES.iter().enumerate() {
            let force_id = new_id();
            let force_name = unique_name(
                &format!("{name} {role}"),
                &taken.iter().map(String::as_str).collect::<Vec<_>>(),
            );
            taken.push(force_name.clone());
            commands.push(Box::new(AddSceneItemCommand::new(
                layer.clone(),
                ItemKind::Force,
                None,
                force_index + offset,
                None,
                force_id.clone(),
            )));
            commands.push(Box::new(SetSceneItemFieldsCommand::new(
                layer.clone(),
                force_id.clone(),
                // Aimed at this swarm only, not every body, water or other swarm.
                ItemFields::Force(ForceFields {
                    force: *force,
                    targets: ForceTargets::Items(vec![id.clone()]),
                }),
            )));
            commands.push(Box::new(RenameSceneItemCommand::new(
                layer.clone(),
                force_id.clone(),
                force_name,
            )));
            // The force's params exist once it is added; they start at the stock recipe's values.
            commands.extend(
                values
                    .iter()
                    .map(|(property, value)| write(layer, &force_id, property, 0.0, *value)),
            );
        }
    }
    if entry.kind.holds_sources() {
        let fill = new_id();
        commands.push(Box::new(AddSceneItemCommand::new(
            layer.clone(),
            ItemKind::WaterSource,
            Some(id.clone()),
            0,
            None,
            fill.clone(),
        )));
        commands.push(Box::new(SetSceneItemFieldsCommand::new(
            layer.clone(),
            fill.clone(),
            ItemFields::WaterSource(WaterSourceFields {
                role: SourceRole::Fill,
                ..WaterSourceFields::default()
            }),
        )));
        commands.push(Box::new(RenameSceneItemCommand::new(
            layer.clone(),
            fill.clone(),
            unique_name("Fill", &names),
        )));
        // Its params exist once it is added; they start at the column's box.
        commands.extend(
            WATER_FILL
                .iter()
                .map(|(property, value)| write(layer, &fill, property, 0.0, *value)),
        );
        // Its own Material, as an Object's, starting at its template's look
        // and, for a Sand, at what it is made of.
        let made = fields();
        let matter = match &made {
            ItemFields::Sand(sand) => Some(sand.matter),
            _ => None,
        };
        let material_name = matter.map_or(WATER_MATERIAL_NAME, |matter| matter.name());
        let material = new_material(new_id(), unique_name(material_name, &names));
        let material_id = material.id.clone();
        commands.push(Box::new(AddSceneItemCommand::with_subtree(
            layer.clone(),
            Some(id.clone()),
            1,
            material,
        )));
        let fields = match made {
            ItemFields::Water(water) => ItemFields::Water(WaterFields {
                material: Some(material_id.clone()),
                ..water
            }),
            ItemFields::Sand(sand) => ItemFields::Sand(SandFields {
                material: Some(material_id.clone()),
                ..sand
            }),
            other => other,
        };
        commands.push(Box::new(SetSceneItemFieldsCommand::new(
            layer.clone(),
            id.clone(),
            fields,
        )));
        // The template's look, with what a Sand is made of over it.
        let look = matter.map_or_else(
            || scene_schema::water_look(entry.kind),
            scene_schema::sand_look,
        );
        commands.extend(
            writable_look(look)
                .into_iter()
                .map(|(property, value)| write(layer, &material_id, &property, 0.0, value)),
        );
    }
    // A Material added inside a Water or Sand is that item's look.
    if entry.kind == ItemKind::Material
        && let Some(domain) = parent.as_ref().and_then(|parent| doc.find(parent))
    {
        let fields = match &domain.fields {
            ItemFields::Water(fields) => Some(ItemFields::Water(WaterFields {
                material: Some(id.clone()),
                ..fields.clone()
            })),
            ItemFields::Sand(fields) => Some(ItemFields::Sand(SandFields {
                material: Some(id.clone()),
                ..fields.clone()
            })),
            _ => None,
        };
        if let Some(fields) = fields {
            commands.push(Box::new(SetSceneItemFieldsCommand::new(
                layer.clone(),
                domain.id.clone(),
                fields,
            )));
        }
    }
    Ok(batch(commands, &format!("Add {}", entry.label), Some(id)))
}

/// What a new Water brings (Blender's Quick Liquid fills its domain the same
/// way): a Fill, the FLIP Fluids Dam Break column standing against the left
/// wall of the default four-metre domain, so play shows a dam break at once.
const WATER_FILL: [(&str, f32); 6] = [
    ("pos_x", -1.25),
    ("pos_y", 1.12),
    ("pos_z", 0.0),
    ("scale_x", 1.18),
    ("scale_y", 1.92),
    ("scale_z", 3.5),
];

/// What a new Water's own Material is called, numbered when taken.
const WATER_MATERIAL_NAME: &str = "Water";

/// What a new Material is called, numbered when taken.
const MATERIAL_NAME: &str = "Material";

/// The Add entry that makes a plane showing a layer: Unlit, alpha cutout.
const LAYER_PLANE: &str = "object.layer_plane";

/// Alpha Mode "Mask" on the unlit and PBR material nodes.
const ALPHA_MASK: f32 = 1.0;

fn names(doc: &SceneDoc) -> Vec<&str> {
    let mut names = Vec::new();
    doc.walk(|item, _| names.push(item.name.as_str()));
    names
}

/// A force a new Swarm brings: its name, type and starting values.
type SwarmForce = (&'static str, ForceType, &'static [(&'static str, f32)]);

/// What a new Swarm brings (Houdini's POP Attract in spring mode), each named
/// after the swarm and aimed at it: two Radial forces at its centre, a Pull
/// and a Core, and drifting Gusts. Beyond the Core's radius the Pull draws in
/// at full strength; inside it the Core's push cancels the pull in
/// proportion, so the pull grows with distance from the centre. The Gusts'
/// Air Resistance relaxes every particle toward the gusts, so the cloud
/// breathes and swirls but never leaves, at stock World gravity with no
/// still air, tuned for a 60 s clip.
const SWARM_FORCES: [SwarmForce; 3] = [
    (
        "Pull",
        ForceType::Radial,
        &[
            ("strength", -20.0),
            ("impulse_strength", -2.0),
            ("radius", 2.5),
            ("falloff", 1.0),
            ("curve", 1.0),
        ],
    ),
    (
        "Core",
        ForceType::Radial,
        &[
            ("strength", 20.0),
            ("radius", 1.5),
            ("falloff", 1.0),
            ("curve", 0.0),
        ],
    ),
    (
        "Gusts",
        ForceType::Wind,
        &[
            ("speed", 0.5),
            ("turbulence", 5.0),
            ("turbulence_size", 0.8),
            ("air_resistance", 3.0),
        ],
    ),
];

/// Remove each of `items` and everything under it, in one undo step. An item
/// inside another removed item goes with it, and so does every Material only
/// removed items drew with.
pub(crate) fn remove(
    project: &Project,
    layer: &LayerId,
    items: &[ItemId],
) -> Result<SceneEdit, String> {
    let doc = doc(project, layer)?;
    if items.iter().any(|item| {
        doc.find(item)
            .is_some_and(|found| found.kind() == ItemKind::World)
    }) {
        return Err("The World cannot be removed".to_owned());
    }
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    let mut gone = std::collections::HashSet::new();
    doc.walk(|item, ancestors| {
        let inside = ancestors.iter().any(|above| items.contains(&above.id));
        if items.contains(&item.id) && !inside {
            commands.push(Box::new(RemoveSceneItemCommand::new(
                layer.clone(),
                item.id.clone(),
            )));
        }
        if inside || items.contains(&item.id) {
            gone.insert(item.id.clone());
        }
    });
    if commands.is_empty() {
        return Err("That item is no longer in the scene".to_owned());
    }
    // A Material goes when every item drawing with it goes.
    let effective = doc.effective_materials();
    let mut leaving: std::collections::HashMap<&ItemId, usize> = std::collections::HashMap::new();
    doc.walk(|item, _| {
        if gone.contains(&item.id)
            && manifold_core::scene_doc::draws_with_material(item)
            && let Some(material) = effective.get(&item.id)
        {
            *leaving.entry(material).or_default() += 1;
        }
    });
    let users = doc.material_users();
    doc.walk(|item, _| {
        if leaving
            .get(&item.id)
            .is_some_and(|count| users.get(&item.id) == Some(count))
            && !gone.contains(&item.id)
        {
            commands.push(Box::new(RemoveSceneItemCommand::new(
                layer.clone(),
                item.id.clone(),
            )));
        }
    });
    Ok(batch(commands, "Remove", None))
}

/// Duplicate each of `items` and everything under it, right after its
/// original, in one undo step (Cmd+D). An item inside another duplicated
/// item comes with it. The last item's copy is selected.
pub(crate) fn duplicate(
    project: &Project,
    layer: &LayerId,
    items: &[ItemId],
) -> Result<SceneEdit, String> {
    let doc = doc(project, layer)?;
    let mut copies = Vec::new();
    doc.walk(|item, ancestors| {
        if items.contains(&item.id)
            && item.kind() != ItemKind::World
            && !ancestors.iter().any(|a| items.contains(&a.id))
        {
            copies.push(item);
        }
    });
    if copies.is_empty() {
        return Err(if items.is_empty() {
            "Select the items to duplicate first"
        } else {
            "The World cannot be duplicated"
        }
        .to_owned());
    }
    let mut placed: Vec<(Option<ItemId>, usize, &Item, ItemId)> = copies
        .into_iter()
        .filter_map(|item| {
            doc.locate(&item.id)
                .map(|(parent, index)| (parent, index, item, new_id()))
        })
        .collect();
    let select = items
        .iter()
        .rev()
        .find_map(|id| placed.iter().find(|(_, _, item, _)| item.id == *id))
        .map(|(.., copy)| copy.clone());
    // Later siblings first, so each copy lands right after its own original.
    placed.sort_by(|a, b| b.1.cmp(&a.1));
    let commands: Vec<Box<dyn Command>> = placed
        .into_iter()
        .map(|(parent, index, item, id)| {
            Box::new(AddSceneItemCommand::new(
                layer.clone(),
                item.kind(),
                parent,
                index + 1,
                Some(item.id.clone()),
                id,
            )) as Box<dyn Command>
        })
        .collect();
    Ok(batch(commands, "Duplicate", select))
}

pub(crate) fn group(
    project: &Project,
    layer: &LayerId,
    items: &[ItemId],
) -> Result<SceneEdit, String> {
    let doc = doc(project, layer)?;
    let id = new_id();
    let commands = group_commands(layer, doc, items, id.clone());
    if commands.is_empty() {
        return Err("Select the items to group first".to_owned());
    }
    Ok(batch(commands, "Group", Some(id)))
}

pub(crate) fn ungroup(
    project: &Project,
    layer: &LayerId,
    group: &ItemId,
) -> Result<SceneEdit, String> {
    let commands = ungroup_commands(layer, doc(project, layer)?, group);
    if commands.is_empty() {
        return Err("Select a group to ungroup".to_owned());
    }
    Ok(batch(commands, "Ungroup", None))
}

/// A structural field set to one of its row's values. `None` when the item
/// already holds it.
pub(crate) fn set_field(
    project: &Project,
    layer: &LayerId,
    item: &ItemId,
    field: &str,
    pick: &FieldPick,
) -> Result<Option<SceneEdit>, String> {
    let doc = doc(project, layer)?;
    let found = doc
        .find(item)
        .ok_or("That item is no longer in the scene")?;
    let row = field_for(&found.fields, field)
        .ok_or_else(|| format!("{field} is not a setting of this item"))?;
    let value = match pick {
        FieldPick::Choice(key) => FieldValue::Choice(key.clone()),
        FieldPick::Item(target) => FieldValue::Item(target.clone()),
        FieldPick::Layer(target) => FieldValue::Layer(target.clone()),
        FieldPick::File(path) => FieldValue::File(path.clone()),
        FieldPick::All => FieldValue::Everything,
        FieldPick::Toggle(target) => {
            let mut set = match (row.read)(&found.fields) {
                FieldValue::Items(set) => set,
                FieldValue::Everything => Vec::new(),
                _ => return Err(format!("{field} does not take a set of items")),
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
        let mut commands = shape_change(layer, doc, found, &old.shape, fields.clone());
        if new.shape == Shape::Ocean {
            commands.extend(ocean_material(project, layer, doc, found, &fields)?);
        }
        return Ok(Some(batch(commands, "Set Shape", None)));
    }
    // A Sand made of something else keeps its domain, cells and fill, and
    // takes the new matter's look and surface, and its name while it still
    // has the old matter's.
    if let (ItemFields::Sand(old), ItemFields::Sand(new)) = (&found.fields, &fields)
        && old.matter != new.matter
    {
        let mut commands: Vec<Box<dyn Command>> = vec![Box::new(SetSceneItemFieldsCommand::new(
            layer.clone(),
            item.clone(),
            fields.clone(),
        ))];
        commands.extend(swap_writes(
            project,
            layer,
            item,
            scene_schema::matter_starts(old.matter),
            scene_schema::matter_starts(new.matter),
        ));
        let mut renamed = vec![(item.clone(), found.name.clone())];
        if let Some(material) = &new.material {
            commands.extend(swap_writes(
                project,
                layer,
                material,
                &writable_look(scene_schema::sand_look(old.matter)),
                &writable_look(scene_schema::sand_look(new.matter)),
            ));
            if let Some(found) = doc.find(material) {
                renamed.push((material.clone(), found.name.clone()));
            }
        }
        // As when added, the Sand and its material share the one fresh name.
        let renamed: Vec<_> = renamed
            .into_iter()
            .filter(|(_, name)| named_after(name, old.matter.name()))
            .collect();
        let taken: Vec<&str> = names(doc)
            .into_iter()
            .filter(|name| !renamed.iter().any(|(_, old)| old == name))
            .collect();
        let fresh = unique_name(new.matter.name(), &taken);
        for (id, _) in renamed {
            commands.push(Box::new(RenameSceneItemCommand::new(
                layer.clone(),
                id,
                fresh.clone(),
            )));
        }
        return Ok(Some(batch(commands, "Set Matter", None)));
    }
    // A layer fed into Emission shows at once: an emission the performer has
    // not touched starts as the Emission feature does, white at full strength.
    if let (ItemFields::Material(old), ItemFields::Material(new)) = (&found.fields, &fields)
        && old.texture_feeds != TextureFeeds::Emission
        && new.texture_feeds == TextureFeeds::Emission
    {
        let mut commands: Vec<Box<dyn Command>> = vec![Box::new(SetSceneItemFieldsCommand::new(
            layer.clone(),
            item.clone(),
            fields,
        ))];
        commands.extend(seed_writes(
            project,
            layer,
            item,
            feature_seeds(UiFeature::Emission),
        ));
        return Ok(Some(batch(commands, "Set Layer Feeds", None)));
    }
    Ok(Some(SceneEdit {
        command: Box::new(SetSceneItemFieldsCommand::new(
            layer.clone(),
            item.clone(),
            fields,
        )),
        select: None,
    }))
}

/// The material file `path` names: the `.mtlx` itself, or the one `.mtlx` in
/// a folder, as material downloads unpack. `None` when there is none.
pub(crate) fn material_file_of(
    path: &std::path::Path,
) -> Option<Result<std::path::PathBuf, String>> {
    let is_materialx =
        |p: &std::path::Path| manifold_core::materialx::is_materialx(&p.to_string_lossy());
    if !path.is_dir() {
        return is_materialx(path).then(|| Ok(path.to_path_buf()));
    }
    let mut files: Vec<_> = std::fs::read_dir(path)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|p| is_materialx(p))
        .collect();
    match files.len() {
        0 => None,
        1 => files.pop().map(Ok),
        n => Some(Err(format!(
            "That folder holds {n} material files; drop the one you want"
        ))),
    }
}

/// The Material a dropped material file lands on: the selected Material, or
/// the selected Object's Material.
pub(crate) fn drop_material_target(
    project: &Project,
    layer: &LayerId,
    item: &ItemId,
) -> Result<ItemId, String> {
    let doc = doc(project, layer)?;
    match &doc
        .find(item)
        .ok_or("That item is no longer in the scene")?
        .fields
    {
        ItemFields::Material(_) => Ok(item.clone()),
        ItemFields::Object(object) => object
            .material
            .clone()
            .ok_or_else(|| "This object has no Material to take the file".to_owned()),
        _ => Err("Select a Material or an Object, then drop the material file".to_owned()),
    }
}

/// A Material takes the MaterialX file `file`, one undo step: its texture
/// becomes the file's material, and every row the performer has not touched
/// starts at the file's value, so the material looks as the file says
/// (box mapped, its roughness from the picture). The file's facts must be on
/// the document already, so the copy compiled here sees its values.
pub(crate) fn set_material_file(
    project: &Project,
    layer: &LayerId,
    item: &ItemId,
    file: &str,
) -> Result<SceneEdit, String> {
    let doc = doc(project, layer)?;
    let found = doc
        .find(item)
        .ok_or("That Material is no longer in the scene")?;
    let ItemFields::Material(old) = &found.fields else {
        return Err("A material file goes on a Material".to_owned());
    };
    let fields = ItemFields::Material(MaterialFields {
        texture: TextureSource::Model {
            file: file.to_owned(),
            material: Some(0),
        },
        ..old.clone()
    });
    let inst = instance(project, layer).ok_or("This layer has no scene")?;
    let mut probe = inst.clone();
    let mut probe_doc = doc.clone();
    if let Some(material) = probe_doc.find_mut(item) {
        material.fields = fields.clone();
    }
    probe.install_scene_document(probe_doc)?;
    let prefix = format!("{}.", item.as_str());
    let mut commands: Vec<Box<dyn Command>> = vec![Box::new(SetSceneItemFieldsCommand::new(
        layer.clone(),
        item.clone(),
        fields,
    ))];
    for started in probe.params.iter() {
        let Some(property) = started.id().strip_prefix(&prefix) else {
            continue;
        };
        let Some(held) = inst.params.get(started.id()) else {
            continue;
        };
        let untouched = (held.base - held.spec.default_value).abs() <= f32::EPSILON;
        if untouched && (held.base - started.spec.default_value).abs() > f32::EPSILON {
            commands.push(write(
                layer,
                item,
                property,
                held.base,
                started.spec.default_value,
            ));
        }
    }
    Ok(batch(commands, "Set Material File", None))
}

/// A Shape change, one undo step: the new Shape; a default name follows it
/// (Cube becomes Sphere), a name the user gave stays; and an imported
/// object that takes a built-in drops its file, so the Material it alone
/// draws with stops reading the file's maps.
fn shape_change(
    layer: &LayerId,
    doc: &SceneDoc,
    object: &Item,
    old: &Shape,
    fields: ItemFields,
) -> Vec<Box<dyn Command>> {
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    if let (Shape::Mesh { .. }, ItemFields::Object(new)) = (old, &fields)
        && !matches!(new.shape, Shape::Mesh { .. })
        && let Some(material) = new.material.as_ref().and_then(|id| doc.find(id))
        && let ItemFields::Material(material_fields) = &material.fields
        && matches!(material_fields.texture, TextureSource::Model { .. })
        && material_users(doc, &material.id) == 1
    {
        let plain = ItemFields::Material(MaterialFields {
            texture: TextureSource::None,
            ..material_fields.clone()
        });
        commands.push(Box::new(SetSceneItemFieldsCommand::new(
            layer.clone(),
            material.id.clone(),
            plain,
        )));
    }
    if let ItemFields::Object(new) = &fields
        && scene_schema::has_shape_name(&object.name, old)
    {
        let names: Vec<&str> = names(doc)
            .into_iter()
            .filter(|name| *name != object.name)
            .collect();
        let name = unique_name(&scene_schema::shape_name(&new.shape), &names);
        commands.push(Box::new(RenameSceneItemCommand::new(
            layer.clone(),
            object.id.clone(),
            name,
        )));
    }
    commands.insert(
        0,
        Box::new(SetSceneItemFieldsCommand::new(
            layer.clone(),
            object.id.clone(),
            fields,
        )),
    );
    commands
}

/// A new sea looks like water at once: the Ocean's own Material takes the
/// water look ([`scene_schema::OCEAN_LOOK`]). A Material it shares or
/// inherits is left alone and the Ocean gets its own, named Water. Runs
/// after the Shape is set, so `fields` are the Ocean's.
fn ocean_material(
    project: &Project,
    layer: &LayerId,
    doc: &SceneDoc,
    object: &Item,
    fields: &ItemFields,
) -> Result<Vec<Box<dyn Command>>, String> {
    let Some(inst) = instance(project, layer) else {
        return Ok(Vec::new());
    };
    let ItemFields::Object(new) = fields else {
        return Ok(Vec::new());
    };
    let own = new
        .material
        .clone()
        .filter(|material| material_users(doc, material) == 1);
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    let (material, source) = match own {
        Some(material) => (material.clone(), Some(material)),
        None => {
            let source = doc.effective_material(&object.id);
            let mut staged = doc.clone();
            if let Some(item) = staged.find_mut(&object.id) {
                item.fields = fields.clone();
            }
            let material = new_id();
            let name = unique_name(WATER_MATERIAL_NAME, &names(doc));
            commands.extend(own_material_commands(
                layer,
                &staged,
                &object.id,
                material.clone(),
                &name,
            )?);
            (material, source)
        }
    };
    for (property, value) in scene_schema::OCEAN_LOOK {
        // A new Material starts at its source's values (or its own
        // defaults); undo removes it whole, so its old value only needs to
        // be a number.
        let old = source
            .as_ref()
            .and_then(|source| base(inst, source, property))
            .unwrap_or(value);
        if source.as_ref() != Some(&material) || (old - value).abs() > f32::EPSILON {
            commands.push(write(layer, &material, property, old, value));
        }
    }
    Ok(commands)
}

/// How many Objects, Groups and Waters draw with `material`.
fn material_users(doc: &SceneDoc, material: &ItemId) -> usize {
    doc.material_users().get(material).copied().unwrap_or(0)
}

pub(crate) fn core_feature(
    feature: UiFeature,
) -> manifold_core::material_inspector::MaterialFeature {
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
/// them. Sheen at roughness 0 has no lobe at all; Blender starts it at 0.5.
fn feature_seeds(feature: UiFeature) -> &'static [(&'static str, f32)] {
    match feature {
        UiFeature::Coat => &[("clearcoat", 1.0)],
        UiFeature::Iridescence => &[("iridescence", 1.0)],
        UiFeature::Emission => &[
            ("emission_r", 1.0),
            ("emission_g", 1.0),
            ("emission_b", 1.0),
            ("emission_intensity", 1.0),
        ],
        UiFeature::Glass => &[("transmission", 1.0)],
        UiFeature::Sheen => &[
            ("sheen_color_r", 0.5),
            ("sheen_color_g", 0.5),
            ("sheen_color_b", 0.5),
            ("sheen_roughness", 0.5),
        ],
        UiFeature::Anisotropy => &[("anisotropy_strength", 0.5)],
        UiFeature::Translucency => &[("translucency", 0.5)],
        UiFeature::Subsurface => &[("subsurface_weight", 0.5)],
    }
}

/// The features whose light passes through the surface. A metal passes none
/// (glTF and Blender scale them by one minus metallic), so they show only
/// below Metallic 1.
pub(crate) const PASSES_LIGHT: [manifold_core::material_inspector::MaterialFeature; 3] = {
    use manifold_core::material_inspector::MaterialFeature as Core;
    [Core::Glass, Core::Translucency, Core::Subsurface]
};

/// Whether a Material at this Metallic is full metal everywhere, where
/// [`PASSES_LIGHT`] features cannot show. With a metallic-roughness map fed,
/// metal is per texel and the map decides, so the factor alone never is.
pub(crate) fn full_metal(doc: &SceneDoc, fields: &MaterialFields, metallic: f32) -> bool {
    metallic >= 1.0 - 1e-4
        && !manifold_nodes_scene::node_graph::scene_compile::fed_map_ports(doc, fields)
            .contains(&"mr_map")
}

/// Add a feature to a material or remove it, in one undo step. An add writes
/// its starting values wherever the performer has not moved them; a feature
/// light passes through, added to a full metal, also sets Metallic 0. Remove
/// keeps the feature and its values, switched to Removed, so adding it again
/// brings them back.
pub(crate) fn material_feature(
    project: &Project,
    layer: &LayerId,
    material: &ItemId,
    feature: UiFeature,
    add: bool,
) -> Result<Option<SceneEdit>, String> {
    let doc = doc(project, layer)?;
    let found = doc
        .find(material)
        .ok_or("That material is no longer in the scene")?;
    let ItemFields::Material(fields) = &found.fields else {
        return Err("That item is not a material".to_owned());
    };
    let core = core_feature(feature);
    let held = fields
        .features
        .iter()
        .position(|setting| setting.feature == core);
    let mut features = fields.features.clone();
    match (held, add) {
        (Some(index), true) if features[index].removed() => {
            features[index].mode = FeatureSetting::ON
        }
        (None, true) => features.push(FeatureSetting::on(core)),
        (Some(index), false) if !features[index].removed() => {
            features[index].mode = FeatureSetting::REMOVED
        }
        _ => return Ok(None),
    }
    let edited = ItemFields::Material(MaterialFields {
        features,
        ..fields.clone()
    });
    let mut commands: Vec<Box<dyn Command>> = vec![Box::new(SetSceneItemFieldsCommand::new(
        layer.clone(),
        material.clone(),
        edited,
    ))];
    if add {
        commands.extend(seed_writes(
            project,
            layer,
            material,
            feature_seeds(feature),
        ));
        if PASSES_LIGHT.contains(&core)
            && let Some(metallic) =
                instance(project, layer).and_then(|inst| base(inst, material, "metallic"))
            && full_metal(doc, fields, metallic)
        {
            commands.push(write(layer, material, "metallic", metallic, 0.0));
        }
    }
    Ok(Some(batch(
        commands,
        if add {
            "Add Material Feature"
        } else {
            "Remove Material Feature"
        },
        None,
    )))
}

/// Writes moving each of `material`'s `seeds` params to its seed value, only
/// where the performer has left it at its default.
/// A look's values a Material takes from it: its feature modes follow the
/// features field and its internal params stay the renderer's.
fn writable_look(look: Vec<(String, f32)>) -> Vec<(String, f32)> {
    let family = scene_schema::MATERIAL_FAMILY;
    look.into_iter()
        .filter(|(property, _)| {
            !family.automatic.contains(&property.as_str())
                && !family.internal.iter().any(|(param, _)| param == property)
        })
        .collect()
}

/// The writes that move `item` from the values `old` set to those `new`
/// sets: each of `new`'s, and its default for any only `old` set, each
/// undoing to what it holds now.
fn swap_writes<A: AsRef<str>, B: AsRef<str>>(
    project: &Project,
    layer: &LayerId,
    item: &ItemId,
    old: &[(A, f32)],
    new: &[(B, f32)],
) -> Vec<Box<dyn Command>> {
    let Some(inst) = instance(project, layer) else {
        return Vec::new();
    };
    let dropped = old
        .iter()
        .filter(|(property, _)| !new.iter().any(|(set, _)| set.as_ref() == property.as_ref()))
        .filter_map(|(property, _)| {
            let param = inst.params.get(&param_id(item, property.as_ref()))?;
            Some((property.as_ref(), param.spec.default_value))
        });
    new.iter()
        .map(|(property, value)| (property.as_ref(), *value))
        .chain(dropped)
        .filter_map(|(property, value)| {
            let held = inst.params.get(&param_id(item, property))?.base;
            ((held - value).abs() > f32::EPSILON).then(|| write(layer, item, property, held, value))
        })
        .collect()
}

/// Whether `name` is still `base` or a numbered copy of it ("Snow 2").
fn named_after(name: &str, base: &str) -> bool {
    name == base
        || name
            .strip_prefix(base)
            .and_then(|rest| rest.strip_prefix(' '))
            .is_some_and(|n| n.parse::<u32>().is_ok())
}

fn seed_writes(
    project: &Project,
    layer: &LayerId,
    material: &ItemId,
    seeds: &[(&str, f32)],
) -> Vec<Box<dyn Command>> {
    let Some(inst) = instance(project, layer) else {
        return Vec::new();
    };
    let mut commands = Vec::new();
    for &(property, seed) in seeds {
        let Some(param) = inst.params.get(&param_id(material, property)) else {
            continue;
        };
        let held = param.base;
        if (held - param.spec.default_value).abs() <= f32::EPSILON
            && (held - seed).abs() > f32::EPSILON
        {
            commands.push(write(layer, material, property, held, seed));
        }
    }
    commands
}

/// Give `owner` a Material for it alone: a copy of the one it shares with
/// others, so edits to it stop moving them (Make Unique), or a new one when
/// it draws with none (New Material).
pub(crate) fn make_material_unique(
    project: &Project,
    layer: &LayerId,
    owner: &ItemId,
) -> Result<SceneEdit, String> {
    let doc = doc(project, layer)?;
    let description = if doc.effective_material(owner).is_some() {
        "Make Material Unique"
    } else {
        "New Material"
    };
    let name = unique_name(MATERIAL_NAME, &names(doc));
    let commands = own_material_commands(layer, doc, owner, new_id(), &name)?;
    Ok(batch(commands, description, None))
}

/// Apply a starting look to a Material: its features become the look's and
/// the look's factors are written, in one undo step. Default puts every
/// factor back and takes every feature off. A feature the look does not
/// have is Removed, as the feature's own remove does, so its values never
/// shade with no rows to show them.
pub(crate) fn material_look(
    project: &Project,
    layer: &LayerId,
    material: &ItemId,
    look: UiLook,
) -> Result<Option<SceneEdit>, String> {
    let doc = doc(project, layer)?;
    let found = doc
        .find(material)
        .ok_or("That material is no longer in the scene")?;
    let ItemFields::Material(fields) = &found.fields else {
        return Err("That item is not a material".to_owned());
    };
    let inst = instance(project, layer).ok_or("There is no scene to edit")?;
    let recipe = crate::scene_material_look::recipe(look);
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    let mut features: Vec<FeatureSetting> = recipe
        .features
        .iter()
        .map(|feature| FeatureSetting::on(core_feature(*feature)))
        .collect();
    for setting in &fields.features {
        if !features.iter().any(|kept| kept.feature == setting.feature) {
            features.push(FeatureSetting {
                feature: setting.feature,
                mode: FeatureSetting::REMOVED,
            });
        }
    }
    if features != fields.features {
        let edited = ItemFields::Material(MaterialFields {
            features,
            ..fields.clone()
        });
        commands.push(Box::new(SetSceneItemFieldsCommand::new(
            layer.clone(),
            material.clone(),
            edited,
        )));
    }
    for (property, value) in crate::scene_material_look::writes(
        inst,
        &crate::scene_material_look::param_ids(inst, material),
        look,
    ) {
        let Some(param) = inst.params.get(&param_id(material, &property)) else {
            continue;
        };
        if (param.base - value).abs() > f32::EPSILON {
            commands.push(write(layer, material, &property, param.base, value));
        }
    }
    if commands.is_empty() {
        return Ok(None);
    }
    Ok(Some(batch(
        commands,
        &format!("Material Look: {}", look.label()),
        None,
    )))
}

pub(crate) fn rename(
    project: &Project,
    layer: &LayerId,
    item: &ItemId,
    name: &str,
) -> Result<Option<SceneEdit>, String> {
    let found = doc(project, layer)?
        .find(item)
        .ok_or("That item is no longer in the scene")?;
    let name = name.trim();
    if name.is_empty() || name == found.name {
        return Ok(None);
    }
    Ok(Some(SceneEdit {
        command: Box::new(RenameSceneItemCommand::new(
            layer.clone(),
            item.clone(),
            name,
        )),
        select: None,
    }))
}

/// Point the first Sun at the brightest part of the HDRI `file`, keeping its
/// distance from the point it aims at.
pub(crate) fn aim_sun_at_hdri(
    project: &Project,
    layer: &LayerId,
    file: &str,
) -> Result<SceneEdit, String> {
    let doc = doc(project, layer)?;
    let inst = instance(project, layer).ok_or("There is no scene to aim")?;
    let (mut sun, mut environment) = (None, None);
    doc.walk(|item, _| match &item.fields {
        ItemFields::Light(light) if light.light == LightType::Sun && sun.is_none() => {
            sun = Some(item.id.clone())
        }
        ItemFields::Light(light)
            if matches!(light.light, LightType::Environment { .. }) && environment.is_none() =>
        {
            environment = Some(item.id.clone());
        }
        _ => {}
    });
    let sun = sun.ok_or("This scene has no Sun light to aim")?;
    let [x, y, z] =
        manifold_nodes_scene::node_graph::hdri_sun::hdri_sun_direction(std::path::Path::new(file))
            .map_err(|reason| format!("Could not aim the Sun: {reason}"))?;
    // The HDRI as drawn: turned counter-clockwise about the up axis, seen
    // from above, by the Environment's Rotation.
    let turn = environment
        .and_then(|environment| base(inst, &environment, "hdri_rotation"))
        .unwrap_or(0.0);
    let (sin, cos) = turn.sin_cos();
    let toward = [x * cos + z * sin, y, z * cos - x * sin];
    let read = |property: &str| base(inst, &sun, property).unwrap_or(0.0);
    let aim = ["aim_x", "aim_y", "aim_z"].map(read);
    let pos = ["pos_x", "pos_y", "pos_z"].map(read);
    let apart = (0..3)
        .map(|k| (pos[k] - aim[k]).powi(2))
        .sum::<f32>()
        .sqrt();
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

/// "Add Loop Camera" on the Scene Loop card: a camera on the Loop rig, after
/// the scene's other cameras, made the World's camera in the same undo step.
/// The World's Camera row cuts back to any other camera.
pub(crate) fn add_loop_camera(project: &Project, layer: &LayerId) -> Result<SceneEdit, String> {
    let doc = doc(project, layer)?;
    let Some(loop_item) = scene_schema::scene_loop(doc) else {
        return Err(scene_schema::NO_SCENE_LOOP.to_owned());
    };
    let loop_id = loop_item.id.clone();
    let world = doc
        .world()
        .map(|world| world.id.clone())
        .ok_or("This scene has no World to pick the camera")?;
    let inst = instance(project, layer).ok_or("There is no scene to add a camera to")?;
    let id = new_id();
    let index = doc
        .items
        .iter()
        .rposition(|item| item.kind() == ItemKind::Camera)
        .map_or(doc.items.len(), |last| last + 1);
    let cut = doc.upcoming_cut();
    let pick = base(inst, &world, manifold_core::scene_doc::CAMERA_PICK).unwrap_or(0.0);
    let commands: Vec<Box<dyn Command>> = vec![
        Box::new(AddSceneItemCommand::new(
            layer.clone(),
            ItemKind::Camera,
            None,
            index,
            None,
            id.clone(),
        )),
        Box::new(SetSceneItemFieldsCommand::new(
            layer.clone(),
            id.clone(),
            ItemFields::Camera(CameraFields {
                rig: CameraRig::Loop,
                ..CameraFields::default()
            }),
        )),
        Box::new(RenameSceneItemCommand::new(
            layer.clone(),
            id.clone(),
            unique_name(LOOP_CAMERA_NAME, &names(doc)),
        )),
        write(
            layer,
            &world,
            manifold_core::scene_doc::CAMERA_PICK,
            pick,
            cut as f32,
        ),
    ];
    Ok(batch(commands, "Add Loop Camera", Some(loop_id)))
}

const LOOP_CAMERA_NAME: &str = "Loop Camera";

/// The camera the World's Camera row shows.
fn camera_of(doc: &SceneDoc, inst: &PresetInstance) -> Option<(ItemId, CameraFields)> {
    let pick = doc
        .camera_pick_param()
        .filter(|id| inst.params.contains(id))
        .map_or(0.0, |id| inst.get_base_param(&id));
    let camera = doc.picked_camera(pick)?;
    match &doc.find(camera)?.fields {
        ItemFields::Camera(fields) => Some((camera.clone(), fields.clone())),
        _ => None,
    }
}

/// Each item's world origin, as the viewport gizmos place it.
fn item_origins(project: &Project, layer: &LayerId) -> impl Fn(&ItemId) -> Option<[f32; 3]> {
    let vm = crate::scene_gizmo::scene_vm(project, layer);
    move |id| {
        manifold_nodes_scene::node_graph::viewport_gizmo::gizmo_targets(vm.as_ref()?)
            .find(|target| target.row.as_ref() == Some(id))
            .map(|target| target.origin)
    }
}

/// Pull the camera back to show `item`: an Orbit camera lifts its pivot to
/// the item's height and backs off; a Look At camera aims at it.
pub(crate) fn frame(
    project: &Project,
    layer: &LayerId,
    item: &ItemId,
) -> Result<SceneEdit, String> {
    let doc = doc(project, layer)?;
    let inst = instance(project, layer).ok_or("There is no scene to frame")?;
    let (camera, fields) = camera_of(doc, inst).ok_or("This scene has no camera")?;
    let origin_of = item_origins(project, layer);
    let at = origin_of(item).ok_or("This item has no position to frame")?;
    // A Look At camera tracking an item aims at its origin plus Target X/Y/Z.
    let tracked = fields
        .target
        .as_ref()
        .and_then(origin_of)
        .unwrap_or([0.0; 3]);
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
            dir = if len > 1e-6 {
                dir.map(|d| d / len)
            } else {
                [0.0, 0.0, -1.0]
            };
            for (k, property) in ["target_x", "target_y", "target_z"].into_iter().enumerate() {
                set(property, at[k] - tracked[k]);
            }
            for (k, property) in ["pos_x", "pos_y", "pos_z"].into_iter().enumerate() {
                set(property, at[k] - dir[k] * 2.2 * radius);
            }
        }
        CameraRig::Free => return Err("Frame is not supported for a free camera".to_owned()),
        CameraRig::Loop => {
            return Err(
                "A Loop camera flies the Scene Loop: place it with Height and Lateral".to_owned(),
            );
        }
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
    let kind = match &doc
        .find(camera)
        .ok_or("That camera is no longer in the scene")?
        .fields
    {
        ItemFields::Camera(fields) => match fields.rig {
            CameraRig::Orbit => ViewCopyCamera::Orbit,
            CameraRig::Free => ViewCopyCamera::Free,
            CameraRig::Loop => {
                return Err(
                    "A Loop camera flies the Scene Loop, so it cannot take this view".to_owned(),
                );
            }
            CameraRig::LookAt => {
                let tracked = fields
                    .target
                    .as_ref()
                    .and_then(item_origins(project, layer))
                    .unwrap_or([0.0; 3]);
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

/// Move an item `delta` places among its siblings, a Modifier card among
/// the chain's cards. `None` at either end.
pub(crate) fn nudge(
    project: &Project,
    layer: &LayerId,
    item: &ItemId,
    delta: i32,
) -> Result<Option<SceneEdit>, String> {
    let doc = doc(project, layer)?;
    if doc
        .find(item)
        .is_some_and(|found| found.kind() == ItemKind::Modifier)
    {
        let chain: Vec<&ItemId> = doc
            .items
            .iter()
            .filter(|card| card.kind() == ItemKind::Modifier)
            .map(|card| &card.id)
            .collect();
        let Some(at) = chain.iter().position(|card| *card == item) else {
            return Ok(None);
        };
        let to = (at as i64 + i64::from(delta)).clamp(0, chain.len() as i64 - 1) as usize;
        return if to == at {
            Ok(None)
        } else {
            chain_move(project, layer, item, to).map(Some)
        };
    }
    let (parent, index) = doc
        .locate(item)
        .ok_or("That item is no longer in the scene")?;
    let siblings = match &parent {
        Some(id) => doc.find(id).map_or(0, |p| p.children.len()),
        None => doc.items.len(),
    };
    let target =
        (index as i64 + i64::from(delta)).clamp(0, siblings.saturating_sub(1) as i64) as usize;
    if target == index {
        return Ok(None);
    }
    Ok(Some(SceneEdit {
        command: Box::new(ReparentSceneItemCommand::new(
            layer.clone(),
            item.clone(),
            parent,
            target,
        )),
        select: None,
    }))
}

/// Move the Modifier card `card` to place `to` in the chain: one undo step.
pub(crate) fn chain_move(
    project: &Project,
    layer: &LayerId,
    card: &ItemId,
    to: usize,
) -> Result<SceneEdit, String> {
    let command = move_modifier(layer, doc(project, layer)?, card, to)?;
    Ok(SceneEdit {
        command: Box::new(command),
        select: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::scene_doc::{Matter, Quality};
    use manifold_core::types::LayerType;

    /// Setting a Shape to Ocean writes the water look onto the Ocean's own
    /// Material in the same undo step, and undo takes both back.
    #[test]
    fn a_shape_set_to_ocean_takes_the_water_look_in_one_step() {
        let (mut project, layer, cube) = cube_scene();
        let material = doc(&project, &layer)
            .unwrap()
            .effective_material(&cube)
            .expect("a new cube has its own Material");
        let before: Vec<Option<f32>> = scene_schema::OCEAN_LOOK
            .iter()
            .map(|(property, _)| base(instance(&project, &layer).unwrap(), &material, property))
            .collect();

        let mut edit = set_field(
            &project,
            &layer,
            &cube,
            "shape",
            &FieldPick::Choice("ocean".to_owned()),
        )
        .unwrap()
        .expect("the Shape changes");
        edit.command.execute(&mut project);
        let doc_now = doc(&project, &layer).unwrap();
        assert!(
            matches!(&doc_now.find(&cube).unwrap().fields, ItemFields::Object(object) if object.shape == Shape::Ocean)
        );
        assert_eq!(
            doc_now.effective_material(&cube),
            Some(material.clone()),
            "the Ocean keeps its own Material"
        );
        for (property, value) in scene_schema::OCEAN_LOOK {
            assert_eq!(
                base(instance(&project, &layer).unwrap(), &material, property),
                Some(value),
                "{property}"
            );
        }

        edit.command.undo(&mut project);
        assert!(
            matches!(&doc(&project, &layer).unwrap().find(&cube).unwrap().fields, ItemFields::Object(object) if object.shape != Shape::Ocean)
        );
        for ((property, _), old) in scene_schema::OCEAN_LOOK.iter().zip(before) {
            if let Some(old) = old {
                assert_eq!(
                    base(instance(&project, &layer).unwrap(), &material, property),
                    Some(old),
                    "{property} after undo"
                );
            }
        }
    }

    /// Pull is the one Radial card at its pulling start; a new Swarm's
    /// forces are Radial and Wind cards named after it and aimed at it.
    #[test]
    fn pull_starts_inward_and_a_swarms_forces_carry_its_name() {
        let (mut project, layer, _) = cube_scene();
        let mut pull = add(&project, &layer, "force.pull", None, &[]).unwrap();
        pull.command.execute(&mut project);
        let pull = pull.select.unwrap();
        let found = doc(&project, &layer).unwrap().find(&pull).unwrap();
        assert!(
            matches!(&found.fields, ItemFields::Force(force) if force.force == ForceType::Radial)
        );
        assert_eq!(found.name, "Pull");
        let inst = instance(&project, &layer).unwrap();
        assert_eq!(
            (base(inst, &pull, "strength"), base(inst, &pull, "curve")),
            (Some(-5.0), Some(1.0))
        );

        let mut swarm = add(&project, &layer, "swarm", None, &[]).unwrap();
        swarm.command.execute(&mut project);
        let swarm = swarm.select.unwrap();
        let doc = doc(&project, &layer).unwrap();
        let forces: Vec<(&str, ForceType)> = doc
            .items
            .iter()
            .filter_map(|item| match &item.fields {
                ItemFields::Force(force)
                    if force.targets == ForceTargets::Items(vec![swarm.clone()]) =>
                {
                    Some((item.name.as_str(), force.force))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            forces,
            [
                ("Swarm Pull", ForceType::Radial),
                ("Swarm Core", ForceType::Radial),
                ("Swarm Gusts", ForceType::Wind)
            ]
        );
    }

    /// Snow made into clay keeps its cells and takes clay's look under clay's
    /// name, and one undo brings the snow back.
    #[test]
    fn a_sand_made_of_something_else_keeps_its_cells_and_swaps_its_look() {
        let (mut project, layer, _) = cube_scene();
        let mut snow = add(&project, &layer, "matter.snow", None, &[]).unwrap();
        snow.command.execute(&mut project);
        let sand = snow.select.unwrap();
        set_field(
            &project,
            &layer,
            &sand,
            "quality",
            &FieldPick::Choice("low".into()),
        )
        .unwrap()
        .unwrap()
        .command
        .execute(&mut project);
        let material = match &doc(&project, &layer).unwrap().find(&sand).unwrap().fields {
            ItemFields::Sand(fields) => fields.material.clone().unwrap(),
            other => panic!("{other:?}"),
        };
        let radius = |project: &Project| {
            base(
                instance(project, &layer).unwrap(),
                &material,
                "subsurface_radius_b",
            )
        };
        assert_eq!(radius(&project), Some(0.012));

        let mut swap = set_field(
            &project,
            &layer,
            &sand,
            "matter",
            &FieldPick::Choice("clay".into()),
        )
        .unwrap()
        .unwrap();
        swap.command.execute(&mut project);
        let doc_now = doc(&project, &layer).unwrap();
        let found = doc_now.find(&sand).unwrap();
        assert!(
            matches!(&found.fields, ItemFields::Sand(s) if s.matter == Matter::Clay && s.quality == Quality::Low)
        );
        assert_eq!(found.name, "Clay");
        assert_eq!(doc_now.find(&material).unwrap().name, "Clay");
        assert_eq!(radius(&project), Some(0.002));

        swap.command.undo(&mut project);
        assert_eq!(radius(&project), Some(0.012));
        assert_eq!(
            doc(&project, &layer).unwrap().find(&sand).unwrap().name,
            "Snow"
        );
    }

    /// A dropped download folder names its one material file, a folder of
    /// folders names none, and a dropped `.mtlx` names itself.
    #[test]
    fn a_dropped_folder_names_its_material_file() {
        let materials =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/materials");
        let file = materials.join("Bricks090/Bricks090_2K-JPG.mtlx");
        assert_eq!(
            material_file_of(&materials.join("Bricks090")),
            Some(Ok(file.clone()))
        );
        assert_eq!(material_file_of(&materials), None);
        assert_eq!(material_file_of(&file), Some(Ok(file.clone())));
        assert_eq!(
            material_file_of(&materials.join("Bricks090/Bricks090_2K-JPG_Color.jpg")),
            None
        );
    }

    /// A material file picked on a Material that has its facts: the texture
    /// becomes the file's material, the untouched rows start at the file's
    /// values (box mapped, roughness from the picture), a row the performer
    /// set keeps its value, and one undo takes it all back.
    #[test]
    fn a_material_file_starts_the_untouched_rows_at_its_values() {
        let (mut project, layer, cube) = cube_scene();
        let material = doc(&project, &layer)
            .unwrap()
            .effective_material(&cube)
            .unwrap();
        let file = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/materials/Bricks090/Bricks090_2K-JPG.mtlx"
        );
        let (facts, _) = manifold_nodes_scene::node_graph::gltf_import::read_file_facts(
            std::path::Path::new(file),
        )
        .unwrap();
        let held = base(instance(&project, &layer).unwrap(), &material, "metallic").unwrap();
        write(&layer, &material, "metallic", held, 0.3).execute(&mut project);
        let (_, scene_layer) = project
            .timeline
            .find_layer_by_id_mut(layer.as_str())
            .unwrap();
        let inst = scene_layer.gen_params_mut().unwrap();
        let mut scene = inst.scene.clone().unwrap();
        scene.models.insert(file.to_owned(), facts);
        inst.install_scene_document(scene).unwrap();

        let mut edit = set_material_file(&project, &layer, &material, file).unwrap();
        edit.command.execute(&mut project);
        let row = |project: &Project, property| {
            base(instance(project, &layer).unwrap(), &material, property).unwrap()
        };
        assert_eq!(
            row(&project, "mapping"),
            1.0,
            "a material file starts box mapped"
        );
        assert_eq!(
            row(&project, "roughness"),
            1.0,
            "the picture carries the roughness"
        );
        assert_eq!(
            row(&project, "break_repeats"),
            0.0,
            "a brick file is not hex-tiled, so its rows stay whole"
        );
        assert_eq!(
            row(&project, "metallic"),
            0.3,
            "a row the performer set stays"
        );

        edit.command.undo(&mut project);
        assert_eq!(row(&project, "mapping"), 0.0);
        assert!(matches!(
            &doc(&project, &layer)
                .unwrap()
                .find(&material)
                .unwrap()
                .fields,
            ItemFields::Material(MaterialFields {
                texture: TextureSource::None,
                ..
            })
        ));
    }

    /// A scene layer holding a new cube: the project, the layer, the cube.
    fn cube_scene() -> (Project, LayerId, ItemId) {
        let mut project = Project::default();
        let index = project
            .timeline
            .add_layer("Scene", LayerType::Generator, PresetTypeId::SCENE);
        let layer = project.timeline.layers[index].layer_id.clone();
        let mut added =
            add(&project, &layer, "object.cube", None, &[]).expect("a cube adds to the base scene");
        added.command.execute(&mut project);
        (
            project,
            layer,
            added.select.expect("the add selects the new cube"),
        )
    }

    fn glass_mode(project: &Project, layer: &LayerId, material: &ItemId) -> Option<u32> {
        let ItemFields::Material(fields) =
            &doc(project, layer).unwrap().find(material).unwrap().fields
        else {
            panic!("not a material")
        };
        fields
            .features
            .iter()
            .find(|setting| setting.feature == core_feature(UiFeature::Glass))
            .map(|setting| setting.mode)
    }

    /// Glass added to a metal turns it dielectric in the same step; removing
    /// it keeps it Removed with its values, and adding it again brings them
    /// back. Undo of the add is exact.
    #[test]
    fn glass_on_metal_sets_metallic_zero_and_remove_keeps_its_values() {
        let (mut project, layer, cube) = cube_scene();
        let material = doc(&project, &layer)
            .unwrap()
            .effective_material(&cube)
            .unwrap();
        let value = |project: &Project, property: &str| {
            base(instance(project, &layer).unwrap(), &material, property).unwrap()
        };
        let metallic = value(&project, "metallic");
        write(&layer, &material, "metallic", metallic, 1.0).execute(&mut project);

        let mut glass = material_feature(&project, &layer, &material, UiFeature::Glass, true)
            .unwrap()
            .expect("Glass adds");
        glass.command.execute(&mut project);
        assert_eq!(
            glass_mode(&project, &layer, &material),
            Some(FeatureSetting::ON)
        );
        assert_eq!(
            value(&project, "metallic"),
            0.0,
            "glass on a metal lets light through"
        );
        assert_eq!(value(&project, "transmission"), 1.0);
        glass.command.undo(&mut project);
        assert_eq!(glass_mode(&project, &layer, &material), None);
        assert_eq!(value(&project, "metallic"), 1.0, "undo puts the metal back");
        assert_eq!(value(&project, "transmission"), 0.0);
        glass.command.execute(&mut project);

        write(&layer, &material, "transmission", 1.0, 0.7).execute(&mut project);
        let mut removed = material_feature(&project, &layer, &material, UiFeature::Glass, false)
            .unwrap()
            .expect("Glass removes");
        removed.command.execute(&mut project);
        assert_eq!(
            glass_mode(&project, &layer, &material),
            Some(FeatureSetting::REMOVED)
        );
        assert_eq!(
            value(&project, "transmission"),
            0.7,
            "remove keeps the values"
        );
        assert_eq!(
            value(&project, "metallic"),
            0.0,
            "remove leaves Metallic alone"
        );
        assert!(
            material_feature(&project, &layer, &material, UiFeature::Glass, false)
                .unwrap()
                .is_none()
        );

        let mut again = material_feature(&project, &layer, &material, UiFeature::Glass, true)
            .unwrap()
            .expect("Glass adds again");
        again.command.execute(&mut project);
        assert_eq!(
            glass_mode(&project, &layer, &material),
            Some(FeatureSetting::ON)
        );
        assert_eq!(
            value(&project, "transmission"),
            0.7,
            "adding again brings the values back"
        );
        again.command.undo(&mut project);
        assert_eq!(
            glass_mode(&project, &layer, &material),
            Some(FeatureSetting::REMOVED)
        );
    }

    /// Removing a cube takes its own Material with it in one step; a Material
    /// another cube still draws with stays.
    #[test]
    fn remove_takes_a_material_only_the_removed_items_drew_with() {
        let (mut project, layer, cube) = cube_scene();
        let material = doc(&project, &layer)
            .unwrap()
            .effective_material(&cube)
            .unwrap();
        let mut removed = remove(&project, &layer, std::slice::from_ref(&cube)).unwrap();
        removed.command.execute(&mut project);
        assert!(
            doc(&project, &layer).unwrap().find(&material).is_none(),
            "a single-use Material leaves with its cube"
        );
        removed.command.undo(&mut project);
        assert_eq!(
            doc(&project, &layer).unwrap().effective_material(&cube),
            Some(material.clone()),
            "undo restores both, bound"
        );

        let mut copied = duplicate(&project, &layer, std::slice::from_ref(&cube)).unwrap();
        copied.command.execute(&mut project);
        let copy = copied.select.expect("the copy is selected");
        assert_eq!(
            doc(&project, &layer).unwrap().effective_material(&copy),
            Some(material.clone()),
            "a duplicate shares the Material"
        );
        remove(&project, &layer, std::slice::from_ref(&cube))
            .unwrap()
            .command
            .execute(&mut project);
        assert_eq!(
            doc(&project, &layer).unwrap().effective_material(&copy),
            Some(material),
            "a shared Material stays"
        );
    }

    fn set_texture(
        project: &mut Project,
        layer: &LayerId,
        material: &ItemId,
        texture: TextureSource,
    ) {
        let ItemFields::Material(fields) = doc(project, layer)
            .unwrap()
            .find(material)
            .unwrap()
            .fields
            .clone()
        else {
            panic!("not a material")
        };
        let edited = ItemFields::Material(MaterialFields { texture, ..fields });
        SetSceneItemFieldsCommand::new(layer.clone(), material.clone(), edited).execute(project);
    }

    /// The Default look puts Glass's values back; adding Glass after it
    /// starts it at its seed again, so it shows.
    #[test]
    fn glass_added_after_the_default_look_shows() {
        let (mut project, layer, cube) = cube_scene();
        let material = doc(&project, &layer)
            .unwrap()
            .effective_material(&cube)
            .unwrap();
        let transmission = |project: &Project| {
            base(
                instance(project, &layer).unwrap(),
                &material,
                "transmission",
            )
            .unwrap()
        };
        material_look(&project, &layer, &material, UiLook::Glass)
            .unwrap()
            .expect("the Glass look applies")
            .command
            .execute(&mut project);
        material_look(&project, &layer, &material, UiLook::Default)
            .unwrap()
            .expect("the Default look applies")
            .command
            .execute(&mut project);
        assert_eq!(
            glass_mode(&project, &layer, &material),
            Some(FeatureSetting::REMOVED)
        );
        assert_eq!(transmission(&project), 0.0);

        material_feature(&project, &layer, &material, UiFeature::Glass, true)
            .unwrap()
            .expect("Glass adds")
            .command
            .execute(&mut project);
        assert_eq!(
            glass_mode(&project, &layer, &material),
            Some(FeatureSetting::ON)
        );
        assert_eq!(transmission(&project), 1.0, "Glass shows");
    }

    /// An import's metal map decides metal per texel: Glass added at
    /// Metallic 1 keeps Metallic, and the rows carry no metal reason.
    #[test]
    fn glass_on_an_import_with_a_metal_map_keeps_metallic() {
        let (mut project, layer, cube) = cube_scene();
        let material = doc(&project, &layer)
            .unwrap()
            .effective_material(&cube)
            .unwrap();
        let file = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/gltf/DamagedHelmet.glb"
        );
        let (facts, _) =
            manifold_nodes_scene::node_graph::gltf_import::read_facts(std::path::Path::new(file))
                .unwrap();
        let (_, scene_layer) = project
            .timeline
            .find_layer_by_id_mut(layer.as_str())
            .unwrap();
        scene_layer
            .gen_params_mut()
            .unwrap()
            .scene
            .as_mut()
            .unwrap()
            .models
            .insert(file.to_owned(), facts);
        set_texture(
            &mut project,
            &layer,
            &material,
            TextureSource::Model {
                file: file.to_owned(),
                material: Some(0),
            },
        );
        let metallic = base(instance(&project, &layer).unwrap(), &material, "metallic").unwrap();
        write(&layer, &material, "metallic", metallic, 1.0).execute(&mut project);

        let scene = doc(&project, &layer).unwrap();
        let ItemFields::Material(fields) = &scene.find(&material).unwrap().fields else {
            panic!("not a material")
        };
        assert!(
            manifold_nodes_scene::node_graph::scene_compile::fed_map_ports(scene, fields)
                .contains(&"mr_map"),
            "the helmet has a metal map"
        );
        assert!(
            !full_metal(scene, fields, 1.0),
            "a metal map is not full metal"
        );
        material_feature(&project, &layer, &material, UiFeature::Glass, true)
            .unwrap()
            .expect("Glass adds")
            .command
            .execute(&mut project);
        assert_eq!(
            base(instance(&project, &layer).unwrap(), &material, "metallic"),
            Some(1.0),
            "the map keeps its metal"
        );
    }

    /// A map's placement rows outlive its texture: removing the texture and
    /// adding it back keeps their values and their LFOs.
    #[test]
    fn a_texture_leaving_and_returning_keeps_its_rows_and_their_lfos() {
        use manifold_core::effects::ParameterDriver;
        use manifold_core::{BeatDivision, DriverWaveform};
        let (mut project, layer, cube) = cube_scene();
        let material = doc(&project, &layer)
            .unwrap()
            .effective_material(&cube)
            .unwrap();
        let picture = TextureSource::File {
            path: "picture.png".to_owned(),
        };
        set_texture(&mut project, &layer, &material, picture.clone());
        let row = param_id(&material, "uv_tx");
        let held =
            base(instance(&project, &layer).unwrap(), &material, "uv_tx").expect("a placement row");
        write(&layer, &material, "uv_tx", held, 0.25).execute(&mut project);
        let (_, scene_layer) = project
            .timeline
            .find_layer_by_id_mut(layer.as_str())
            .unwrap();
        scene_layer
            .gen_params_mut()
            .unwrap()
            .drivers_mut()
            .push(ParameterDriver::new(
                row.clone(),
                BeatDivision::Quarter,
                DriverWaveform::Sine,
            ));
        let check = |project: &Project, hidden: bool| {
            let inst = instance(project, &layer).unwrap();
            assert_eq!(
                base(inst, &material, "uv_tx"),
                Some(0.25),
                "the row keeps its value"
            );
            assert!(
                inst.drivers
                    .iter()
                    .flatten()
                    .any(|driver| driver.param_id == row),
                "the row keeps its LFO"
            );
            let role = inst
                .params
                .get(&row)
                .unwrap()
                .spec
                .scene
                .map(|scene| scene.role);
            assert_eq!(
                role == Some(manifold_core::scene_surface::SurfaceRole::Hidden),
                hidden
            );
        };
        check(&project, false);
        set_texture(&mut project, &layer, &material, TextureSource::None);
        check(&project, true);
        set_texture(&mut project, &layer, &material, picture);
        check(&project, false);
    }
}
