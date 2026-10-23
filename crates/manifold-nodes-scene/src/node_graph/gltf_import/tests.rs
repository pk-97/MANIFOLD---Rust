//! The importer reads a file into parts keyed by file node path, and the
//! mesh loader finds each part by that path.

use std::path::PathBuf;

use manifold_core::scene_doc::{ItemFields, ItemId, Shape};

use super::import_model;
use crate::node_graph::gltf_load::{GltfMeshSelector, load_gltf_mesh};

/// A GLB with one root: `rig` holds `a` (moved +1 in X), `empty` (no mesh)
/// and `b` (moved +2 in Y), each `a` and `b` drawing the same triangle.
fn write_rig_glb() -> PathBuf {
    write_triangle_glb(
        "rig",
        serde_json::json!([
            { "name": "rig", "children": [1, 2, 3] },
            { "name": "a", "mesh": 0, "translation": [1.0, 0.0, 0.0] },
            { "name": "empty" },
            { "name": "b", "mesh": 0, "translation": [0.0, 2.0, 0.0] }
        ]),
    )
}

/// A GLB whose nodes are `nodes` (node 0 the scene's one root), every mesh
/// node drawing one triangle with a corner at its origin.
fn write_triangle_glb(name: &str, nodes: serde_json::Value) -> PathBuf {
    let tri: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let mut bin = Vec::new();
    for v in &tri {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    let doc = serde_json::json!({
        "asset": { "version": "2.0" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": nodes,
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0 } }] }],
        "accessors": [{
            "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
            "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0],
        }],
        "bufferViews": [{ "buffer": 0, "byteOffset": 0, "byteLength": 36 }],
        "buffers": [{ "byteLength": bin.len() }],
    });
    let mut json = serde_json::to_vec(&doc).unwrap();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut glb = Vec::with_capacity(total);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total as u32).to_le_bytes());
    glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&json);
    glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"BIN\0");
    glb.extend_from_slice(&bin);
    let path = std::env::temp_dir().join(format!("manifold_import_{name}_{}.glb", std::process::id()));
    std::fs::write(&path, glb).unwrap();
    path
}

#[test]
fn a_model_becomes_parts_keyed_by_file_node_path() {
    let path = write_rig_glb();
    let file = path.to_string_lossy().into_owned();
    let model = import_model(&path).unwrap();
    // `rig` has two parts below it, so the import is a group of them; the
    // mesh-less `empty` node is gone, and the glTF default material both
    // parts draw with sits beside them.
    let items = model.into_items(&file, ItemId::new("rigid"));
    let [item] = items.as_slice() else {
        panic!("a two-part model is one group");
    };
    let ItemFields::Group(group) = &item.fields else {
        panic!("a two-part model is a group");
    };
    assert_eq!(group.source.as_deref(), Some(file.as_str()));
    let paths: Vec<&str> = item
        .children
        .iter()
        .filter(|child| !matches!(child.fields, ItemFields::Material(_)))
        .map(|child| match &child.fields {
            ItemFields::Object(o) => match &o.shape {
                Shape::Mesh { node_path, vertex_count, .. } => {
                    assert_eq!(*vertex_count, 3);
                    node_path.as_str()
                }
                other => panic!("part is a {other:?}"),
            },
            other => panic!("part is a {other:?}"),
        })
        .collect();
    assert_eq!(paths, ["rig/a", "rig/b"]);

    // Each part loads its own mesh, already placed in the file's world.
    let a = load_gltf_mesh(&path, GltfMeshSelector::Node { path: "rig/a".into(), material: -1, own_space: false }).unwrap();
    let b = load_gltf_mesh(&path, GltfMeshSelector::Node { path: "rig/b".into(), material: -1, own_space: false }).unwrap();
    assert_eq!((a.len(), b.len()), (3, 3));
    assert_eq!(a[0].position, [1.0, 0.0, 0.0]);
    assert_eq!(b[0].position, [0.0, 2.0, 0.0]);
    assert!(load_gltf_mesh(&path, GltfMeshSelector::Node { path: "rig/empty".into(), material: -1, own_space: false }).unwrap().is_empty());
    std::fs::remove_file(path).unwrap();
}

/// The compile reads a model's facts from the document, never from the
/// file: without them the parts compile as plain meshes, with them each part
/// knows the model's size.
#[test]
fn a_compile_reads_a_models_facts_from_the_document() {
    let path = write_rig_glb();
    let file = path.to_string_lossy().into_owned();
    let read = super::read_model(&path).unwrap();
    let mut doc = read.model.scene_doc(&file, ItemId::new("rigid"));
    let sized = |doc: &manifold_core::scene_doc::SceneDoc| {
        let def = crate::node_graph::scene_compile::compile(doc).unwrap();
        def.nodes.iter().filter(|node| node.params.contains_key("source_bbox_radius")).count()
    };
    assert_eq!(sized(&doc), 0, "the compile opened the file");
    doc.models.insert(file, read.facts);
    assert_eq!(sized(&doc), 2, "both parts read the facts");
}

/// Each part keeps its file node's transform, as Blender's glTF import does:
/// a rotated, scaled child under a translated parent nests under that
/// parent, each card starts at the file's values, the mesh stays in its
/// node's own space, and turning the child turns it about its own origin.
#[test]
fn an_imported_part_keeps_its_file_transform_and_pivot() {
    use manifold_core::scene_doc::transform::{mat4_mul, trs_matrix};
    use manifold_core::effect_graph_def::SerializedParamValue;
    use manifold_core::scene_doc::import_child_id;

    let quarter = std::f32::consts::FRAC_PI_2;
    let path = write_triangle_glb(
        "pivots",
        serde_json::json!([
            { "name": "parent", "mesh": 0, "translation": [2.0, 0.0, 0.0], "children": [1] },
            { "name": "child", "mesh": 0, "translation": [0.0, 1.0, 0.0],
              "rotation": [0.0, 0.0, (quarter / 2.0).sin(), (quarter / 2.0).cos()], "scale": [2.0, 2.0, 2.0] }
        ]),
    );
    let (def, model) = super::compile_model_scene(&path).unwrap();
    let id = ItemId::new("model");

    // The parent node is a group at its own place, holding its own mesh and
    // the child.
    let items = model.into_items(&path.to_string_lossy(), id.clone());
    let parent = items[0].children.iter().find(|item| item.name == "parent").expect("the parent node is a group");
    assert!(matches!(parent.fields, ItemFields::Group(_)));
    let names: Vec<&str> = parent.children.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, ["parent", "child"]);

    // Every card starts at the file's values.
    let card = |key: &str, property: &str| {
        let param = format!("{}.{property}", import_child_id(&id, key));
        let spec = def.preset_metadata.as_ref().unwrap().params.iter().find(|spec| spec.id == param);
        spec.unwrap_or_else(|| panic!("no card row {param}")).default_value
    };
    let rest = |key: &str| {
        let [position, rotation, scale] =
            [["pos_x", "pos_y", "pos_z"], ["rot_x", "rot_y", "rot_z"], ["scale_x", "scale_y", "scale_z"]]
                .map(|names| names.map(|property| card(key, property)));
        (position, rotation, scale)
    };
    let (group_at, group_turn, group_scale) = rest("parent");
    assert_eq!((group_at, group_turn, group_scale), ([2.0, 0.0, 0.0], [0.0; 3], [1.0; 3]));
    assert_eq!(rest("parent#mesh"), ([0.0; 3], [0.0; 3], [1.0; 3]), "the parent's mesh sits at its node");
    let (child_at, child_turn, child_scale) = rest("parent/child#mesh");
    assert_eq!(child_at, [0.0, 1.0, 0.0]);
    assert!((child_turn[2] - quarter).abs() < 1e-5 && child_turn[0].abs() < 1e-5 && child_turn[1].abs() < 1e-5, "{child_turn:?}");
    assert!(child_scale.iter().all(|s| (s - 2.0).abs() < 1e-5), "{child_scale:?}");

    // The child draws its node's own mesh, unshifted.
    let source = def
        .nodes
        .iter()
        .find(|node| node.params.get("node_path") == Some(&SerializedParamValue::String { value: "parent/child".into() }))
        .expect("the child's mesh source");
    assert_eq!(source.params.get("own_space"), Some(&SerializedParamValue::Bool { value: true }));
    assert!(!source.params.contains_key("translate_x"), "the mesh is not recentred");

    // Its cards put that mesh exactly where the file does.
    let own = load_gltf_mesh(&path, GltfMeshSelector::Node { path: "parent/child".into(), material: -1, own_space: true }).unwrap();
    let in_file =
        load_gltf_mesh(&path, GltfMeshSelector::Node { path: "parent/child".into(), material: -1, own_space: false }).unwrap();
    let world = |turn: [f32; 3]| mat4_mul(trs_matrix(group_at, group_turn, group_scale), trs_matrix(child_at, turn, child_scale));
    let place = |m: [[f32; 4]; 4], p: [f32; 3]| -> [f32; 3] {
        std::array::from_fn(|row| m[0][row] * p[0] + m[1][row] * p[1] + m[2][row] * p[2] + m[3][row])
    };
    let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5);
    for (local, file) in own.iter().zip(&in_file) {
        assert!(near(place(world(child_turn), local.position), file.position), "{:?} vs {:?}", local.position, file.position);
    }

    // Turning the child moves its mesh about its node's origin: the corner
    // at the origin stays put, the others swing round it.
    let turned = world([child_turn[0], child_turn[1], child_turn[2] + 1.0]);
    assert_eq!(own[0].position, [0.0; 3]);
    assert!(near(place(turned, own[0].position), [2.0, 1.0, 0.0]));
    assert!(!near(place(turned, own[1].position), in_file[1].position));
    std::fs::remove_file(path).unwrap();
}
