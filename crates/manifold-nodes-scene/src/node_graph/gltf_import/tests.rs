//! The importer reads a file into parts keyed by file node path, and the
//! mesh loader finds each part by that path.

use std::path::PathBuf;

use manifold_core::scene_doc::{ItemFields, ItemId, Shape};

use super::import_model;
use crate::node_graph::gltf_load::{GltfMeshSelector, load_gltf_mesh};

/// A GLB with one root: `rig` holds `a` (moved +1 in X), `empty` (no mesh)
/// and `b` (moved +2 in Y), each `a` and `b` drawing the same triangle.
fn write_rig_glb() -> PathBuf {
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
        "nodes": [
            { "name": "rig", "children": [1, 2, 3] },
            { "name": "a", "mesh": 0, "translation": [1.0, 0.0, 0.0] },
            { "name": "empty" },
            { "name": "b", "mesh": 0, "translation": [0.0, 2.0, 0.0] }
        ],
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
    let path = std::env::temp_dir().join(format!("manifold_import_rig_{}.glb", std::process::id()));
    std::fs::write(&path, glb).unwrap();
    path
}

#[test]
fn a_model_becomes_parts_keyed_by_file_node_path() {
    let path = write_rig_glb();
    let file = path.to_string_lossy().into_owned();
    let model = import_model(&path).unwrap();
    // `rig` has two parts below it, so the import is a group of them; the
    // mesh-less `empty` node is gone.
    let item = model.into_item(&file, ItemId::new("rigid"));
    let ItemFields::Group(group) = &item.fields else {
        panic!("a two-part model is a group");
    };
    assert_eq!(group.source.as_deref(), Some(file.as_str()));
    let paths: Vec<&str> = item
        .children
        .iter()
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
    let a = load_gltf_mesh(&path, GltfMeshSelector::Node { path: "rig/a".into() }).unwrap();
    let b = load_gltf_mesh(&path, GltfMeshSelector::Node { path: "rig/b".into() }).unwrap();
    assert_eq!((a.len(), b.len()), (3, 3));
    assert_eq!(a[0].position, [1.0, 0.0, 0.0]);
    assert_eq!(b[0].position, [0.0, 2.0, 0.0]);
    assert!(load_gltf_mesh(&path, GltfMeshSelector::Node { path: "rig/empty".into() }).unwrap().is_empty());
    std::fs::remove_file(path).unwrap();
}
