//! Import-time scene scaling (BUG-upfq slider ranges), authored lights, and the
//! environment and ray tracing defaults a fresh import stamps.

use super::testkit::*;
use super::*;
use super::assembly::*;
use super::merge::*;
use super::scene::*;
use crate::node_graph::gltf_load::GltfImportSummary;
use manifold_core::effect_graph_def::BindingTarget;
use manifold_core::effect_graph_def::SerializedParamValue;


// -----------------------------------------------------------------
// BUG-upfq P1+P2 — scene-derived slider ranges at import
// -----------------------------------------------------------------

/// Build the import graph for a synthetic summary whose bbox is a cube of
/// half-extent `half_extent` centered at the origin. Uses the full importer
/// (`build_import_graph`), which is where the P1/P2 range override runs.
fn scene_import_with_half_extent(half_extent: f32) -> (manifold_core::effect_graph_def::EffectGraphDef, ImportReport) {
    let summary = GltfImportSummary {
        materials: vec![full_material(0, "Hero", 500)],
        bbox_min: [-half_extent, -half_extent, -half_extent],
        bbox_max: [half_extent, half_extent, half_extent],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_scene_scale.glb");
    build_import_graph(&summary, path).expect("build scene-scale import graph")
}

/// The card param for `node_id`+`param` on an import def, as the live
/// lookup joins it (ranges scale from the saved `scene_bounds`).
fn stamped_card_param(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    node_id: u32,
    param: &str,
) -> manifold_core::effect_graph_def::ParamSpecDef {
    let id = format!("{node_id}_{param}");
    manifold_core::param_lookup::Declared::new(def)
        .and_then(|mut d| d.spec(&id))
        .unwrap_or_else(|| panic!("no card param `{id}`"))
}

/// BUG-upfq P1: an import's light / focus / light-range slider ranges must
/// scale with the bbox, never stay at the primitive's generic defaults.
/// Uses half-extents 5 and 50 — the import's fixed sun rig (5, 2, 3) sits
/// INSIDE both derived position bands (±2·radius), so neither band gets the
/// widen-for-default distortion and the pure derived ratio is exactly 10.
#[test]
fn scene_range_scales_with_bbox_size() {
    assert!(scene_derived_range_ratio_matches_bbox(5.0, 50.0));
}

/// The workhorse assertion used by the small-vs-large range test.
fn scene_derived_range_ratio_matches_bbox(small_half: f32, large_half: f32) -> bool {
    let (small_def, _) = scene_import_with_half_extent(small_half);
    let (large_def, _) = scene_import_with_half_extent(large_half);

    let small_meta = &small_def;
    let large_meta = &large_def;

    let small_sun_id = small_def.nodes.iter().find(|n| n.type_id == "node.light").unwrap().id;
    let large_sun_id = large_def.nodes.iter().find(|n| n.type_id == "node.light").unwrap().id;
    let small_lens_id = small_def.nodes.iter().find(|n| n.type_id == "node.camera_lens").unwrap().id;
    let large_lens_id = large_def.nodes.iter().find(|n| n.type_id == "node.camera_lens").unwrap().id;

    let small_r = super::scene_scale::SceneScale::from_bbox([-small_half; 3], [small_half; 3]).radius;
    let large_r = super::scene_scale::SceneScale::from_bbox([-large_half; 3], [large_half; 3]).radius;
    assert!((large_r / small_r - 10.0).abs() < 1e-3, "bbox radius ratio must be exactly 10");

    // Position slider (sun): ±2·radius, symmetric around 0.
    let small_pos = stamped_card_param(small_meta, small_sun_id, "pos_x");
    let large_pos = stamped_card_param(large_meta, large_sun_id, "pos_x");
    assert!((small_pos.min - -2.0 * small_r).abs() < 1e-3, "small sun pos band is ±2·radius, got min {}", small_pos.min);
    assert!((small_pos.max - 2.0 * small_r).abs() < 1e-3, "small sun pos band is ±2·radius, got max {}", small_pos.max);
    assert!((large_pos.max - 2.0 * large_r).abs() < 1e-3, "large sun pos band is ±2·radius, got max {}", large_pos.max);
    assert!((large_pos.min - -2.0 * large_r).abs() < 1e-3, "large sun pos band is ±2·radius, got min {}", large_pos.min);

    // Focus distance: 0..4·radius, one-sided.
    let small_focus = stamped_card_param(small_meta, small_lens_id, "focus_distance");
    let large_focus = stamped_card_param(large_meta, large_lens_id, "focus_distance");
    assert!(small_focus.min == 0.0, "focus band starts at the hyperfocal 0");
    assert!((small_focus.max - 4.0 * small_r).abs() < 1e-3, "small focus band is 0..4·radius, got max {}", small_focus.max);
    assert!((large_focus.max - 4.0 * large_r).abs() < 1e-3, "large focus band is 0..4·radius, got max {}", large_focus.max);
    // Focus default is the framing distance (~2.2·radius) — inside 0..4·radius.
    assert!(small_focus.default_value >= small_focus.min && small_focus.default_value <= small_focus.max);

    true
}

/// BUG-upfq P4: the unexposed ssao `radius` node default must keep tracking
/// 0.5·scene-radius on big scenes — the old absolute `clamp(0.01, 5.0)`
/// pinned every scene with radius >10 at the same 5.0 AO reach.
#[test]
fn scene_ssao_radius_default_tracks_scene_scale() {
    fn ssao_radius(def: &manifold_core::effect_graph_def::EffectGraphDef) -> f32 {
        def.nodes
            .iter()
            .filter_map(|n| n.group.as_ref())
            .find_map(|g| g.nodes.iter().find(|n| n.type_id == "node.ssao_gtao"))
            .and_then(|n| n.params.get("radius"))
            .map(|v| match v {
                SerializedParamValue::Float { value } => *value,
                other => panic!("ssao radius must be a stamped float, got {other:?}"),
            })
            .expect("import has a grouped ssao_gtao node")
    }

    let (small_def, _) = scene_import_with_half_extent(5.0);
    let (large_def, _) = scene_import_with_half_extent(50.0);
    let small_r = super::scene_scale::SceneScale::from_bbox([-5.0; 3], [5.0; 3]).radius;
    let large_r = super::scene_scale::SceneScale::from_bbox([-50.0; 3], [50.0; 3]).radius;

    let small = ssao_radius(&small_def);
    let large = ssao_radius(&large_def);
    assert!((small - 0.5 * small_r).abs() < 1e-3, "small default is 0.5·radius, got {small}");
    assert!((large - 0.5 * large_r).abs() < 1e-3, "large default is 0.5·radius, got {large}");
    assert!(large > 5.0, "radius-86 scene must not pin at the old 5.0 ceiling, got {large}");
}

/// BUG-upfq P1: the position slider default — the import-stamped value, not
/// the primitive's generic 0.0 — is never moved by the range override, only
/// the min/max metadata.
#[test]
fn scene_range_override_never_moves_defaults() {
    let (def, _) = scene_import_with_half_extent(5.0);
    let meta = &def;
    let sun_id = def.nodes.iter().find(|n| n.type_id == "node.light").unwrap().id;
    let sun_node = def.nodes.iter().find(|n| n.type_id == "node.light").unwrap();

    let pos = stamped_card_param(meta, sun_id, "pos_x");
    // Default stays whatever the import stamped on the sun node.
    let stamped_default = match sun_node.params.get("pos_x") {
        Some(SerializedParamValue::Float { value }) => *value,
        other => panic!("sun pos_x must be a stamped float, got {other:?}"),
    };
    assert_eq!(pos.default_value, stamped_default, "default value must match the node's stamped value, not the primitive default (BUG-303)");
}

/// BUG-upfq P1: the light attenuation (`range`) slider scales with the
/// bbox and keeps its primitive's 0.01 floor. The sun's shadow range is
/// seeded at 1.5·radius — the band must contain it.
#[test]
fn scene_light_range_scales_and_keeps_zero_one_floor() {
    let (def, _) = scene_import_with_half_extent(2.0);
    let meta = &def;
    let sun_id = def.nodes.iter().find(|n| n.type_id == "node.light").unwrap().id;
    let range_spec = stamped_card_param(meta, sun_id, "range");

    let r = super::scene_scale::SceneScale::from_bbox([-2.0; 3], [2.0; 3]).radius;
    assert!((range_spec.max - 4.0 * r).abs() < 1e-3, "light range max = 4·radius, got {}", range_spec.max);
    assert_eq!(range_spec.min, 0.01, "light range min stays at the primitive's 0.01 floor");
    // The sun's seeded shadow range (1.5·radius) must stay inside the slide band.
    assert!(range_spec.default_value >= range_spec.min && range_spec.default_value <= range_spec.max);
}

/// BUG-bdwd: an import's CoC node carries `world_to_mm = 1000/scene_radius`
/// (the scene reads as real meter-scale distances in the lens physics) and
/// no card slider is stamped for it — it is plumbing. The f_stop card
/// slider range is scene-derived (`0.5..max(32, 64·radius)`) so a migrated
/// project's `f_stop × R` values stay on-slider.
#[test]
fn import_stamps_world_to_mm_and_scene_f_stop_range() {
    let (def, _) = scene_import_with_half_extent(5.0);
    let meta = def.preset_metadata.as_ref().unwrap();
    let r = super::scene_scale::SceneScale::from_bbox([-5.0; 3], [5.0; 3]).radius;

    // The coc node lives inside the dof group; find it by type recursively.
    fn find_coc(
        nodes: &[manifold_core::effect_graph_def::EffectGraphNode],
    ) -> Option<&manifold_core::effect_graph_def::EffectGraphNode> {
        nodes
            .iter()
            .find(|n| n.type_id == "node.coc_from_depth")
            .or_else(|| {
                nodes
                    .iter()
                    .find_map(|n| n.group.as_ref().and_then(|g| find_coc(&g.nodes)))
            })
    }
    let coc = find_coc(&def.nodes)
        .expect("import must carry a coc_from_depth node (inside the dof group)");
    let w2m = match coc.params.get("world_to_mm") {
        Some(SerializedParamValue::Float { value }) => *value,
        other => panic!("coc world_to_mm must be a stamped float, got {other:?}"),
    };
    assert!(
        (w2m - 1000.0 / r).abs() < 1e-3,
        "coc world_to_mm must be 1000/radius, got {w2m}, radius {r}"
    );
    assert!(w2m <= 100_000.0, "coc world_to_mm must be floored at 100,000, got {w2m}");

    // No card slider surfaced for world_to_mm (plumbing, not a control).
    assert!(
        !meta
            .params
            .iter()
            .any(|p| p.id.starts_with(&coc.id.to_string()) && p.id.contains("world_to_mm")),
        "world_to_mm must not be stamped as a card slider"
    );

    // f_stop card range is scene-derived, one-sided top.
    let lens_list: Vec<_> = def.nodes.iter().filter(|n| n.type_id == "node.camera_lens").collect();
    let lens_id = lens_list[0].id;
    let fstop = stamped_card_param(&def, lens_id, "f_stop");
    assert_eq!(fstop.min, 0.5, "f_stop band keeps the photographic 0.5 floor");
    assert!(
        (fstop.max - (32.0f32).max(64.0 * r)).abs() < 1e-3,
        "f_stop band top must be max(32, 64·radius), got {} radius {r}",
        fstop.max
    );
    assert!(fstop.default_value >= fstop.min && fstop.default_value <= fstop.max);
}

/// The merged object's transform pos_x card param.
fn merged_pos_x(
    target: &manifold_core::effect_graph_def::EffectGraphDef,
) -> (MergePlan, manifold_core::effect_graph_def::ParamSpecDef) {
    // A 10x-larger asset than the half-extent-1 target.
    let summary = merge_summary(vec![full_material(0, "Big", 100)], 10.0);
    let path = std::path::Path::new("/tmp/synthetic_merge_scene_range.glb");
    let plan = merge_import_into_graph(target, &summary, path).expect("merge");
    let transformed = plan
        .new_nodes
        .iter()
        .find_map(|n| n.group.as_ref())
        .and_then(|g| g.nodes.iter().find(|n| n.type_id == "node.transform_3d"))
        .expect("merged object has a transform_3d");
    let mut merged = target.clone();
    merged.nodes.extend(plan.new_nodes.iter().cloned());
    let meta = merged.preset_metadata.as_mut().expect("target has metadata");
    meta.params.extend(plan.new_card_params.iter().cloned());
    meta.bindings.extend(plan.new_card_bindings.iter().cloned());
    meta.scene_bounds = Some(plan.scene_bounds);
    let pos = stamped_card_param(&merged, transformed.id, "pos_x");
    (plan, pos)
}

/// One radius per graph (SCENE_SURFACES ruling L1): merging into a scene
/// that already has bounds scales the new transform sliders by the scene's
/// radius, not the incoming asset's, so the live lookup reads the same band.
#[test]
fn merge_into_bounded_scene_uses_scene_radius() {
    let target = scene_def_with_bbox_half_extent(1.0);
    let (plan, pos) = merged_pos_x(&target);
    let r = super::scene_scale::SceneScale::from_bbox([-1.0; 3], [1.0; 3]).radius;
    assert_eq!(pos.default_value, 0.0);
    assert!((pos.max - 2.0 * r).abs() < 1e-3, "pos slider must be ±2·scene-radius, got {}", pos.max);
    assert_eq!(plan.scene_bounds, ([-1.0; 3], [1.0; 3]), "the scene keeps its own bounds");
}

/// A scene with no bounds takes the incoming asset's box as its bounds, and
/// the merged transform sliders scale by that radius.
#[test]
fn merge_into_unbounded_scene_uses_incoming_radius() {
    let mut target = scene_def_with_bbox_half_extent(1.0);
    target.preset_metadata.as_mut().expect("meta").scene_bounds = None;
    let (plan, pos) = merged_pos_x(&target);
    let r = super::scene_scale::SceneScale::from_bbox([-10.0; 3], [10.0; 3]).radius;
    assert!((pos.max - 2.0 * r).abs() < 1e-3, "pos slider must be ±2·incoming-radius, got {}", pos.max);
    assert_eq!(plan.scene_bounds, ([-10.0; 3], [10.0; 3]), "the incoming box becomes the scene's bounds");
}

/// BUG-upfq follow-up: orbit camera `distance`/`near`/`far` slider ranges
/// must scale with the bbox radius. Small-bbox import (half-extent 0.137,
/// photoscan branch scale) and large-bbox import (half-extent 50) both get
/// distance max ≈ 6·radius, near max ≈ 2·radius, far max ≈ 20·radius.
#[test]
fn orbit_camera_ranges_scale_with_bbox_radius() {
    let (small_def, _) = scene_import_with_half_extent(0.137);
    let (large_def, _) = scene_import_with_half_extent(50.0);

    let small_meta = &small_def;
    let large_meta = &large_def;

    let small_cam_id = small_def.nodes.iter().find(|n| n.type_id == "node.orbit_camera").unwrap().id;
    let large_cam_id = large_def.nodes.iter().find(|n| n.type_id == "node.orbit_camera").unwrap().id;

    let small_r = super::scene_scale::SceneScale::from_bbox([-0.137; 3], [0.137; 3]).radius;
    let large_r = super::scene_scale::SceneScale::from_bbox([-50.0; 3], [50.0; 3]).radius;

    // distance band: 0.01..6·radius
    let small_dist = stamped_card_param(small_meta, small_cam_id, "distance");
    let large_dist = stamped_card_param(large_meta, large_cam_id, "distance");
    assert!((small_dist.max - 6.0 * small_r).abs() < 1e-3, "small orbit distance max = 6·radius, got {}", small_dist.max);
    assert!((large_dist.max - 6.0 * large_r).abs() < 1e-3, "large orbit distance max = 6·radius, got {}", large_dist.max);
    assert_eq!(small_dist.min, 0.01, "distance min stays at the primitive's 0.01 floor");

    // near band: 0.001..2·radius
    let small_near = stamped_card_param(small_meta, small_cam_id, "near");
    let large_near = stamped_card_param(large_meta, large_cam_id, "near");
    assert!((small_near.max - 2.0 * small_r).abs() < 1e-3, "small orbit near max = 2·radius, got {}", small_near.max);
    assert!((large_near.max - 2.0 * large_r).abs() < 1e-3, "large orbit near max = 2·radius, got {}", large_near.max);
    assert_eq!(small_near.min, 0.001, "near min stays at the primitive's 0.001 floor");

    // far band: 1.0..min(20·radius, 10_000), widened to contain the stamped
    // default (the import stamps far at max(DEFAULT_FAR=200, distance+1.5·r),
    // so small scenes get far=200 which is wider than 20·r).
    let small_far = stamped_card_param(small_meta, small_cam_id, "far");
    let large_far = stamped_card_param(large_meta, large_cam_id, "far");
    let small_far_expected = (20.0 * small_r).clamp(200.0, 10_000.0); // widened by stamped default
    let large_far_expected = (20.0 * large_r).min(10_000.0);
    assert!((small_far.max - small_far_expected).abs() < 1e-3, "small orbit far max = min(20·radius, 10000) widened to contain stamped default, got {}", small_far.max);
    assert!((large_far.max - large_far_expected).abs() < 1e-3, "large orbit far max = min(20·radius, 10000), got {}", large_far.max);
    assert_eq!(small_far.min, 1.0, "far min stays at the primitive's 1.0 floor");
}

/// BUG-upfq follow-up: a stamped default that falls outside the derived band
/// must be contained by the widen rule, never moved.
#[test]
fn orbit_camera_range_override_never_moves_defaults() {
    let (def, _) = scene_import_with_half_extent(0.137);
    let meta = &def;
    let cam_id = def.nodes.iter().find(|n| n.type_id == "node.orbit_camera").unwrap();
    let cam_node = &cam_id;
    let cam_id = cam_id.id;

    for param in &["distance", "near", "far"] {
        let spec = stamped_card_param(meta, cam_id, param);
        let stamped_default = match cam_node.params.get(*param) {
            Some(SerializedParamValue::Float { value }) => *value,
            other => panic!("{param} must be a stamped float, got {other:?}"),
        };
        assert_eq!(spec.default_value, stamped_default, "{param}: default value must match the node's stamped value");
        assert!(spec.default_value >= spec.min, "{param}: default {} must be >= min {}", spec.default_value, spec.min);
        assert!(spec.default_value <= spec.max, "{param}: default {} must be <= max {}", spec.default_value, spec.max);
    }
}

/// KHR_lights_punctual assembly keeps authored photometric values and cone
/// metadata in the graph. An authored light also replaces the synthetic sun
/// slot, so the render_scene port count and wire target stay one-to-one.
#[test]
fn authored_spot_light_preserves_raw_intensity_cone_and_physical_range() {
    use super::gltf_load::{GltfLightKind, GltfPunctualLight};

    let summary = GltfImportSummary {
        materials: vec![full_material(0, "SpotMaterial", 3)],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: vec![GltfPunctualLight {
            name: Some("Key Spot".to_string()),
            kind: GltfLightKind::Spot {
                inner_cone_angle: 0.2,
                outer_cone_angle: 0.5,
            },
            color: [0.25, 0.5, 0.75],
            intensity: 2400.0,
            range: Some(12.5),
            world_pos: [1.0, 2.0, 3.0],
            world_forward: [0.0, 0.0, -1.0],
        }],
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };

    let path = std::path::Path::new("/tmp/synthetic_authored_spot.glb");
    let (def, report) = build_import_graph(&summary, path).expect("build authored spot graph");
    let render = def
        .nodes
        .iter()
        .find(|n| n.type_id == "node.render_scene")
        .expect("render_scene node");
    assert_eq!(render.params.get("lights"), Some(&int(1)));
    assert!(
        !def.nodes.iter().any(|n| n.node_id == "sun"),
        "authored lights must not get a synthetic sun"
    );

    let light = def
        .nodes
        .iter()
        .find(|n| n.node_id == "light_0")
        .expect("authored light_0 node");
    assert_eq!(light.params.get("mode"), Some(&enum_val(2)));
    assert_eq!(light.params.get("falloff"), Some(&enum_val(1)));
    assert_eq!(light.params.get("intensity"), Some(&float(2400.0)));
    assert_eq!(light.params.get("range"), Some(&float(12.5)));
    assert_eq!(light.params.get("inner_cone_angle"), Some(&float(0.2)));
    assert_eq!(light.params.get("outer_cone_angle"), Some(&float(0.5)));
    assert!(def.wires.iter().any(|wire| {
        wire.from_node == light.id && wire.to_node == render.id && wire.to_port == "light_0"
    }));
    assert!(report.report_lines.iter().any(|line| {
        line.contains("raw") && line.contains("candela") && line.contains("spot cone")
    }));
}

/// With authored lights, the sky's sun is the first directional one; a
/// scene with only point and spot lights has no sun disc.
#[test]
fn sky_sun_is_the_first_authored_directional_light() {
    use super::gltf_load::{GltfLightKind, GltfPunctualLight};
    let light = |name: &str, kind| GltfPunctualLight {
        name: Some(name.to_string()),
        kind,
        color: [1.0, 1.0, 1.0],
        intensity: 10.0,
        range: None,
        world_pos: [0.0, 1.0, 0.0],
        world_forward: [0.0, -1.0, 0.0],
    };
    let import = |lights| {
        let summary = GltfImportSummary {
            materials: vec![full_material(0, "M", 3)],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
            animations: Vec::new(),
            animation_report_lines: Vec::new(),
            extension_report_lines: Vec::new(),
            lights,
            cameras: Vec::new(),
            camera_report_lines: Vec::new(),
            texture_dims: Vec::new(),
        };
        build_import_graph(&summary, std::path::Path::new("/tmp/synthetic_sky_sun.glb")).expect("build").0
    };
    let sky_sun = |def: &manifold_core::effect_graph_def::EffectGraphDef| {
        let envmap = def.nodes.iter().find(|n| n.type_id == "node.bake_environment").unwrap().id;
        def.wires
            .iter()
            .find(|w| w.to_node == envmap && w.to_port == "sun")
            .and_then(|w| def.nodes.iter().find(|n| n.id == w.from_node))
            .map(|n| n.node_id.to_string())
    };

    let def = import(vec![light("Lamp", GltfLightKind::Point), light("Key", GltfLightKind::Directional)]);
    assert_eq!(sky_sun(&def).as_deref(), Some("light_1"));
    let def = import(vec![light("Lamp", GltfLightKind::Point)]);
    assert_eq!(sky_sun(&def), None);
}


/// One Lighting Mode (Gradient, Softbox, HDRI) lives on the bake; the HDRI
/// feeds it and the visible sky; the old second switch is gone. The sky is off
/// until the Background switch is turned on, and its Blur and Brightness read
/// only then.
#[test]
fn import_has_one_environment_mode_and_an_off_by_default_hdri_background() {

    let summary = GltfImportSummary {
        materials: vec![full_material(0, "Solid", 500)],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let (def, _) = build_import_graph(&summary, std::path::Path::new("/tmp/synthetic_background.glb")).expect("build");
    let handle = |h: &str| def.nodes.iter().find(|n| n.node_id == h).unwrap_or_else(|| panic!("node `{h}`"));
    let wired = |from: &str, from_port: &str, to: &str, to_port: &str| {
        let (from, to) = (handle(from).id, handle(to).id);
        def.wires.iter().any(|w| w.from_node == from && w.from_port == from_port && w.to_node == to && w.to_port == to_port)
    };

    for gone in ["env_select", "hdri_gain"] {
        assert!(def.nodes.iter().all(|n| n.node_id != gone), "{gone} is the second mode path");
    }
    let meta = def.preset_metadata.as_ref().unwrap();
    assert!(meta.params.iter().all(|p| p.id != "env_mode"), "no second Environment Mode param");
    assert!(wired("hdri", "out", "envmap", "hdri"), "the HDRI feeds the bake's HDRI mode");
    assert!(wired("envmap", "envmap", "render", "envmap"));

    assert!(wired("hdri", "out", "bg_sky", "sky"));
    assert!(wired("lens", "out", "bg_sky", "camera"), "the sky is seen through the scene camera");
    assert!(wired("bg_sky", "out", "bg_blur", "source"));
    assert!(wired("bg_blur", "out", "bg_gain", "in"));
    assert!(wired("bg_gain", "out", "bg_over", "bottom"));
    assert!(wired("bg_over", "out", "bg_select", "in_1"));
    assert!(wired("bg_select", "out", "final", "in"));
    let selector = handle("bg_select").params.get("selector");
    assert_eq!(selector, Some(&SerializedParamValue::Float { value: 0.0 }), "Background starts off");

    let spec = |id: &str| meta.params.iter().find(|p| p.id == id).unwrap_or_else(|| panic!("card param `{id}`"));
    let target = |id: &str| {
        let binding = meta.bindings.iter().find(|b| b.id == id).unwrap();
        match &binding.target {
            BindingTarget::Node { node_id, param } => (node_id.as_str().to_string(), param.clone()),
            other => panic!("expected a node target, got {other:?}"),
        }
    };
    assert_eq!(target("bg_enabled"), ("bg_select".to_string(), "selector".to_string()));
    assert_eq!(target("bg_blur"), ("bg_blur".to_string(), "radius".to_string()));
    assert_eq!(target("bg_brightness"), ("bg_gain".to_string(), "gain".to_string()));
    assert_eq!(spec("bg_enabled").default_value, 0.0);
    assert!(spec("bg_enabled").is_toggle);
    assert_eq!(spec("bg_brightness").default_value, 1.0);
}

/// A fresh import renders as it always has: the Ray Tracing surface row and
/// the render_scene node agree, and both are off. The row's value re-asserts
/// over the node every frame, so a row that disagreed would flip RT on or off.
#[test]
fn fresh_import_ray_tracing_row_agrees_with_the_node() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/cc0__tiger_lily.glb");
    let (def, _report) = assemble_import_graph(&path).expect("assemble tiger lily");
    let scene = def.nodes.iter().find(|n| n.type_id == "node.render_scene").expect("render_scene");
    let node_on = matches!(scene.params.get("rt_enabled"), Some(SerializedParamValue::Bool { value: true }));
    let meta = def.preset_metadata.as_ref().expect("import stamps metadata");
    let row = meta
        .bindings
        .iter()
        .find_map(|b| match &b.target {
            BindingTarget::Node { node_id, param } if *node_id == scene.node_id && param == "rt_enabled" => Some(b.id.clone()),
            _ => None,
        })
        .expect("the Ray Tracing row is stamped");
    let spec = meta.params.iter().find(|p| p.id == row).expect("row spec");
    assert_eq!(spec.default_value >= 0.5, node_on, "row default and node must agree");
    assert!(!node_on, "a fresh import has ray tracing off, as before");
}
