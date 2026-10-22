//! Qualifies a scene modifier recipe the way the Add menu inserts it: schema
//! check, prepare against a real imported scene, insert into the host, then
//! expand. `check-presets` and the bundled-preset validation test both call
//! this, so the CLI and the cheap test tier refuse the same recipes.

use std::path::Path;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, SerializedParamValue};
use manifold_core::scene_modifier_preset::{
    SceneTargetSelection, validate_scene_modifier_schema,
};
use manifold_node_engine::load::expand::prepare_scene_modifiers;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_nodes_scene::node_graph::gltf_import::compile_model_scene;
use manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_new_scene_modifier;

pub const MUSHROOM_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/cc0___mushroom.glb"
);

/// The scene every recipe is qualified against: the imported mushroom in
/// the base scene.
pub fn mushroom_host() -> Result<EffectGraphDef, String> {
    compile_model_scene(Path::new(MUSHROOM_FIXTURE))
        .map(|(def, _)| def)
        .map_err(|error| format!("mushroom validation fixture failed: {error}"))
}

/// Inserts `recipe` into `mushroom_host` (Shatter into a Physics host built
/// from it) and expands the result. `Err` carries the first refusal.
pub fn qualify_scene_modifier(
    recipe: &EffectGraphDef,
    label: &str,
    registry: &PrimitiveRegistry,
    mushroom_host: &EffectGraphDef,
) -> Result<(), String> {
    validate_scene_modifier_schema(recipe).map_err(|error| error.to_string())?;
    let is_shatter = recipe
        .preset_metadata
        .as_ref()
        .and_then(|metadata| metadata.scene_modifier.as_ref())
        .is_some_and(|modifier| modifier.shatter.is_some());
    let shatter_host;
    let (host, targets) = if is_shatter {
        shatter_host = prepare_shatter_host(mushroom_host)?;
        (
            &shatter_host,
            SceneTargetSelection::Explicit {
                objects: vec![NodeId::new("physics_demo_114")],
            },
        )
    } else {
        (mushroom_host, SceneTargetSelection::AllObjects)
    };
    let scene_node = host
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .ok_or_else(|| "mushroom fixture has no node.render_scene host".to_string())?;
    let instance = prepare_new_scene_modifier(
        host,
        recipe,
        format!("check:{label}").into(),
        scene_node.node_id.clone(),
        targets,
    )
    .map_err(|error| error.to_string())?;
    let candidate = manifold_nodes_scene::node_graph::scene_modifier_authoring::attach_scene_modifier(host, instance)?;
    prepare_scene_modifiers(&candidate, registry).map_err(|error| error.to_string())?;
    Ok(())
}

fn nested_gltf_mesh_source(nodes: &[EffectGraphNode]) -> Option<&EffectGraphNode> {
    nodes.iter().find_map(|node| {
        if node.type_id == "node.gltf_mesh_source" {
            Some(node)
        } else {
            node.group
                .as_deref()
                .and_then(|group| nested_gltf_mesh_source(&group.nodes))
        }
    })
}

// Shatter requires an imported mesh driven by Physics. Reuse the same body and
// target as the expansion contract fixture, with real mushroom mesh metadata.
fn prepare_shatter_host(mushroom_host: &EffectGraphDef) -> Result<EffectGraphDef, String> {
    let source = nested_gltf_mesh_source(&mushroom_host.nodes)
        .ok_or_else(|| "mushroom fixture has no nested node.gltf_mesh_source".to_string())?;
    let mut host: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/generator-presets/PhysicsSolids.json"
    )))
    .map_err(|error| format!("PhysicsSolids fixture failed to parse: {error}"))?;
    let mesh = host
        .nodes
        .iter_mut()
        .find(|node| node.id == 112 && node.type_id == "node.platonic_solid_mesh")
        .ok_or("PhysicsSolids fixture is missing mesh node 112")?;
    mesh.type_id = "node.gltf_mesh_source".into();
    mesh.params = source.params.clone();
    mesh.params.insert(
        "path".into(),
        SerializedParamValue::String {
            value: MUSHROOM_FIXTURE.into(),
        },
    );
    host.wires.retain(|wire| {
        !(wire.from_node == 111
            && wire.from_port == "shape"
            && wire.to_node == 112
            && wire.to_port == "shape")
    });
    let body = host
        .nodes
        .iter_mut()
        .find(|node| node.id == 111 && node.type_id == "node.rigid_body")
        .ok_or("PhysicsSolids fixture is missing body node 111")?;
    body.params.insert(
        "path".into(),
        SerializedParamValue::String {
            value: MUSHROOM_FIXTURE.into(),
        },
    );
    Ok(host)
}
