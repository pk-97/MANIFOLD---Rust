//! The node half of the param lookup: every param a primitive declares,
//! registered as core's `NodeParamSource`, the one node param lookup every
//! crate reads. Where a scene param lives on a card is the (kind, property)
//! table in `manifold_core::scene_schema`, not here.

use manifold_core::effect_graph_def::EffectGraphNode;
use manifold_core::param_lookup::NodeParamMetadata;

use crate::node_graph::material_inspector::material_param_role;
use manifold_node_engine::parameters::{ParamDef, ParamType};
use manifold_node_engine::persistence::PrimitiveRegistry;

static NODE_REGISTRY: std::sync::LazyLock<PrimitiveRegistry> =
    std::sync::LazyLock::new(PrimitiveRegistry::with_builtin);

/// An unbounded angle has no primitive range to copy into a card row, but a
/// card still needs a useful initial scrub band. Keep the band in the
/// primitive's storage unit (radians); the `is_angle` flag makes the card
/// present and edit it in degrees. The primitive itself remains unbounded.
const UNBOUNDED_ANGLE_RANGE: (f32, f32) = (-std::f32::consts::TAU, std::f32::consts::TAU);

/// Every param `type_id` declares, each as a card row would describe it.
/// Empty when the type is unknown.
pub fn all_params_for_node_type(type_id: &str) -> Vec<NodeParamMetadata> {
    let Some(node) = NODE_REGISTRY.construct(type_id) else {
        return Vec::new();
    };
    params_metadata(type_id, node.parameters())
}

/// The params `type_id` stamps on a card: the registered source minus the
/// params the scene table hides. The importer and the GPU FLIP preset still
/// stamp (lanes L3 and L7 remove them).
pub fn metadata_for_node_type(type_id: &str) -> Vec<NodeParamMetadata> {
    manifold_core::param_lookup::stamped_node_params(type_id)
}

/// Every param `node` declares as the graph loader builds it
/// (`configure_from_doc`), so a render scene's per-object params exist once
/// its object count does. `None` when the type is unknown, which the loader
/// refuses outright.
pub fn declared_params_for_node(node: &EffectGraphNode) -> Option<Vec<NodeParamMetadata>> {
    let mut built = NODE_REGISTRY.construct(&node.type_id)?;
    manifold_node_engine::load::graph_loader::configure_from_doc(built.as_mut(), node);
    Some(params_metadata(&node.type_id, built.parameters()))
}

fn params_metadata(type_id: &str, params: &[ParamDef]) -> Vec<NodeParamMetadata> {
    params
        .iter()
        .map(|pd| {
            let (min, max) = pd.range.unwrap_or({
                if matches!(pd.ty, ParamType::Angle) {
                    UNBOUNDED_ANGLE_RANGE
                } else {
                    (0.0, 1.0)
                }
            });
            let default_value: manifold_core::effect_graph_def::SerializedParamValue =
                pd.default.clone().into();
            let convert = match pd.ty {
                ParamType::Bool => manifold_core::effects::ParamConvert::BoolThreshold,
                ParamType::Int => manifold_core::effects::ParamConvert::IntRound,
                ParamType::Enum => manifold_core::effects::ParamConvert::EnumRound,
                ParamType::Trigger => manifold_core::effects::ParamConvert::Trigger,
                _ => manifold_core::effects::ParamConvert::Float,
            };
            NodeParamMetadata {
                name: pd.name.to_string(),
                label: pd.label.to_string(),
                min,
                max,
                default_value,
                is_angle: matches!(pd.ty, ParamType::Angle),
                // Periodicity is authored by the row, not inferred from every
                // angle (FOV and tilt are intentionally bounded).
                wraps: false,
                whole_numbers: matches!(pd.ty, ParamType::Int | ParamType::Enum),
                is_toggle: matches!(pd.ty, ParamType::Bool),
                is_trigger: matches!(pd.ty, ParamType::Trigger),
                // Display labels aren't Enum-exclusive: a modulatable `Float`
                // threshold (`node.light`'s `cast_shadows`) can declare
                // `enum_values` as display text, read whatever the type.
                value_labels: pd.enum_values.iter().map(|s| s.to_string()).collect(),
                convert,
                material_role: material_param_role(type_id, pd.name.as_ref()),
                scene: None,
                unit: None,
                limit: Default::default(),
            }
        })
        .collect()
}

/// The Physics World controls `type_id` declares it takes
/// (`EffectNode::world_control_inputs`).
pub fn world_inputs_for_node_type(
    type_id: &str,
) -> &'static [manifold_core::scene_world_controls::WorldControlInput] {
    NODE_REGISTRY
        .construct(type_id)
        .map_or(&[], |node| node.world_control_inputs())
}

inventory::submit! {
    manifold_core::param_lookup::NodeParamSource {
        params_for_type: all_params_for_node_type,
        world_inputs_for_type: world_inputs_for_node_type,
    }
}

/// Registration parity for the node param seam
/// (RENDERER_CRATE_SPLIT_DESIGN.md section 6 (Decided, do not reopen) item 14):
/// core's `NodeParamSource` answers exactly what this crate builds.
#[cfg(test)]
mod node_param_source_parity {
    #[test]
    fn core_node_param_source_matches_the_registry_for_every_type() {
        for type_id in super::NODE_REGISTRY.known_type_ids().chain(["missing"]) {
            assert_eq!(
                manifold_core::param_lookup::node_params(type_id),
                super::all_params_for_node_type(type_id),
                "{type_id}"
            );
        }
    }
}
