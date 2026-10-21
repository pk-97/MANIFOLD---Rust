//! BUG-ju9cb (the water column can't be removed): a saved show's column
//! loads as an Initial Fill over the same box, and the water's domain is
//! handed exactly the box the column gave it.

use std::sync::Mutex;

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, SerializedParamValue};
use manifold_core::initial_fill::{FLUID_ROLE_SOURCE_TYPE_ID, INITIAL_VOLUME_PORT, carries_initial_volume, migrate_initial_volume};
use manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
use manifold_node_engine::exec::execution_plan::compile;
use manifold_node_engine::exec::extent::{AtomExtent, EXTENT_RULES, ExtentRule, Verdict, check_graph};
use manifold_node_engine::graph::Graph;
use manifold_node_engine::load::graph_loader::{BoundaryHandling, HandleScope, instantiate_def};
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::scene::mesh_change::PreparedMeshRules;
use manifold_node_engine::scene::transform::Transform;
use manifold_water_liquid::extent::walked_roles;

/// Peter's saved water layer, from before the column was an object.
const SAVED_LAYER: &str = include_str!("../../../../../manifold-io/tests/fixtures/water_layer_graph_v1160.json");

/// The fill each GPU FLIP domain's extent walk was handed.
static WALKED: Mutex<Vec<Option<Transform>>> = Mutex::new(Vec::new());

fn probe(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    let (fill, _) = walked_roles(x, "GPU FLIP")?;
    WALKED.lock().unwrap().push(fill);
    let real = EXTENT_RULES.iter().find(|rule| rule.type_id == GPU_FLIP_DOMAIN_TYPE_ID).expect("the domain's rule");
    (real.check)(x)
}

/// The saved layer after the project loader's versioned migrations, which
/// run before any graph migration.
fn saved_layer() -> EffectGraphDef {
    let graph: serde_json::Value = serde_json::from_str(SAVED_LAYER).expect("the saved layer");
    let project = serde_json::json!({"projectVersion": "1.16.0", "timeline": {"layers": [{"genParams": {"params": null, "graph": graph}}]}});
    let migrated = manifold_io::migrate::migrate_if_needed(&project.to_string()).expect("the project migrates");
    let mut project: serde_json::Value = serde_json::from_str(&migrated).expect("migrated project");
    serde_json::from_value(project["timeline"]["layers"][0]["genParams"]["graph"].take()).expect("the layer's graph")
}

/// A `node.transform_3d`'s authored box, as its params set it.
fn authored(params: &std::collections::BTreeMap<String, SerializedParamValue>) -> Transform {
    let read = |name: &str, default: f32| match params.get(name) {
        Some(SerializedParamValue::Float { value }) => *value,
        _ => default,
    };
    Transform {
        pos: ["pos_x", "pos_y", "pos_z"].map(|name| read(name, 0.0)),
        scale: ["scale_x", "scale_y", "scale_z"].map(|name| read(name, 1.0)),
        ..Transform::default()
    }
}

#[test]
fn a_saved_column_loads_as_an_initial_fill_over_the_same_box() {
    let saved = saved_layer();
    let wire = saved.wires.iter().find(|wire| wire.to_port == INITIAL_VOLUME_PORT).expect("the saved layer wires a column");
    let column = saved.nodes.iter().find(|node| node.id == wire.from_node).expect("the column").clone();
    let domain = saved.nodes.iter().find(|node| node.id == wire.to_node).expect("the domain").node_id.clone();
    assert!(["rot_x", "rot_y", "rot_z"].iter().all(|name| !column.params.contains_key(*name)), "the column is axis-aligned");

    let mut loaded = saved.clone();
    assert!(migrate_initial_volume(&mut loaded));
    assert!(!carries_initial_volume(&loaded));
    let once = loaded.clone();
    assert!(!migrate_initial_volume(&mut loaded), "a second load changes nothing");
    assert_eq!(loaded, once);

    // The column node moved into the fill whole: its stable id and params,
    // so every card bound to it still drives it.
    let water = EffectGraphDef { scene_modifiers: Vec::new(), ..loaded.clone() };
    let flat = manifold_core::flatten::flatten_groups(&water).expect("flattens");
    let moved = flat.nodes.iter().find(|node| node.params == column.params && node.type_id == column.type_id).expect("the column survives");
    let domain = flat.nodes.iter().find(|node| node.node_id == domain).expect("the domain survives").id;
    let role = flat.wires.iter().find(|wire| wire.from_node == moved.id && wire.to_port == "transform").expect("the column places the fill").to_node;
    let role = flat.nodes.iter().find(|node| node.id == role).unwrap();
    assert_eq!(role.type_id, FLUID_ROLE_SOURCE_TYPE_ID);
    assert_eq!(role.params["role"], SerializedParamValue::Enum { value: 0 });
    assert!(flat.wires.iter().any(|wire| wire.from_node == role.id && wire.to_node == domain && wire.to_port.starts_with("role_")));
    let bound = |def: &EffectGraphDef| {
        def.preset_metadata.as_ref().map_or(0, |metadata| {
            metadata.bindings.iter().filter(|binding| matches!(&binding.target,
                BindingTarget::Node { node_id, .. } if *node_id == column.node_id)).count()
        })
    };
    assert_eq!(bound(&loaded), bound(&saved), "the column's cards still bind it");

    // The saved layer, loaded the way the app loads it, hands the domain's
    // walk the column's own box. No force touches the fill, so the forces
    // stay out of the walk.
    let water = EffectGraphDef { scene_modifiers: Vec::new(), ..saved.clone() };
    let mut graph = Graph::new();
    instantiate_def(&mut graph, &water, &PrimitiveRegistry::with_builtin(), HandleScope::Global, BoundaryHandling::Standalone, &PreparedMeshRules::default())
        .expect("the saved layer loads");
    let plan = compile(&graph).expect("the saved layer compiles");
    let rules: Vec<_> = EXTENT_RULES
        .iter()
        .map(|rule| if rule.type_id == GPU_FLIP_DOMAIN_TYPE_ID { ExtentRule { type_id: rule.type_id, check: probe } } else { *rule })
        .collect();
    WALKED.lock().unwrap().clear();
    check_graph(&mut graph, &plan, &rules).expect("the saved layer walks");
    let walked = WALKED.lock().unwrap().clone();
    assert!(!walked.is_empty());
    assert!(walked.iter().all(|fill| *fill == Some(authored(&column.params))), "{walked:?}");
}
