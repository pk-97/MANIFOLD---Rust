use super::*;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

fn unique_temp_root(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "manifold-gltf-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    assert!(
        !root.exists(),
        "test root already exists: {}",
        root.display()
    );
    root
}

fn mesh_project(paths: &[&Path]) -> Project {
    assert!(!paths.is_empty());
    let default_path = paths[0].to_string_lossy().into_owned();
    let mut project = Project {
        project_name: "glTF dependencies".into(),
        ..Project::default()
    };
    project.upsert_embedded_preset(path_preset(
        "gltf-dependencies",
        vec![sp("model_path", &default_path, true)],
        vec![StringBindingDef {
            id: "model_path".into(),
            label: "Model".into(),
            default_value: default_path,
            target: BindingTarget::Node {
                node_id: NodeId::new("mesh"),
                param: "path".into(),
            },
        }],
        vec![node("mesh", "node.gltf_mesh_source")],
    ));

    for (index, path) in paths.iter().enumerate() {
        let mut layer = Layer::new_generator(
            format!("Mesh {index}"),
            PresetTypeId::new("gltf-dependencies"),
            index as i32,
        );
        let mut clip = TimelineClip::new_generator(
            manifold_core::Beats::ZERO,
            manifold_core::Beats::from_f32(4.0),
        );
        clip.string_params = Some(std::collections::BTreeMap::from([(
            "model_path".into(),
            path.to_string_lossy().into_owned(),
        )]));
        layer.clips.push(clip);
        project.timeline.layers.push(layer);
    }
    project
}

fn project_mesh_paths(project: &Project) -> Vec<PathBuf> {
    collect_asset_paths(project)
        .into_iter()
        .filter(|asset| asset.kind == AssetKind::Mesh)
        .map(|asset| asset.path)
        .collect()
}

fn primary_file(path: &Path, original_name: &str) -> PathBuf {
    assert!(
        path.is_file(),
        "collected model is a file: {}",
        path.display()
    );
    assert_eq!(path.file_name().unwrap(), original_name);
    path.to_path_buf()
}

fn gltf_json(primary: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(primary).expect("read collected glTF"))
        .expect("parse glTF JSON")
}

fn gltf_uri(primary: &Path, key: &str, index: usize) -> String {
    gltf_json(primary)[key][index]["uri"]
        .as_str()
        .expect("external URI")
        .to_owned()
}

fn copied_resource(primary: &Path, uri: &str) -> PathBuf {
    let decoded = urlencoding::decode(uri).expect("valid collected resource URI");
    primary.parent().unwrap().join(decoded.as_ref())
}

fn make_gltf(buffer_uri: &str, image_uri: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "asset": { "version": "2.0" },
        "buffers": [{ "byteLength": 4, "uri": buffer_uri }],
        "images": [{ "uri": image_uri }],
        "extensionsUsed": ["EXT_unknown_preserved"],
        "extensions": {"EXT_unknown_preserved": {"nested": [1, 2, 3], "strength": 0.375}},
        "extras": { "testMarker": "keep me" }
    }))
    .expect("serialize synthetic glTF")
}

fn make_glb(image_uri: &str, bin: &[u8]) -> Vec<u8> {
    let mut json = serde_json::to_vec(&serde_json::json!({
        "asset": { "version": "2.0" },
        "buffers": [{ "byteLength": bin.len() }],
        "images": [{ "uri": image_uri }],
        "extensionsUsed": ["EXT_unknown_preserved"],
        "extensions": {"EXT_unknown_preserved": {"nested": [1, 2, 3], "strength": 0.375}},
        "extras": { "testMarker": "keep me" }
    }))
    .expect("serialize synthetic GLB JSON");
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut bin_chunk = bin.to_vec();
    while !bin_chunk.len().is_multiple_of(4) {
        bin_chunk.push(0);
    }
    let total_len = 12 + 8 + json.len() + 8 + bin_chunk.len();
    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total_len as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x4e4f534au32.to_le_bytes());
    out.extend_from_slice(&json);
    out.extend_from_slice(&(bin_chunk.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x004e4942u32.to_le_bytes());
    out.extend_from_slice(&bin_chunk);
    out
}

fn read_glb_json_and_bin(path: &Path) -> (serde_json::Value, Vec<u8>) {
    let bytes = std::fs::read(path).expect("read collected GLB");
    assert_eq!(&bytes[0..4], b"glTF");
    assert_eq!(
        u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize,
        bytes.len()
    );
    assert_eq!(&bytes[16..20], b"JSON");
    let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let json_start = 20;
    let json: serde_json::Value = serde_json::from_slice(&bytes[json_start..json_start + json_len])
        .expect("parse collected GLB JSON");
    let bin_header = json_start + json_len;
    assert_eq!(&bytes[bin_header + 4..bin_header + 8], b"BIN\0");
    let bin_len =
        u32::from_le_bytes(bytes[bin_header..bin_header + 4].try_into().unwrap()) as usize;
    let bin_start = bin_header + 8;
    (json, bytes[bin_start..bin_start + bin_len].to_vec())
}

fn assert_media_mesh(path: &Path) {
    assert!(
        path.components()
            .any(|component| component.as_os_str() == std::ffi::OsStr::new("Meshes")),
        "collected mesh should be under Media/Meshes: {}",
        path.display()
    );
}

#[test]
fn collect_gltf_external_dependencies_reloads_after_sources_disappear() {
    let root = unique_temp_root("external-round-trip");
    let source_dir = root.join("sources/models");
    std::fs::create_dir_all(source_dir.join("textures")).unwrap();
    std::fs::create_dir_all(source_dir.parent().unwrap().join("data")).unwrap();
    let primary = source_dir.join("scene.gltf");
    let buffer = source_dir.parent().unwrap().join("data/mesh.bin");
    let image = source_dir.join("textures/bright image.png");
    std::fs::write(&buffer, b"BIN!").unwrap();
    std::fs::write(&image, b"PNG bytes").unwrap();
    std::fs::write(source_dir.join("unrelated.txt"), b"do not copy").unwrap();
    std::fs::write(
        &primary,
        make_gltf("../data/mesh.bin", "textures/bright%20image.png"),
    )
    .unwrap();
    let primary_before = std::fs::read(&primary).unwrap();
    let buffer_before = std::fs::read(&buffer).unwrap();
    let image_before = std::fs::read(&image).unwrap();
    let project_path = root.join("show/Show.manifold");
    let mut project = mesh_project(&[&primary]);

    let report = collect_all_and_save(&mut project, &project_path).expect("collect glTF bundle");
    assert_eq!(report.copied, 1);
    assert_eq!(std::fs::read(&primary).unwrap(), primary_before);
    assert_eq!(std::fs::read(&buffer).unwrap(), buffer_before);
    assert_eq!(std::fs::read(&image).unwrap(), image_before);

    let collected = project_mesh_paths(&project);
    assert_eq!(collected.len(), 1);
    assert_media_mesh(&collected[0]);
    let collected_primary = primary_file(&collected[0], "scene.gltf");
    let copied_buffer = copied_resource(
        &collected_primary,
        &gltf_uri(&collected_primary, "buffers", 0),
    );
    let copied_image = copied_resource(
        &collected_primary,
        &gltf_uri(&collected_primary, "images", 0),
    );
    assert_eq!(
        report.bytes_copied,
        std::fs::metadata(&collected_primary).unwrap().len()
            + buffer_before.len() as u64
            + image_before.len() as u64
    );
    assert_eq!(std::fs::read(copied_buffer).unwrap(), buffer_before);
    assert_eq!(std::fs::read(copied_image).unwrap(), image_before);
    assert!(
        !collected_primary
            .parent()
            .unwrap()
            .join("unrelated.txt")
            .exists()
    );
    assert_eq!(
        gltf_json(&collected_primary)["extras"]["testMarker"],
        "keep me"
    );
    assert_eq!(
        gltf_json(&collected_primary)["extensionsUsed"][0],
        "EXT_unknown_preserved"
    );
    let source_json: serde_json::Value = serde_json::from_slice(&primary_before).unwrap();
    assert_eq!(
        gltf_json(&collected_primary)["extensions"],
        source_json["extensions"]
    );

    let second = collect_all_and_save(&mut project, &project_path).expect("repeat collect");
    assert_eq!(
        second.copied, 0,
        "a portable bundle should not be copied twice"
    );

    let moved = root.join("Moved Show");
    copy_dir_recursive(project_path.parent().unwrap(), &moved, &mut 0).unwrap();
    for originals_present in [true, false] {
        if !originals_present {
            std::fs::remove_dir_all(root.join("sources")).unwrap();
            std::fs::remove_dir_all(project_path.parent().unwrap()).unwrap();
        }
        let loaded = crate::loader::load_project(&moved.join("Show.manifold"))
            .expect("reload moved project");
        let loaded_path = project_mesh_paths(&loaded).into_iter().next().unwrap();
        assert!(loaded_path.starts_with(std::fs::canonicalize(&moved).unwrap()));
        let loaded_primary = primary_file(&loaded_path, "scene.gltf");
        assert_eq!(
            std::fs::read(copied_resource(
                &loaded_primary,
                &gltf_uri(&loaded_primary, "buffers", 0)
            ))
            .unwrap(),
            buffer_before
        );
        assert_eq!(
            std::fs::read(copied_resource(
                &loaded_primary,
                &gltf_uri(&loaded_primary, "images", 0)
            ))
            .unwrap(),
            image_before
        );
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn collect_glb_external_image_keeps_bin_chunk() {
    let root = unique_temp_root("glb-image");
    let source_dir = root.join("sources");
    std::fs::create_dir_all(source_dir.join("textures")).unwrap();
    let primary = source_dir.join("scene.glb");
    let image = source_dir.join("textures/image.png");
    let bin = b"embedded BIN";
    std::fs::write(&primary, make_glb("textures/image.png", bin)).unwrap();
    std::fs::write(&image, b"external image").unwrap();
    let project_path = root.join("show/Show.manifold");
    let mut project = mesh_project(&[&primary]);
    collect_all_and_save(&mut project, &project_path).expect("collect GLB bundle");
    let collected = project_mesh_paths(&project).pop().unwrap();
    assert_media_mesh(&collected);
    let collected_primary = primary_file(&collected, "scene.glb");
    let (json, copied_bin) = read_glb_json_and_bin(&collected_primary);
    assert_eq!(copied_bin, bin);
    assert_eq!(json["extras"]["testMarker"], "keep me");
    assert_eq!(json["extensionsUsed"][0], "EXT_unknown_preserved");
    let copied_image = copied_resource(
        &collected_primary,
        json["images"][0]["uri"].as_str().unwrap(),
    );
    assert_eq!(std::fs::read(copied_image).unwrap(), b"external image");
    std::fs::remove_dir_all(source_dir).unwrap();
    let loaded = crate::loader::load_project(&project_path).expect("reload GLB bundle");
    let loaded_primary = primary_file(&project_mesh_paths(&loaded).pop().unwrap(), "scene.glb");
    let (_, loaded_bin) = read_glb_json_and_bin(&loaded_primary);
    assert_eq!(loaded_bin, bin);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn collect_identical_primary_with_different_dependencies_keeps_bundles_distinct_and_dedups_equal() {
    let root = unique_temp_root("bundle-identity");
    let mut primaries = Vec::new();
    for (name, image_bytes) in [
        ("a", b"A".as_slice()),
        ("b", b"B".as_slice()),
        ("c", b"A".as_slice()),
    ] {
        let dir = root.join(format!("sources/{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("texture.png"), image_bytes).unwrap();
        let primary = dir.join("shared.gltf");
        std::fs::write(&primary, make_gltf("buffer.bin", "texture.png")).unwrap();
        std::fs::write(dir.join("buffer.bin"), b"same buffer").unwrap();
        primaries.push(primary);
    }
    let refs: Vec<&Path> = primaries.iter().map(PathBuf::as_path).collect();
    let mut project = mesh_project(&refs);
    let project_path = root.join("show/Show.manifold");
    let report = collect_all_and_save(&mut project, &project_path).expect("collect bundles");
    assert_eq!(report.copied, 2, "A and B differ; C should dedup with A");
    let collected = project_mesh_paths(&project);
    assert_eq!(collected.len(), 3);
    assert_eq!(collected[0], collected[2]);
    assert_ne!(collected[0], collected[1]);
    for (index, path) in collected.iter().enumerate() {
        assert_media_mesh(path);
        let primary = primary_file(path, "shared.gltf");
        assert_eq!(
            std::fs::read(copied_resource(&primary, &gltf_uri(&primary, "images", 0))).unwrap(),
            if index == 1 { b"B" } else { b"A" }
        );
    }
    let dirs: HashSet<PathBuf> = collected
        .iter()
        .map(|path| path.parent().unwrap().to_path_buf())
        .collect();
    assert_eq!(dirs.len(), 2);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn collect_local_primary_with_absolute_dependency_rewrites_and_copies_bundle() {
    let root = unique_temp_root("local-primary");
    let project_dir = root.join("show");
    let source_dir = root.join("outside");
    std::fs::create_dir_all(project_dir.join("Media/Meshes")).unwrap();
    std::fs::create_dir_all(&source_dir).unwrap();
    let image = source_dir.join("absolute.png");
    std::fs::write(&image, b"absolute image").unwrap();
    let primary = project_dir.join("Media/Meshes/local.gltf");
    std::fs::write(
        &primary,
        make_gltf(
            "data:application/octet-stream;base64,AAAAAA==",
            &image.to_string_lossy(),
        ),
    )
    .unwrap();
    let mut project = mesh_project(&[&primary]);
    let before = std::fs::read(&primary).unwrap();
    let project_path = project_dir.join("Show.manifold");
    let report = collect_all_and_save(&mut project, &project_path).expect("collect local primary");
    assert_eq!(
        report.copied, 1,
        "external dependency makes local primary non-portable"
    );
    assert_eq!(std::fs::read(&primary).unwrap(), before);
    let collected = project_mesh_paths(&project).pop().unwrap();
    assert_media_mesh(&collected);
    let collected_primary = primary_file(&collected, "local.gltf");
    let json = gltf_json(&collected_primary);
    assert!(!json["images"][0]["uri"].as_str().unwrap().starts_with("/"));
    assert_eq!(
        std::fs::read(copied_resource(
            &collected_primary,
            json["images"][0]["uri"].as_str().unwrap()
        ))
        .unwrap(),
        b"absolute image"
    );
    assert_eq!(
        json["buffers"][0]["uri"],
        "data:application/octet-stream;base64,AAAAAA=="
    );
    std::fs::remove_file(image).unwrap();
    let loaded = crate::loader::load_project(&project_path).expect("reload local-primary bundle");
    let loaded_primary = primary_file(&project_mesh_paths(&loaded).pop().unwrap(), "local.gltf");
    assert_eq!(
        std::fs::read(copied_resource(
            &loaded_primary,
            &gltf_uri(&loaded_primary, "images", 0)
        ))
        .unwrap(),
        b"absolute image"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn collect_local_file_uris_remain_portable_and_use_literal_file_paths() {
    let root = unique_temp_root("local-file-uri");
    let project_dir = root.join("show");
    std::fs::create_dir_all(&project_dir).unwrap();
    // The native importer's file: scheme reads '%' literally. Relative URIs
    // instead percent-decode, so collection must encode this filename again.
    let image = project_dir.join("image%20literal.png");
    std::fs::write(&image, b"literal image").unwrap();
    std::fs::write(project_dir.join("image literal.png"), b"wrong image").unwrap();
    let primary = project_dir.join("model.gltf");
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "images": [
            {"uri": format!("file://{}", image.display())},
            {"uri": format!("file:{}", image.display())},
            {"uri": image.to_str().unwrap()}
        ]
    });
    std::fs::write(&primary, serde_json::to_vec(&document).unwrap()).unwrap();
    // The third URI is a relative-scheme absolute path, which the native
    // importer percent-decodes; it therefore intentionally reads the decoy.
    let mut project = mesh_project(&[&primary]);
    let project_path = project_dir.join("Show.manifold");
    let report = collect_all_and_save(&mut project, &project_path).unwrap();
    assert_eq!(
        report.copied, 1,
        "local absolute URIs still require rewriting"
    );
    let moved = root.join("moved");
    copy_dir_recursive(&project_dir, &moved, &mut 0).unwrap();
    std::fs::remove_dir_all(&project_dir).unwrap();
    let loaded = crate::loader::load_project(&moved.join("Show.manifold")).unwrap();
    let path = project_mesh_paths(&loaded).pop().unwrap();
    assert!(path.starts_with(std::fs::canonicalize(&moved).unwrap()));
    let json = gltf_json(&path);
    for index in 0..3 {
        let uri = json["images"][index]["uri"].as_str().unwrap();
        assert!(!Path::new(uri).is_absolute() && !uri.contains(':'));
        let expected: &[u8] = if index == 2 {
            b"wrong image"
        } else {
            b"literal image"
        };
        assert_eq!(
            std::fs::read(copied_resource(&path, uri)).unwrap(),
            expected
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn collect_rejects_bad_mesh_dependencies_before_repoint_and_does_not_copy_neighbors() {
    for (label, contents, neighbor) in [
        ("missing", make_gltf("missing.bin", "texture.png"), true),
        ("malformed", b"{not json".to_vec(), true),
    ] {
        let root = unique_temp_root(label);
        let source_dir = root.join("sources");
        std::fs::create_dir_all(&source_dir).unwrap();
        let primary = source_dir.join("bad.gltf");
        std::fs::write(&primary, contents).unwrap();
        if neighbor {
            std::fs::write(source_dir.join("unrelated.txt"), b"do not copy").unwrap();
        }
        let project_path = root.join("show/Show.manifold");
        let mut project = mesh_project(&[&primary]);
        let before = serde_json::to_value(&project).unwrap();
        assert!(
            collect_all_and_save(&mut project, &project_path).is_err(),
            "{label} should reject"
        );
        assert_eq!(
            serde_json::to_value(&project).unwrap(),
            before,
            "{label} must fail before repoint"
        );
        assert!(!root.join("show/Media").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    let root = unique_temp_root("malformed-glb");
    let source_dir = root.join("sources");
    std::fs::create_dir_all(&source_dir).unwrap();
    let primary = source_dir.join("bad.glb");
    let mut malformed_glb = vec![0u8; 20];
    malformed_glb[0..4].copy_from_slice(b"glTF");
    malformed_glb[4..8].copy_from_slice(&2u32.to_le_bytes());
    malformed_glb[8..12].copy_from_slice(&8u32.to_le_bytes());
    std::fs::write(&primary, malformed_glb).unwrap();
    let mut project = mesh_project(&[&primary]);
    let before = serde_json::to_value(&project).unwrap();
    assert!(collect_all_and_save(&mut project, &root.join("show/Show.manifold")).is_err());
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn collect_rejects_unsupported_dependency_scheme_before_repoint() {
    let root = unique_temp_root("unsupported-uri");
    let source_dir = root.join("sources");
    std::fs::create_dir_all(&source_dir).unwrap();
    let primary = source_dir.join("unsupported.gltf");
    std::fs::write(
        &primary,
        make_gltf("https://example.invalid/buffer.bin", "texture.png"),
    )
    .unwrap();
    std::fs::write(source_dir.join("texture.png"), b"image").unwrap();
    let mut project = mesh_project(&[&primary]);
    let before = serde_json::to_value(&project).unwrap();
    assert!(collect_all_and_save(&mut project, &root.join("show/Show.manifold")).is_err());
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
    assert!(!root.join("show/Media").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn collect_mesh_destination_collision_preserves_existing_directory() {
    let root = unique_temp_root("collision");
    let source_dir = root.join("sources");
    let project_dir = root.join("show");
    std::fs::create_dir_all(&source_dir).unwrap();
    let existing = project_dir.join("Media/Meshes/model");
    std::fs::create_dir_all(&existing).unwrap();
    std::fs::write(existing.join("keep.txt"), b"keep me").unwrap();
    let primary = source_dir.join("model.gltf");
    std::fs::write(&primary, make_gltf("buffer.bin", "texture.png")).unwrap();
    std::fs::write(source_dir.join("buffer.bin"), b"buffer").unwrap();
    std::fs::write(source_dir.join("texture.png"), b"image").unwrap();
    let mut project = mesh_project(&[&primary]);
    let collected_path = project_dir.join("Show.manifold");
    collect_all_and_save(&mut project, &collected_path).expect("collect after collision");
    let collected = project_mesh_paths(&project).pop().unwrap();
    assert_media_mesh(&collected);
    assert_ne!(collected.parent().unwrap(), existing);
    assert_eq!(
        std::fs::read(existing.join("keep.txt")).unwrap(),
        b"keep me"
    );
    assert_eq!(
        gltf_json(&primary_file(&collected, "model.gltf"))["asset"]["version"],
        "2.0"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn collect_bundle_identity_includes_each_repeated_uri_occurrence() {
    use std::os::unix::fs::symlink;

    let root = unique_temp_root("repeated-uri-identity");
    let one = root.join("sources/one");
    let two = root.join("sources/two");
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    let document = serde_json::to_vec(&serde_json::json!({
        "asset": { "version": "2.0" },
        "buffers": [
            { "byteLength": 1, "uri": "a.bin" },
            { "byteLength": 1, "uri": "b.bin" },
            { "byteLength": 1, "uri": "c.bin" }
        ]
    }))
    .unwrap();
    std::fs::write(one.join("model.gltf"), &document).unwrap();
    std::fs::write(two.join("model.gltf"), &document).unwrap();
    std::fs::write(one.join("x.bin"), b"X").unwrap();
    std::fs::write(one.join("y.bin"), b"Y").unwrap();
    std::fs::write(two.join("x.bin"), b"X").unwrap();
    std::fs::write(two.join("y.bin"), b"Y").unwrap();
    symlink(one.join("x.bin"), one.join("a.bin")).unwrap();
    symlink(one.join("x.bin"), one.join("b.bin")).unwrap();
    symlink(one.join("y.bin"), one.join("c.bin")).unwrap();
    symlink(two.join("x.bin"), two.join("a.bin")).unwrap();
    symlink(two.join("y.bin"), two.join("b.bin")).unwrap();
    symlink(two.join("x.bin"), two.join("c.bin")).unwrap();

    let first = one.join("model.gltf");
    let second = two.join("model.gltf");
    let mut project = mesh_project(&[first.as_path(), second.as_path()]);
    let project_path = root.join("show/Show.manifold");
    let report =
        collect_all_and_save(&mut project, &project_path).expect("collect repeated URI bundles");
    assert_eq!(
        report.copied, 2,
        "different logical URI occurrences need distinct bundles"
    );
    let collected = project_mesh_paths(&project);
    assert_eq!(collected.len(), 2);
    assert_ne!(collected[0], collected[1]);

    let bytes_for = |path: &Path| {
        let primary = primary_file(path, "model.gltf");
        let json = gltf_json(&primary);
        (0..3)
            .map(|index| {
                std::fs::read(copied_resource(
                    &primary,
                    json["buffers"][index]["uri"].as_str().unwrap(),
                ))
                .unwrap()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        bytes_for(&collected[0]),
        vec![b"X".to_vec(), b"X".to_vec(), b"Y".to_vec()]
    );
    assert_eq!(
        bytes_for(&collected[1]),
        vec![b"X".to_vec(), b"Y".to_vec(), b"X".to_vec()]
    );
    let _ = std::fs::remove_dir_all(root);
}
