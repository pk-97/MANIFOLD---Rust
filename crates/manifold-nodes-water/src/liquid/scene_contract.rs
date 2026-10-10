//! Invariant I3 (`docs/LIQUID_SOLVER_SEAM_DESIGN.md`): every liquid domain
//! exposes what the scene uses, under FLIP's names. A gap is allowed only
//! when `LIQUID_SCENE_OWED` names the phase that closes it; the list only
//! shrinks, so an owed item that is already met fails too.

use manifold_core::liquid_domain::{LIQUID_DOMAIN_TYPE_IDS, liquid_dial_params};
use manifold_physics::input::EventStamp;
use manifold_physics::{FieldValue, Seconds};

use manifold_node_engine::exec::effect_node::EffectNode;
use manifold_core::fluid_domain::MAX_FLUID_ROLES;
use manifold_node_engine::parameters::ParamType;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_core::scene_impulse::ImpulseTarget;
use crate::physics_events::ResolvedNodeImpulse;
use manifold_node_engine::ports::{PortKind, PortType};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SceneItem {
    RolePorts,
    AccelerationField,
    Speed,
    Reset,
    DialRow,
    FluidImpulses,
}

const SCENE_ITEMS: [SceneItem; 6] = [
    SceneItem::RolePorts,
    SceneItem::AccelerationField,
    SceneItem::Speed,
    SceneItem::Reset,
    SceneItem::DialRow,
    SceneItem::FluidImpulses,
];

/// (domain type id, missing item, the phase that closes it).
const LIQUID_SCENE_OWED: &[(&str, SceneItem, &str)] = &[];

fn has_input(node: &dyn EffectNode, name: &str, ty: PortType) -> bool {
    node.inputs().iter().any(|port| port.kind == PortKind::Input && port.name == name && port.ty == ty)
}

fn has_param(node: &dyn EffectNode, name: &str, ty: ParamType) -> bool {
    node.parameters().iter().any(|param| param.name == name && param.ty == ty)
}

/// A fresh domain that does not route liquid impulses has no native interface
/// or answers with its default ("does not accept"). Any other answer, including "the
/// clock has not started", means the hook routes them.
fn routes_fluid_impulses(node: &mut dyn EffectNode) -> bool {
    let Some(node) = crate::node::get_mut(node) else { return false; };
    let stamp = EventStamp { epoch: 0, time: Seconds(0.0), sequence: 0 };
    let impulse = ResolvedNodeImpulse {
        field: FieldValue::uniform([1.0, 0.0, 0.0]).expect("uniform field"),
        target: ImpulseTarget::Fluid,
    };
    match node.enqueue_physics_impulse(stamp, impulse) {
        Ok(_) => true,
        Err(error) => !error.contains("does not accept physics impulses"),
    }
}

fn is_met(type_id: &str, node: &mut dyn EffectNode, item: SceneItem) -> bool {
    match item {
        SceneItem::RolePorts => {
            (0..MAX_FLUID_ROLES).all(|index| has_input(node, &format!("role_{index}"), PortType::FluidRole))
        }
        SceneItem::AccelerationField => has_input(node, "acceleration_field", PortType::VectorField),
        SceneItem::Speed => has_param(node, "speed", ParamType::Float),
        SceneItem::Reset => has_param(node, "reset", ParamType::Trigger),
        SceneItem::DialRow => liquid_dial_params(type_id).is_some_and(|dials| {
            dials.iter().all(|dial| node.parameters().iter().any(|param| param.name == *dial))
        }),
        SceneItem::FluidImpulses => routes_fluid_impulses(node),
    }
}

#[test]
fn liquid_domain_scene_contract() {
    let registry = PrimitiveRegistry::with_builtin();
    let mut failures = Vec::new();
    for &type_id in LIQUID_DOMAIN_TYPE_IDS {
        // A retired domain type stays a liquid domain for saved graphs, but
        // has no constructor and so no scene contract.
        if !registry.contains(type_id) {
            continue;
        }
        let mut node = registry.construct(type_id).unwrap_or_else(|| panic!("{type_id} is not registered"));
        for item in SCENE_ITEMS {
            let owed = LIQUID_SCENE_OWED.iter().find(|(owner, owed, _)| *owner == type_id && *owed == item);
            match (is_met(type_id, node.as_mut(), item), owed) {
                (true, Some((_, _, phase))) => {
                    failures.push(format!("{type_id} meets {item:?}; delete its {phase} entry from LIQUID_SCENE_OWED"))
                }
                (false, None) => failures.push(format!("{type_id} misses {item:?} and no phase owes it")),
                _ => {}
            }
        }
    }
    for (owner, item, phase) in LIQUID_SCENE_OWED {
        assert!(LIQUID_DOMAIN_TYPE_IDS.contains(owner) && registry.contains(owner), "{owner} owes {item:?} to {phase} but is not a product liquid domain");
    }
    assert!(failures.is_empty(), "liquid scene contract:\n{}", failures.join("\n"));
}
