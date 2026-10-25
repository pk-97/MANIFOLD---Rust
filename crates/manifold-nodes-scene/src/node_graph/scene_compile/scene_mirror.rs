//! Scene Mirror:
//! the whole scene reflected across a plane. Each drawn object's instances
//! pass through `node.reflect_array`, so every object gets its mirrored copy
//! from the stock `SceneMirror` recipe in its place in the chain: after the
//! Scene Loop it mirrors the whole corridor, after Spatial Echoes every echo.
//! The plane starts at the scene's floor, the bottom of its rest-pose box

use manifold_core::NodeId;
use manifold_core::scene_doc::{Item, ModifierFields, param_id};
use manifold_core::scene_modifier_preset::{SceneModifierInstanceDef, SceneTargetSelection};
use manifold_core::scene_schema::modifier_type;

use super::builder::{Builder, SCENE, rest_box};
use super::forces::{bindings, node, recipe_card};

/// The most objects one instance targets (`frames::selected_objects`).
const TARGETS_PER_INSTANCE: usize = 256;

/// Plane Offset's reach past the scene's box, in box sizes either way.
const OFFSET_REACH: f32 = 2.0;

/// Emit one Scene Mirror card at its place in the chain.
pub(crate) fn emit(b: &mut Builder<'_>, item: &Item, card: &ModifierFields) -> Result<(), String> {
    let entry = modifier_type(card.modifier);
    let lookup = |param: &str| entry.rows.iter().find(|row| row.property == param);
    let host_id = |param: &str| param_id(&item.id, param);
    let bounds = rest_box(b.drawn.iter().map(|drawn| &drawn.rest));
    let floor = bounds.map_or(0.0, |(lo, _)| lo[1]);
    let (graph, mut specs) =
        recipe_card(entry.recipe, &lookup, &host_id, &[("plane_offset", floor)])?;
    if let Some((lo, hi)) = bounds {
        let size = (0..3)
            .map(|axis| hi[axis] - lo[axis])
            .fold(1.0f32, f32::max);
        let low = lo.iter().copied().fold(f32::INFINITY, f32::min) - OFFSET_REACH * size;
        let high = hi.iter().copied().fold(f32::NEG_INFINITY, f32::max) + OFFSET_REACH * size;
        for (spec, param) in &mut specs {
            if param == "plane_offset" {
                (spec.min, spec.max) = (low, high);
            }
        }
    }
    // The card keeps its params with nothing to act on, so its values,
    // mappings and modulation survive a toggle.
    b.push_params(specs.iter().map(|(spec, _)| spec.clone()));
    let objects = if card.on {
        mirrored_objects(b)
    } else {
        Vec::new()
    };
    for (chunk, part) in objects.chunks(TARGETS_PER_INSTANCE).enumerate() {
        let suffix = if chunk == 0 {
            "mirror".to_owned()
        } else {
            format!("mirror:{chunk}")
        };
        let id = node(item.id.as_str(), &suffix);
        let instance = SceneModifierInstanceDef {
            id: id.clone(),
            scene: node(SCENE, "render"),
            targets: SceneTargetSelection::Explicit {
                objects: part.to_vec(),
            },
            mesh_frames: Vec::new(),
            legacy_math_view_carrier: None,
            graph: Box::new(graph.clone()),
        };
        b.push_modifier(instance, Vec::new(), bindings(&specs, &id), None);
    }
    Ok(())
}

/// Every scene object the render draws.
fn mirrored_objects(b: &mut Builder<'_>) -> Vec<NodeId> {
    let objects: Vec<u32> = b
        .objects
        .iter()
        .copied()
        .filter(|&object| b.node_type(object) == Some("node.scene_object"))
        .collect();
    for &object in &objects {
        if b.input(object, "instances").is_none() {
            b.set(
                object,
                "instances_space",
                manifold_core::effect_graph_def::SerializedParamValue::Enum { value: 2 },
            );
        }
    }
    objects
        .into_iter()
        .filter_map(|object| b.node_id_of(object))
        .collect()
}
