//! Bounds of instanced and skinned nodes: the box measured must be where the
//! mesh is drawn.

use super::*;

fn le(floats: &[f32]) -> Vec<u8> {
    floats.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// A GLB of `doc` (JSON) over `bin`.
fn write_glb(name: &str, doc: serde_json::Value, bin: &[u8]) -> std::path::PathBuf {
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
    glb.extend_from_slice(bin);
    let path = std::env::temp_dir().join(format!("manifold_{name}_{}.glb", std::process::id()));
    std::fs::write(&path, glb).unwrap();
    path
}

const TRIANGLE: [f32; 9] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

#[test]
fn an_instanced_node_is_measured_across_every_instance() {
    let mut bin = le(&TRIANGLE);
    bin.extend(le(&[0.0, 0.0, 0.0, 50.0, 0.0, 0.0, 100.0, 0.0, 0.0]));
    let path = write_glb(
        "instanced_bounds",
        serde_json::json!({
            "asset": { "version": "2.0" },
            "extensionsUsed": ["EXT_mesh_gpu_instancing"],
            "scene": 0,
            "scenes": [{ "nodes": [0] }],
            "nodes": [{ "name": "tree", "mesh": 0, "extensions": {
                "EXT_mesh_gpu_instancing": { "attributes": { "TRANSLATION": 1 } } } }],
            "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0 } }] }],
            "accessors": [
                { "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
                  "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] },
                { "bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3" }
            ],
            "bufferViews": [
                { "buffer": 0, "byteOffset": 0, "byteLength": 36 },
                { "buffer": 0, "byteOffset": 36, "byteLength": 36 }
            ],
            "buffers": [{ "byteLength": bin.len() }],
        }),
        &bin,
    );
    let (document, buffers) = parse_document_and_buffers(&path).unwrap();
    let (lo, hi) = file_bounds(&document, &buffers).unwrap();
    assert_eq!((lo[0], hi[0]), (0.0, 101.0), "the forest spans every instance");
    let tree = file_node_tree(&document, &buffers);
    let part = tree[0].parts[0];
    assert_eq!(part.vertex_count, 9);
    assert_eq!(part.center[0], 50.5);
    assert_eq!(part.half_extent[0], 50.5);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn a_skinned_part_is_measured_where_its_skin_poses_it() {
    let mut bin = le(&TRIANGLE);
    bin.extend([0u16; 12].iter().flat_map(|j| j.to_le_bytes()));
    bin.extend(le(&[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]));
    let path = write_glb(
        "skinned_bounds",
        serde_json::json!({
            "asset": { "version": "2.0" },
            "scene": 0,
            "scenes": [{ "nodes": [0, 1] }],
            // glTF ignores a skinned mesh node's own transform: the node's
            // 100 m offset and 0.01 scale must not move or shrink the box.
            "nodes": [
                { "name": "body", "mesh": 0, "skin": 0, "translation": [100.0, 0.0, 0.0], "scale": [0.01, 0.01, 0.01] },
                { "name": "joint" }
            ],
            "skins": [{ "joints": [1] }],
            "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0, "JOINTS_0": 1, "WEIGHTS_0": 2 } }] }],
            "accessors": [
                { "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
                  "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0] },
                { "bufferView": 1, "componentType": 5123, "count": 3, "type": "VEC4" },
                { "bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC4" }
            ],
            "bufferViews": [
                { "buffer": 0, "byteOffset": 0, "byteLength": 36 },
                { "buffer": 0, "byteOffset": 36, "byteLength": 24 },
                { "buffer": 0, "byteOffset": 60, "byteLength": 48 }
            ],
            "buffers": [{ "byteLength": bin.len() }],
        }),
        &bin,
    );
    let (document, buffers) = parse_document_and_buffers(&path).unwrap();
    let (lo, hi) = file_bounds(&document, &buffers).unwrap();
    assert_eq!((lo[0], hi[0]), (0.0, 1.0));
    let tree = file_node_tree(&document, &buffers);
    let part = tree[0].parts[0];
    assert_eq!(part.center, [0.5, 0.5, 0.0]);
    assert_eq!(part.half_extent, [0.5, 0.5, 0.0]);
    std::fs::remove_file(path).unwrap();
}
