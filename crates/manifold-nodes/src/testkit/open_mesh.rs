//! An open mesh on disk for the open-mesh solid proofs
//! (SCENE_SETUP_PROGRAMME.md D8 (open meshes are solids)), shared by the MPM
//! and GPU FLIP scenes.

/// Writes the unit square in the y = 0 plane as a one-sided glTF triangle
/// list, an open mesh with no inside, to a fresh temp directory, and returns
/// the .gltf path. The caller removes the directory.
pub fn write_open_quad_gltf() -> std::path::PathBuf {
    let corners = [[-0.5f32, 0.0, -0.5], [0.5, 0.0, -0.5], [0.5, 0.0, 0.5], [-0.5, 0.0, 0.5]];
    let bin: Vec<u8> = [0, 2, 1, 0, 3, 2]
        .iter()
        .flat_map(|&i: &usize| corners[i])
        .flat_map(f32::to_le_bytes)
        .collect();
    let doc = serde_json::json!({
        "asset": { "version": "2.0" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0 }],
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0 } }] }],
        "buffers": [{ "uri": "quad.bin", "byteLength": bin.len() }],
        "bufferViews": [{ "buffer": 0, "byteOffset": 0, "byteLength": bin.len() }],
        "accessors": [{ "bufferView": 0, "componentType": 5126, "count": 6, "type": "VEC3",
            "min": [-0.5, 0.0, -0.5], "max": [0.5, 0.0, 0.5] }]
    });
    let dir = std::env::temp_dir().join(format!("manifold-open-quad-{}", manifold_core::short_id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(dir.join("quad.bin"), bin).expect("quad buffer");
    std::fs::write(dir.join("quad.gltf"), serde_json::to_vec(&doc).expect("quad json")).expect("quad gltf");
    dir.join("quad.gltf")
}
