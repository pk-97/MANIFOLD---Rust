//! Cheap scene-bounds backfill for old embedded scene presets.
//!
//! Pre-2026-08-15 projects imported their 3D scenes before
//! `PresetMetadata::scene_bounds` existed. The load-time scene-exposure
//! repair pass only applies scene-scaled slider bands when that field is
//! present, so those projects keep the generic bands forever unless we
//! backfill the bounds.
//!
//! This module reads **only the JSON chunk** of a GLB: it walks the node
//! hierarchy and uses the required POSITION accessor `min`/`max` arrays.
//! No geometry buffer is decoded, so even large photoscans are cheap to
//! probe at load.

use std::fs;
use std::io::{self, Read};
use std::path::Path;

use manifold_core::effect_graph_def::PresetMetadata;
use manifold_core::preset_def::PresetKind;
use manifold_core::project::Project;

/// Backfill `scene_bounds` on every embedded generator preset that carries a
/// `model_file` string param but no bounds, then re-run the scene-exposure
/// migration so the stamped slider bands become scene-scaled.
///
/// Returns how many presets were changed. The caller must re-install the
/// preset overlay and re-run `Project::reconcile_param_manifests()` for the
/// new ranges to reach layer instances.
pub fn repair_project_embedded_scene_bounds(project: &mut Project) -> usize {
    let mut changed = 0usize;
    for preset in &mut project.embedded_presets {
        if preset.kind != PresetKind::Generator {
            continue;
        }
        let Some(meta) = preset.def.preset_metadata.as_mut() else {
            continue;
        };
        if meta.scene_bounds.is_some() {
            continue;
        }
        let Some(model_path) = model_file_path(meta) else {
            continue;
        };

        match glb_scene_bounds(model_path) {
            Ok(Some(bounds)) => {
                meta.scene_bounds = Some(bounds);
                crate::node_graph::scene_exposure::migrate_scene_exposures(&mut preset.def);
                project.load_report.scene_bounds_backfilled += 1;
                changed += 1;
            }
            Ok(None) => {
                log::warn!(
                    "[SceneBoundsBackfill] no POSITION accessors found in {} — leaving generic bands",
                    model_path
                );
            }
            Err(e) => {
                log::warn!(
                    "[SceneBoundsBackfill] failed to probe {}: {} — leaving generic bands",
                    model_path,
                    e
                );
            }
        }
    }
    changed
}

fn model_file_path(meta: &PresetMetadata) -> Option<&str> {
    meta.string_params
        .iter()
        .find(|p| p.id == "model_file")
        .map(|p| p.default_value.as_str())
        .filter(|s| !s.is_empty())
}

fn glb_scene_bounds<P: AsRef<Path>>(path: P) -> io::Result<Option<([f32; 3], [f32; 3])>> {
    let mut file = fs::File::open(path)?;

    let mut header = [0u8; 12];
    file.read_exact(&mut header)?;
    let magic = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    if magic != 0x4654_6C67 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a GLB file"));
    }
    let _version = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    let _length = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);

    let mut chunk_header = [0u8; 8];
    file.read_exact(&mut chunk_header)?;
    let chunk_length = u32::from_le_bytes([chunk_header[0], chunk_header[1], chunk_header[2], chunk_header[3]]) as usize;
    let chunk_type = u32::from_le_bytes([chunk_header[4], chunk_header[5], chunk_header[6], chunk_header[7]]);
    if chunk_type != 0x4E4F_534A {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "GLB first chunk is not JSON",
        ));
    }

    let mut json_bytes = vec![0u8; chunk_length];
    file.read_exact(&mut json_bytes)?;
    let doc: serde_json::Value =
        serde_json::from_slice(&json_bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    Ok(scene_bounds_from_gltf(&doc))
}

fn scene_bounds_from_gltf(doc: &serde_json::Value) -> Option<([f32; 3], [f32; 3])> {
    let nodes = doc.get("nodes")?.as_array()?;
    let meshes = doc.get("meshes")?.as_array()?;
    let accessors = doc.get("accessors")?.as_array()?;
    let scenes = doc.get("scenes")?.as_array()?;
    let scene_index = doc.get("scene").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let scene = scenes.get(scene_index)?;

    let mut bounds: Option<([f32; 3], [f32; 3])> = None;
    for node_ref in scene.get("nodes")?.as_array()? {
        let idx = node_ref.as_u64()? as usize;
        visit_node(idx, identity(), &mut bounds, nodes, meshes, accessors);
    }
    bounds
}

fn visit_node(
    idx: usize,
    parent: [[f32; 4]; 4],
    bounds: &mut Option<([f32; 3], [f32; 3])>,
    nodes: &[serde_json::Value],
    meshes: &[serde_json::Value],
    accessors: &[serde_json::Value],
) {
    let Some(node) = nodes.get(idx) else {
        return;
    };

    let local = node_transform(node);
    let world = mat_mul(parent, local);

    if let Some(mesh_idx) = node.get("mesh").and_then(|v| v.as_u64())
        && let Some(mesh) = meshes.get(mesh_idx as usize)
        && let Some(prims) = mesh.get("primitives").and_then(|v| v.as_array())
    {
        for prim in prims {
                    let Some(acc_idx) = prim
                        .get("attributes")
                        .and_then(|a| a.get("POSITION"))
                        .and_then(|v| v.as_u64())
                    else {
                        continue;
                    };
                    let Some(acc) = accessors.get(acc_idx as usize) else {
                        continue;
                    };
                    let Some(min) = acc.get("min").and_then(json_vec3) else {
                        continue;
                    };
                    let Some(max) = acc.get("max").and_then(json_vec3) else {
                        continue;
                    };

                    for corner in corners(min, max) {
                        let p = transform_point(world, corner);
                        *bounds = Some(match *bounds {
                            Some((bmin, bmax)) => (
                                [
                                    bmin[0].min(p[0]),
                                    bmin[1].min(p[1]),
                                    bmin[2].min(p[2]),
                                ],
                                [
                                    bmax[0].max(p[0]),
                                    bmax[1].max(p[1]),
                                    bmax[2].max(p[2]),
                                ],
                            ),
                            None => (p, p),
                        });
                    }
                }
            }

    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for child_ref in children {
            if let Some(child_idx) = child_ref.as_u64() {
                visit_node(
                    child_idx as usize,
                    world,
                    bounds,
                    nodes,
                    meshes,
                    accessors,
                );
            }
        }
    }
}

fn identity() -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn mat_mul(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for i in 0..4 {
        for j in 0..4 {
            out[i][j] = a[i][0] * b[0][j]
                + a[i][1] * b[1][j]
                + a[i][2] * b[2][j]
                + a[i][3] * b[3][j];
        }
    }
    out
}

fn transform_point(m: [[f32; 4]; 4], p: [f32; 3]) -> [f32; 3] {
    let x = m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2] + m[0][3];
    let y = m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2] + m[1][3];
    let z = m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2] + m[2][3];
    let w = m[3][0] * p[0] + m[3][1] * p[1] + m[3][2] * p[2] + m[3][3];
    if w == 0.0 {
        [x, y, z]
    } else {
        [x / w, y / w, z / w]
    }
}

fn node_transform(node: &serde_json::Value) -> [[f32; 4]; 4] {
    if let Some(arr) = node.get("matrix").and_then(|v| v.as_array()) {
        let mut m = [[0.0f32; 4]; 4];
        for i in 0..16 {
            if let Some(v) = arr.get(i).and_then(|x| x.as_f64()) {
                m[i / 4][i % 4] = v as f32;
            }
        }
        return m;
    }

    let t = node.get("translation").and_then(json_vec3).unwrap_or([0.0; 3]);
    let r = node.get("rotation").and_then(json_quat).unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let s = node.get("scale").and_then(json_vec3).unwrap_or([1.0; 3]);

    let [[m00, m01, m02, _], [m10, m11, m12, _], [m20, m21, m22, _], [_, _, _, _]] =
        quat_to_mat(r);

    // Compose T * R * S.
    [
        [m00 * s[0], m01 * s[1], m02 * s[2], t[0]],
        [m10 * s[0], m11 * s[1], m12 * s[2], t[1]],
        [m20 * s[0], m21 * s[1], m22 * s[2], t[2]],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn quat_to_mat(q: [f32; 4]) -> [[f32; 4]; 4] {
    let [x, y, z, w] = q;
    let xx = 2.0 * x * x;
    let yy = 2.0 * y * y;
    let zz = 2.0 * z * z;
    let xy = 2.0 * x * y;
    let xz = 2.0 * x * z;
    let yz = 2.0 * y * z;
    let wx = 2.0 * w * x;
    let wy = 2.0 * w * y;
    let wz = 2.0 * w * z;

    [
        [1.0 - yy - zz, xy + wz, xz - wy, 0.0],
        [xy - wz, 1.0 - xx - zz, yz + wx, 0.0],
        [xz + wy, yz - wx, 1.0 - xx - yy, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn json_vec3(v: &serde_json::Value) -> Option<[f32; 3]> {
    let arr = v.as_array()?;
    Some([
        arr.first()?.as_f64()? as f32,
        arr.get(1)?.as_f64()? as f32,
        arr.get(2)?.as_f64()? as f32,
    ])
}

fn json_quat(v: &serde_json::Value) -> Option<[f32; 4]> {
    let arr = v.as_array()?;
    Some([
        arr.first()?.as_f64()? as f32,
        arr.get(1)?.as_f64()? as f32,
        arr.get(2)?.as_f64()? as f32,
        arr.get(3)?.as_f64()? as f32,
    ])
}

fn corners(min: [f32; 3], max: [f32; 3]) -> [[f32; 3]; 8] {
    [
        [min[0], min[1], min[2]],
        [max[0], min[1], min[2]],
        [min[0], max[1], min[2]],
        [max[0], max[1], min[2]],
        [min[0], min[1], max[2]],
        [max[0], min[1], max[2]],
        [min[0], max[1], max[2]],
        [max[0], max[1], max[2]],
    ]
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn minimal_glb_json(bounds: ([f32; 3], [f32; 3])) -> Vec<u8> {
        let (min, max) = bounds;
        let doc = serde_json::json!({
            "scene": 0,
            "scenes": [{"nodes": [0]}],
            "nodes": [{"mesh": 0}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
            "accessors": [{"componentType": 5126, "type": "VEC3", "min": min, "max": max}],
        });
        json_to_glb(doc)
    }

    fn json_to_glb(doc: serde_json::Value) -> Vec<u8> {
        let mut json_bytes = serde_json::to_vec(&doc).unwrap();
        // GLB JSON chunk must be 4-byte aligned.
        while !json_bytes.len().is_multiple_of(4) {
            json_bytes.push(b' ');
        }
        let total = 12 + 8 + json_bytes.len();
        let mut out = Vec::with_capacity(total);

        out.extend_from_slice(&0x4654_6C67u32.to_le_bytes()); // magic
        out.extend_from_slice(&2u32.to_le_bytes()); // version
        out.extend_from_slice(&(total as u32).to_le_bytes()); // length

        out.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes()); // JSON
        out.extend_from_slice(&json_bytes);

        out
    }

    #[test]
    fn probes_identity_bounds_from_glb() {
        let dir = std::env::temp_dir();
        let path = dir.join("manifold_bounds_probe_identity.glb");
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(&minimal_glb_json(([-1.0, -2.0, -3.0], [1.0, 2.0, 3.0])))
            .unwrap();
        f.flush().unwrap();
        drop(f);

        let (min, max) = glb_scene_bounds(&path).unwrap().unwrap();
        assert_eq!(min, [-1.0, -2.0, -3.0]);
        assert_eq!(max, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn applies_translation_to_bounds() {
        let doc = serde_json::json!({
            "scene": 0,
            "scenes": [{"nodes": [0]}],
            "nodes": [{"mesh": 0, "translation": [10.0, 0.0, 0.0]}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
            "accessors": [{"componentType": 5126, "type": "VEC3", "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 1.0]}],
        });
        let dir = std::env::temp_dir();
        let path = dir.join("manifold_bounds_probe_translate.glb");
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(&json_to_glb(doc)).unwrap();
        f.flush().unwrap();
        drop(f);

        let (min, max) = glb_scene_bounds(&path).unwrap().unwrap();
        assert!((min[0] - 10.0).abs() < 1e-4);
        assert!((max[0] - 11.0).abs() < 1e-4);
    }

    #[test]
    fn returns_none_for_empty_scene() {
        let doc = serde_json::json!({
            "scene": 0,
            "scenes": [{"nodes": []}],
            "nodes": [],
            "meshes": [],
            "accessors": [],
        });
        let dir = std::env::temp_dir();
        let path = dir.join("manifold_bounds_probe_empty.glb");
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(&json_to_glb(doc)).unwrap();
        f.flush().unwrap();
        drop(f);

        assert!(glb_scene_bounds(&path).unwrap().is_none());
    }

    #[test]
    fn backfills_project_embedded_scene_preset_bounds() {
        use manifold_core::effect_graph_def::{
            EffectGraphDef, EffectGraphNode, PresetMetadata, StringParamSpecDef,
        };
        use manifold_core::project::EmbeddedOrigin;
        use std::collections::BTreeMap;

        let dir = std::env::temp_dir();
        let model_path = dir.join("manifold_bounds_probe_preset.glb");
        let mut f = fs::File::create(&model_path).unwrap();
        f.write_all(&minimal_glb_json(([-5.0, -1.0, -2.0], [5.0, 1.0, 2.0])))
            .unwrap();
        f.flush().unwrap();
        drop(f);

        let meta = PresetMetadata {
            id: manifold_core::preset_type_id::PresetTypeId::from_string("test.scene".to_string()),
            display_name: "Test Scene".to_string(),
            category: "Spatial".to_string(),
            osc_prefix: "/test".to_string(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
                layer_types: None,
            params: Vec::new(),
            bindings: Vec::new(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: vec![StringParamSpecDef {
                id: "model_file".to_string(),
                name: "Model File".to_string(),
                default_value: model_path.to_string_lossy().to_string(),
                is_file_picker: true,
                use_dropdown: false,
                is_file_path: false,
            }],
            string_bindings: Vec::new(),
            scene_bounds: None,
        };

        let def = EffectGraphDef {
            version: 2,
            name: Some("Test".to_string()),
            description: None,
            preset_metadata: Some(meta),
            nodes: vec![EffectGraphNode {
                id: 5,
                node_id: "orbit".into(),
                type_id: "node.orbit_camera".to_string(),
                handle: Some("orbit".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: Default::default(),
                output_canvas_scales: Default::default(),
                group: None,
            }],
            wires: Vec::new(),
        };

        let mut project = Project::default();
        project.embedded_presets.push(manifold_core::project::EmbeddedPreset {
            kind: PresetKind::Generator,
            def,
            origin: EmbeddedOrigin::Saved,
        });

        let changed = repair_project_embedded_scene_bounds(&mut project);
        assert_eq!(changed, 1);

        let meta_after = project.embedded_presets[0]
            .def
            .preset_metadata
            .as_ref()
            .unwrap();
        let (min, max) = meta_after.scene_bounds.unwrap();
        assert!((min[0] - -5.0).abs() < 1e-4);
        assert!((max[0] - 5.0).abs() < 1e-4);

        // Migration should have stamped scene-vocabulary params for the orbit_camera.
        assert!(!meta_after.params.is_empty());
    }
}
