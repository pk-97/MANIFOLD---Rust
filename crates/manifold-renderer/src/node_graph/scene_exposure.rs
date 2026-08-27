//! Renderer-side implementation of `docs/SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md` P1.
//!
//! - `metadata_for_node_type` reads a primitive's `ParamDef` table through the
//!   registry.
//! - `migrate_scene_exposures` is the load-time idempotent migration that stamps
//!   exposures onto every scene-vocabulary node in an existing graph.
//! - `PrimitiveRegistrySceneExposureProvider` implements the core trait for
//!   creation-site commands that cannot depend on `manifold_renderer` directly.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_exposure::{SceneExposureMetadataProvider, SceneParamMetadata};

use crate::node_graph::parameters::ParamType;
use crate::node_graph::persistence::PrimitiveRegistry;

static SCENE_EXPOSURE_REGISTRY: std::sync::LazyLock<PrimitiveRegistry> =
    std::sync::LazyLock::new(PrimitiveRegistry::with_builtin);

/// Scene-vocabulary type ids — the nodes whose params the scene panel wants to
/// address. Kept in sync with `scene_vm.rs`.
const SCENE_VOCABULARY_TYPE_IDS: &[&str] = &[
    "node.transform_3d",
    "node.pbr_material",
    "node.phong_material",
    "node.unlit_material",
    "node.cel_material",
    "node.light",
    "node.orbit_camera",
    "node.free_camera",
    "node.look_at_camera",
    "node.camera_lens",
    "node.atmosphere",
    "node.bake_environment",
    "node.scene_object",
    "node.bend_mesh",
    "node.twist_mesh",
    "node.taper_mesh",
    "node.push_along_normals",
    "node.push_mesh",
    "node.morph_mesh",
    "node.rotate_3d",
    // RAYTRACING_DESIGN.md D14/section 5.2: the scene-level RT toggles live on the
    // `node.render_scene` root. Curated to the RT subset in
    // `metadata_for_node_type` — the root node's other params (sun, env,
    // counts) stay hand-curated exposures, never auto-stamped.
    "node.render_scene",
];

/// The curated `node.render_scene` auto-stamp subset (see the vocabulary
/// entry above): the per-scene RT toggle (D14), the MetalFX temporal
/// quality toggle (P4), the per-scene reflection toggle (section 9 RD9),
/// the per-scene ML-denoiser feed toggle (RAYTRACING_DESIGN.md section
/// 17 DN4), and the per-term RT toggles (shadows/AO/GI — the hybrid-split
/// levers). Everything else on the root node is deliberately NOT auto-stamped.
const RENDER_SCENE_STAMPED_PARAMS: &[&str] = &[
    "rt_enabled",
    "temporal_upscale",
    "rt_reflections",
    "rt_shadows",
    "rt_ao",
    "rt_gi",
    "rt_denoise_feed",
];

/// Return the full param manifest for `type_id` from the primitive registry,
/// converting `ParamDef` metadata into the crate-neutral `SceneParamMetadata`
/// shape. Empty when the type is unknown.
pub fn metadata_for_node_type(type_id: &str) -> Vec<SceneParamMetadata> {
    let Some(node) = SCENE_EXPOSURE_REGISTRY.construct(type_id) else {
        return Vec::new();
    };
    node.parameters()
        .iter()
        .filter(|pd| {
            type_id != "node.render_scene" || RENDER_SCENE_STAMPED_PARAMS.contains(&pd.name.as_ref())
        })
        .map(|pd| {
            let (min, max) = pd.range.unwrap_or((0.0, 1.0));
            let default_value: manifold_core::effect_graph_def::SerializedParamValue =
                pd.default.clone().into();
            let is_angle = matches!(pd.ty, ParamType::Angle);
            let whole_numbers = matches!(pd.ty, ParamType::Int | ParamType::Enum);
            let is_toggle = matches!(pd.ty, ParamType::Bool);
            let is_trigger = matches!(pd.ty, ParamType::Trigger);
            // R2 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): display labels
            // aren't Enum-exclusive — a modulatable `Float` threshold param
            // (e.g. `node.light`'s `cast_shadows`, kept `Float` on purpose so
            // it stays LFO/trigger-modulatable) can declare `enum_values` as
            // display-only text for the outer-card slider
            // (`format_param_value` substitutes by index regardless of the
            // param's real type). Read straight off whatever the primitive
            // declared, not gated by `ty`.
            let value_labels = pd.enum_values.iter().map(|s| s.to_string()).collect();
            let convert = match pd.ty {
                ParamType::Bool => manifold_core::effects::ParamConvert::BoolThreshold,
                ParamType::Int => manifold_core::effects::ParamConvert::IntRound,
                ParamType::Enum => manifold_core::effects::ParamConvert::EnumRound,
                ParamType::Trigger => manifold_core::effects::ParamConvert::Trigger,
                _ => manifold_core::effects::ParamConvert::Float,
            };
            SceneParamMetadata {
                name: pd.name.to_string(),
                label: pd.label.to_string(),
                min,
                max,
                default_value,
                is_angle,
                whole_numbers,
                is_toggle,
                is_trigger,
                value_labels,
                convert,
            }
        })
        .collect()
}

/// Idempotent load-time migration: stamp exposures for every scene-vocabulary
/// node in `def`. Returns `true` iff anything changed. Safe to run on any graph
/// (non-scene defs are untouched).
pub fn migrate_scene_exposures(def: &mut EffectGraphDef) -> bool {
    let repaired = repair_legacy_lens_f_stop(def);
    let provider = PrimitiveRegistrySceneExposureProvider;
    let migrated = manifold_core::scene_exposure::migrate_scene_exposures(
        def,
        SCENE_VOCABULARY_TYPE_IDS,
        section_name_for_node,
        &provider,
    );
    repaired || migrated
}

/// Legacy tail repair (2026-08-27): pre-fix projects carry the lens's old
/// neutral `f_stop = 1000` seed. 1000 sat outside the param's 0.5–32 band,
/// and the stamper's widen rule stretched every stamped f-stop slider to
/// fit — the unusable-ranges bug Peter reported on a fresh import. The same
/// fix that brings the value back in band (32) also moves "DoF off" off the
/// f-stop axis entirely: off is bokeh's `enabled` toggle now, seeded false
/// (no f-stop value is off on close-up scenes — f/32 blurs visibly past the
/// focus plane). A stored 1000 is proof the lens was never dialed, so every
/// bokeh in the def is forced to enabled=false alongside the rewrite —
/// otherwise migrated projects would GAIN visible DoF on load, changing
/// their look. Guarded on exactly 1000.0: any other f-stop is a performer's
/// choice (or already repaired) and the whole pass is left alone. Runs
/// BEFORE the core stamp/repair passes so they see the corrected defaults;
/// idempotent — a second run finds no 1000.0 and writes nothing.
fn repair_legacy_lens_f_stop(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::{BindingTarget, SerializedParamValue};

    let mut lens_node_ids: Vec<manifold_core::NodeId> = Vec::new();
    let mut bokeh_node_ids: Vec<manifold_core::NodeId> = Vec::new();
    fn collect_and_repair(
        nodes: &mut [manifold_core::effect_graph_def::EffectGraphNode],
        lenses: &mut Vec<manifold_core::NodeId>,
    ) -> bool {
        let mut changed = false;
        for node in nodes.iter_mut() {
            if node.type_id.as_str() == "node.camera_lens"
                && let Some(SerializedParamValue::Float { value }) = node.params.get_mut("f_stop")
                && *value == 1000.0
            {
                *value = 32.0;
                lenses.push(node.node_id.clone());
                changed = true;
            }
            if let Some(group) = node.group.as_deref_mut() {
                changed |= collect_and_repair(&mut group.nodes, lenses);
            }
        }
        changed
    }
    fn collect_bokehs(
        nodes: &mut [manifold_core::effect_graph_def::EffectGraphNode],
        bokehs: &mut Vec<manifold_core::NodeId>,
    ) {
        for node in nodes.iter_mut() {
            if node.type_id.as_str() == "node.bokeh_gather" {
                node.params.insert(
                    "enabled".to_string(),
                    SerializedParamValue::Bool { value: false },
                );
                bokehs.push(node.node_id.clone());
            }
            if let Some(group) = node.group.as_deref_mut() {
                collect_bokehs(&mut group.nodes, bokehs);
            }
        }
    }
    if !collect_and_repair(&mut def.nodes, &mut lens_node_ids) {
        return false;
    }
    collect_bokehs(&mut def.nodes, &mut bokeh_node_ids);

    // Keep the stamps in step: the default repairs in core key off the node
    // param, but they only widen ranges — the stretched max needs the band
    // re-derived from the current metadata (0.5–32 widened by the new 32
    // default is exactly the band). Bokeh stamps follow the new off default.
    let f_stop_meta = metadata_for_node_type("node.camera_lens")
        .into_iter()
        .find(|m| m.name == "f_stop");
    if let Some(preset) = def.preset_metadata.as_mut() {
        if let Some(meta) = f_stop_meta {
            for node_id in &lens_node_ids {
                let Some(binding) = preset.bindings.iter_mut().find(|b| {
                    !b.user_added
                        && matches!(
                            &b.target,
                            BindingTarget::Node { node_id: nid, param }
                                if nid == node_id && param == "f_stop"
                        )
                }) else {
                    continue;
                };
                if binding.default_value == 1000.0 {
                    binding.default_value = 32.0;
                }
                let binding_id = binding.id.clone();
                if let Some(spec) = preset.params.iter_mut().find(|p| p.id == binding_id) {
                    if spec.default_value == 1000.0 {
                        spec.default_value = 32.0;
                    }
                    spec.min = meta.min.min(spec.default_value);
                    spec.max = meta.max.max(spec.default_value);
                }
            }
        }
        for node_id in &bokeh_node_ids {
            let Some(binding) = preset.bindings.iter_mut().find(|b| {
                !b.user_added
                    && matches!(
                        &b.target,
                        BindingTarget::Node { node_id: nid, param }
                            if nid == node_id && param == "enabled"
                    )
            }) else {
                continue;
            };
            binding.default_value = 0.0;
            let binding_id = binding.id.clone();
            if let Some(spec) = preset.params.iter_mut().find(|p| p.id == binding_id) {
                spec.default_value = 0.0;
            }
        }
    }
    true
}

fn section_name_for_node(node: &manifold_core::effect_graph_def::EffectGraphNode) -> String {
    let display = node
        .title
        .as_deref()
        .or(node.handle.as_deref())
        .unwrap_or("Scene");
    let category = match node.type_id.as_str() {
        "node.transform_3d" => "Transform".to_string(),
        "node.pbr_material" | "node.phong_material" | "node.unlit_material" | "node.cel_material" => {
            "Material".to_string()
        }
        "node.light" => return display.to_string(),
        "node.orbit_camera" | "node.free_camera" | "node.look_at_camera" | "node.camera_lens" => {
            "Camera".to_string()
        }
        "node.atmosphere" => "Atmosphere".to_string(),
        "node.bake_environment" => "Environment".to_string(),
        "node.render_scene" => return "Rendering".to_string(),
        "node.scene_object" => "Object".to_string(),
        _ => {
            // Modifiers and anything else: use the type id suffix.
            node.type_id
                .strip_prefix("node.")
                .map(|s| {
                    let mut s = s.to_string();
                    s.replace_range(0..1, &s[0..1].to_uppercase());
                    s
                })
                .unwrap_or_else(|| "Modifier".to_string())
        }
    };
    format!("{} — {}", display, category)
}

/// Zero-sized provider backed by the lazy static registry. Commands in
/// `manifold_editing` store a `Box<dyn SceneExposureMetadataProvider>` and call
/// this at execute time.
pub struct PrimitiveRegistrySceneExposureProvider;

impl SceneExposureMetadataProvider for PrimitiveRegistrySceneExposureProvider {
    fn metadata_for_type(&self, type_id: &str) -> Vec<SceneParamMetadata> {
        metadata_for_node_type(type_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::NodeId;

    #[test]
    fn metadata_for_light_includes_enum_and_float_params() {
        let meta = metadata_for_node_type("node.light");
        assert!(!meta.is_empty());
        let mode = meta.iter().find(|m| m.name == "mode").expect("mode present");
        assert!(matches!(mode.convert, manifold_core::effects::ParamConvert::EnumRound));
        assert!(!mode.value_labels.is_empty());
        let intensity = meta
            .iter()
            .find(|m| m.name == "intensity")
            .expect("intensity present");
        assert!(matches!(
            intensity.convert,
            manifold_core::effects::ParamConvert::Float
        ));
    }

    /// R2 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): `cast_shadows` stays
    /// `Float`/0..1 (modulatable by an LFO/trigger — `ParamType::Bool` would
    /// lose that) but declares `enum_values: ["Off", "On"]` as display-only
    /// labels; `metadata_for_node_type` must carry them into `value_labels`
    /// even though the param's real type isn't `Enum`. Regression coverage
    /// for the R2 bug: the scene panel's Cast Shadows row showed the raw
    /// float ("1.00") instead of "On"/"Off" because `value_labels` was
    /// previously populated ONLY for `ParamType::Enum`.
    #[test]
    fn metadata_for_light_carries_cast_shadows_display_labels_despite_float_type() {
        let meta = metadata_for_node_type("node.light");
        let cast_shadows = meta
            .iter()
            .find(|m| m.name == "cast_shadows")
            .expect("cast_shadows present");
        assert!(
            matches!(cast_shadows.convert, manifold_core::effects::ParamConvert::Float),
            "stays a modulatable Float param, not converted to Bool"
        );
        assert_eq!(
            cast_shadows.value_labels,
            vec!["Off".to_string(), "On".to_string()],
            "display labels carried through despite the Float type"
        );
    }

    #[test]
    fn metadata_for_unknown_type_is_empty() {
        assert!(metadata_for_node_type("node.definitely_not_real").is_empty());
    }

    /// R2 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md), manifest level: the
    /// REAL production metadata (`metadata_for_node_type`, not a synthetic
    /// fixture) stamped through `stamp_scene_node_exposures_into` (the exact
    /// call `AddSceneLightCommand::execute` makes) must produce a
    /// `ParamSpecDef` for `cast_shadows` carrying `value_labels`, so the
    /// scene panel's `format_param_value` (which reads straight off the
    /// manifest's `ParamSpecDef.value_labels`) substitutes "On"/"Off" text
    /// instead of the raw float.
    #[test]
    fn stamped_light_manifest_carries_cast_shadows_value_labels() {
        use manifold_core::scene_exposure::stamp_scene_node_exposures_into;

        let node_id = NodeId::new("light_0");
        let light_metadata = metadata_for_node_type("node.light");
        let mut params = Vec::new();
        let mut bindings = Vec::new();
        stamp_scene_node_exposures_into(
            &mut params,
            &mut bindings,
            1,
            &node_id,
            "node.light",
            "Light 1",
            &light_metadata,
            &std::collections::BTreeMap::new(),
        );

        let cast_shadows_spec = params
            .iter()
            .find(|p| p.name == "Cast Shadows")
            .expect("cast_shadows exposed onto the manifest");
        assert_eq!(
            cast_shadows_spec.value_labels,
            vec!["Off".to_string(), "On".to_string()],
            "the stamped manifest ParamSpecDef carries the display labels"
        );
    }

    /// RAYTRACING_DESIGN.md D14/section 5.2/section 9 RD9: the scene root's RT toggles surface on
    /// the scene panel via the same vocabulary migration as every other
    /// scene control — curated to EXACTLY the three toggles, so the root
    /// node's dozens of other params never flood the panel.
    #[test]
    fn migrate_stamps_render_scene_rt_toggles_only_under_rendering_section() {
        use std::collections::BTreeMap;

        let def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![manifold_core::effect_graph_def::EffectGraphNode {
                id: 7,
                node_id: NodeId::new("scene_root"),
                type_id: "node.render_scene".to_string(),
                handle: None,
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }],
            wires: vec![],
        };

        let mut migrated = def.clone();
        assert!(migrate_scene_exposures(&mut migrated));
        let meta = migrated.preset_metadata.expect("metadata stamped");
        let stamped: Vec<&str> = meta
            .bindings
            .iter()
            .filter_map(|b| match &b.target {
                manifold_core::effect_graph_def::BindingTarget::Node { param, .. } => {
                    Some(param.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            stamped,
            vec![
                "rt_enabled",
                "temporal_upscale",
                "rt_reflections",
                "rt_shadows",
                "rt_ao",
                "rt_gi",
                "rt_denoise_feed"
            ],
            "exactly the seven RT toggles, nothing else from the root node"
        );
        for spec in &meta.params {
            assert_eq!(spec.section.as_deref(), Some("Rendering"));
            assert!(spec.is_toggle, "{} must surface as a toggle row", spec.name);
            assert!(
                spec.card_visible,
                "{} must be card-visible (the curated inspector card shows ALL stamped RT toggles)",
                spec.name,
            );
        }
    }

    /// 2026-08-27 (Peter's unusable-ranges report): a pre-fix project carries
    /// the lens's legacy neutral `f_stop = 1000` plus a stamp the widen rule
    /// stretched to 0.5–1000. Load must rewrite the stored value to 32
    /// (top of the band) and un-stretch the stamp — AND force bokeh
    /// `enabled` false (the 1000 proves DoF was never dialed; off is the
    /// toggle now, and migrated projects must not GAIN visible DoF on load).
    /// Any other stored f-stop is a performer's choice and must survive.
    #[test]
    fn migrate_repairs_legacy_lens_f_stop_1000_and_unstretches_stamp() {
        use manifold_core::effect_graph_def::SerializedParamValue;
        use std::collections::BTreeMap;

        let lens = |f_stop: f32, id: u32, node_id: &str| {
            let mut params = BTreeMap::new();
            params.insert("f_stop".to_string(), SerializedParamValue::Float { value: f_stop });
            manifold_core::effect_graph_def::EffectGraphNode {
                id,
                node_id: NodeId::new(node_id),
                type_id: "node.camera_lens".to_string(),
                handle: Some(node_id.to_string()),
                params,
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }
        };

        let bokeh = manifold_core::effect_graph_def::EffectGraphNode {
            id: 5,
            node_id: NodeId::new("bokeh"),
            type_id: "node.bokeh_gather".to_string(),
            handle: Some("bokeh".to_string()),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        };

        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![lens(1000.0, 3, "lens_legacy"), lens(2.8, 4, "lens_dialed"), bokeh],
            wires: vec![],
        };

        // First pass stamps and repairs; doctor the repaired lens back to
        // the exact pre-fix state (legacy seed + stretched stamp + DoF
        // default-on) to prove the load path repairs a REAL old project,
        // not just a fresh one.
        assert!(migrate_scene_exposures(&mut def));
        {
            let legacy = def.nodes.iter_mut().find(|n| n.id == 3).unwrap();
            legacy.params.insert(
                "f_stop".to_string(),
                SerializedParamValue::Float { value: 1000.0 },
            );
            let bokeh = def.nodes.iter_mut().find(|n| n.id == 5).unwrap();
            bokeh.params.insert(
                "enabled".to_string(),
                SerializedParamValue::Bool { value: true },
            );
            let meta = def.preset_metadata.as_mut().unwrap();
            let binding = meta
                .bindings
                .iter_mut()
                .find(|b| b.id == "3_f_stop")
                .expect("stamped f_stop binding");
            binding.default_value = 1000.0;
            let spec = meta.params.iter_mut().find(|p| p.id == "3_f_stop").unwrap();
            spec.default_value = 1000.0;
            spec.max = 1000.0;
            // The bokeh stamp a real tail project carries — bokeh_gather is
            // not in the scene vocabulary, so the generic stamper never
            // makes one; the import assembly / v1130 migration does.
            meta.bindings.push(manifold_core::effect_graph_def::BindingDef {
                id: "5_enabled".to_string(),
                label: "Enabled".to_string(),
                default_value: 1.0,
                target: manifold_core::effect_graph_def::BindingTarget::Node {
                    node_id: NodeId::new("bokeh"),
                    param: "enabled".to_string(),
                },
                convert: manifold_core::effects::ParamConvert::BoolThreshold,
                user_added: false,
                scale: 1.0,
                offset: 0.0,
                default_mirrors_node_param: true,
            });
            meta.params.push(manifold_core::effect_graph_def::ParamSpecDef {
                id: "5_enabled".to_string(),
                name: "Enabled".to_string(),
                min: 0.0,
                max: 1.0,
                default_value: 1.0,
                section: Some("Camera".to_string()),
                ..Default::default()
            });
        }

        assert!(
            migrate_scene_exposures(&mut def),
            "legacy state must be repaired on load"
        );
        let legacy = def.nodes.iter().find(|n| n.id == 3).unwrap();
        assert_eq!(
            legacy.params.get("f_stop"),
            Some(&SerializedParamValue::Float { value: 32.0 }),
            "legacy 1000 seed rewritten to the in-band neutral"
        );
        let bokeh = def.nodes.iter().find(|n| n.id == 5).unwrap();
        assert_eq!(
            bokeh.params.get("enabled"),
            Some(&SerializedParamValue::Bool { value: false }),
            "DoF forced off — the legacy 1000 proves it was never dialed"
        );
        let meta = def.preset_metadata.as_ref().unwrap();
        let spec = meta.params.iter().find(|p| p.id == "3_f_stop").unwrap();
        assert_eq!(spec.default_value, 32.0);
        assert_eq!((spec.min, spec.max), (0.5, 32.0), "stretched stamp un-stretched to the band");
        assert_eq!(
            meta.bindings.iter().find(|b| b.id == "3_f_stop").unwrap().default_value,
            32.0
        );
        assert_eq!(
            meta.params.iter().find(|p| p.id == "5_enabled").unwrap().default_value,
            0.0,
            "bokeh stamp default follows the forced-off node param"
        );
        assert_eq!(
            meta.bindings.iter().find(|b| b.id == "5_enabled").unwrap().default_value,
            0.0
        );
        let dialed = def.nodes.iter().find(|n| n.id == 4).unwrap();
        assert_eq!(
            dialed.params.get("f_stop"),
            Some(&SerializedParamValue::Float { value: 2.8 }),
            "a performer's chosen aperture is never rewritten"
        );

        let after_repair = def.clone();
        assert!(
            !migrate_scene_exposures(&mut def),
            "second migration run is a no-op once repaired"
        );
        assert_eq!(def, after_repair);
    }

    #[test]
    fn migrate_is_idempotent() {
        use std::collections::BTreeMap;

        let def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![manifold_core::effect_graph_def::EffectGraphNode {
                id: 1,
                node_id: NodeId::new("sun"),
                type_id: "node.light".to_string(),
                handle: Some("Sun".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }],
            wires: vec![],
        };

        let mut first = def.clone();
        assert!(migrate_scene_exposures(&mut first));
        let mut second = first.clone();
        assert!(!migrate_scene_exposures(&mut second));
        assert_eq!(first, second);
    }
}
