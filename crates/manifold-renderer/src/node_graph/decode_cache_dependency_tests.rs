use super::*;
use std::fs;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_GLTF_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempCacheRoot {
    path: PathBuf,
}

impl Deref for TempCacheRoot {
    type Target = std::path::Path;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl Drop for TempCacheRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct ExternalGltfFixture {
    dir: PathBuf,
    primary: PathBuf,
    position_buffer: PathBuf,
}

impl Drop for ExternalGltfFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn temp_fixture_dir(label: &str) -> PathBuf {
    let counter = TEMP_GLTF_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("manifold-{label}-{}-{counter}", std::process::id()));
    fs::create_dir(&path).unwrap();
    path
}

fn temp_cache_root() -> TempCacheRoot {
    TempCacheRoot {
        path: temp_fixture_dir("gltf-cache-root"),
    }
}

fn reset_counters() {
    super::HDRI_HITS.with(|counter| counter.set(0));
    super::HDRI_MISSES.with(|counter| counter.set(0));
    super::GLTF_MESH_HITS.with(|counter| counter.set(0));
    super::GLTF_MESH_MISSES.with(|counter| counter.set(0));
}

fn f32_bytes(positions: [[f32; 3]; 3]) -> Vec<u8> {
    positions
        .into_iter()
        .flat_map(|row| row.into_iter().flat_map(f32::to_le_bytes))
        .collect()
}

fn triangle_indices() -> Vec<u8> {
    [0u16, 1, 2]
        .into_iter()
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn write_external_gltf(positions: [[f32; 3]; 3]) -> ExternalGltfFixture {
    let dir = temp_fixture_dir("gltf-cache-dependencies");
    let primary = dir.join("triangle.gltf");
    let index_buffer = dir.join("indices.bin");
    let position_buffer = dir.join("position buffer.bin");
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": [{
            "attributes": {"POSITION": 1},
            "indices": 0
        }]}],
        "buffers": [
            {"uri": "indices.bin", "byteLength": 6},
            {"uri": "position%20buffer.bin", "byteLength": 36}
        ],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 6},
            {"buffer": 1, "byteOffset": 0, "byteLength": 36}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5123, "count": 3, "type": "SCALAR"},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3",
             "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]}
        ]
    });
    fs::write(&primary, serde_json::to_vec(&document).unwrap()).unwrap();
    fs::write(index_buffer, triangle_indices()).unwrap();
    fs::write(&position_buffer, f32_bytes(positions)).unwrap();
    ExternalGltfFixture {
        dir,
        primary,
        position_buffer,
    }
}

fn positions(vertices: &[MeshVertex]) -> Vec<[f32; 3]> {
    vertices.iter().map(|vertex| vertex.position).collect()
}

fn move_fixture(fixture: &mut ExternalGltfFixture) {
    let moved_dir = temp_fixture_dir("gltf-cache-moved");
    fs::remove_dir(&moved_dir).unwrap();
    fs::rename(&fixture.dir, &moved_dir).unwrap();
    fixture.dir = moved_dir.clone();
    fixture.primary = moved_dir.join("triangle.gltf");
    fixture.position_buffer = moved_dir.join("position buffer.bin");
}

fn write_embedded_triangle_glb() -> (PathBuf, PathBuf) {
    let dir = temp_fixture_dir("gltf-cache-glb");
    let path = dir.join("triangle.glb");
    let mut binary = triangle_indices();
    binary.extend_from_slice(&[0, 0]);
    binary.extend_from_slice(&f32_bytes([
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
    ]));
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": [{
            "attributes": {"POSITION": 1},
            "indices": 0
        }]}],
        "buffers": [{"byteLength": 44}],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 6},
            {"buffer": 0, "byteOffset": 8, "byteLength": 36}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5123, "count": 3, "type": "SCALAR"},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3",
             "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]}
        ]
    });
    let mut json = serde_json::to_vec(&document).unwrap();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    assert!(binary.len().is_multiple_of(4));

    let total_length = 12 + 8 + json.len() + 8 + binary.len();
    let mut glb = Vec::with_capacity(total_length);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total_length as u32).to_le_bytes());
    glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&json);
    glb.extend_from_slice(&(binary.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"BIN\0");
    glb.extend_from_slice(&binary);
    fs::write(&path, glb).unwrap();
    (dir, path)
}

#[test]
fn external_buffer_warm_decode_hits_cache() {
    reset_counters();
    let cache = temp_cache_root();
    let fixture = write_external_gltf([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);

    let first = cached_load_gltf_mesh_with_root(
        &fixture.primary,
        GltfMeshSelector::WholeScene,
        Some(cache.to_path_buf()),
    )
    .unwrap();
    let second = cached_load_gltf_mesh_with_root(
        &fixture.primary,
        GltfMeshSelector::WholeScene,
        Some(cache.to_path_buf()),
    )
    .unwrap();

    assert_eq!(positions(&first), positions(&second));
    assert_eq!(first.len(), 3);
    assert_eq!(gltf_mesh_hits(), 1);
    assert_eq!(gltf_mesh_misses(), 1);
}

#[test]
fn changing_same_size_second_external_buffer_misses_and_updates_vertices() {
    reset_counters();
    let cache = temp_cache_root();
    let fixture = write_external_gltf([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
    let primary_before = fs::read(&fixture.primary).unwrap();
    let first = cached_load_gltf_mesh_with_root(
        &fixture.primary,
        GltfMeshSelector::WholeScene,
        Some(cache.to_path_buf()),
    )
    .unwrap();

    fs::write(
        &fixture.position_buffer,
        f32_bytes([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.5, 1.0, 0.0]]),
    )
    .unwrap();
    assert_eq!(primary_before, fs::read(&fixture.primary).unwrap());
    let second = cached_load_gltf_mesh_with_root(
        &fixture.primary,
        GltfMeshSelector::WholeScene,
        Some(cache.to_path_buf()),
    )
    .unwrap();

    assert_ne!(positions(&first), positions(&second));
    assert_eq!(positions(&second)[2], [0.5, 1.0, 0.0]);
    assert_eq!(gltf_mesh_hits(), 0);
    assert_eq!(gltf_mesh_misses(), 2);
}

#[test]
fn identical_primary_files_with_different_external_buffers_stay_distinct() {
    reset_counters();
    let cache = temp_cache_root();
    let first_fixture = write_external_gltf([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
    let second_fixture = write_external_gltf([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.25, 1.0, 0.0]]);
    assert_eq!(
        fs::read(&first_fixture.primary).unwrap(),
        fs::read(&second_fixture.primary).unwrap()
    );

    let first = cached_load_gltf_mesh_with_root(
        &first_fixture.primary,
        GltfMeshSelector::WholeScene,
        Some(cache.to_path_buf()),
    )
    .unwrap();
    let second = cached_load_gltf_mesh_with_root(
        &second_fixture.primary,
        GltfMeshSelector::WholeScene,
        Some(cache.to_path_buf()),
    )
    .unwrap();

    assert_ne!(positions(&first), positions(&second));
    assert_eq!(positions(&second)[2], [0.25, 1.0, 0.0]);
    assert_eq!(gltf_mesh_hits(), 0);
    assert_eq!(gltf_mesh_misses(), 2);
}

#[test]
fn moving_unchanged_primary_and_external_buffers_preserves_cache_hit() {
    reset_counters();
    let cache = temp_cache_root();
    let mut fixture = write_external_gltf([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
    let first = cached_load_gltf_mesh_with_root(
        &fixture.primary,
        GltfMeshSelector::WholeScene,
        Some(cache.to_path_buf()),
    )
    .unwrap();
    move_fixture(&mut fixture);
    let second = cached_load_gltf_mesh_with_root(
        &fixture.primary,
        GltfMeshSelector::WholeScene,
        Some(cache.to_path_buf()),
    )
    .unwrap();

    assert_eq!(positions(&first), positions(&second));
    assert_eq!(gltf_mesh_hits(), 1);
    assert_eq!(gltf_mesh_misses(), 1);
}

#[test]
fn decoded_snapshot_retains_geometry_after_source_changes_and_identity_changes() {
    let fixture = write_external_gltf([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
    let snapshot = parse_buffer_snapshot(&fixture.primary).unwrap();
    fs::write(
        &fixture.position_buffer,
        f32_bytes([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.75, 1.0, 0.0]]),
    )
    .unwrap();
    let changed = parse_buffer_snapshot(&fixture.primary).unwrap();
    fs::remove_file(&fixture.primary).unwrap();
    fs::remove_file(&fixture.position_buffer).unwrap();
    // Flatten only after the underlying files have changed and disappeared.
    let retained = load_gltf_mesh_from_buffers(
        &snapshot.document,
        &snapshot.buffers,
        GltfMeshSelector::WholeScene,
    )
    .unwrap();
    let fresh = load_gltf_mesh_from_buffers(
        &changed.document,
        &changed.buffers,
        GltfMeshSelector::WholeScene,
    )
    .unwrap();

    assert_ne!(snapshot.identity, changed.identity);
    assert_eq!(positions(&retained)[2], [0.0, 1.0, 0.0]);
    assert_eq!(positions(&fresh)[2], [0.75, 1.0, 0.0]);
}

#[test]
fn missing_or_truncated_external_buffer_cannot_use_populated_cache() {
    reset_counters();
    let cache = temp_cache_root();
    let fixture = write_external_gltf([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
    cached_load_gltf_mesh_with_root(
        &fixture.primary,
        GltfMeshSelector::WholeScene,
        Some(cache.to_path_buf()),
    )
    .unwrap();

    fs::remove_file(&fixture.position_buffer).unwrap();
    let missing = cached_load_gltf_mesh_with_root(
        &fixture.primary,
        GltfMeshSelector::WholeScene,
        Some(cache.to_path_buf()),
    );
    assert!(
        missing.is_err(),
        "missing dependency must reject a warm cache entry"
    );
    assert_eq!(gltf_mesh_hits(), 0);

    fs::write(&fixture.position_buffer, [0u8; 4]).unwrap();
    let truncated = cached_load_gltf_mesh_with_root(
        &fixture.primary,
        GltfMeshSelector::WholeScene,
        Some(cache.to_path_buf()),
    );
    assert!(
        truncated.is_err(),
        "truncated dependency must reject a warm cache entry"
    );
    assert_eq!(gltf_mesh_hits(), 0);
}

#[test]
fn embedded_glb_mesh_still_warms_cache() {
    reset_counters();
    let cache = temp_cache_root();
    let (dir, path) = write_embedded_triangle_glb();
    let first = cached_load_gltf_mesh_with_root(
        &path,
        GltfMeshSelector::WholeScene,
        Some(cache.to_path_buf()),
    )
    .unwrap();
    let second = cached_load_gltf_mesh_with_root(
        &path,
        GltfMeshSelector::WholeScene,
        Some(cache.to_path_buf()),
    )
    .unwrap();

    assert_eq!(positions(&first), positions(&second));
    assert_eq!(first.len(), 3);
    assert_eq!(gltf_mesh_hits(), 1);
    assert_eq!(gltf_mesh_misses(), 1);
    fs::remove_dir_all(dir).unwrap();
}
