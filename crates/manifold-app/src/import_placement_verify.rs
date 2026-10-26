//! Import placement through the real async completion seam: a clean outer
//! root, one persistent centring translation, and no camera mutation.

#![cfg(test)]

use std::collections::BTreeMap;

use manifold_core::LayerId;
use manifold_core::scene_doc::{
    ItemFields, ItemId, ModelFacts, SceneDoc, import_placement_id, param_id,
};
use manifold_editing::service::EditingService;

use crate::app::Application;
use crate::content_command::ContentCommand;
use crate::edit_selection::{EditSelection, SelectAfterEdit};
use crate::import_worker::{ImportProgress, ImportTarget};

struct Parsed {
    path: std::path::PathBuf,
    model: manifold_core::scene_doc::ImportedModel,
    facts: ModelFacts,
    center: [f32; 3],
}

fn parsed(file: &str) -> Parsed {
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/gltf/"
    ))
    .join(file);
    let read = manifold_nodes_scene::node_graph::gltf_import::read_model(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let (center, _) = manifold_nodes_scene::node_graph::gltf_import::world_bounds(&read.facts)
        .unwrap_or_else(|| panic!("{}: no mesh bounds", path.display()));
    Parsed {
        path,
        model: read.model,
        facts: read.facts,
        center,
    }
}

fn headless_application() -> (Application, crossbeam_channel::Receiver<ContentCommand>) {
    let mut app = Application::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    app.content_tx = Some(tx);
    (app, rx)
}

fn add_scene_layer(app: &mut Application) -> LayerId {
    let mut project = (*app.local_project).clone();
    let index = project.timeline.add_layer(
        "Scene",
        manifold_core::types::LayerType::Generator,
        manifold_core::PresetTypeId::SCENE,
    );
    let layer_id = project.timeline.layers[index].layer_id.clone();
    app.local_project = crate::ui_bridge::LocalProject::new(project);
    layer_id
}

fn send(app: &mut Application, parsed: &Parsed, target: ImportTarget) {
    app.import_progress_tx
        .send(ImportProgress::Done {
            path: parsed.path.clone(),
            file: parsed.path.clone(),
            model: Box::new(parsed.model.clone()),
            facts: parsed.facts.clone(),
            warnings: Vec::new(),
            conversion_report_line: None,
            target,
        })
        .unwrap();
    app.drain_import_progress();
}

/// Run the import's selecting edit on the content thread's side; the group it
/// selects comes back.
fn execute_into(
    app: &mut Application,
    service: &mut EditingService,
    rx: &crossbeam_channel::Receiver<ContentCommand>,
) -> ItemId {
    let sent: Vec<_> = rx.try_iter().collect();
    let selecting = sent
        .iter()
        .filter(|c| {
            matches!(
                c,
                ContentCommand::ExecuteSelecting(_, SelectAfterEdit::NewItem(..))
            )
        })
        .count();
    assert_eq!(selecting, 1, "one selecting import command");
    let outcomes = crate::edit_runner::land_on_app(app, service, sent);
    outcomes
        .iter()
        .find_map(|outcome| match outcome.selection.as_ref() {
            Some(EditSelection::Item { item, .. }) => Some(item.clone()),
            _ => None,
        })
        .expect("the into import selected its group")
}

fn layer_with_file(app: &Application, file: &str) -> LayerId {
    app.local_project
        .timeline
        .layers
        .iter()
        .find(|layer| {
            layer
                .gen_params()
                .and_then(|fx| fx.scene.as_ref())
                .is_some_and(|doc| doc.models.contains(file))
        })
        .map(|layer| layer.layer_id.clone())
        .expect("the import's layer")
}

fn doc<'a>(app: &'a Application, layer: &LayerId) -> &'a SceneDoc {
    app.local_project.scene_of(layer).expect("a scene document")
}

fn import_root(doc: &SceneDoc, file: &str) -> ItemId {
    doc.items
        .iter()
        .find(|item| matches!(&item.fields, ItemFields::Group(group) if group.source.as_deref() == Some(file)))
        .map(|item| item.id.clone())
        .expect("the outer import Group")
}

fn param(app: &Application, layer: &LayerId, item: &ItemId, property: &str) -> Option<f32> {
    let (_, layer) = app
        .local_project
        .timeline
        .find_layer_by_id(layer.as_str())?;
    layer
        .gen_params()?
        .params
        .get(&param_id(item, property))
        .map(|param| param.base)
}

fn placement(app: &Application, layer: &LayerId, group: &ItemId) -> [f32; 3] {
    let placement = import_placement_id(group);
    ["pos_x", "pos_y", "pos_z"]
        .map(|property| param(app, layer, &placement, property).unwrap_or(0.0))
}

fn camera_values(app: &Application, layer: &LayerId) -> (ItemId, BTreeMap<String, f32>) {
    let doc = doc(app, layer);
    let (_, layer) = app
        .local_project
        .timeline
        .find_layer_by_id(layer.as_str())
        .unwrap();
    let inst = layer.gen_params().unwrap();
    let pick = doc
        .camera_pick_param()
        .filter(|id| inst.params.contains(id))
        .map_or(0.0, |id| inst.get_base_param(&id));
    let camera = doc.picked_camera(pick).expect("the show camera").clone();
    let prefix = format!("{camera}.");
    let values = inst
        .params
        .iter()
        .filter(|param| param.id().starts_with(&prefix))
        .map(|param| (param.id().to_owned(), param.base))
        .collect();
    (camera, values)
}

fn assert_placement(app: &Application, layer: &LayerId, group: &ItemId, center: [f32; 3]) {
    assert_eq!(placement(app, layer, group), center.map(|value| -value));
}

#[test]
fn a_fresh_drop_centres_and_selects_a_clean_root_without_fitting_the_camera() {
    let parsed = parsed("DamagedHelmet.glb");
    let file = parsed.path.to_string_lossy().into_owned();
    let (mut app, rx) = headless_application();
    let reference = add_scene_layer(&mut app);
    let default_camera = camera_values(&app, &reference);

    send(
        &mut app,
        &parsed,
        ImportTarget::Drop {
            beat: 0.0,
            under: None,
        },
    );
    crate::edit_runner::land_on_app(&mut app, &mut EditingService::new(), rx.try_iter());

    let layer = layer_with_file(&app, &file);
    let doc = doc(&app, &layer);
    let group = import_root(doc, &file);
    assert_eq!(
        camera_values(&app, &reference),
        default_camera,
        "the existing camera is untouched"
    );
    assert_eq!(
        camera_values(&app, &layer),
        default_camera,
        "a new import layer keeps the default camera"
    );
    for property in ["pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z"] {
        assert_eq!(
            param(&app, &layer, &group, property),
            Some(0.0),
            "outer root {property}"
        );
    }
    for property in ["scale_x", "scale_y", "scale_z"] {
        assert_eq!(
            param(&app, &layer, &group, property),
            Some(1.0),
            "outer root {property}"
        );
    }
    assert_placement(&app, &layer, &group, parsed.center);
    assert!(
        doc.find(&import_placement_id(&group)).is_some(),
        "a single-mesh import also gets the persistent placement Group"
    );
    let (_, layer_ref) = app
        .local_project
        .timeline
        .find_layer_by_id(layer.as_str())
        .unwrap();
    let state = crate::ui_bridge::scene_state(
        &app.local_project,
        Some(layer_ref),
        &crate::layer_notices::LayerNotices::default(),
        &[],
    );
    let vm = state.as_live().expect("live scene state");
    assert!(
        vm.item(&import_placement_id(&group)).is_none(),
        "the internal File Placement wrapper group is hidden from the Scene tree"
    );
    assert!(
        vm.items
            .iter()
            .any(|item| item.parent.as_ref() == Some(&group) && item.depth == 1),
        "imported mesh parts appear directly at depth 1 under the outer import group"
    );
    assert_eq!(
        app.ws.ui_root.scene_setup_panel.selected_of(&layer),
        [manifold_ui::panels::scene_setup_panel::SceneSelection::Item(group)],
        "the clean outer root is selected"
    );
}

#[test]
fn repeated_imports_undo_facts_and_swaps_preserve_camera_and_placement() {
    let helmet = parsed("DamagedHelmet.glb");
    let lowe = parsed("lowe.glb");
    let helmet_file = helmet.path.to_string_lossy().into_owned();
    let lowe_file = lowe.path.to_string_lossy().into_owned();
    let (mut app, rx) = headless_application();
    let mut service = EditingService::new();
    let layer = add_scene_layer(&mut app);
    let camera = camera_values(&app, &layer);

    send(
        &mut app,
        &helmet,
        ImportTarget::Into {
            layer: layer.clone(),
            parent: None,
        },
    );
    let first = execute_into(&mut app, &mut service, &rx);
    let first_placement = placement(&app, &layer, &first);
    assert_eq!(camera_values(&app, &layer), camera);
    assert_placement(&app, &layer, &first, helmet.center);

    send(
        &mut app,
        &lowe,
        ImportTarget::Into {
            layer: layer.clone(),
            parent: None,
        },
    );
    let second = execute_into(&mut app, &mut service, &rx);
    let second_placement = placement(&app, &layer, &second);
    assert_eq!(
        placement(&app, &layer, &first),
        first_placement,
        "the second import leaves the first alone"
    );
    assert_placement(&app, &layer, &second, lowe.center);
    assert_eq!(
        camera_values(&app, &layer),
        camera,
        "the second import leaves the camera alone"
    );

    crate::edit_runner::land_on_app(&mut app, &mut service, [ContentCommand::Undo]);
    assert!(
        doc(&app, &layer).find(&second).is_none(),
        "undo removes the second import"
    );
    assert_eq!(placement(&app, &layer, &first), first_placement);
    assert_eq!(
        camera_values(&app, &layer),
        camera,
        "undo does not move the camera"
    );
    crate::edit_runner::land_on_app(&mut app, &mut service, [ContentCommand::Redo]);

    send(
        &mut app,
        &helmet,
        ImportTarget::Swap {
            layer: layer.clone(),
            item: second.clone(),
        },
    );
    crate::edit_runner::land_on_app(&mut app, &mut service, rx.try_iter());
    assert!(
        doc(&app, &layer).models.contains(&helmet_file),
        "the swap attached the new facts"
    );
    assert!(
        doc(&app, &layer).models.contains(&lowe_file),
        "the earlier file's facts remain available"
    );
    assert_eq!(
        placement(&app, &layer, &second),
        second_placement,
        "a swap never recentres"
    );
    assert_eq!(placement(&app, &layer, &first), first_placement);
    assert_eq!(
        camera_values(&app, &layer),
        camera,
        "a swap never moves the camera"
    );

    let (facts, _) = manifold_nodes_scene::node_graph::gltf_import::read_facts(&lowe.path).unwrap();
    app.import_progress_tx
        .send(ImportProgress::Facts {
            path: lowe_file,
            read: Ok((facts, Vec::new())),
        })
        .unwrap();
    app.drain_import_progress();
    crate::edit_runner::land_on_app(&mut app, &mut service, rx.try_iter());
    assert_eq!(placement(&app, &layer, &second), second_placement);
    assert_eq!(
        camera_values(&app, &layer),
        camera,
        "a later facts read changes no camera values"
    );
}
