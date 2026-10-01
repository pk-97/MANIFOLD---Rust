//! Ordinary scalar sources shared by the World and its newly authored liquid.

use super::*;
use manifold_core::effect_graph_def::{BindingTarget, GROUP_INPUT_TYPE_ID};
use manifold_core::liquid_domain::{NestedLiquidDomain, liquid_dial_params};
use manifold_core::scene_exposure::stamp_scene_node_exposures;

const CONTROLS: [(&str, &str); 5] = [
    ("gravity_x", "gravity_x"),
    ("gravity_y", "gravity"),
    ("gravity_z", "gravity_z"),
    ("speed", "speed"),
    ("reset", "reset"),
];

pub(super) fn is_shared_fluid_control(name: &str) -> bool {
    CONTROLS.iter().any(|(_, fluid)| *fluid == name)
}

/// The body of the group `path` ends at, entering each group in turn.
fn group_body_mut<'a>(
    nodes: &'a mut [EffectGraphNode],
    path: &[u32],
) -> Result<&'a mut GroupDef, &'static str> {
    let (first, rest) = path.split_first().ok_or("Add Fluid control group is unavailable")?;
    let body = nodes
        .iter_mut()
        .find(|node| node.id == *first && node.type_id == GROUP_TYPE_ID)
        .and_then(|node| node.group.as_deref_mut())
        .ok_or("Add Fluid control group is unavailable")?;
    if rest.is_empty() {
        Ok(body)
    } else {
        group_body_mut(&mut body.nodes, rest)
    }
}

fn next_id(def: &EffectGraphDef) -> Result<u32, &'static str> {
    max_node_id_over(&def.nodes)
        .checked_add(1)
        .ok_or("Add Fluid document id space is exhausted")
}

pub(super) fn share_world_controls(
    def: &mut EffectGraphDef,
    group_id: u32,
    domain: &NestedLiquidDomain,
    metadata: &[SceneParamMetadata],
) -> Result<(), &'static str> {
    if CONTROLS
        .iter()
        .any(|(world, _)| !metadata.iter().any(|m| m.name == *world))
    {
        return Err("Add Fluid requires complete Physics World control metadata");
    }
    let worlds: Vec<_> = def
        .nodes
        .iter()
        .filter(|node| node.type_id == "node.physics_world")
        .map(|node| node.id)
        .collect();
    if worlds.len() > 1 {
        return Err("Add Fluid requires one shared root Physics World");
    }
    let world_id = if let Some(id) = worlds.first() {
        *id
    } else {
        let id = next_id(def)?;
        let mut handles = HashSet::new();
        collect_all_handles(&def.nodes, &mut handles);
        let handle = dedup_handle("Physics World", &mut handles);
        let params = metadata
            .iter()
            .filter(|m| CONTROLS.iter().any(|(world, _)| *world == m.name))
            .map(|m| (m.name.clone(), m.default_value.clone()))
            .collect();
        def.nodes.push(scene_build_node(
            id,
            "node.physics_world",
            Some(handle),
            params,
        ));
        id
    };
    let unwired_metadata: Vec<_> = metadata
        .iter()
        .filter(|m| {
            !def.wires
                .iter()
                .any(|wire| wire.to_node == world_id && wire.to_port == m.name)
        })
        .cloned()
        .collect();
    stamp_scene_node_exposures(def, world_id, "Physics World", &unwired_metadata);
    let world = def.nodes.iter().find(|node| node.id == world_id).unwrap();
    let world_node_id = world.node_id.clone();
    let world_params = world.params.clone();

    // The fluid group, then each group nested on the way to the domain.
    let path: Vec<u32> = std::iter::once(group_id)
        .chain(domain.groups.iter().copied())
        .collect();
    let mut inputs = Vec::with_capacity(path.len());
    for depth in 0..path.len() {
        let id = next_id(def)?;
        let body = group_body_mut(&mut def.nodes, &path[..=depth])?;
        let sentinels: Vec<u32> = body
            .nodes
            .iter()
            .filter(|node| node.type_id == GROUP_INPUT_TYPE_ID)
            .map(|node| node.id)
            .collect();
        inputs.push(match sentinels.as_slice() {
            [] => {
                body.nodes
                    .push(scene_build_node(id, GROUP_INPUT_TYPE_ID, None, BTreeMap::new()));
                id
            }
            [existing] => *existing,
            _ => return Err("Add Fluid control group has more than one input sentinel"),
        });
    }
    let domain_type = group_body_mut(&mut def.nodes, &path)?
        .nodes
        .iter()
        .find(|node| node.id == domain.node)
        .map(|node| node.type_id.clone())
        .ok_or("Add Fluid simulation node is unavailable")?;
    let dials = liquid_dial_params(&domain_type).unwrap_or_default();
    if CONTROLS.iter().any(|(_, fluid)| !dials.contains(fluid)) {
        return Err("Add Fluid liquid domain lacks the shared World controls");
    }

    for (world_param, fluid_param) in CONTROLS {
        let incoming: Vec<_> = def
            .wires
            .iter()
            .filter(|wire| wire.to_node == world_id && wire.to_port == world_param)
            .cloned()
            .collect();
        if incoming.len() > 1 {
            return Err("Add Fluid found multiple wires driving one World control");
        }
        let (source_id, source_port) = if let Some(wire) = incoming.first() {
            // Preserve the actual authored signal, including graph modulation.
            (wire.from_node, wire.from_port.clone())
        } else {
            let id = next_id(def)?;
            let metadata = metadata.iter().find(|m| m.name == world_param).unwrap();
            let value = world_params
                .get(world_param)
                .unwrap_or(&metadata.default_value)
                .clone();
            let mut handles = HashSet::new();
            collect_all_handles(&def.nodes, &mut handles);
            let handle = dedup_handle(&format!("World {}", metadata.label), &mut handles);
            let mut node = scene_build_node(
                id,
                "node.value",
                Some(handle),
                BTreeMap::from([("value".into(), value)]),
            );
            if def
                .nodes
                .iter_mut()
                .find(|node| node.id == world_id)
                .unwrap()
                .exposed_params
                .remove(world_param)
            {
                node.exposed_params.insert("value".into());
            }
            let source_node_id = node.node_id.clone();
            def.nodes.push(node);
            def.wires
                .push(scene_build_wire(id, "out", world_id, world_param));
            for binding in &mut def.preset_metadata.as_mut().unwrap().bindings {
                if matches!(&binding.target, BindingTarget::Node { node_id, param }
                    if node_id == &world_node_id && param == world_param)
                {
                    // Preserve ID, calibration, conversion and modulation routing;
                    // the ordinary value source becomes the authored parameter.
                    binding.target = BindingTarget::Node {
                        node_id: source_node_id.clone(),
                        param: "value".into(),
                    };
                }
            }
            (id, "out".to_owned())
        };

        for depth in 0..path.len() {
            let body = group_body_mut(&mut def.nodes, &path[..=depth])?;
            if body.interface.inputs.iter().any(|port| port.name == world_param) {
                return Err("Add Fluid control group already has a World control input");
            }
            body.interface.inputs.push(InterfacePortDef {
                name: world_param.into(),
                port_type: "Scalar(F32)".into(),
            });
            let (to_node, to_port) = match path.get(depth + 1) {
                Some(inner) => (*inner, world_param),
                None => (domain.node, fluid_param),
            };
            if body
                .wires
                .iter()
                .any(|wire| wire.to_node == to_node && wire.to_port == to_port)
            {
                return Err("Add Fluid template already drives a shared World control");
            }
            body.wires
                .push(scene_build_wire(inputs[depth], world_param, to_node, to_port));
        }
        def.wires.push(scene_build_wire(
            source_id,
            &source_port,
            group_id,
            world_param,
        ));
    }
    Ok(())
}
