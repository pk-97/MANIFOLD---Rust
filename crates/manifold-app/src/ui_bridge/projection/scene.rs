//! The Scene tab's view model, built from the scene document
//! (SCENE_DOCUMENT_DESIGN.md section 8): the items as a depth-first list,
//! each with the manifest params it owns, its structural field rows resolved
//! to names, and the Material it draws with. Reads only.

use crate::ui_root::UIRoot;
use manifold_core::effects::PresetInstance;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::scene_doc::{
    DrawAs, EnvironmentSource, Item, ItemFields, ItemId, ItemKind, LightType, MaterialType,
    PhysicsSetting, SceneDoc, Shape, draws_with_material, import_placement_id, param_id,
    split_param_id,
};
use manifold_core::scene_schema::{
    ADD_ENTRIES, Choice, FieldControl, FieldRow, FieldValue, MODIFIER_HEADER_FIELDS,
    acts_on_copies, applicable_fields, break_unavailable, choice_disabled, copies_refused,
    has_loop_camera, impulse_unavailable, is_on, item_disabled, new_card_targets, on_off_key,
    waits_for_loop_camera,
};
use manifold_core::scene_surface::FieldPick;
use manifold_core::types::LayerType;
use manifold_core::{GraphTarget, LayerId};
use manifold_ui::panels::scene_setup_panel::{
    AddEntryVm, FieldControlVm, FieldOptionVm, FieldRowVm, MaterialBlockVm, SceneItemVm,
    SceneSetupState, SceneSetupVm,
};

fn is_import_placement(item: &Item, parent: Option<&Item>) -> bool {
    parent.is_some_and(|p| {
        matches!(&p.fields, ItemFields::Group(g) if g.source.is_some())
            && item.id == import_placement_id(&p.id)
    })
}

/// The panel's state for the selected layer. A layer `notices` lists as
/// stopped offers Draw Again on its World. `selected` is the panel's item
/// selection, which a new Modifier card aims at.
pub(crate) fn scene_state(
    project: &Project,
    layer: Option<&Layer>,
    notices: &crate::layer_notices::LayerNotices,
    selected: &[ItemId],
) -> SceneSetupState {
    let Some(layer) = layer else {
        return SceneSetupState::NoSelection("Select a layer to set up its scene.".to_owned());
    };
    if layer.layer_type != LayerType::Generator {
        return SceneSetupState::NoSelection(
            "Select a generator layer to set up its scene.".to_owned(),
        );
    }
    let layer_id = layer.layer_id.clone();
    let (Some(inst), Some(doc)) = (layer.gen_params(), project.scene_of(&layer_id)) else {
        return SceneSetupState::NoScene { layer_id };
    };
    let _ = notices;
    let mut vm = scene_vm(project, layer_id, doc, inst, selected);
    for item in vm
        .items
        .iter_mut()
        .filter(|item| item.kind == ItemKind::World)
    {
        item.draw_again = false;
    }
    SceneSetupState::Live(Box::new(vm))
}

fn scene_vm(
    project: &Project,
    layer_id: LayerId,
    doc: &SceneDoc,
    inst: &PresetInstance,
    selected: &[ItemId],
) -> SceneSetupVm {
    // The manifest ids each item owns, in manifest order.
    let mut owned: ahash::AHashMap<&str, Vec<String>> = ahash::AHashMap::new();
    for param in inst.params.iter() {
        if let Some((item, _)) = split_param_id(param.id()) {
            owned.entry(item).or_default().push(param.id().to_owned());
        }
    }
    let layers: Vec<(LayerId, String)> = project
        .timeline
        .layers
        .iter()
        .filter(|other| other.layer_id != layer_id && other.layer_type != LayerType::Group)
        .map(|other| (other.layer_id.clone(), other.name.clone()))
        .collect();
    let materials = Materials {
        effective: doc.effective_materials(),
        users: doc.material_users(),
    };
    let mut items = Vec::new();
    doc.walk(|item, ancestors| {
        if is_import_placement(item, ancestors.last().copied()) {
            return;
        }
        // Exclude any import-placement group from the ancestor chain so its
        // children sit directly under the outer import group.
        let mut filtered: Vec<&Item> = Vec::new();
        for (idx, a) in ancestors.iter().copied().enumerate() {
            let is_placement = idx > 0 && is_import_placement(a, Some(ancestors[idx - 1]));
            if !is_placement {
                filtered.push(a);
            }
        }
        items.push(item_vm(
            doc, inst, item, &filtered, &owned, &layers, &materials,
        ));
    });
    // A copy card aims where a new card would; it is refused there when it
    // would move nothing.
    let copy_targets = new_card_targets(doc, selected);
    let add_entries = ADD_ENTRIES
        .iter()
        .map(|entry| AddEntryVm {
            key: entry.key.to_owned(),
            group: entry.group.to_owned(),
            label: entry.label.to_owned(),
            top_level: manifold_core::scene_doc::can_hold(None, entry.kind),
            holders: ItemKind::ALL.into_iter().filter(|holder| manifold_core::scene_doc::can_hold(Some(*holder), entry.kind)).collect(),
            disabled: entry
                .fields
                .and_then(|fields| {
                    let fields = fields();
                    let moves_copies = matches!(&fields, ItemFields::Modifier(card) if acts_on_copies(card.modifier));
                    manifold_core::scene_schema::add_refused(doc, &fields).or_else(|| copies_refused(doc, &copy_targets).filter(|_| moves_copies))
                })
                .map(str::to_owned),
        })
        .collect();
    SceneSetupVm {
        layer_id,
        items,
        add_entries,
    }
}

fn item_vm(
    doc: &SceneDoc,
    inst: &PresetInstance,
    item: &Item,
    ancestors: &[&Item],
    owned: &ahash::AHashMap<&str, Vec<String>>,
    layers: &[(LayerId, String)],
    materials: &Materials,
) -> SceneItemVm {
    // A row only a Loop camera reads stays hidden until a camera rides the loop.
    let params_of = |id: &ItemId| {
        let mut ids = owned.get(id.as_str()).cloned().unwrap_or_default();
        ids.retain(|param| {
            !split_param_id(param)
                .is_some_and(|(_, property)| waits_for_loop_camera(doc, item, property))
        });
        ids
    };
    let fields = field_rows(doc, item, layers);
    let material_item = match item.kind() {
        ItemKind::Material => Some(item.id.clone()),
        _ if draws_with_material(item) => materials.effective.get(&item.id).cloned(),
        _ => None,
    };
    let material = material_item.and_then(|id| doc.find(&id)).map(|mat| {
        material_block(
            doc,
            inst,
            mat,
            owned,
            layers,
            materials.users.get(&mat.id).copied().unwrap_or(0),
        )
    });
    let (environment, aim_sun_file) = match &item.fields {
        ItemFields::Light(light) => match &light.light {
            LightType::Environment { source } => (
                true,
                match source {
                    EnvironmentSource::Hdri { file } | EnvironmentSource::SeaHorizon { file }
                        if !file.is_empty() =>
                    {
                        Some(file.clone())
                    }
                    _ => None,
                },
            ),
            _ => (false, None),
        },
        _ => (false, None),
    };
    let row_reasons = row_reasons(doc, inst, item, material.as_ref());
    SceneItemVm {
        id: item.id.clone(),
        kind: item.kind(),
        name: item.name.clone(),
        depth: ancestors.len(),
        parent: ancestors.last().map(|parent| parent.id.clone()),
        has_children: if matches!(&item.fields, ItemFields::Group(g) if g.source.is_some())
            && item.children.len() == 1
            && item.children[0].id == import_placement_id(&item.id)
        {
            !item.children[0].children.is_empty()
        } else {
            !item.children.is_empty()
        },
        parameter_ids: params_of(&item.id),
        fields,
        material,
        removable: item.kind() != ItemKind::World,
        plane: matches!(&item.fields, ItemFields::Object(object) if object.shape == Shape::Plane),
        copy_view: matches!(&item.fields, ItemFields::Camera(_)),
        environment,
        aim_sun_file,
        draw_again: false,
        add_loop_camera: is_scene_loop(item) && !has_loop_camera(doc),
        make_active_camera: match &item.fields {
            ItemFields::Camera(cam) => {
                let pick = doc
                    .camera_pick_param()
                    .filter(|id| inst.params.contains(id))
                    .map_or(0.0, |id| inst.get_base_param(&id));
                (doc.picked_camera(pick) != Some(&item.id)).then_some(cam.cut as f32)
            }
            _ => None,
        },
        row_reasons,
        body: item.fields.physics() == PhysicsSetting::Body,
        look: match &item.fields {
            ItemFields::Swarm(swarm) => match &swarm.draw_as {
                DrawAs::Item(id) => doc.find(id).map(|drawn| (id.clone(), drawn.name.clone())),
                _ => None,
            },
            _ => None,
        },
    }
}

/// Why rows of `item` cannot be used: a Force whose Impulse has nothing to
/// push, a Shatter card with no Body to break, a metal Material's features
/// that light passes through.
fn row_reasons(
    doc: &SceneDoc,
    inst: &PresetInstance,
    item: &Item,
    material: Option<&MaterialBlockVm>,
) -> Vec<(String, String)> {
    let mut reasons = Vec::new();
    if let Some(material) = material.filter(|material| material.pbr)
        && let Some(metallic) = inst.params.get(&param_id(&material.item, "metallic"))
        && let Some(ItemFields::Material(fields)) =
            doc.find(&material.item).map(|found| &found.fields)
        && crate::scene_edit::full_metal(doc, fields, metallic.base)
    {
        use manifold_core::material_inspector::{MaterialGroup, MaterialParamRole};
        for id in &material.parameter_ids {
            let passes_light = inst
                .params
                .get(id)
                .and_then(|param| param.spec.material_role)
                .is_some_and(|role| match role {
                    MaterialParamRole::Scalar(MaterialGroup::Feature(feature))
                    | MaterialParamRole::Colour(MaterialGroup::Feature(feature), ..) => {
                        crate::scene_edit::PASSES_LIGHT.contains(&feature)
                    }
                    _ => false,
                });
            if passes_light {
                reasons.push((id.clone(), "Metal lets no light through".to_owned()));
            }
        }
    }
    if let Some(reason) = impulse_unavailable(doc, item) {
        reasons.push((param_id(&item.id, "fire"), reason.to_owned()));
    }
    if let Some(reason) = break_unavailable(doc, item) {
        reasons.push((param_id(&item.id, "shatter"), reason.to_owned()));
    }
    reasons
}

/// Which Material each item draws with and how many draw with each, found
/// once per projection so the panel stays linear in the scene's size.
struct Materials {
    effective: std::collections::HashMap<ItemId, ItemId>,
    users: std::collections::HashMap<ItemId, usize>,
}

fn material_block(
    doc: &SceneDoc,
    inst: &PresetInstance,
    material: &Item,
    owned: &ahash::AHashMap<&str, Vec<String>>,
    layers: &[(LayerId, String)],
    used_by: usize,
) -> MaterialBlockVm {
    // Looks and features are PBR: an Unlit or Cel Material compiles neither.
    let pbr = matches!(&material.fields, ItemFields::Material(fields)
        if manifold_core::scene_schema::material_node_type(fields) == MaterialType::Pbr.node_type());
    let features = match &material.fields {
        ItemFields::Material(fields) if pbr => fields
            .features
            .iter()
            .filter(|setting| !setting.removed())
            .map(|setting| super::material::feature(setting.feature))
            .collect(),
        _ => Vec::new(),
    };
    let parameter_ids = owned.get(material.id.as_str()).cloned().unwrap_or_default();
    let look = match &material.fields {
        ItemFields::Material(fields) if pbr => {
            crate::scene_material_look::matching(inst, &material.id, &parameter_ids, fields)
        }
        _ => None,
    };
    MaterialBlockVm {
        item: material.id.clone(),
        name: material.name.clone(),
        used_by,
        features,
        parameter_ids,
        fields: field_rows(doc, material, layers),
        look,
        pbr,
    }
}

fn field_rows(doc: &SceneDoc, item: &Item, layers: &[(LayerId, String)]) -> Vec<FieldRowVm> {
    applicable_fields(&item.fields)
        .map(|row| {
            let control = control(doc, item, row, layers);
            // A switch whose flip is refused is a disabled row.
            let refused_flip = match &control {
                FieldControlVm::Switch {
                    toggle: FieldPick::Choice(key),
                    ..
                } => choice_disabled(doc, item, row.id, key),
                _ => None,
            };
            FieldRowVm {
                id: row.id.to_owned(),
                label: row.label.to_owned(),
                section: row.section,
                control,
                disabled: (row.disabled)(doc, item)
                    .or(refused_flip)
                    .map(str::to_owned),
                header: item.kind() == ItemKind::Modifier
                    && MODIFIER_HEADER_FIELDS.contains(&row.id),
            }
        })
        .collect()
}

fn closed(choices: &[Choice], held: &FieldValue) -> Vec<FieldOptionVm> {
    choices
        .iter()
        .map(|choice| FieldOptionVm {
            label: choice.label.to_owned(),
            pick: FieldPick::Choice(choice.key.to_owned()),
            checked: matches!(held, FieldValue::Choice(key) if key == choice.key),
            disabled: None,
            children: Vec::new(),
        })
        .collect()
}

fn item_options(
    doc: &SceneDoc,
    item: &Item,
    field: &str,
    passes: fn(&Item) -> bool,
    held: &FieldValue,
) -> Vec<FieldOptionVm> {
    let mut options = Vec::new();
    doc.walk(|other, _| {
        if other.id != item.id && passes(other) {
            let checked = matches!(held, FieldValue::Item(Some(id)) if *id == other.id);
            options.push(FieldOptionVm {
                label: other.name.clone(),
                pick: FieldPick::Item(Some(other.id.clone())),
                checked,
                disabled: item_disabled(doc, item, field, other)
                    .filter(|_| !checked)
                    .map(str::to_owned),
                children: Vec::new(),
            });
        }
    });
    options
}

fn control(
    doc: &SceneDoc,
    item: &Item,
    row: &FieldRow,
    layers: &[(LayerId, String)],
) -> FieldControlVm {
    let held = (row.read)(&item.fields);
    let pick = |options: Vec<FieldOptionVm>| {
        let mut current = options
            .iter()
            .filter(|option| option.checked)
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if current.is_empty() {
            current.push_str("Choose");
        }
        FieldControlVm::Pick { current, options }
    };
    match row.control {
        FieldControl::OnOff => {
            let on = held == is_on(true);
            FieldControlVm::Switch {
                on,
                toggle: FieldPick::Choice(on_off_key(!on).to_owned()),
            }
        }
        FieldControl::Closed(choices) => {
            let mut options = closed(choices, &held);
            for (option, choice) in options.iter_mut().zip(choices) {
                option.disabled = choice_disabled(doc, item, row.id, choice.key).map(str::to_owned);
            }
            pick(options)
        }
        FieldControl::ClosedOrItem {
            closed: choices,
            items,
        } => {
            let mut options = closed(choices, &held);
            options.extend(item_options(doc, item, row.id, items, &held));
            pick(options)
        }
        FieldControl::Item { items, none } => {
            let mut options = vec![FieldOptionVm {
                label: none.to_owned(),
                pick: FieldPick::Item(None),
                checked: matches!(held, FieldValue::Item(None)),
                disabled: None,
                children: Vec::new(),
            }];
            options.extend(item_options(doc, item, row.id, items, &held));
            pick(options)
        }
        FieldControl::ItemSet {
            items,
            all,
            refused,
        } => {
            let set = match &held {
                FieldValue::Items(set) => set.as_slice(),
                _ => &[],
            };
            let everything = held == FieldValue::Everything;
            let mut options = vec![FieldOptionVm {
                label: all.to_owned(),
                pick: FieldPick::All,
                checked: everything,
                disabled: None,
                children: Vec::new(),
            }];
            doc.walk(|other, _| {
                if other.id != item.id && items(other) {
                    let checked = set.contains(&other.id);
                    options.push(FieldOptionVm {
                        label: other.name.clone(),
                        pick: FieldPick::Toggle(other.id.clone()),
                        checked,
                        // A held target stays pickable, so it can be taken off.
                        disabled: refused(doc, item, other)
                            .filter(|_| !checked)
                            .map(str::to_owned),
                        children: Vec::new(),
                    });
                }
            });
            pick(options)
        }
        FieldControl::ClosedOrLayer(choices) => {
            let mut options = closed(choices, &held);
            // A Material's Texture offers the starter library as its own menu.
            let library = crate::file_dialogs::starter_library();
            if item.kind() == ItemKind::Material && row.id == "texture" && !library.is_empty() {
                options.push(FieldOptionVm {
                    label: "Library \u{203a}".to_owned(),
                    pick: FieldPick::Choice(String::new()),
                    checked: false,
                    disabled: None,
                    children: library
                        .iter()
                        .map(|(name, file)| FieldOptionVm {
                            label: name.clone(),
                            pick: FieldPick::File(file.to_string_lossy().into_owned()),
                            checked: false,
                            disabled: None,
                            children: Vec::new(),
                        })
                        .collect(),
                });
            }
            options.extend(layers.iter().map(|(id, name)| FieldOptionVm {
                label: name.clone(),
                pick: FieldPick::Layer(id.clone()),
                checked: matches!(&held, FieldValue::Layer(held) if held == id),
                disabled: None,
                children: Vec::new(),
            }));
            pick(options)
        }
        FieldControl::Listed { names } => pick(
            names(&item.fields)
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    let key = index.to_string();
                    let checked = matches!(&held, FieldValue::Choice(held) if *held == key);
                    FieldOptionVm {
                        label: name.clone(),
                        pick: FieldPick::Choice(key),
                        checked,
                        disabled: None,
                        children: Vec::new(),
                    }
                })
                .collect(),
        ),
        FieldControl::File { extensions } => FieldControlVm::File {
            path: match held {
                FieldValue::File(path) => path,
                _ => String::new(),
            },
            extensions: extensions
                .iter()
                .map(|extension| (*extension).to_owned())
                .collect(),
        },
    }
}

/// Per-frame VALUE sync for the Scene tab's rows — the scene-row sibling of
/// `sync_card_values`: push each built row's current value from the layer's
/// manifest onto the already-built panel (rows and outliner eyes), so rows
/// track OSC, command and other-window writes between structural syncs. Rows
/// draw the same way as cards: the drag in progress, else the live value, else
/// your value. No-op while the panel is closed or not Live.
pub fn sync_scene_row_values(
    ui: &mut UIRoot,
    project: &Project,
    live: Option<&crate::content_state::ModulationSnapshot>,
    drag: Option<&crate::ui_bridge::scrub::ResolvedScrub>,
) {
    if !ui.scene_setup_panel.is_open() {
        return;
    }
    let Some(layer_id) = ui.scene_setup_panel.live_layer_id() else {
        return;
    };
    let Some((_, layer)) = project.timeline.find_layer_by_id(layer_id.as_str()) else {
        return;
    };
    // The panel joins by id (BUG-313): a manifest param the current
    // selection isn't showing simply misses the join and is ignored.
    if let Some(gp) = layer.gen_params() {
        let owner = GraphTarget::Generator(layer.layer_id.clone());
        let values = live.and_then(|s| s.values_of(&owner, gp.params.topology()));
        crate::ui_translate::with_param_slots(
            gp,
            values,
            |id| drag.and_then(|d| d.live_param(&owner, id)),
            |slots| {
                ui.scene_setup_panel
                    .sync_properties_values(&mut ui.tree, slots)
            },
        );
    }
}

/// Whether `item` is the scene's Scene Loop card.
fn is_scene_loop(item: &Item) -> bool {
    matches!(&item.fields, ItemFields::Modifier(card) if card.modifier == manifold_core::scene_doc::ModifierType::SceneLoop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::scene_doc::{BASE_CUBE_MATERIAL, SwarmFields};
    use manifold_ui::panels::scene_setup_panel::SceneSetupState;

    /// A Swarm drawing its own solids is a user of its Material: the
    /// Material counts it, and the Swarm's card carries the Material block.
    #[test]
    fn a_swarm_counts_as_a_user_of_its_material() {
        let mut project = Project::default();
        let index = project.timeline.add_layer(
            "A",
            LayerType::Generator,
            PresetTypeId::from_string("Scene".to_string()),
        );
        let layer = project.timeline.layers[index].layer_id.clone();
        let mut doc = project.scene_of(&layer).expect("a scene layer").clone();
        let material = ItemId::new(BASE_CUBE_MATERIAL);
        let swarm = Item::new(
            ItemId::new("swarm"),
            "Swarm",
            ItemFields::Swarm(SwarmFields {
                material: Some(material.clone()),
                ..SwarmFields::default()
            }),
        );
        doc.items.push(swarm);
        let inst = project.timeline.layers[index]
            .gen_params_mut()
            .expect("scene params");
        inst.install_scene_document(doc)
            .expect("the scene compiles");

        let SceneSetupState::Live(vm) = scene_state(
            &project,
            Some(&project.timeline.layers[index]),
            &Default::default(),
            &[],
        ) else {
            panic!("the scene tab is live");
        };
        let block = vm
            .items
            .iter()
            .find(|item| item.id.as_str() == "swarm")
            .and_then(|item| item.material.as_ref());
        let block = block.expect("the Swarm's card shows its Material");
        assert_eq!(block.item, material);
        assert_eq!(block.used_by, 2, "the cube and the Swarm both draw with it");
    }

    /// The Scene Loop card hides the rows only a Loop camera reads and offers
    /// Add Loop Camera; the button adds a Loop camera, makes it the World's
    /// camera, and the rows come back.
    #[test]
    fn add_loop_camera_puts_a_camera_on_the_loop_and_shows_its_rows() {
        let mut project = Project::default();
        let index = project
            .timeline
            .add_layer("A", LayerType::Generator, PresetTypeId::SCENE);
        let layer = project.timeline.layers[index].layer_id.clone();
        let mut add = crate::scene_edit::add(&project, &layer, "modifier.scene_loop", None, &[])
            .expect("Scene Loop adds");
        add.command.execute(&mut project);
        let card = add.select.expect("the add selects the card");
        let loop_card = |project: &Project| {
            let SceneSetupState::Live(vm) = scene_state(
                project,
                Some(&project.timeline.layers[index]),
                &Default::default(),
                &[],
            ) else {
                panic!("the scene tab is live");
            };
            vm.items
                .into_iter()
                .find(|item| item.id == card)
                .expect("the Scene Loop card")
        };
        let camera_only = |item: &SceneItemVm| {
            manifold_core::scene_schema::LOOP_CAMERA_ONLY
                .into_iter()
                .filter(|property| item.parameter_ids.contains(&param_id(&card, property)))
                .count()
        };

        let after = loop_card(&project);
        assert!(
            !after.add_loop_camera,
            "adding Scene Loop automatically creates and activates a Loop camera"
        );
        assert_eq!(
            camera_only(&after),
            manifold_core::scene_schema::LOOP_CAMERA_ONLY.len(),
            "the Loop camera's rows show on Scene Loop"
        );
        let doc = project.scene_of(&layer).expect("the scene");
        let pick = doc.camera_pick_param().expect("the World's Camera row");
        let inst = project.timeline.layers[index]
            .gen_params()
            .expect("scene params");
        let active_cam = doc
            .picked_camera(inst.get_base_param(&pick))
            .expect("active camera");
        assert!(
            matches!(doc.find(active_cam).map(|i| &i.fields), Some(ItemFields::Camera(c)) if c.rig == manifold_core::scene_doc::CameraRig::Loop)
        );

        // If the user deletes the Loop Camera, Add Loop Camera is offered again and restores it.
        let mut rm = crate::scene_edit::remove(&project, &layer, std::slice::from_ref(active_cam))
            .expect("remove loop camera");
        rm.command.execute(&mut project);
        let without_cam = loop_card(&project);
        assert!(
            without_cam.add_loop_camera,
            "a Scene Loop with no Loop camera offers one"
        );
        assert_eq!(
            camera_only(&without_cam),
            0,
            "rows only a Loop camera reads stay hidden"
        );
        let mut edit =
            crate::scene_edit::add_loop_camera(&project, &layer).expect("the button adds a camera");
        edit.command.execute(&mut project);
        let after = loop_card(&project);
        assert!(!after.add_loop_camera, "one Loop camera is enough");
        assert_eq!(
            camera_only(&after),
            manifold_core::scene_schema::LOOP_CAMERA_ONLY.len(),
            "the Loop camera's rows show"
        );
        // make_active_camera UX
        let SceneSetupState::Live(vm) = scene_state(
            &project,
            Some(&project.timeline.layers[index]),
            &Default::default(),
            &[],
        ) else {
            panic!()
        };
        let cam_items: Vec<_> = vm
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::Camera)
            .collect();
        // original Camera (non-loop) offers Use as Active Camera with cut 0.0
        assert!(
            cam_items.iter().any(|i| i.make_active_camera == Some(0.0)),
            "original camera offers make active"
        );
        // Loop Camera is active -> no button
        assert!(
            cam_items.iter().any(|i| i.make_active_camera.is_none()),
            "active loop camera has no button"
        );
        // Scene Loop exposes new params
        assert!(after.parameter_ids.contains(&param_id(&card, "twist")));
        assert!(
            after
                .parameter_ids
                .contains(&param_id(&card, "spiral_radius"))
        );
        assert!(after.parameter_ids.contains(&param_id(&card, "alternate")));
    }
}
