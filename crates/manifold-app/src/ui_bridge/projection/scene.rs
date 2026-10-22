//! The Scene tab's view model, built from the scene document
//! (SCENE_DOCUMENT_DESIGN.md section 8): the items as a depth-first list,
//! each with the manifest params it owns, its structural field rows resolved
//! to names, and the Material it draws with. Reads only.

use crate::ui_root::UIRoot;
use manifold_core::effects::PresetInstance;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::scene_doc::{EnvironmentSource, Item, ItemFields, ItemId, ItemKind, LightType, MaterialType, SceneDoc, Shape, param_id, split_param_id};
use manifold_core::scene_schema::{
    ADD_ENTRIES, Choice, FieldControl, FieldRow, FieldValue, applicable_fields, break_unavailable, choice_disabled, impulse_unavailable, item_disabled,
};
use manifold_core::types::LayerType;
use manifold_core::LayerId;
use manifold_core::scene_surface::FieldPick;
use manifold_ui::panels::scene_setup_panel::{
    AddEntryVm, FieldControlVm, FieldOptionVm, FieldRowVm, MaterialBlockVm, SceneItemVm, SceneSetupState, SceneSetupVm,
};

/// The panel's state for the selected layer.
pub(crate) fn scene_state(project: &Project, layer: Option<&Layer>) -> SceneSetupState {
    let Some(layer) = layer else {
        return SceneSetupState::NoSelection("Select a layer to set up its scene.".to_owned());
    };
    if layer.layer_type != LayerType::Generator {
        return SceneSetupState::NoSelection("Select a generator layer to set up its scene.".to_owned());
    }
    let layer_id = layer.layer_id.clone();
    let (Some(inst), Some(doc)) = (layer.gen_params(), project.scene_of(&layer_id)) else {
        return SceneSetupState::NoScene { layer_id };
    };
    SceneSetupState::Live(Box::new(scene_vm(project, layer_id, doc, inst)))
}

fn scene_vm(project: &Project, layer_id: LayerId, doc: &SceneDoc, inst: &PresetInstance) -> SceneSetupVm {
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
    let materials = Materials { effective: doc.effective_materials(), users: doc.material_users() };
    let mut items = Vec::new();
    doc.walk(|item, ancestors| {
        items.push(item_vm(doc, inst, item, ancestors, &owned, &layers, &materials));
    });
    let add_entries = ADD_ENTRIES
        .iter()
        .map(|entry| AddEntryVm {
            key: entry.key.to_owned(),
            group: entry.group.to_owned(),
            label: entry.label.to_owned(),
            top_level: manifold_core::scene_doc::can_hold(None, entry.kind),
            holders: ItemKind::ALL.into_iter().filter(|holder| manifold_core::scene_doc::can_hold(Some(*holder), entry.kind)).collect(),
            disabled: entry.fields.and_then(|fields| manifold_core::scene_schema::add_refused(doc, &fields())).map(str::to_owned),
        })
        .collect();
    SceneSetupVm { layer_id, items, add_entries }
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
    let params_of = |id: &ItemId| owned.get(id.as_str()).cloned().unwrap_or_default();
    let fields = field_rows(doc, item, layers);
    let material_item = match item.kind() {
        ItemKind::Material => Some(item.id.clone()),
        ItemKind::Object | ItemKind::Group | ItemKind::Water => materials.effective.get(&item.id).cloned(),
        _ => None,
    };
    let material = material_item
        .and_then(|id| doc.find(&id))
        .map(|mat| material_block(doc, inst, mat, owned, layers, materials.users.get(&mat.id).copied().unwrap_or(0)));
    let (environment, aim_sun_file) = match &item.fields {
        ItemFields::Light(light) => match &light.light {
            LightType::Environment { source } => (
                true,
                match source {
                    EnvironmentSource::Hdri { file } | EnvironmentSource::SeaHorizon { file } if !file.is_empty() => Some(file.clone()),
                    _ => None,
                },
            ),
            _ => (false, None),
        },
        _ => (false, None),
    };
    SceneItemVm {
        id: item.id.clone(),
        kind: item.kind(),
        name: item.name.clone(),
        depth: ancestors.len(),
        parent: ancestors.last().map(|parent| parent.id.clone()),
        has_children: !item.children.is_empty(),
        parameter_ids: params_of(&item.id),
        fields,
        material,
        removable: item.kind() != ItemKind::World,
        plane: matches!(&item.fields, ItemFields::Object(object) if object.shape == Shape::Plane),
        copy_view: matches!(&item.fields, ItemFields::Camera(_)),
        environment,
        aim_sun_file,
        row_reasons: row_reasons(doc, item),
    }
}

/// Why rows of `item` cannot be used: a Force whose Impulse has nothing to
/// push, a Shatter card with no Body to break.
fn row_reasons(doc: &SceneDoc, item: &Item) -> Vec<(String, String)> {
    let mut reasons = Vec::new();
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
        ItemFields::Material(fields) if pbr => fields.features.iter().map(|setting| super::material::feature(setting.feature)).collect(),
        _ => Vec::new(),
    };
    let parameter_ids = owned.get(material.id.as_str()).cloned().unwrap_or_default();
    let look = match &material.fields {
        ItemFields::Material(fields) if pbr => crate::scene_material_look::matching(inst, &material.id, &parameter_ids, fields),
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
        .map(|row| FieldRowVm {
            id: row.id.to_owned(),
            label: row.label.to_owned(),
            section: row.section,
            control: control(doc, item, row, layers),
            disabled: (row.disabled)(doc, item).map(str::to_owned),
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
        })
        .collect()
}

fn item_options(doc: &SceneDoc, item: &Item, field: &str, passes: fn(&Item) -> bool, held: &FieldValue) -> Vec<FieldOptionVm> {
    let mut options = Vec::new();
    doc.walk(|other, _| {
        if other.id != item.id && passes(other) {
            let checked = matches!(held, FieldValue::Item(Some(id)) if *id == other.id);
            options.push(FieldOptionVm {
                label: other.name.clone(),
                pick: FieldPick::Item(Some(other.id.clone())),
                checked,
                disabled: item_disabled(doc, item, field, other).filter(|_| !checked).map(str::to_owned),
            });
        }
    });
    options
}

fn control(doc: &SceneDoc, item: &Item, row: &FieldRow, layers: &[(LayerId, String)]) -> FieldControlVm {
    let held = (row.read)(&item.fields);
    let pick = |options: Vec<FieldOptionVm>| {
        let mut current = options.iter().filter(|option| option.checked).map(|option| option.label.as_str()).collect::<Vec<_>>().join(", ");
        if current.is_empty() {
            current.push_str("Choose");
        }
        FieldControlVm::Pick { current, options }
    };
    match row.control {
        FieldControl::Closed(choices) => {
            let mut options = closed(choices, &held);
            for (option, choice) in options.iter_mut().zip(choices) {
                option.disabled = choice_disabled(doc, item, row.id, choice.key).map(str::to_owned);
            }
            pick(options)
        }
        FieldControl::ClosedOrItem { closed: choices, items } => {
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
            }];
            options.extend(item_options(doc, item, row.id, items, &held));
            pick(options)
        }
        FieldControl::ItemSet { items, all, refused } => {
            let set = match &held {
                FieldValue::Items(set) => set.as_slice(),
                _ => &[],
            };
            let everything = held == FieldValue::Everything;
            let mut options = vec![FieldOptionVm { label: all.to_owned(), pick: FieldPick::All, checked: everything, disabled: None }];
            doc.walk(|other, _| {
                if other.id != item.id && items(other) {
                    let checked = set.contains(&other.id);
                    options.push(FieldOptionVm {
                        label: other.name.clone(),
                        pick: FieldPick::Toggle(other.id.clone()),
                        checked,
                        // A held target stays pickable, so it can be taken off.
                        disabled: refused(&item.fields, other).filter(|_| !checked).map(str::to_owned),
                    });
                }
            });
            pick(options)
        }
        FieldControl::ClosedOrLayer(choices) => {
            let mut options = closed(choices, &held);
            options.extend(layers.iter().map(|(id, name)| FieldOptionVm {
                label: name.clone(),
                pick: FieldPick::Layer(id.clone()),
                checked: matches!(&held, FieldValue::Layer(held) if held == id),
                disabled: None,
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
                    FieldOptionVm { label: name.clone(), pick: FieldPick::Choice(key), checked, disabled: None }
                })
                .collect(),
        ),
        FieldControl::File { extensions } => FieldControlVm::File {
            path: match held {
                FieldValue::File(path) => path,
                _ => String::new(),
            },
            extensions: extensions.iter().map(|extension| (*extension).to_owned()).collect(),
        },
    }
}

/// Per-frame VALUE sync for the Scene tab's rows — the scene-row sibling of
/// `sync_card_values`: push each built row's current value from the layer's
/// manifest onto the already-built panel (rows and outliner eyes), so rows
/// track OSC, command and other-window writes between structural syncs.
/// No-op while the panel is closed or not Live.
pub fn sync_scene_row_values(ui: &mut UIRoot, project: &Project) {
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
        crate::ui_translate::with_param_slots(&gp.params, |slots| ui.scene_setup_panel.sync_properties_values(&mut ui.tree, slots));
    }
}

/// The outliner eyes follow each item's Visible between structural syncs: the
/// eyes are rows of the same value channel the properties read.
pub fn sync_scene_outliner_eyes(ui: &mut UIRoot, project: &Project) {
    sync_scene_row_values(ui, project);
}
