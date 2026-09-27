use super::*;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    model: PathBuf,
    buffer: PathBuf,
    model_bytes: Vec<u8>,
    buffer_bytes: Vec<u8>,
}

impl Fixture {
    fn new(bind_pose: f32, keyframe: f32) -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "manifold-gltf-anim-cache-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&root).expect("create synthetic glTF fixture directory");
        let model = root.join("animated.gltf");
        let buffer = root.join("keyframes.bin");
        let model_bytes = model_json(bind_pose).into_bytes();
        let buffer_bytes = keyframe_buffer(keyframe);
        std::fs::write(&model, &model_bytes).expect("write synthetic glTF model");
        std::fs::write(&buffer, &buffer_bytes).expect("write synthetic keyframe buffer");
        Self {
            root,
            model,
            buffer,
            model_bytes,
            buffer_bytes,
        }
    }

    fn clone_at(&self, root: PathBuf) -> PathBuf {
        std::fs::create_dir(&root).expect("create relocated fixture directory");
        let model = root.join("animated.gltf");
        let buffer = root.join("keyframes.bin");
        std::fs::write(&model, &self.model_bytes).expect("write relocated glTF model");
        std::fs::write(&buffer, &self.buffer_bytes).expect("write relocated keyframe buffer");
        model
    }

    fn set_model_bind_pose(&self, bind_pose: f32) {
        std::fs::write(&self.model, model_json(bind_pose)).expect("rewrite glTF bind pose");
    }

    fn set_keyframe(&self, keyframe: f32) {
        std::fs::write(&self.buffer, keyframe_buffer(keyframe)).expect("rewrite keyframe buffer");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn model_json(bind_pose: f32) -> String {
    format!(
        r#"{{
  "asset": {{"version": "2.0"}},
  "scene": 0,
  "scenes": [{{"nodes": [0]}}],
  "nodes": [{{"translation": [{bind_pose}, 0.0, 0.0]}}],
  "buffers": [{{"uri": "keyframes.bin", "byteLength": 32}}],
  "bufferViews": [
    {{"buffer": 0, "byteOffset": 0, "byteLength": 8}},
    {{"buffer": 0, "byteOffset": 8, "byteLength": 24}}
  ],
  "accessors": [
    {{"bufferView": 0, "componentType": 5126, "count": 2, "type": "SCALAR", "min": [0.0], "max": [1.0]}},
    {{"bufferView": 1, "componentType": 5126, "count": 2, "type": "VEC3"}}
  ],
  "animations": [{{
    "samplers": [{{"input": 0, "output": 1, "interpolation": "LINEAR"}}],
    "channels": [{{"sampler": 0, "target": {{"node": 0, "path": "translation"}}}}]
  }}]
}}"#
    )
}

fn keyframe_buffer(keyframe: f32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(32);
    for value in [0.0_f32, 1.0, 0.0, 0.0, 0.0, keyframe, 0.0, 0.0] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn load(path: &Path) -> Result<Arc<GltfAnimSet>, String> {
    spawn_load(path)
        .recv_timeout(Duration::from_secs(10))
        .expect("background glTF animation load did not finish within 10 seconds")
}

fn translation_value(set: &GltfAnimSet) -> f32 {
    set.clips[0].channels[0].values[3]
}

#[test]
fn changed_external_and_primary_content_are_observed_while_old_arc_stays_immutable() {
    let fixture = Fixture::new(1.0, 2.0);
    let first = load(&fixture.model).expect("initial synthetic glTF should load");
    assert_eq!(translation_value(&first), 2.0);
    assert_eq!(first.node_bind_trs[0].translation[0], 1.0);

    fixture.set_keyframe(7.0);
    let changed_buffer = load(&fixture.model).expect("changed external buffer should load");
    assert!(!Arc::ptr_eq(&first, &changed_buffer));
    assert_eq!(
        translation_value(&first),
        2.0,
        "resident old payload must be immutable"
    );
    assert_eq!(translation_value(&changed_buffer), 7.0);

    fixture.set_model_bind_pose(9.0);
    let changed_primary = load(&fixture.model).expect("changed primary glTF should load");
    assert!(!Arc::ptr_eq(&changed_buffer, &changed_primary));
    assert_eq!(first.node_bind_trs[0].translation[0], 1.0);
    assert_eq!(changed_primary.node_bind_trs[0].translation[0], 9.0);

    std::fs::remove_file(&fixture.buffer).expect("remove external keyframe buffer");
    assert!(
        load(&fixture.model).is_err(),
        "missing buffer must reject a warm cache entry"
    );

    std::fs::write(&fixture.buffer, [0_u8; 8]).expect("write truncated external keyframe buffer");
    assert!(
        load(&fixture.model).is_err(),
        "truncated buffer must reject a warm cache entry"
    );
}

#[test]
fn identical_content_reuses_arc_across_paths_and_restoring_bytes_reuses_original_arc() {
    let fixture = Fixture::new(3.0, 4.0);
    let original = load(&fixture.model).expect("initial synthetic glTF should load");
    let unchanged = load(&fixture.model).expect("unchanged glTF should load");
    assert!(Arc::ptr_eq(&original, &unchanged));

    let relocated_model = fixture.clone_at(fixture.root.join("relocated"));
    let relocated = load(&relocated_model).expect("relocated identical glTF should load");
    assert!(Arc::ptr_eq(&original, &relocated));

    fixture.set_keyframe(11.0);
    let changed = load(&fixture.model).expect("changed keyframe buffer should load");
    assert!(!Arc::ptr_eq(&original, &changed));
    assert_eq!(translation_value(&original), 4.0);
    assert_eq!(translation_value(&changed), 11.0);

    std::fs::write(&fixture.buffer, &fixture.buffer_bytes)
        .expect("restore original keyframe bytes");
    let restored = load(&fixture.model).expect("restored original content should load");
    assert!(Arc::ptr_eq(&original, &restored));
    assert_eq!(translation_value(&restored), 4.0);
}

#[test]
fn decoding_uses_the_snapshot_even_if_files_change_after_validation() {
    let fixture = Fixture::new(13.0, 17.0);
    let snapshot = gltf_load::parse_buffer_snapshot(&fixture.model).unwrap();
    fixture.set_keyframe(19.0);
    fixture.set_model_bind_pose(23.0);
    let captured = decode_anim_set(snapshot);
    assert_eq!(translation_value(&captured), 17.0);
    assert_eq!(captured.node_bind_trs[0].translation[0], 13.0);
    let current = load(&fixture.model).unwrap();
    assert_eq!(translation_value(&current), 19.0);
    assert_eq!(current.node_bind_trs[0].translation[0], 23.0);
}

#[test]
fn concurrent_cold_requests_publish_one_shared_payload() {
    let fixture = Fixture::new(29.0, 31.0);
    let first = spawn_load(&fixture.model);
    let second = spawn_load(&fixture.model);
    let first = first
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let second = second
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(translation_value(&first), 31.0);
}

#[test]
fn dropped_loaded_arc_releases_cached_payload() {
    let fixture = Fixture::new(5.0, 6.0);
    let loaded = load(&fixture.model).expect("synthetic glTF should load");
    let weak = Arc::downgrade(&loaded);
    assert!(weak.upgrade().is_some());
    drop(loaded);
    assert!(
        weak.upgrade().is_none(),
        "Weak cache must not retain the animation payload"
    );
}
