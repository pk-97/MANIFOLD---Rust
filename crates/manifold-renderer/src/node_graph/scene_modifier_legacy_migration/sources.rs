//! Known Loop/Fog signatures. No factory defaults or inferred takeover history.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, GroupDef,
    GroupInterface, InterfacePortDef, ParamSpecDef, PresetMetadata,
};
use manifold_core::effects::ParamConvert;
use manifold_core::flatten::flatten_groups;
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::scene_modifier_preset::{
    SceneEndpoint, SceneModifierInstanceDef, SceneModifierRecipe, SceneModifierStageDef,
    SceneStageInput, SceneStageOutput, SceneStageScope, SceneStageSource, SceneTargetSelection,
};
use manifold_core::{NodeId, SceneNodeRef};

use crate::node_graph::{PrimitiveRegistry, scene_modifier_expand::prepare_scene_modifiers};

const LOOP: &[(&str, &str)] = &[
    ("loop_phase", "node.beat_ramp"),
    ("scene_array", "node.scene_array"),
    ("loop_camera", "node.loop_camera"),
    ("loop_cam_switch", "node.camera_switch"),
];
const FOG: &[(&str, &str)] = &[
    ("fog_atm", "node.atmosphere"),
    ("fog_enabled", "node.value"),
    ("fog_amount", "node.value"),
    ("fog_mul", "node.math"),
];

pub(super) fn extract(
    owner: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Result<Option<EffectGraphDef>, String> {
    let loop_nodes = signature(owner, LOOP)?;
    let fog_nodes = signature(owner, FOG)?;
    if loop_nodes.is_none() && fog_nodes.is_none() {
        return Ok(None);
    }
    let mut candidate = owner.clone();
    // Geometry stages may already have been adopted by the caller. Loop is
    // inserted first because its instance source must precede transformations.
    if let Some(nodes) = loop_nodes {
        adopt_loop(&mut candidate, &nodes)?;
    }
    if let Some(nodes) = fog_nodes {
        adopt_fog(&mut candidate, &nodes)?;
    }
    candidate.version = candidate.version.max(3);
    // Prove the source adoption itself changes no primitive-to-primitive edge.
    // This catches unsupported lens chains and stray boundary consumers without
    // guessing an attachment from a node's display name.
    verify_source_edges(owner, &candidate, registry)?;
    Ok(Some(candidate))
}

fn signature(
    owner: &EffectGraphDef,
    names: &[(&str, &str)],
) -> Result<Option<Vec<EffectGraphNode>>, String> {
    fn find<'a>(nodes: &'a [EffectGraphNode], id: &str, out: &mut Vec<&'a EffectGraphNode>) {
        for node in nodes {
            if node.node_id.as_str() == id {
                out.push(node);
            }
            if let Some(group) = &node.group {
                find(&group.nodes, id, out);
            }
        }
    }
    let mut found = Vec::new();
    for (id, ty) in names {
        let mut matches = Vec::new();
        find(&owner.nodes, id, &mut matches);
        if matches.len() > 1 {
            return Err(format!("duplicate legacy identity {id}"));
        }
        if let Some(node) = matches.first() {
            if node.type_id != *ty
                || node.group.is_some()
                || !owner.nodes.iter().any(|n| std::ptr::eq(n, *node))
            {
                return Err(format!("custom or nested legacy node {id}"));
            }
            found.push((*node).clone());
        }
    }
    if found.is_empty() {
        Ok(None)
    } else if found.len() != names.len() {
        Err(format!("incomplete legacy {} signature", names[0].0))
    } else {
        Ok(Some(found))
    }
}

fn wire(from: u32, output: &str, to: u32, input: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node: from,
        from_port: output.into(),
        to_node: to,
        to_port: input.into(),
    }
}

fn node(id: u32, name: &str, ty: &str) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: NodeId::new(name),
        type_id: ty.into(),
        handle: Some(name.into()),
        params: Default::default(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: Default::default(),
        output_canvas_scales: Default::default(),
        group: None,
    }
}

fn port(name: &str, ty: &str) -> InterfacePortDef {
    InterfacePortDef {
        name: name.into(),
        port_type: ty.into(),
    }
}

fn group(
    name: &str,
    mut nodes: Vec<EffectGraphNode>,
    wires: Vec<EffectGraphWire>,
    inputs: Vec<InterfacePortDef>,
    outputs: Vec<InterfacePortDef>,
) -> EffectGraphNode {
    // Existing document IDs stay intact inside the new scope.
    let mut wrapper = node(1, name, "group");
    nodes.shrink_to_fit();
    wrapper.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs,
            outputs,
            params: Vec::new(),
        },
        nodes,
        wires,
        tint: None,
    }));
    wrapper
}

fn one_input(
    owner: &EffectGraphDef,
    target: u32,
    port: &str,
) -> Result<Option<EffectGraphWire>, String> {
    let mut matches = owner
        .wires
        .iter()
        .filter(|w| w.to_node == target && w.to_port == port);
    let first = matches.next().cloned();
    if matches.next().is_some() {
        return Err(format!("multiple producers at {target}.{port}"));
    }
    Ok(first)
}

fn require_edge(owner: &EffectGraphDef, edge: EffectGraphWire) -> Result<(), String> {
    if one_input(owner, edge.to_node, &edge.to_port)?.as_ref() != Some(&edge) {
        return Err(format!(
            "custom legacy edge at {}.{}",
            edge.to_node, edge.to_port
        ));
    }
    Ok(())
}

fn unique_scene(owner: &EffectGraphDef) -> Result<&EffectGraphNode, String> {
    let mut scenes = owner
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.render_scene");
    let scene = scenes.next().ok_or("legacy source has no root scene")?;
    if scenes.next().is_some() {
        return Err("legacy source has multiple scenes".into());
    }
    Ok(scene)
}

fn metadata(
    owner: &mut EffectGraphDef,
    owned: &[EffectGraphNode],
    instance: &NodeId,
    preset: &str,
    name: &str,
    enabled_node: &str,
    enabled_param: &str,
) -> Result<(PresetMetadata, String), String> {
    let ids: BTreeSet<_> = owned.iter().map(|n| n.node_id.as_str()).collect();
    let meta = owner
        .preset_metadata
        .as_mut()
        .ok_or("legacy controls have no metadata")?;
    // Legacy Loop toggled the camera mux directly and exposed no public slot.
    // Preserve the saved enum, then give that existing control a stable address.
    if enabled_node == "loop_cam_switch" && !meta.bindings.iter().any(|b| matches!(&b.target, BindingTarget::Node {node_id,param} if node_id.as_str()==enabled_node && param==enabled_param)) {
        let control = owned.iter().find(|n| n.node_id.as_str()==enabled_node).ok_or("missing Loop camera switch")?;
        let value = match control.params.get(enabled_param) {
            Some(manifold_core::effect_graph_def::SerializedParamValue::Enum {value}) if (0..=1).contains(value) => *value as f32,
            _ => return Err("unsupported legacy camera switch selection".into()),
        };
        let id = "legacy:scene_loop:camera_travel".to_string();
        if meta.params.iter().any(|p|p.id==id) || meta.bindings.iter().any(|b|b.id==id) {return Err("legacy camera travel address already exists".into());}
        meta.params.push(ParamSpecDef {id:id.clone(),name:"Camera Travel".into(),min:0.0,max:1.0,default_value:value,is_toggle:true,..Default::default()});
        meta.bindings.push(BindingDef {id,label:"Camera Travel".into(),default_value:value,target:BindingTarget::Node {node_id:control.node_id.clone(),param:enabled_param.into()},convert:ParamConvert::EnumRound,user_added:false,scale:1.0,offset:0.0,default_mirrors_node_param:true});
    }
    let local_bindings: Vec<_> = meta.bindings.iter().filter(|b| matches!(&b.target, BindingTarget::Node {node_id,..} if ids.contains(node_id.as_str()))).cloned().collect();
    if meta.string_bindings.iter().any(
        |b| matches!(&b.target, BindingTarget::Node {node_id,..} if ids.contains(node_id.as_str())),
    ) {
        return Err("custom string controls on legacy source".into());
    }
    let enabled: Vec<_> = local_bindings.iter().filter(|b| matches!(&b.target, BindingTarget::Node {node_id,param} if node_id.as_str()==enabled_node && param==enabled_param)).collect();
    if enabled.len() != 1 {
        return Err("legacy enabled control is absent or ambiguous".into());
    }
    let enabled = enabled[0].id.clone();
    let param_ids: BTreeSet<_> = local_bindings.iter().map(|b| b.id.as_str()).collect();
    let mut local = meta.clone();
    local.id = PresetTypeId::from_string(preset.into());
    local.display_name = name.into();
    local.params.retain(|p| param_ids.contains(p.id.as_str()));
    if local.params.len() != param_ids.len() {
        return Err("legacy source manifest is incomplete".into());
    }
    local.bindings = local_bindings.clone();
    local.scene_bounds = None;
    local.param_aliases.clear();
    local.value_aliases.clear();
    local.string_params.clear();
    local.string_bindings.clear();
    local.scene_modifier = None;
    let mut first = BTreeMap::new();
    for b in &local_bindings {
        if let Some(old) = first.insert(b.id.clone(), b)
            && (
                old.default_value,
                old.default_mirrors_node_param,
                old.user_added,
                &old.label,
            ) != (
                b.default_value,
                b.default_mirrors_node_param,
                b.user_added,
                &b.label,
            ) {
                return Err(format!("incompatible fanout metadata for {}", b.id));
        }
    }
    let mut emitted = BTreeSet::new();
    let mut bindings = Vec::with_capacity(meta.bindings.len());
    for b in &meta.bindings {
        if matches!(&b.target, BindingTarget::Node {node_id,..} if ids.contains(node_id.as_str())) {
            if emitted.insert(b.id.clone()) {
                let mut host = b.clone();
                host.target = BindingTarget::SceneModifier {
                    modifier_id: instance.clone(),
                    param_id: b.id.clone(),
                };
                host.convert = ParamConvert::Float;
                host.scale = 1.0;
                host.offset = 0.0;
                bindings.push(host);
            }
        } else {
            bindings.push(b.clone());
        }
    }
    meta.bindings = bindings;
    Ok((local, enabled))
}

fn adopt_fog(owner: &mut EffectGraphDef, owned: &[EffectGraphNode]) -> Result<(), String> {
    let [atm, enabled, amount, mul] = owned else {
        return Err("invalid fog ownership".into());
    };
    let scene = unique_scene(owner)?.clone();
    for edge in [
        wire(enabled.id, "out", mul.id, "a"),
        wire(amount.id, "out", mul.id, "b"),
        wire(mul.id, "out", atm.id, "fog_density"),
        wire(atm.id, "atmosphere", scene.id, "atmosphere"),
    ] {
        require_edge(owner, edge)?;
    }
    let ids: BTreeSet<_> = owned.iter().map(|n| n.id).collect();
    let mut internal = Vec::new();
    for w in &owner.wires {
        match (ids.contains(&w.from_node), ids.contains(&w.to_node)) {
            (true, true) => internal.push(w.clone()),
            (false, true) => return Err("custom input into legacy Fog".into()),
            (true, false) if *w != wire(atm.id, "atmosphere", scene.id, "atmosphere") => {
                return Err("legacy Fog has another consumer".into());
            }
            _ => {}
        }
    }
    let mut nodes = owned.to_vec();
    let next = ids
        .last()
        .copied()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or("node ID capacity")?;
    nodes.push(node(next, "legacy_fog_output", "system.group_output"));
    internal.push(wire(atm.id, "atmosphere", next, "atmosphere"));
    let stage = group(
        "legacy_fog_stage",
        nodes,
        internal,
        Vec::new(),
        vec![port("atmosphere", "Atmosphere")],
    );
    let instance = NodeId::new("legacy:scene_fog");
    let (mut meta, enabled) = metadata(
        owner,
        owned,
        &instance,
        "SceneFog",
        "Scene Fog",
        "fog_enabled",
        "value",
    )?;
    meta.scene_modifier = Some(SceneModifierRecipe {
        schema_version: 1,
        singleton: true,
        enabled_param: enabled,
        preparation_params: Vec::new(),
        initializers: Vec::new(),
        calibrations: Vec::new(),
        stages: vec![SceneModifierStageDef {
            group: stage.node_id.clone(),
            scope: SceneStageScope::Scene,
            inputs: Vec::new(),
            outputs: vec![SceneStageOutput {
                port: "atmosphere".into(),
                endpoint: SceneEndpoint::Atmosphere,
            }],
        }],
    });
    owner.nodes.retain(|n| !ids.contains(&n.id));
    owner
        .wires
        .retain(|w| !ids.contains(&w.from_node) && !ids.contains(&w.to_node));
    owner.scene_modifiers.push(SceneModifierInstanceDef {
        id: instance,
        scene: SceneNodeRef {
            scope: Vec::new(),
            node: scene.node_id,
        },
        targets: SceneTargetSelection::AllObjects,
        mesh_frames: Vec::new(),
        graph: Box::new(EffectGraphDef {
            version: 3,
            name: Some("Scene Fog".into()),
            description: None,
            preset_metadata: Some(meta),
            scene_modifiers: Vec::new(),
            nodes: vec![stage],
            wires: Vec::new(),
        }),
    });
    Ok(())
}

fn adopt_loop(owner: &mut EffectGraphDef, owned: &[EffectGraphNode]) -> Result<(), String> {
    let [phase, array, camera, switch] = owned else {
        return Err("invalid Loop ownership".into());
    };
    let scene = unique_scene(owner)?.clone();
    for edge in [
        wire(phase.id, "out", camera.id, "phase"),
        wire(camera.id, "out", array.id, "camera"),
        wire(camera.id, "out", switch.id, "b"),
    ] {
        require_edge(owner, edge)?;
    }
    let previous = one_input(owner, switch.id, "a")?;
    let output: Vec<_> = owner
        .wires
        .iter()
        .filter(|w| w.from_node == switch.id)
        .cloned()
        .collect();
    if output.len() != 1 || output[0].from_port != "out" || output[0].to_port != "camera" {
        return Err("legacy Loop camera ownership is ambiguous".into());
    }
    let ids: BTreeSet<_> = owned.iter().map(|n| n.id).collect();
    let mut internal = Vec::new();
    let mut targets = Vec::new();
    let mut splices = Vec::new();
    for w in &owner.wires {
        match (ids.contains(&w.from_node), ids.contains(&w.to_node)) {
            (true, true) => internal.push(w.clone()),
            (false, true) if Some(w) != previous.as_ref() => {
                return Err("custom input into legacy Loop".into());
            }
            (true, false)
                if w.from_node == array.id && w.from_port == "out" && w.to_port == "instances" =>
            {
                let wrapper = owner
                    .nodes
                    .iter()
                    .find(|n| n.id == w.to_node)
                    .ok_or("Loop target missing")?;
                if !owner.wires.iter().any(|e| {
                    e.from_node == wrapper.id
                        && e.to_node == scene.id
                        && (e.to_port.starts_with("object_") || e.to_port.starts_with("mesh_"))
                }) {
                    return Err("Loop instance consumer is outside its scene".into());
                }
                let body = wrapper
                    .group
                    .as_ref()
                    .ok_or("custom direct Loop instance consumer")?;
                let boundaries: BTreeSet<_> = body
                    .nodes
                    .iter()
                    .filter(|n| n.type_id == "system.group_input")
                    .map(|n| n.id)
                    .collect();
                let edges: Vec<_> = body
                    .wires
                    .iter()
                    .filter(|e| boundaries.contains(&e.from_node) && e.from_port == "instances")
                    .collect();
                if edges.len() != 1 || edges[0].to_port != "instances" {
                    return Err("custom Loop instance boundary".into());
                }
                let target = body
                    .nodes
                    .iter()
                    .find(|n| n.id == edges[0].to_node && n.type_id == "node.scene_object")
                    .ok_or("Loop boundary has no scene object")?;
                if body
                    .wires
                    .iter()
                    .filter(|e| e.to_node == target.id && e.to_port == "instances")
                    .count()
                    != 1
                {
                    return Err("multiple instance producers".into());
                }
                targets.push(SceneNodeRef {
                    scope: vec![wrapper.node_id.clone()],
                    node: target.node_id.clone(),
                });
                splices.push((wrapper.id, edges[0].clone()));
            }
            (true, false) if *w != output[0] => {
                return Err("legacy Loop has another consumer".into());
            }
            _ => {}
        }
    }
    if targets.is_empty() {
        return Err("legacy Loop has no instance targets".into());
    }
    let mut nodes = owned.to_vec();
    let next = ids
        .last()
        .copied()
        .unwrap_or(0)
        .checked_add(3)
        .ok_or("node ID capacity")?;
    let mut inputs = Vec::new();
    let mut recipe_inputs = Vec::new();
    if previous.is_some() {
        nodes.push(node(next - 2, "legacy_loop_input", "system.group_input"));
        internal.push(wire(next - 2, "camera", switch.id, "a"));
        inputs.push(port("camera", "Camera"));
        recipe_inputs.push(SceneStageInput {
            port: "camera".into(),
            source: SceneStageSource::Previous {
                endpoint: SceneEndpoint::Camera,
            },
        });
    }
    nodes.push(node(next - 1, "legacy_loop_output", "system.group_output"));
    internal.push(wire(switch.id, "out", next - 1, "camera"));
    internal.push(wire(array.id, "out", next - 1, "instances"));
    let stage = group(
        "legacy_loop_stage",
        nodes,
        internal,
        inputs,
        vec![
            port("camera", "Camera"),
            port("instances", "Array(InstanceTransform)"),
        ],
    );
    let mut instance_stage = group(
        "legacy_loop_instances",
        vec![
            node(1, "legacy_instances_input", "system.group_input"),
            node(2, "legacy_instances_output", "system.group_output"),
        ],
        vec![wire(1, "instances", 2, "instances")],
        vec![port("instances", "Array(InstanceTransform)")],
        vec![port("instances", "Array(InstanceTransform)")],
    );
    instance_stage.id = 2;
    let instance = NodeId::new("legacy:scene_loop");
    let (mut meta, enabled) = metadata(
        owner,
        owned,
        &instance,
        "SceneLoop",
        "Scene Loop",
        "loop_cam_switch",
        "select",
    )?;
    meta.scene_modifier = Some(SceneModifierRecipe {
        schema_version: 1,
        singleton: true,
        enabled_param: enabled,
        preparation_params: Vec::new(),
        initializers: Vec::new(),
        calibrations: Vec::new(),
        stages: vec![
            SceneModifierStageDef {
                group: stage.node_id.clone(),
                scope: SceneStageScope::Scene,
                inputs: recipe_inputs,
                outputs: vec![SceneStageOutput {
                    port: "camera".into(),
                    endpoint: SceneEndpoint::Camera,
                }],
            },
            SceneModifierStageDef {
                group: instance_stage.node_id.clone(),
                scope: SceneStageScope::EachObject,
                inputs: vec![SceneStageInput {
                    port: "instances".into(),
                    source: SceneStageSource::StageOutput {
                        stage: stage.node_id.clone(),
                        port: "instances".into(),
                    },
                }],
                outputs: vec![SceneStageOutput {
                    port: "instances".into(),
                    endpoint: SceneEndpoint::Instances,
                }],
            },
        ],
    });
    owner
        .wires
        .retain(|w| !ids.contains(&w.from_node) && !ids.contains(&w.to_node));
    if let Some(previous) = previous {
        owner.wires.push(wire(
            previous.from_node,
            &previous.from_port,
            output[0].to_node,
            &output[0].to_port,
        ));
    }
    for (wrapper, edge) in splices {
        let body = owner
            .nodes
            .iter_mut()
            .find(|n| n.id == wrapper)
            .and_then(|n| n.group.as_mut())
            .ok_or("Loop target changed")?;
        body.wires.retain(|w| *w != edge);
        body.interface.inputs.retain(|p| p.name != "instances");
        // Only remove the boundary node if no other authored edge uses it.
        if !body
            .wires
            .iter()
            .any(|w| w.from_node == edge.from_node || w.to_node == edge.from_node)
        {
            body.nodes.retain(|n| n.id != edge.from_node);
        }
    }
    owner.nodes.retain(|n| !ids.contains(&n.id));
    owner.scene_modifiers.insert(
        0,
        SceneModifierInstanceDef {
            id: instance,
            scene: SceneNodeRef {
                scope: Vec::new(),
                node: scene.node_id,
            },
            targets: SceneTargetSelection::Explicit { objects: targets },
            mesh_frames: Vec::new(),
            graph: Box::new(EffectGraphDef {
                version: 3,
                name: Some("Scene Loop".into()),
                description: None,
                preset_metadata: Some(meta),
                scene_modifiers: Vec::new(),
                nodes: vec![stage, instance_stage],
                wires: Vec::new(),
            }),
        },
    );
    Ok(())
}

type Edge = (String, String, String, String);
fn verify_source_edges(
    before: &EffectGraphDef,
    after: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Result<(), String> {
    let old = prepare_scene_modifiers(before, registry).map_err(|e| e.to_string())?;
    let new = prepare_scene_modifiers(after, registry).map_err(|e| e.to_string())?;
    let old_flat = flatten_groups(&old.def).map_err(|e| e.to_string())?;
    let mut aliases = BTreeMap::new();
    for route in &new.routes {
        if matches!(
            route.modifier_id.as_str(),
            "legacy:scene_loop" | "legacy:scene_fog"
        ) {
            for copy in &route.copies {
                aliases.insert(copy.node_id.to_string(), route.local.node.to_string());
            }
        }
    }
    fn edges(
        def: &EffectGraphDef,
        aliases: &BTreeMap<String, String>,
    ) -> Result<Vec<Edge>, String> {
        let names: BTreeMap<_, _> = def
            .nodes
            .iter()
            .map(|n| {
                (
                    n.id,
                    aliases
                        .get(n.node_id.as_str())
                        .cloned()
                        .unwrap_or_else(|| n.node_id.to_string()),
                )
            })
            .collect();
        let mut out = Vec::new();
        for w in &def.wires {
            out.push((
                names
                    .get(&w.from_node)
                    .ok_or("missing edge producer")?
                    .clone(),
                w.from_port.clone(),
                names
                    .get(&w.to_node)
                    .ok_or("missing edge consumer")?
                    .clone(),
                w.to_port.clone(),
            ));
        }
        out.sort();
        Ok(out)
    }
    if edges(&old_flat, &BTreeMap::new())? != edges(&new.def, &aliases)? {
        return Err(
            "source adoption would change an existing camera, atmosphere or instance connection"
                .into(),
        );
    }
    Ok(())
}
