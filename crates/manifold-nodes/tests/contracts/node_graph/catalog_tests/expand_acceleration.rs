use manifold_core::NodeId;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::liquid_domain_of;
use manifold_core::scene_impulse::ImpulseTarget;
use manifold_core::scene_index::FlatSceneIndex;
use manifold_core::scene_modifier_preset::SceneTargetSelection;
use manifold_node_engine::load::expand::testkit::{impulse_recipients_with_index, recipient_key};
use manifold_node_engine::persistence::PrimitiveRegistry;

fn preset(json: &str) -> EffectGraphDef {
    serde_json::from_str(json).expect("preset parses")
}

fn top(node: &str) -> NodeId {
    NodeId::new(node)
}

/// A matter domain is found by the same walk, through its group, and
/// takes scene forces and impulses on the same port FLIP does.
#[test]
fn matter_surface_water_resolves_to_its_domain_force_port() {
    let def = preset(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/presets/SandColumn.json"
    )));
    let registry = PrimitiveRegistry::with_builtin();
    let index = FlatSceneIndex::build(&def).unwrap();
    let water = top("water_object");
    let domain = liquid_domain_of(&index, &water)
        .unwrap()
        .expect("matter water has a domain");
    assert_eq!(domain, NodeId::new("matter_domain"));
    assert_eq!(
        recipient_key(&index, &water, &registry).unwrap(),
        Some((domain, "acceleration_field".to_string()))
    );
    let recipients = impulse_recipients_with_index(
        &index,
        &top("scene"),
        &SceneTargetSelection::Explicit {
            objects: vec![water],
        },
        &registry,
    )
    .unwrap();
    assert_eq!(
        recipients,
        vec![(NodeId::new("matter_domain"), ImpulseTarget::Fluid)]
    );
}

/// A compiled swarm's object takes scene forces on its forces node's
/// port, so a Force item reaches the particles, not the drawn mesh.
#[test]
fn a_compiled_swarm_takes_forces_on_its_forces_node() {
    use manifold_core::scene_doc::{Item, ItemFields, ItemId, ItemKind, SceneDoc};
    let mut doc = SceneDoc::base();
    let at = doc.items.len();
    doc.insert(
        None,
        at,
        Item::new(
            ItemId::new("swarm"),
            "swarm",
            ItemFields::default_for(ItemKind::Swarm),
        ),
    )
    .unwrap();
    let mut layer = manifold_core::layer::Layer::new_generator(
        "Scene".into(),
        manifold_core::PresetTypeId::from_string("Scene".into()),
        0,
    );
    layer
        .gen_params_mut()
        .unwrap()
        .install_scene_document(doc)
        .unwrap();
    let def = layer.gen_params().unwrap().graph.clone().unwrap();
    let registry = PrimitiveRegistry::with_builtin();
    let index = FlatSceneIndex::build(&def).unwrap();
    assert_eq!(
        recipient_key(&index, &top("swarm:object"), &registry).unwrap(),
        Some((top("swarm:forces"), "acceleration_field".to_string()))
    );
}

/// A Force aimed at one swarm reaches that swarm's particles and no other
/// swarm's: a new Swarm's own forces stay its own.
#[test]
fn a_force_aimed_at_a_swarm_reaches_that_swarm_only() {
    use manifold_core::scene_doc::{
        ForceFields, ForceTargets, ForceType, Item, ItemFields, ItemId, ItemKind, SceneDoc,
    };
    let mut doc = SceneDoc::base();
    for id in ["swarm", "other"] {
        let at = doc.items.len();
        doc.insert(
            None,
            at,
            Item::new(
                ItemId::new(id),
                id,
                ItemFields::default_for(ItemKind::Swarm),
            ),
        )
        .unwrap();
    }
    let force = ItemFields::Force(ForceFields {
        force: ForceType::Vortex,
        targets: ForceTargets::Items(vec![ItemId::new("swarm")]),
    });
    let at = doc.items.len();
    doc.insert(None, at, Item::new(ItemId::new("spin"), "spin", force))
        .unwrap();
    let mut layer = manifold_core::layer::Layer::new_generator(
        "Scene".into(),
        manifold_core::PresetTypeId::from_string("Scene".into()),
        0,
    );
    layer
        .gen_params_mut()
        .unwrap()
        .install_scene_document(doc)
        .unwrap();
    let def = layer.gen_params().unwrap().graph.clone().unwrap();
    let spin = def
        .scene_modifiers
        .iter()
        .find(|instance| instance.id == top("spin:force"))
        .expect("the force compiles");
    let SceneTargetSelection::Explicit { objects } = &spin.targets else {
        panic!("an aimed force names its objects")
    };
    assert_eq!(objects, &vec![top("swarm:object")]);
    let registry = PrimitiveRegistry::with_builtin();
    let index = FlatSceneIndex::build(&def).unwrap();
    assert_eq!(
        recipient_key(&index, &objects[0], &registry).unwrap(),
        Some((top("swarm:forces"), "acceleration_field".to_string()))
    );
}
