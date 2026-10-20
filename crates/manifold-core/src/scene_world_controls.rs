//! Ordinary scalar sources shared by the Physics World and a simulation a
//! template inserts: the World controls a node declares it takes reach it
//! through every group boundary on the way. A liquid and a swarm take the
//! World's gravity and clock.

use std::collections::{BTreeMap, HashSet};

use crate::NodeId;
use crate::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef,
    InterfacePortDef,
};
use crate::param_lookup::stamped_node_params;
use crate::scene_exposure::stamp_scene_node_exposures;

const WORLD_TYPE_ID: &str = "node.physics_world";

/// One Physics World control a node takes, and the node's own input that
/// takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldControlInput {
    /// The World's param.
    pub world: &'static str,
    /// The taking node's input port.
    pub input: &'static str,
}

const fn take(world: &'static str, input: &'static str) -> WorldControlInput {
    WorldControlInput { world, input }
}

/// Every Physics World control a simulation takes (a liquid, a swarm): its
/// gravity, and its clock, so the node follows World Speed and restarts on
/// World Reset, which clip triggers fire.
pub const WORLD_INPUTS: [WorldControlInput; 5] = [
    take("gravity_x", "gravity_x"),
    take("gravity_y", "gravity"),
    take("gravity_z", "gravity_z"),
    take("speed", "speed"),
    take("reset", "reset"),
];

/// A node under some graph level that takes World controls: the doc ids of
/// the groups entered to reach it, outermost first, its own doc id, and what
/// it takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorldControlTaker {
    pub groups: Vec<u32>,
    pub node: u32,
    pub type_id: String,
    pub inputs: &'static [WorldControlInput],
}

/// Every node in `nodes`, and in the groups nested under them, whose type
/// declares World controls it takes. A liquid domain always takes them, so
/// one declaring less is refused on insert rather than left unwired.
pub(crate) fn world_control_takers(nodes: &[EffectGraphNode]) -> Vec<WorldControlTaker> {
    fn walk(nodes: &[EffectGraphNode], groups: &mut Vec<u32>, out: &mut Vec<WorldControlTaker>) {
        for node in nodes {
            let inputs = crate::param_lookup::world_control_inputs(&node.type_id);
            if !inputs.is_empty() || crate::liquid_domain::is_liquid_domain(&node.type_id) {
                out.push(WorldControlTaker { groups: groups.clone(), node: node.id, type_id: node.type_id.clone(), inputs });
            }
            if node.type_id == GROUP_TYPE_ID
                && let Some(body) = node.group.as_deref()
            {
                groups.push(node.id);
                walk(&body.nodes, groups, out);
                groups.pop();
            }
        }
    }
    let mut out = Vec::new();
    walk(nodes, &mut Vec::new(), &mut out);
    out
}

fn build_node(
    def: &EffectGraphDef,
    id: u32,
    type_id: &str,
    handle: Option<String>,
    params: BTreeMap<String, crate::effect_graph_def::SerializedParamValue>,
) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: fresh_stable_id(def, type_id, id),
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

/// `<type>_<doc>`, suffixed until no node in `def` has it: the same graph
/// always gets the same ids, so a preset built by insertion is reproducible.
fn fresh_stable_id(def: &EffectGraphDef, type_id: &str, doc: u32) -> NodeId {
    fn collect(nodes: &[EffectGraphNode], out: &mut HashSet<String>) {
        for node in nodes {
            out.insert(node.node_id.as_str().to_owned());
            if let Some(body) = node.group.as_deref() {
                collect(&body.nodes, out);
            }
        }
    }
    let mut taken = HashSet::new();
    collect(&def.nodes, &mut taken);
    let base = format!("{}_{doc}", type_id.rsplit('.').next().unwrap_or(type_id));
    let mut candidate = base.clone();
    let mut suffix = 2u32;
    while taken.contains(&candidate) {
        candidate = format!("{base}_{suffix}");
        suffix += 1;
    }
    NodeId::new(candidate)
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

/// Wire the World controls `taker` declares to it, inside the root-level
/// group `group_id`, creating the World when the scene has none. A liquid
/// domain must take the World's gravity and clock both. A graph where a
/// control cannot be found is left untouched and refused.
pub(crate) fn share_world_controls(
    def: &mut EffectGraphDef,
    group_id: u32,
    taker: &WorldControlTaker,
) -> Result<(), &'static str> {
    if crate::liquid_domain::is_liquid_domain(&taker.type_id) && taker.inputs != WORLD_INPUTS {
        return Err("This liquid does not take the Physics World's controls");
    }
    let metadata = stamped_node_params(WORLD_TYPE_ID);
    if taker.inputs.iter().any(|input| !metadata.iter().any(|m| m.name == input.world)) {
        return Err("A simulation needs complete Physics World control metadata");
    }
    let worlds: Vec<_> = def.nodes.iter().filter(|node| node.type_id == WORLD_TYPE_ID).map(|node| node.id).collect();
    if worlds.len() > 1 {
        return Err("A simulation needs one shared root Physics World");
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
            .filter(|m| WORLD_INPUTS.iter().any(|input| input.world == m.name))
            .map(|m| (m.name.clone(), m.default_value.clone()))
            .collect();
        let world = build_node(def, id, WORLD_TYPE_ID, Some(handle), params);
        def.nodes.push(world);
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

    // The group, then each group nested on the way to the taker.
    let path: Vec<u32> = std::iter::once(group_id).chain(taker.groups.iter().copied()).collect();
    let mut inputs = Vec::with_capacity(path.len());
    for depth in 0..path.len() {
        let id = next_id(def)?;
        let sentinel = build_node(def, id, GROUP_INPUT_TYPE_ID, None, BTreeMap::new());
        let body = group_body_mut(&mut def.nodes, &path[..=depth])?;
        let sentinels: Vec<u32> =
            body.nodes.iter().filter(|node| node.type_id == GROUP_INPUT_TYPE_ID).map(|node| node.id).collect();
        inputs.push(match sentinels.as_slice() {
            [] => {
                body.nodes.push(sentinel);
                id
            }
            [existing] => *existing,
            _ => return Err("A control group has more than one input sentinel"),
        });
    }
    if !group_body_mut(&mut def.nodes, &path)?.nodes.iter().any(|node| node.id == taker.node) {
        return Err("The simulation node is unavailable");
    }

    for input in taker.inputs {
        let world_param = input.world;
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
            let mut node = build_node(def, id, "node.value", Some(handle), BTreeMap::from([("value".into(), value)]));
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
            // The World's card control also drives the new value source: one
            // control, two targets. The World's own param stays the first
            // target, so it still describes the control (a trigger stays a
            // trigger every clip trigger fires; its scene facts stay the
            // World's). Its ID, calibration, conversion and modulation
            // routing are kept.
            let bindings = &mut def.preset_metadata.as_mut().ok_or("Scene has no card")?.bindings;
            let mut index = 0;
            while index < bindings.len() {
                if matches!(&bindings[index].target, BindingTarget::Node { node_id, param }
                    if node_id == &world_node_id && param == world_param)
                {
                    let mut source = bindings[index].clone();
                    source.target = BindingTarget::Node { node_id: source_node_id.clone(), param: "value".into() };
                    bindings.insert(index + 1, source);
                    index += 1;
                }
                index += 1;
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
                None => (taker.node, input.input),
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
