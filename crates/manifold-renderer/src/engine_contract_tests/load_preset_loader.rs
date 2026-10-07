//! Renderer-owned catalog contracts for the runtime preset loader.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use manifold_node_engine::load::preset_loader::{
    blob_mask, build_catalog_with_overlays, resolve_stock_root, select_assets_root,
    try_load_catalog, EFFECT_CATALOG, EFFECT_DIRS, GENERATOR_CATALOG, GENERATOR_DIRS,
    SCENE_MODIFIER_CATALOG, KindDirs, OverlayEntries, PresetAssetsRoot,
};

mod tests {
    use super::*;

    #[test]
    fn missing_stock_root_lists_bundle_and_registered_assets_candidates() {
        let dirs = KindDirs {
            label: "missing test kind",
            bundle_subdir: "missing-preset-loader-test-kind",
            dev_subdir: "missing-preset-loader-test-kind",
        };
        let (root, tried) = resolve_stock_root(&dirs);
        assert!(root.is_none());
        let exe = std::env::current_exe().unwrap();
        let assets = select_assets_root(
            inventory::iter::<PresetAssetsRoot>.into_iter().map(|root| root.dir).collect(),
        ).expect("renderer must register its assets");
        assert_eq!(tried, vec![
            exe.parent().unwrap().join("../Resources/presets").join(dirs.bundle_subdir),
            Path::new(assets).join(dirs.dev_subdir),
        ]);
        let error = try_load_catalog(&dirs).err().expect("missing stock must fail loudly");
        for candidate in tried {
            assert!(error.contains(&candidate.display().to_string()));
        }
    }

    /// The dev stock root must resolve via registration and
    /// scan to a non-empty set when no packaged bundle is present.
    #[test]
    fn dev_effect_catalog_is_non_empty() {
        assert!(
            !EFFECT_CATALOG.load().is_empty(),
            "effect catalog must load from the dev assets dir",
        );
    }

    #[test]
    fn dev_generator_catalog_is_non_empty() {
        assert!(
            !GENERATOR_CATALOG.load().is_empty(),
            "generator catalog must load from the dev assets dir",
        );
    }

    #[test]
    fn dev_scene_modifier_catalog_discovers_hidden_stock_files() {
        assert!(
            !SCENE_MODIFIER_CATALOG.load().is_empty(),
            "scene modifier catalog must load from the dev assets dir",
        );
    }

    /// Type ids are filename stems and the catalog is sorted.
    #[test]
    fn catalog_is_sorted_by_type_id() {
        let ids: Vec<Arc<str>> = EFFECT_CATALOG.load().type_ids().collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "effect catalog must be sorted by type id");
    }

    /// STATIC_THUMBNAILS D5: every wet/dry-style factory param (id
    /// `amount`/`mix`) defaults to 1.0. Walks the raw preset JSON (both
    /// stock dirs) so a hand-edit that skips the loader still fails here.
    #[test]
    fn factory_amount_defaults_full() {
        let mut violations: Vec<String> = Vec::new();
        for kind in [&EFFECT_DIRS, &GENERATOR_DIRS] {
            let (dir, tried) = resolve_stock_root(kind);
            let dir = dir.unwrap_or_else(|| panic!("stock preset root missing; tried {tried:?}"));
            let entries = fs::read_dir(&dir).expect("stock preset dir must read");
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let text = fs::read_to_string(&path).expect("preset JSON must read");
                let value: serde_json::Value =
                    serde_json::from_str(&text).expect("preset JSON must parse");
                collect_amount_violations(&value, "", &path, &mut violations);
            }
        }
        assert!(
            violations.is_empty(),
            "amount/mix defaults must be 1.0:\n{}",
            violations.join("\n"),
        );
    }

    #[test]
    fn mask_blob_derives_detector_group_and_controls_from_v2() {
        let source_json = stock_blob_tracking_json();
        let source: serde_json::Value = serde_json::from_str(&source_json).unwrap();
        let derived: serde_json::Value =
            serde_json::from_str(&blob_mask::synthesize_mask_blob_json(&source_json).unwrap())
                .unwrap();
        let source_group = source["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["nodeId"] == "Blob Detection")
            .unwrap();
        let derived_group = derived["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["nodeId"] == "Blob Detection")
            .unwrap();
        for node in source_group["group"]["nodes"].as_array().unwrap() {
            assert!(
                derived_group["group"]["nodes"]
                    .as_array()
                    .unwrap()
                    .contains(node)
            );
        }
        for wire in source_group["group"]["wires"].as_array().unwrap() {
            assert!(
                derived_group["group"]["wires"]
                    .as_array()
                    .unwrap()
                    .contains(wire)
            );
        }
        let output_names: std::collections::HashSet<&str> =
            derived_group["group"]["interface"]["outputs"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|port| port["name"].as_str())
                .collect();
        assert!(output_names.is_superset(&std::collections::HashSet::from([
            "boxes", "labels", "valid", "tracks",
        ])));

        let metadata = &derived["presetMetadata"];
        assert_eq!(metadata["id"], "MaskBlob");
        assert_eq!(metadata["displayName"], "Mask Blob Detector");
        let params: std::collections::HashSet<&str> = metadata["params"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|param| param["id"].as_str())
            .collect();
        assert!(params.contains("detection_mode"));
        assert!(params.contains("selection"));
        let shape = metadata["params"]
            .as_array()
            .unwrap()
            .iter()
            .find(|param| param["id"] == "shape")
            .expect("shape control");
        assert_eq!(shape["defaultValue"], 0.0);
        assert_eq!(shape["min"], 0.0);
        assert_eq!(shape["max"], 1.0);
        assert_eq!(shape["wholeNumbers"], false);
        assert!(params.contains("amount"));
        assert!(!params.contains("connect"));
        let bindings = metadata["bindings"].as_array().unwrap();
        assert!(bindings.iter().any(|binding| {
            binding["id"] == "detection_mode" && binding["target"]["nodeId"] == "detection_mode"
        }));
        assert!(!bindings.iter().any(|binding| binding["id"] == "connect"));
        assert!(bindings.iter().any(|binding| {
            binding["id"] == "shape"
                && binding["target"]["nodeId"] == "region_mask"
                && binding["target"]["param"] == "shape"
                && binding["convert"]["type"] == "Float"
        }));
    }

    #[test]
    fn mask_blob_follows_source_group_id_and_control_mutations() {
        let source_json = stock_blob_tracking_json();
        let mut source: serde_json::Value = serde_json::from_str(&source_json).unwrap();
        let group = source["nodes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|node| node["nodeId"] == "Blob Detection")
            .unwrap();
        group["id"] = serde_json::json!(91);
        group["group"]["nodes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|node| node["nodeId"] == "detection_mode")
            .unwrap()["params"]["selector"]["value"] = serde_json::json!(1);
        source["presetMetadata"]["params"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|param| param["id"] == "detection_mode")
            .unwrap()["max"] = serde_json::json!(2.0);
        source["presetMetadata"]["bindings"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|binding| binding["id"] == "detection_mode")
            .unwrap()["defaultValue"] = serde_json::json!(1.0);
        let derived: serde_json::Value = serde_json::from_str(
            &blob_mask::synthesize_mask_blob_json(&serde_json::to_string(&source).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert!(derived["wires"].as_array().unwrap().iter().any(|wire| {
            wire["fromNode"] == 19 && wire["toNode"] == 7 && wire["toPort"] == "labels"
        }));
        assert!(
            !derived["wires"]
                .as_array()
                .unwrap()
                .iter()
                .any(|wire| { wire["fromNode"] == 91 })
        );
        let mode_param = derived["presetMetadata"]["params"]
            .as_array()
            .unwrap()
            .iter()
            .find(|param| param["id"] == "detection_mode")
            .unwrap();
        assert_eq!(mode_param["max"], 2.0);
        let mode_binding = derived["presetMetadata"]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["id"] == "detection_mode")
            .unwrap();
        assert_eq!(mode_binding["defaultValue"], 1.0);
        let mode_node = derived["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["nodeId"] == "Blob Detection")
            .unwrap()["group"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["nodeId"] == "detection_mode")
            .unwrap();
        assert_eq!(mode_node["params"]["selector"]["value"], 1);
    }

    #[test]
    fn mask_blob_snapshot_and_saved_overlays_keep_precedence() {
        let stock = scratch("mask-derive-stock");
        fs::write(
            stock.join("BlobTrackingV2.json"),
            stock_blob_tracking_json(),
        )
        .unwrap();
        let snapshot: OverlayEntries = vec![(Arc::from("MaskBlob"), Arc::from("snapshot"))];
        let saved: OverlayEntries = vec![(Arc::from("MaskBlob"), Arc::from("saved"))];

        let snapshot_catalog =
            build_catalog_with_overlays("effect", &stock, None, &snapshot, &OverlayEntries::new())
                .unwrap();
        let derived_snapshot = snapshot_catalog.json("MaskBlob").unwrap();
        assert!(derived_snapshot.contains("detection_mode"));
        assert!(!derived_snapshot.contains("snapshot"));

        let saved_catalog =
            build_catalog_with_overlays("effect", &stock, None, &snapshot, &saved).unwrap();
        assert_eq!(saved_catalog.json("MaskBlob").unwrap().as_ref(), "saved");
        let _ = fs::remove_dir_all(stock);
    }

    /// Recurses `node`, flagging every object whose `id` is a wet/dry
    /// param whose `defaultValue` isn't 1.0. `pointer` is the JSON path
    /// for the failure message.
    fn collect_amount_violations(
        node: &serde_json::Value,
        pointer: &str,
        path: &Path,
        violations: &mut Vec<String>,
    ) {
        match node {
            serde_json::Value::Object(map) => {
                if matches!(
                    map.get("id").and_then(|v| v.as_str()),
                    Some("amount" | "mix")
                ) && let Some(default) = map.get("defaultValue").and_then(|v| v.as_f64())
                    && default != 1.0
                {
                    violations.push(format!(
                        "{}: {pointer} defaultValue={default}",
                        path.display()
                    ));
                }
                for (key, value) in map {
                    let child = if pointer.is_empty() {
                        format!("/{key}")
                    } else {
                        format!("{pointer}/{key}")
                    };
                    collect_amount_violations(value, &child, path, violations);
                }
            }
            serde_json::Value::Array(items) => {
                for (i, value) in items.iter().enumerate() {
                    collect_amount_violations(value, &format!("{pointer}/{i}"), path, violations);
                }
            }
            _ => {}
        }
    }

    /// Unique scratch dir per test, cleaned up at the end.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "manifold-preset-{tag}-{}-{}",
            std::process::id(),
            // monotonic-ish suffix so concurrent tests don't collide
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn stock_blob_tracking_json() -> String {
        let (root, tried) = resolve_stock_root(&EFFECT_DIRS);
        let root = root.unwrap_or_else(|| panic!("stock effect root missing; tried {tried:?}"));
        fs::read_to_string(root.join("BlobTrackingV2.json")).expect("stock blob preset must read")
    }
}
