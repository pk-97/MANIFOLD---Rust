//! Cheap scene-bounds backfill for old embedded scene presets.
//!
//! Pre-2026-08-15 projects imported their 3D scenes before
//! `PresetMetadata::scene_bounds` existed, so they keep the generic slider
//! bands unless we backfill the bounds.
//!
//! Bounds come from `gltf_import_summary`, the importer's own box, so the
//! backfilled slider bands equal the ones a fresh import stamps. A cheaper
//! JSON-chunk probe boxed transformed accessor corners and came out larger.

use manifold_core::effect_graph_def::{BindingTarget, PresetMetadata};
use manifold_core::preset_def::PresetKind;
use manifold_core::project::Project;

/// Backfill `scene_bounds` on every embedded generator preset that carries a
/// `model_file` string param but no bounds.
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

        match crate::node_graph::gltf_load::gltf_import_summary(std::path::Path::new(model_path)) {
            Ok(summary) => {
                meta.scene_bounds = Some((summary.bbox_min, summary.bbox_max));
                project.load_report.scene_bounds_backfilled += 1;
                changed += 1;
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
    changed += repair_layer_graph_scene_bounds(project);
    changed
}

/// Backfill `scene_bounds` on per-layer generator graphs that hold a
/// `render_scene`, have no bounds, and carry mesh-source `path` string
/// bindings: scenes a model was merged into before the merge wrote bounds.
/// Follows the merge's rule (`assemble_merge_plan`): the scene takes the
/// first merged asset's bounds, so the first `path` binding in order wins.
/// Bounds come from `gltf_import_summary`, the merge's source. A missing or
/// unreadable GLB leaves bounds unset with one warning.
fn repair_layer_graph_scene_bounds(project: &mut Project) -> usize {
    let mut changed = 0usize;
    for layer in &mut project.timeline.layers {
        let Some(def) = layer.gen_params_mut().and_then(|gp| gp.graph.as_mut()) else {
            continue;
        };
        if !def
            .nodes
            .iter()
            .any(|n| n.type_id == crate::node_graph::scene_vm::RENDER_SCENE_TYPE_ID)
        {
            continue;
        }
        let Some(meta) = def.preset_metadata.as_mut() else {
            continue;
        };
        if meta.scene_bounds.is_some() {
            continue;
        }
        let Some(model_path) = meta
            .string_bindings
            .iter()
            .find(|b| {
                matches!(&b.target, BindingTarget::Node { param, .. } if param == "path")
                    && !b.default_value.is_empty()
            })
            .map(|b| b.default_value.clone())
        else {
            continue;
        };
        // The merge's own summary, not the cheap JSON probe: the probe boxes
        // transformed accessor AABBs and comes out larger, which would give
        // different ranges than the merge wrote. Full decode, once per layer;
        // the bounds then persist on save.
        match crate::node_graph::gltf_load::gltf_import_summary(std::path::Path::new(&model_path)) {
            Ok(summary) => {
                meta.scene_bounds = Some((summary.bbox_min, summary.bbox_max));
                project.load_report.scene_bounds_backfilled += 1;
                changed += 1;
            }
            Err(e) => log::warn!(
                "[SceneBoundsBackfill] failed to probe {model_path}: {e}; layer scene keeps declared ranges"
            ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backfills_project_embedded_scene_preset_bounds() {
        use manifold_core::effect_graph_def::{
            EffectGraphDef, EffectGraphNode, PresetMetadata, StringParamSpecDef,
        };
        use manifold_core::project::EmbeddedOrigin;
        use std::collections::BTreeMap;

        let model_path = super::super::synthetic_glbs::write_synthetic_sized_glb(5.0);

        let meta = PresetMetadata {
            shape: None,
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
            scene_modifier: None,
            scene_bounds: None,
        };

        let def = EffectGraphDef {
            version: 2,
            name: Some("Test".to_string()),
            description: None,
            preset_metadata: Some(meta),
            scene_modifiers: Vec::new(),
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
    }

    /// A scene a model was merged into before the merge wrote bounds loads
    /// with the bounds the importer derives for that GLB; a missing GLB
    /// leaves them unset.
    #[test]
    fn backfills_layer_scene_graph_bounds_from_merged_mesh_path() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/cc0__tiger_lily.glb");
        let summary = crate::node_graph::gltf_load::gltf_import_summary(&path).expect("summary");
        let (mut def, _) = super::super::assemble_import_graph(&path).expect("imports");
        def.preset_metadata.as_mut().expect("meta").scene_bounds = None;

        let mut missing = def.clone();
        for b in &mut missing.preset_metadata.as_mut().unwrap().string_bindings {
            b.default_value = "/nonexistent/manifold_missing.glb".to_string();
        }

        let mut project = Project::default();
        for (i, graph) in [def, missing].into_iter().enumerate() {
            let mut layer = manifold_core::layer::Layer::new_generator(
                format!("L{i}"),
                manifold_core::preset_type_id::PresetTypeId::NONE,
                i as i32,
            );
            layer.gen_params_mut().unwrap().graph = Some(graph);
            project.timeline.layers.push(layer);
        }

        assert_eq!(repair_project_embedded_scene_bounds(&mut project), 1);
        let got = project.timeline.layers[0].generator_graph().unwrap()
            .preset_metadata.as_ref().unwrap().scene_bounds.expect("backfilled");
        for k in 0..3 {
            assert!((got.0[k] - summary.bbox_min[k]).abs() < 1e-3, "min {got:?} vs {:?}", summary.bbox_min);
            assert!((got.1[k] - summary.bbox_max[k]).abs() < 1e-3, "max {got:?} vs {:?}", summary.bbox_max);
        }
        assert!(project.timeline.layers[1].generator_graph().unwrap()
            .preset_metadata.as_ref().unwrap().scene_bounds.is_none());
    }
}
