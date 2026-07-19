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
        .map(|pd| {
            let (min, max) = pd.range.unwrap_or((0.0, 1.0));
            let default_value: manifold_core::effect_graph_def::SerializedParamValue =
                pd.default.clone().into();
            let is_angle = matches!(pd.ty, ParamType::Angle);
            let whole_numbers = matches!(pd.ty, ParamType::Int | ParamType::Enum);
            let is_toggle = matches!(pd.ty, ParamType::Bool);
            let is_trigger = matches!(pd.ty, ParamType::Trigger);
            let value_labels = if matches!(pd.ty, ParamType::Enum) {
                pd.enum_values.iter().map(|s| s.to_string()).collect()
            } else {
                Vec::new()
            };
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
    let provider = PrimitiveRegistrySceneExposureProvider;
    manifold_core::scene_exposure::migrate_scene_exposures(
        def,
        SCENE_VOCABULARY_TYPE_IDS,
        section_name_for_node,
        &provider,
    )
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

    #[test]
    fn metadata_for_unknown_type_is_empty() {
        assert!(metadata_for_node_type("node.definitely_not_real").is_empty());
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
