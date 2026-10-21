//! Peter's cliftest show: three photoscans merged into one scene, each a
//! physics object. It loads through the app's load path with one Model File
//! per file, and adding a Radial Force changes no object (BUG-2bulf, typed
//! identity D5). The fixture is the show's own project.json, taken from a
//! copy with project_tool.
use super::*;
use manifold_core::binding_identity;

const CLIFTEST: &str = include_str!("../../../manifold-io/tests/fixtures/cliftest_project.json");

/// The object index of a file-bound node: `mesh_1`, `tex_1`, … and the
/// rigid bodies, which carry hashed ids, by their file.
const BODY_NODES: [&str; 3] = ["78a4664073ed1dd4", "2c3b4bcc327027b7", "722ad50d60d7bdb2"];
const FILES: [&str; 3] = ["coastal_cliff_02_2k.gltf", "moss_rock_04_rock_pack_vol.1.glb", "rock_16_rock_pack_vol.3.glb"];

fn object_of(node: &str) -> Option<usize> {
    if let Some(k) = BODY_NODES.iter().position(|body| *body == node) {
        return Some(k);
    }
    node.rsplit_once('_').and_then(|(_, k)| k.parse().ok())
}

fn loaded() -> (Project, LayerId, Vec<String>) {
    let mut project = manifold_io::loader::load_project_from_json_with(
        CLIFTEST,
        crate::project_io::install_embedded_presets,
    )
    .expect("cliftest loads");
    let notices = crate::project_io::prepare_loaded_project(&mut project);
    let layer = project
        .timeline
        .layers
        .iter()
        .find(|layer| layer.gen_params().is_some_and(|host| host.graph.is_some()))
        .expect("the scene layer")
        .layer_id
        .clone();
    (project, layer, notices)
}

/// Each object's mesh, textures and collider bind its own file under its own
/// id, and that id's Model File names the file.
fn assert_each_object_reads_its_own_file(graph: &EffectGraphDef) {
    let meta = graph.preset_metadata.as_ref().unwrap();
    let mut ids: [Option<String>; 3] = Default::default();
    for binding in meta.string_bindings.iter().filter(|b| b.id.starts_with("model_file")) {
        let BindingTarget::Node { node_id, .. } = &binding.target else { panic!("node binding") };
        let k = object_of(node_id.as_str()).unwrap_or_else(|| panic!("{node_id} belongs to an object"));
        assert!(binding.default_value.ends_with(FILES[k]), "{node_id} reads {}", binding.default_value);
        let id = ids[k].get_or_insert_with(|| binding.id.clone());
        assert_eq!(*id, binding.id, "{node_id} binds under its object's id");
        let spec = meta.string_params.iter().find(|spec| spec.id == binding.id).expect("a Model File per id");
        assert!(spec.default_value.ends_with(FILES[k]), "{} names {}", spec.id, spec.default_value);
    }
    let ids: Vec<String> = ids.into_iter().map(|id| id.expect("every object binds a file")).collect();
    assert_eq!(ids, ["model_file", "model_file_2", "model_file_3"]);
}

#[test]
fn cliftest_loads_one_model_file_per_file() {
    let (project, layer, notices) = loaded();
    let graph = host_graph(&project, &layer);
    assert!(binding_identity::check(graph.preset_metadata.as_ref().unwrap()).is_empty());
    assert_eq!(notices.iter().filter(|line| line.contains("split")).count(), 2, "{notices:?}");
    assert_each_object_reads_its_own_file(graph);

    // The moss and rock cards hold Peter's Scale 2.0 and render it: the
    // binding cache writes a card value whatever the node held at build.
    let host = project.timeline.find_layer_by_id(&layer).unwrap().1.gen_params().unwrap();
    for id in ["124_scale_x", "124_scale_y", "124_scale_z", "135_scale_x", "135_scale_y", "135_scale_z"] {
        assert_eq!(host.params.get(id).map(|p| p.value), Some(2.0), "{id}");
    }

    // A second load changes nothing more.
    let mut again = project.clone();
    assert!(crate::project_io::migrate_project_scene_graphs(&mut again).iter().all(|line| !line.contains("split")));
    assert_eq!(host_graph(&again, &layer), graph);
}

#[test]
fn a_radial_force_on_cliftest_changes_no_object() {
    let (mut project, layer, _) = loaded();
    let before = host_graph(&project, &layer).clone();
    let mut editing = EditingService::new();
    apply_stock(&mut editing, &mut project, &layer, "RadialForce");
    let forced = host_graph(&project, &layer).clone();
    assert_eq!(forced.scene_modifiers.len(), 1);
    assert_each_object_reads_its_own_file(&forced);

    let registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
    let prepared = manifold_node_engine::load::expand::prepare_scene_modifiers(&forced, &registry)
        .expect("the force prepares on cliftest")
        .def;
    let authored = manifold_core::flatten::flatten_groups(&before).unwrap();
    for node in &authored.nodes {
        let prepared = prepared
            .nodes
            .iter()
            .find(|candidate| candidate.node_id == node.node_id)
            .unwrap_or_else(|| panic!("{} survives preparation", node.node_id));
        assert_eq!(prepared.params, node.params, "{} changed when the force was added", node.node_id);
    }
    let prepared_meta = prepared.preset_metadata.as_ref().unwrap();
    for binding in prepared_meta.string_bindings.iter().filter(|b| b.id.starts_with("model_file")) {
        let BindingTarget::Node { node_id, .. } = &binding.target else { continue };
        let k = object_of(node_id.as_str()).unwrap();
        assert!(binding.default_value.ends_with(FILES[k]), "prepared {node_id} reads {}", binding.default_value);
    }
}
