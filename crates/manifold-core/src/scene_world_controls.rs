//! Ordinary scalar sources shared by the Physics World and a liquid a
//! template inserts: the World's gravity, speed and reset reach the liquid
//! domain through every group boundary on the way.

use std::collections::{BTreeMap, HashSet};

use crate::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef,
    InterfacePortDef,
};
use crate::liquid_domain::NestedLiquidDomain;
use crate::param_lookup::stamped_node_params;
use crate::scene_exposure::stamp_scene_node_exposures;

const WORLD_TYPE_ID: &str = "node.physics_world";

const CONTROLS: [(&str, &str); 5] = [
    ("gravity_x", "gravity_x"),
    ("gravity_y", "gravity"),
    ("gravity_z", "gravity_z"),
    ("speed", "speed"),
    ("reset", "reset"),
];

fn build_node(
    id: u32,
    type_id: &str,
    handle: Option<String>,
    params: BTreeMap<String, crate::effect_graph_def::SerializedParamValue>,
) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: crate::NodeId::new(crate::short_id()),
        type_id: type_id.to_owned(),
        handle,
        params,
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

fn build_wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire { from_node, from_port: from_port.into(), to_node, to_port: to_port.into() }
}

fn max_id(nodes: &[EffectGraphNode]) -> u32 {
    nodes.iter().map(|n| n.id.max(n.group.as_deref().map_or(0, |g| max_id(&g.nodes)))).max().unwrap_or(0)
}

fn handles(nodes: &[EffectGraphNode], out: &mut HashSet<String>) {
    for n in nodes {
        if let Some(h) = &n.handle {
            out.insert(h.clone());
        }
        if let Some(body) = n.group.as_deref() {
            handles(&body.nodes, out);
        }
    }
}

fn unique_handle(base: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(base) {
        return base.to_owned();
    }
    (2u32..).map(|i| format!("{base}_{i}")).find(|c| !taken.contains(c)).expect("unbounded")
}

/// The body of the group `path` ends at, entering each group in turn.
fn group_body_mut<'a>(nodes: &'a mut [EffectGraphNode], path: &[u32]) -> Result<&'a mut GroupDef, &'static str> {
    let (first, rest) = path.split_first().ok_or("Scene control group is unavailable")?;
    let body = nodes
        .iter_mut()
        .find(|node| node.id == *first && node.type_id == GROUP_TYPE_ID)
        .and_then(|node| node.group.as_deref_mut())
        .ok_or("Scene control group is unavailable")?;
    if rest.is_empty() { Ok(body) } else { group_body_mut(&mut body.nodes, rest) }
}

fn next_id(def: &EffectGraphDef) -> Result<u32, &'static str> {
    max_id(&def.nodes).checked_add(1).ok_or("Scene document id space is exhausted")
}

/// Wire the World's gravity, speed and reset to the one liquid `domain`
/// inside the root-level group `group_id`, creating the World when the scene
/// has none. A graph where a node type's controls cannot be found is left
/// untouched and refused.
pub(crate) fn share_world_controls(
    def: &mut EffectGraphDef,
    group_id: u32,
    domain: &NestedLiquidDomain,
) -> Result<(), &'static str> {
    let metadata = stamped_node_params(WORLD_TYPE_ID);
    if CONTROLS.iter().any(|(world, _)| !metadata.iter().any(|m| m.name == *world)) {
        return Err("A liquid needs complete Physics World control metadata");
    }
    let worlds: Vec<_> = def.nodes.iter().filter(|node| node.type_id == WORLD_TYPE_ID).map(|node| node.id).collect();
    if worlds.len() > 1 {
        return Err("A liquid needs one shared root Physics World");
    }
    let world_id = if let Some(id) = worlds.first() {
        *id
    } else {
        let id = next_id(def)?;
        let mut taken = HashSet::new();
        handles(&def.nodes, &mut taken);
        let handle = unique_handle("Physics World", &taken);
        let params = metadata
            .iter()
            .filter(|m| CONTROLS.iter().any(|(world, _)| *world == m.name))
            .map(|m| (m.name.clone(), m.default_value.clone()))
            .collect();
        def.nodes.push(build_node(id, WORLD_TYPE_ID, Some(handle), params));
        id
    };
    let unwired_metadata: Vec<_> = metadata
        .iter()
        .filter(|m| !def.wires.iter().any(|wire| wire.to_node == world_id && wire.to_port == m.name))
        .cloned()
        .collect();
    stamp_scene_node_exposures(def, world_id, "Physics World", &unwired_metadata);
    let world = def.nodes.iter().find(|node| node.id == world_id).ok_or("Physics World is unavailable")?;
    let world_node_id = world.node_id.clone();
    let world_params = world.params.clone();

    // The group, then each group nested on the way to the domain.
    let path: Vec<u32> = std::iter::once(group_id).chain(domain.groups.iter().copied()).collect();
    let mut inputs = Vec::with_capacity(path.len());
    for depth in 0..path.len() {
        let id = next_id(def)?;
        let body = group_body_mut(&mut def.nodes, &path[..=depth])?;
        let sentinels: Vec<u32> =
            body.nodes.iter().filter(|node| node.type_id == GROUP_INPUT_TYPE_ID).map(|node| node.id).collect();
        inputs.push(match sentinels.as_slice() {
            [] => {
                body.nodes.push(build_node(id, GROUP_INPUT_TYPE_ID, None, BTreeMap::new()));
                id
            }
            [existing] => *existing,
            _ => return Err("A control group has more than one input sentinel"),
        });
    }
    if !group_body_mut(&mut def.nodes, &path)?.nodes.iter().any(|node| node.id == domain.node) {
        return Err("The liquid domain node is unavailable");
    }

    for (world_param, fluid_param) in CONTROLS {
        let incoming: Vec<_> =
            def.wires.iter().filter(|wire| wire.to_node == world_id && wire.to_port == world_param).cloned().collect();
        if incoming.len() > 1 {
            return Err("Found multiple wires driving one World control");
        }
        let (source_id, source_port) = if let Some(wire) = incoming.first() {
            // Keep the authored signal, including graph modulation.
            (wire.from_node, wire.from_port.clone())
        } else {
            let id = next_id(def)?;
            let metadata = metadata.iter().find(|m| m.name == world_param).ok_or("World control is unavailable")?;
            let value = world_params.get(world_param).unwrap_or(&metadata.default_value).clone();
            let mut taken = HashSet::new();
            handles(&def.nodes, &mut taken);
            let handle = unique_handle(&format!("World {}", metadata.label), &taken);
            let mut node = build_node(id, "node.value", Some(handle), BTreeMap::from([("value".into(), value)]));
            if def
                .nodes
                .iter_mut()
                .find(|node| node.id == world_id)
                .ok_or("Physics World is unavailable")?
                .exposed_params
                .remove(world_param)
            {
                node.exposed_params.insert("value".into());
            }
            let source_node_id = node.node_id.clone();
            def.nodes.push(node);
            def.wires.push(build_wire(id, "out", world_id, world_param));
            for binding in &mut def.preset_metadata.as_mut().ok_or("Scene has no card")?.bindings {
                if matches!(&binding.target, BindingTarget::Node { node_id, param }
                    if node_id == &world_node_id && param == world_param)
                {
                    // Keep ID, calibration, conversion and modulation routing;
                    // the ordinary value source becomes the authored parameter.
                    binding.target = BindingTarget::Node { node_id: source_node_id.clone(), param: "value".into() };
                }
            }
            (id, "out".to_owned())
        };

        for depth in 0..path.len() {
            let body = group_body_mut(&mut def.nodes, &path[..=depth])?;
            if body.interface.inputs.iter().any(|port| port.name == world_param) {
                return Err("A control group already has a World control input");
            }
            body.interface.inputs.push(InterfacePortDef { name: world_param.into(), port_type: "Scalar(F32)".into() });
            let (to_node, to_port) = match path.get(depth + 1) {
                Some(inner) => (*inner, world_param),
                None => (domain.node, fluid_param),
            };
            if body.wires.iter().any(|wire| wire.to_node == to_node && wire.to_port == to_port) {
                return Err("The template already drives a shared World control");
            }
            body.wires.push(build_wire(inputs[depth], world_param, to_node, to_port));
        }
        def.wires.push(build_wire(source_id, &source_port, group_id, world_param));
    }
    Ok(())
}
