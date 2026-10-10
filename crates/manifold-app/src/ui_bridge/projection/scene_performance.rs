//! Compact performance controls for a scene generator card.
//!
//! Scene rows are still ordinary manifest rows. This module only chooses and
//! orders them; it never creates a second address or state store for a scene
//! control. The graph definition is used for ownership, while the row keeps
//! its real id, value, modulation, mapping, and range facts.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashSet};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode};
use manifold_core::effects::PresetInstance;
use manifold_core::params::ParamOrigin;
use manifold_nodes_scene::node_graph::scene_vm::{CameraVm, EnvironmentVm, SceneLightVm, SceneVm};
use manifold_ui::param_surface::{ParamRow, RowMapping, RowSpec, RowValue};

/// Project the stable, scene-level controls onto the existing card rows.
///
/// `false` means that `def` is not a live scene graph and `rows` was left
/// untouched. A live scene always gets the fixed standard groups, with a
/// disabled placeholder for any target that is absent from the graph or its
/// manifest. Remaining rows are limited to explicit user additions and
/// authored composite macros; auto-stamped low-level scene controls are
/// intentionally omitted from this compact surface.
pub(super) fn curate_scene_rows(
    rows: &mut Vec<ParamRow>,
    inst: &PresetInstance,
    def: &EffectGraphDef,
) -> bool {
    let Some(vm) = SceneVm::from_def(def) else {
        return false;
    };

    let original = std::mem::take(rows);
    let metadata = def.preset_metadata.as_ref();
    let mut used_indices = HashSet::new();
    let mut used_ids = BTreeSet::new();
    let mut standard = Vec::with_capacity(12);

    let camera_nodes = if matches!(&vm.camera, CameraVm::None) {
        Vec::new()
    } else {
        vm.camera_controls.clone()
    };

    let environment_nodes = environment_nodes(&vm, def);
    let lens_nodes = camera_nodes_for_type(def, &camera_nodes, "node.camera_lens");
    let main_light_nodes = vm
        .lights
        .iter()
        .find_map(|light| match light {
            SceneLightVm::Known(light) if light.index == 0 => Some(light.node.clone()),
            _ => None,
        })
        .into_iter()
        .collect::<Vec<_>>();
    let bokeh_nodes = camera_nodes_for_type(def, &camera_nodes, "node.bokeh_gather");
    let motion_nodes = camera_nodes_for_type(def, &camera_nodes, "node.motion_blur");
    let targets = [
        StandardTarget {
            node_ids: &camera_nodes,
            params: &["orbit", "yaw"],
            section: "Camera",
            label: "Horizontal Angle",
            toggle: false,
            synthetic_key: "camera_horizontal_angle",
            missing_reason: "Camera horizontal angle is unavailable in this scene",
        },
        StandardTarget {
            node_ids: &camera_nodes,
            params: &["tilt", "pitch"],
            section: "Camera",
            label: "Vertical Angle",
            toggle: false,
            synthetic_key: "camera_vertical_angle",
            missing_reason: "Camera vertical angle is unavailable in this scene",
        },
        StandardTarget {
            node_ids: &camera_nodes,
            params: &["distance"],
            section: "Camera",
            label: "Distance",
            toggle: false,
            synthetic_key: "camera_distance",
            missing_reason: "Camera distance is unavailable in this scene",
        },
        StandardTarget {
            node_ids: &camera_nodes,
            params: &["fov_y"],
            section: "Camera",
            label: "Field of View",
            toggle: false,
            synthetic_key: "camera_field_of_view",
            missing_reason: "Camera field of view is unavailable in this scene",
        },
        StandardTarget {
            node_ids: &lens_nodes,
            params: &["exposure_ev"],
            section: "Lighting",
            label: "Exposure",
            toggle: false,
            synthetic_key: "lighting_exposure",
            missing_reason: "Lighting exposure is unavailable in this scene",
        },
        StandardTarget {
            node_ids: &environment_nodes,
            params: &["intensity"],
            section: "Lighting",
            label: "Environment Strength",
            toggle: false,
            synthetic_key: "environment_strength",
            missing_reason: "Environment strength is unavailable in this scene",
        },
        StandardTarget {
            node_ids: &main_light_nodes,
            params: &["intensity"],
            section: "Lighting",
            label: "Main Light Intensity",
            toggle: false,
            synthetic_key: "main_light_intensity",
            missing_reason: "Main light intensity is unavailable in light_0",
        },
        StandardTarget {
            node_ids: &bokeh_nodes,
            params: &["enabled"],
            section: "Depth of Field",
            label: "On/Off",
            toggle: true,
            synthetic_key: "depth_of_field_enabled",
            missing_reason: "Depth of field is unavailable on the active camera",
        },
        StandardTarget {
            node_ids: &lens_nodes,
            params: &["focus_distance"],
            section: "Depth of Field",
            label: "Focus Distance",
            toggle: false,
            synthetic_key: "depth_of_field_focus_distance",
            missing_reason: "Focus distance is unavailable on the active camera lens",
        },
        StandardTarget {
            node_ids: &lens_nodes,
            params: &["f_stop"],
            section: "Depth of Field",
            label: "Aperture",
            toggle: false,
            synthetic_key: "depth_of_field_aperture",
            missing_reason: "Aperture is unavailable on the active camera lens",
        },
        StandardTarget {
            node_ids: &motion_nodes,
            params: &["enabled"],
            section: "Motion Blur",
            label: "On/Off",
            toggle: true,
            synthetic_key: "motion_blur_enabled",
            missing_reason: "Motion blur is unavailable on the active camera",
        },
        StandardTarget {
            node_ids: &lens_nodes,
            params: &["shutter_angle"],
            section: "Motion Blur",
            label: "Shutter Angle",
            toggle: false,
            synthetic_key: "motion_blur_shutter_angle",
            missing_reason: "Shutter angle is unavailable on the active camera lens",
        },
    ];
    for target in targets {
        standard.push(standard_row(
            &original,
            metadata,
            inst,
            target,
            &mut used_indices,
            &mut used_ids,
        ));
    }

    let mut result = standard;
    result.extend(original.into_iter().enumerate().filter_map(|(index, mut row)| {
        if used_indices.contains(&index) {
            return None;
        }
        if is_retained_extra(&row, inst, def) {
            row.spec.section = Some("Scene Controls".into());
            Some(row)
        } else {
            None
        }
    }));
    *rows = result;
    true
}

struct StandardTarget<'a> {
    node_ids: &'a [NodeId],
    params: &'a [&'a str],
    section: &'static str,
    label: &'static str,
    toggle: bool,
    synthetic_key: &'static str,
    missing_reason: &'static str,
}

fn standard_row(
    rows: &[ParamRow],
    metadata: Option<&manifold_core::effect_graph_def::PresetMetadata>,
    inst: &PresetInstance,
    target: StandardTarget<'_>,
    used_indices: &mut HashSet<usize>,
    used_ids: &mut BTreeSet<String>,
) -> ParamRow {
    let candidate = best_candidate(
        rows,
        metadata,
        inst,
        target.node_ids,
        target.params,
        used_indices,
    );
    if let Some(index) = candidate {
        used_indices.insert(index);
        let mut row = rows[index].clone();
        used_ids.insert(row.id.to_string());
        row.spec.name = target.label.to_string();
        row.spec.section = Some(target.section.to_string());
        return row;
    }

    let id = synthetic_id(target.synthetic_key, used_ids, rows);
    used_ids.insert(id.clone());
    ParamRow {
        id: Cow::Owned(id),
        spec: RowSpec {
            tooltip: Some(target.missing_reason.to_string()),
            name: target.label.to_string(),
            min: 0.0,
            max: 1.0,
            default: 0.0,
            whole_numbers: target.toggle,
            is_angle: false,
            is_toggle: target.toggle,
            is_trigger: false,
            is_trigger_gate: false,
            value_labels: None,
            section: Some(target.section.to_string()),
            disabled: Some("Unavailable".into()),
            material_role: None,
            inactive_reason: None,
        },
        value: RowValue {
            base: 0.0,
            effective: 0.0,
            exposed: true,
            driven: false,
        },
        audio: Default::default(),
        clip_trigger: None,
        modulation: Default::default(),
        mapping: RowMapping {
            osc_address: None,
            ableton_display: None,
            ableton_range: None,
            mappable: false,
        },
        scene_addr: None,
        rgb_members: None,
        material_attached: false,
    }
}

fn best_candidate(
    rows: &[ParamRow],
    metadata: Option<&manifold_core::effect_graph_def::PresetMetadata>,
    inst: &PresetInstance,
    node_ids: &[NodeId],
    params: &[&str],
    used_indices: &HashSet<usize>,
) -> Option<usize> {
    let metadata = metadata?;
    let mut best: Option<(usize, (u8, u8, usize, usize))> = None;
    for (binding_index, binding) in metadata.bindings.iter().enumerate() {
        let BindingTarget::Node { node_id, param } = &binding.target else {
            continue;
        };
        let Some(param_rank) = params.iter().position(|candidate| *candidate == param) else {
            continue;
        };
        if !node_ids.iter().any(|candidate| candidate == node_id) {
            continue;
        }
        for (row_index, row) in rows.iter().enumerate() {
            if used_indices.contains(&row_index) || row.id.as_ref() != binding.id.as_str() {
                continue;
            }
            let origin_user = inst
                .params
                .get(row.id.as_ref())
                .is_some_and(|param| matches!(param.origin, ParamOrigin::UserAdded));
            let rank = (
                if row.value.exposed { 0 } else { 1 },
                if binding.user_added || origin_user {
                    1
                } else {
                    0
                },
                param_rank,
                binding_index,
            );
            if best.as_ref().is_none_or(|(_, current)| rank < *current) {
                best = Some((row_index, rank));
            }
        }
    }
    best.map(|(index, _)| index)
}

fn environment_nodes(vm: &SceneVm, def: &EffectGraphDef) -> Vec<NodeId> {
    match &vm.environment {
        EnvironmentVm::Importer(environment) => node_id_for_doc(def, environment.bake_node_id)
            .into_iter()
            .collect(),
        EnvironmentVm::Bare(environment) => node_id_for_doc(def, environment.node_doc_id)
            .into_iter()
            .collect(),
        EnvironmentVm::Custom { .. } | EnvironmentVm::None => Vec::new(),
    }
}

fn camera_nodes_for_type(
    def: &EffectGraphDef,
    camera_nodes: &[NodeId],
    type_id: &str,
) -> Vec<NodeId> {
    camera_nodes
        .iter()
        .filter(|node_id| node_type(def, node_id).is_some_and(|candidate| candidate == type_id))
        .cloned()
        .collect()
}

fn node_id_for_doc(def: &EffectGraphDef, doc_id: u32) -> Option<NodeId> {
    fn find(nodes: &[EffectGraphNode], doc_id: u32) -> Option<NodeId> {
        nodes.iter().find_map(|node| {
            if node.id == doc_id {
                Some(node.node_id.clone())
            } else {
                node.group
                    .as_ref()
                    .and_then(|group| find(&group.nodes, doc_id))
            }
        })
    }
    find(&def.nodes, doc_id).filter(|id| !id.is_empty())
}

fn node_type<'a>(def: &'a EffectGraphDef, node_id: &NodeId) -> Option<&'a str> {
    fn find<'a>(nodes: &'a [EffectGraphNode], node_id: &NodeId) -> Option<&'a str> {
        nodes.iter().find_map(|node| {
            if &node.node_id == node_id {
                Some(node.type_id.as_str())
            } else {
                node.group
                    .as_ref()
                    .and_then(|group| find(&group.nodes, node_id))
            }
        })
    }
    find(&def.nodes, node_id)
}

fn is_user_added(
    row: &ParamRow,
    inst: &PresetInstance,
    metadata: Option<&manifold_core::effect_graph_def::PresetMetadata>,
) -> bool {
    if inst
        .params
        .get(row.id.as_ref())
        .is_some_and(|param| matches!(param.origin, ParamOrigin::UserAdded))
    {
        return true;
    }
    metadata.is_some_and(|metadata| {
        metadata
            .bindings
            .iter()
            .any(|binding| binding.id == row.id.as_ref() && binding.user_added)
    })
}

fn is_retained_extra(
    row: &ParamRow,
    inst: &PresetInstance,
    def: &EffectGraphDef,
) -> bool {
    let metadata = def.preset_metadata.as_ref();
    if is_user_added(row, inst, metadata) {
        return row.value.exposed;
    }
    let Some(param) = inst.params.get(row.id.as_ref()) else {
        return false;
    };
    if !param.spec.card_visible || !row.value.exposed {
        return false;
    }
    metadata.is_some_and(|metadata| {
        metadata.bindings.iter().any(|binding| {
            binding.id == row.id.as_ref()
                && match &binding.target {
                    BindingTarget::Composite { .. } => true,
                    BindingTarget::Node { node_id, param } => node_type(def, node_id).is_some_and(|ty| {
                        if manifold_core::liquid_domain::is_liquid_domain(ty) {
                            manifold_core::scene_exposure::card_visible_for(ty, param)
                        } else {
                            !manifold_nodes_scene::node_graph::scene_exposure::is_scene_setup_node(ty)
                                && ty != "node.coc_from_depth"
                        }
                    }),
                    BindingTarget::SceneModifier { .. } => false,
                }
        })
    })
}

fn synthetic_id(key: &str, used: &BTreeSet<String>, rows: &[ParamRow]) -> String {
    let existing = |id: &str| used.contains(id) || rows.iter().any(|row| row.id.as_ref() == id);
    let base = format!("scene_performance.{key}");
    if !existing(&base) {
        return base;
    }
    (1..)
        .map(|suffix| format!("{base}.{suffix}"))
        .find(|id| !existing(id))
        .expect("synthetic scene row id space is finite")
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::EffectGraphDef;
    use manifold_core::params::{Param, ParamManifest};
    use serde_json::json;

    fn scene_fixture() -> (PresetInstance, EffectGraphDef, Vec<ParamRow>) {
        let mut def: EffectGraphDef = serde_json::from_value(json!({
            "version": 2,
            "nodes": [
                {"id": 1, "nodeId": "camera", "typeId": "node.orbit_camera"},
                {"id": 2, "nodeId": "lens", "typeId": "node.camera_lens"},
                {"id": 3, "nodeId": "environment", "typeId": "node.bake_environment"},
                {"id": 4, "nodeId": "light0", "typeId": "node.light"},
                {"id": 5, "nodeId": "light1", "typeId": "node.light"},
                {"id": 6, "nodeId": "object_transform", "typeId": "node.transform_3d"},
                {"id": 10, "nodeId": "scene", "typeId": "node.render_scene",
                    "params": {"lights": {"type": "Float", "value": 1.0}}},
                {"id": 20, "nodeId": "final", "typeId": "system.final_output"}
            ],
            "wires": [
                {"fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "camera"},
                {"fromNode": 2, "fromPort": "out", "toNode": 10, "toPort": "camera"},
                {"fromNode": 3, "fromPort": "envmap", "toNode": 10, "toPort": "envmap"},
                {"fromNode": 4, "fromPort": "out", "toNode": 10, "toPort": "light_0"},
                {"fromNode": 10, "fromPort": "color", "toNode": 20, "toPort": "in"}
            ],
            "presetMetadata": {
                "id": "scene_test", "displayName": "Scene Test", "category": "Diagnostic",
                "oscPrefix": "scene_test", "params": [], "bindings": []
            }
        }))
        .expect("scene fixture JSON");

        let bindings = [
            ("cam_orbit", "camera", "orbit"),
            ("cam_tilt", "camera", "tilt"),
            ("cam_distance", "camera", "distance"),
            ("cam_fov", "camera", "fov_y"),
            ("lens_exposure", "lens", "exposure_ev"),
            ("env_intensity", "environment", "intensity"),
            ("light_intensity", "light0", "intensity"),
            ("lens_focus", "lens", "focus_distance"),
            ("lens_fstop", "lens", "f_stop"),
            ("lens_shutter", "lens", "shutter_angle"),
            ("object_position", "object_transform", "pos_x"),
            ("other_light", "light1", "intensity"),
            ("lens_quality", "lens", "quality"),
            ("lens_radius", "lens", "radius"),
            ("camera_target", "camera", "target_x"),
            ("physics_amount", "physics", "copy_count"),
        ];
        let metadata = def.preset_metadata.as_mut().expect("metadata");
        for (id, node_id, param) in bindings {
            metadata.params.push(json_spec(id));
            metadata.bindings.push(json_binding(id, node_id, param));
        }
        let user_spec = json_spec("user_scene_control");
        metadata.params.push(user_spec.clone());

        let mut inst = PresetInstance::new_generator(PresetTypeId::new("scene_test"));
        let mut params = metadata
            .params
            .iter()
            .filter(|spec| spec.id != "user_scene_control")
            .cloned()
            .map(Param::bundled)
            .collect::<Vec<_>>();
        params.push(Param::user_added(user_spec));
        inst.params = ParamManifest::from_params(params);
        let rows = metadata
            .params
            .iter()
            .map(|spec| row(&spec.id, spec.id == "cam_orbit"))
            .collect();
        inst.graph = Some(def.clone());
        (inst, def, rows)
    }

    fn json_spec(id: &str) -> manifold_core::effect_graph_def::ParamSpecDef {
        serde_json::from_value(json!({
            "id": id, "name": id, "min": 0.0, "max": 10.0, "defaultValue": 0.0
        }))
        .expect("param spec")
    }

    fn json_binding(
        id: &str,
        node_id: &str,
        param: &str,
    ) -> manifold_core::effect_graph_def::BindingDef {
        serde_json::from_value(json!({
            "id": id, "label": id, "defaultValue": 0.0,
            "target": {"kind": "node", "nodeId": node_id, "param": param}
        }))
        .expect("binding")
    }

    fn row(id: &str, rich_state: bool) -> ParamRow {
        let mut row = ParamRow {
            id: Cow::Owned(id.to_string()),
            spec: RowSpec {
                tooltip: None,
                name: id.to_string(),
                min: 0.0,
                max: 10.0,
                default: 0.0,
                whole_numbers: false,
                is_angle: false,
                is_toggle: false,
                is_trigger: false,
                is_trigger_gate: false,
                value_labels: None,
                section: None,
                disabled: None,
                material_role: None,
                inactive_reason: None,
            },
            value: RowValue {
                base: 0.5,
                effective: 0.5,
                exposed: true,
                driven: false,
            },
            audio: Default::default(),
            clip_trigger: None,
            modulation: Default::default(),
            mapping: RowMapping {
                osc_address: None,
                ableton_display: None,
                ableton_range: None,
                mappable: true,
            },
            scene_addr: None,
            rgb_members: None,
            material_attached: false,
        };
        if rich_state {
            row.value.base = 3.25;
            row.value.effective = 2.5;
            row.value.driven = true;
            row.modulation.driver_active = true;
            row.mapping.osc_address = Some("/scene/camera/orbit".into());
        }
        row
    }

    #[test]
    fn standard_surface_has_fixed_order_and_four_sections() {
        let (inst, def, mut rows) = scene_fixture();
        assert!(curate_scene_rows(&mut rows, &inst, &def));
        assert_eq!(rows[12].id.as_ref(), "user_scene_control");
        assert_eq!(rows[12].spec.section.as_deref(), Some("Scene Controls"));
        let rows = &rows[..12];
        assert_eq!(
            rows.len(),
            12,
            "all standard rows remain positionally stable"
        );
        assert_eq!(
            rows.iter()
                .map(|row| row.spec.name.as_str())
                .collect::<Vec<_>>(),
            [
                "Horizontal Angle",
                "Vertical Angle",
                "Distance",
                "Field of View",
                "Exposure",
                "Environment Strength",
                "Main Light Intensity",
                "On/Off",
                "Focus Distance",
                "Aperture",
                "On/Off",
                "Shutter Angle",
            ]
        );
        assert_eq!(
            rows.iter()
                .map(|row| row.spec.section.as_deref())
                .collect::<Vec<_>>(),
            [
                Some("Camera"),
                Some("Camera"),
                Some("Camera"),
                Some("Camera"),
                Some("Lighting"),
                Some("Lighting"),
                Some("Lighting"),
                Some("Depth of Field"),
                Some("Depth of Field"),
                Some("Depth of Field"),
                Some("Motion Blur"),
                Some("Motion Blur"),
            ]
        );
        assert!(
            rows[7].spec.disabled.is_some(),
            "missing DoF gets a disabled placeholder"
        );
        assert!(
            rows[10].spec.disabled.is_some(),
            "missing motion blur gets a disabled placeholder"
        );
    }

    #[test]
    fn real_row_keeps_identity_and_state() {
        let (inst, def, mut rows) = scene_fixture();
        let before = rows
            .iter()
            .find(|row| row.id.as_ref() == "cam_orbit")
            .unwrap();
        let expected_value = before.value;
        let expected_osc = before.mapping.osc_address.clone();
        let expected_driver = before.modulation.driver_active;
        assert!(curate_scene_rows(&mut rows, &inst, &def));
        let actual = rows
            .iter()
            .find(|row| row.id.as_ref() == "cam_orbit")
            .unwrap();
        assert_eq!(actual.value, expected_value);
        assert_eq!(actual.mapping.osc_address, expected_osc);
        assert_eq!(actual.modulation.driver_active, expected_driver);
        assert_eq!(actual.id.as_ref(), "cam_orbit");
    }

    #[test]
    fn automatic_object_and_extra_light_rows_are_removed_but_user_rows_remain() {
        let (inst, def, mut rows) = scene_fixture();
        assert!(curate_scene_rows(&mut rows, &inst, &def));
        assert!(!rows.iter().any(|row| row.id.as_ref() == "object_position"));
        assert!(!rows.iter().any(|row| row.id.as_ref() == "other_light"));
        assert!(!rows.iter().any(|row| row.id.as_ref() == "lens_quality"));
        assert!(!rows.iter().any(|row| row.id.as_ref() == "lens_radius"));
        assert!(!rows.iter().any(|row| row.id.as_ref() == "camera_target"));
        assert!(!rows.iter().any(|row| row.id.as_ref() == "physics_amount"));
        assert!(
            rows.iter()
                .any(|row| row.id.as_ref() == "user_scene_control")
        );
    }

    #[test]
    fn ocean_keeps_authored_performance_controls_below_standard_rows() {
        let inst = PresetInstance::new_generator(PresetTypeId::new("Ocean"));
        let surface = super::super::cards::gen_params_to_surface(
            &inst, "scene", None, &[], super::super::cards::SurfaceVisibility::CuratedCard,
            (manifold_core::Bpm(120.0), 0.0),
        );
        let extras = &surface.rows[12..];
        for id in ["wind_speed", "wind_direction", "choppiness", "wave_size", "swell_speed", "foam"] {
            assert!(extras.iter().any(|row| row.id.as_ref() == id), "missing authored control {id}");
        }
        assert!(extras.iter().all(|row| row.spec.section.as_deref() == Some("Scene Controls")));
    }

    #[test]
    fn non_scene_graph_is_left_untouched() {
        let (inst, mut def, mut rows) = scene_fixture();
        def.nodes.retain(|node| node.type_id != "node.render_scene");
        let before = rows
            .iter()
            .map(|row| row.id.to_string())
            .collect::<Vec<_>>();
        assert!(!curate_scene_rows(&mut rows, &inst, &def));
        assert_eq!(
            rows.iter()
                .map(|row| row.id.to_string())
                .collect::<Vec<_>>(),
            before
        );
    }
}
