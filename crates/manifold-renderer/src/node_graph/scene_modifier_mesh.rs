//! Data driven mesh-stage scene modifiers used by the photo-scan workflow.
//!
//! The three cards deliberately share one adapter.  Their assets are ordinary
//! v2 graph definitions containing one reusable group; this module only binds
//! that group to each imported object and creates the card controls.  The
//! editing crate owns the actual nested splice operation.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_TYPE_ID,
    SerializedParamValue,
};
use manifold_core::flatten::flatten_groups;
use manifold_core::scene_modifier::{
    EnablePlan, MeshStageSplice, SceneModifierPlan, SharedParamBinding, ToggleDecl,
};

use super::{SceneModifierDescriptor, SlotGroup, TraceNode, f32_param, mint_node, plan_skeleton};

const ELASTIC_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/scene-modifier-presets/ElasticSculpture.json"
));
const SURFACE_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/scene-modifier-presets/SurfacePeel.json"
));
const VORTEX_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/scene-modifier-presets/VortexFragments.json"
));

pub const ELASTIC_KIND_ID: &str = "elastic_sculpture";
pub const SURFACE_KIND_ID: &str = "surface_peel";
pub const VORTEX_KIND_ID: &str = "vortex_fragments";

const ELASTIC_CONTROLS: &[(&str, &str, &str)] = &[
    ("photoscan/elastic_sculpture/bend", "value", "Bend"),
    (
        "photoscan/elastic_sculpture/cross_bend",
        "value",
        "Cross Bend",
    ),
    ("photoscan/elastic_sculpture/detail", "value", "Detail"),
    ("photoscan/elastic_sculpture/phase", "value", "Phase"),
    ("photoscan/elastic_sculpture/yaw", "value", "Yaw"),
    ("photoscan/elastic_sculpture/pitch", "value", "Pitch"),
    ("photoscan/elastic_sculpture/enabled", "value", "Enabled"),
];
const SURFACE_CONTROLS: &[(&str, &str, &str)] = &[
    ("photoscan/surface_peel/lift", "value", "Lift"),
    ("photoscan/surface_peel/curl", "value", "Curl"),
    ("photoscan/surface_peel/spread", "value", "Spread"),
    ("photoscan/surface_peel/phase", "value", "Phase"),
    ("photoscan/surface_peel/detail", "value", "Detail"),
    ("photoscan/surface_peel/yaw", "value", "Yaw"),
    ("photoscan/surface_peel/pitch", "value", "Pitch"),
    ("photoscan/surface_peel/enabled", "value", "Enabled"),
];
const VORTEX_CONTROLS: &[(&str, &str, &str)] = &[
    ("photoscan/vortex_fragments/orbit", "value", "Orbit"),
    ("photoscan/vortex_fragments/rise", "value", "Rise"),
    (
        "photoscan/vortex_fragments/separation",
        "value",
        "Separation",
    ),
    ("photoscan/vortex_fragments/phase", "value", "Phase"),
    ("photoscan/vortex_fragments/detail", "value", "Detail"),
    ("photoscan/vortex_fragments/yaw", "value", "Yaw"),
    ("photoscan/vortex_fragments/pitch", "value", "Pitch"),
    ("photoscan/vortex_fragments/enabled", "value", "Enabled"),
];

const ELASTIC_TRACE: &[TraceNode] = &[
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/elastic_sculpture/bend",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/elastic_sculpture/cross_bend",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/elastic_sculpture/detail",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/elastic_sculpture/phase",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/elastic_sculpture/yaw",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/elastic_sculpture/pitch",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/elastic_sculpture/enabled",
        required: true,
    },
];
const SURFACE_TRACE: &[TraceNode] = &[
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/surface_peel/lift",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/surface_peel/curl",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/surface_peel/spread",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/surface_peel/phase",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/surface_peel/detail",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/surface_peel/yaw",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/surface_peel/pitch",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/surface_peel/enabled",
        required: true,
    },
];
const VORTEX_TRACE: &[TraceNode] = &[
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/vortex_fragments/orbit",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/vortex_fragments/rise",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/vortex_fragments/separation",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/vortex_fragments/phase",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/vortex_fragments/detail",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/vortex_fragments/yaw",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/vortex_fragments/pitch",
        required: true,
    },
    TraceNode {
        type_id: "node.value",
        node_id: "photoscan/vortex_fragments/enabled",
        required: true,
    },
];

pub static ELASTIC_SCULPTURE_DESCRIPTOR: SceneModifierDescriptor = SceneModifierDescriptor {
    kind_id: ELASTIC_KIND_ID,
    display_name: "Elastic Sculpture",
    slot_group: SlotGroup::Objects,
    plan_builder: build_elastic_plan,
    applicable: |def, scene| applicable(def, scene, RecipeKind::Elastic),
    trace: ELASTIC_TRACE,
    row_whitelist: Some(ELASTIC_CONTROLS),
    coupled_writes: &[],
    enable: super::EnableDecl::Value {
        node_id: "photoscan/elastic_sculpture/enabled",
    },
};

pub static SURFACE_PEEL_DESCRIPTOR: SceneModifierDescriptor = SceneModifierDescriptor {
    kind_id: SURFACE_KIND_ID,
    display_name: "Surface Peel",
    slot_group: SlotGroup::Objects,
    plan_builder: build_surface_plan,
    applicable: |def, scene| applicable(def, scene, RecipeKind::Surface),
    trace: SURFACE_TRACE,
    row_whitelist: Some(SURFACE_CONTROLS),
    coupled_writes: &[],
    enable: super::EnableDecl::Value {
        node_id: "photoscan/surface_peel/enabled",
    },
};

pub static VORTEX_FRAGMENTS_DESCRIPTOR: SceneModifierDescriptor = SceneModifierDescriptor {
    kind_id: VORTEX_KIND_ID,
    display_name: "Vortex Fragments",
    slot_group: SlotGroup::Objects,
    plan_builder: build_vortex_plan,
    applicable: |def, scene| applicable(def, scene, RecipeKind::Vortex),
    trace: VORTEX_TRACE,
    row_whitelist: Some(VORTEX_CONTROLS),
    coupled_writes: &[],
    enable: super::EnableDecl::Value {
        node_id: "photoscan/vortex_fragments/enabled",
    },
};

#[derive(Clone, Copy)]
enum RecipeKind {
    Elastic,
    Surface,
    Vortex,
}

impl RecipeKind {
    fn kind_id(self) -> &'static str {
        match self {
            Self::Elastic => ELASTIC_KIND_ID,
            Self::Surface => SURFACE_KIND_ID,
            Self::Vortex => VORTEX_KIND_ID,
        }
    }
    fn display_name(self) -> &'static str {
        match self {
            Self::Elastic => "Elastic Sculpture",
            Self::Surface => "Surface Peel",
            Self::Vortex => "Vortex Fragments",
        }
    }
}

fn recipe(kind: RecipeKind) -> &'static EffectGraphDef {
    static ELASTIC: LazyLock<EffectGraphDef> = LazyLock::new(|| {
        serde_json::from_str(ELASTIC_JSON).expect("ElasticSculpture.json must parse")
    });
    static SURFACE: LazyLock<EffectGraphDef> =
        LazyLock::new(|| serde_json::from_str(SURFACE_JSON).expect("SurfacePeel.json must parse"));
    static VORTEX: LazyLock<EffectGraphDef> = LazyLock::new(|| {
        serde_json::from_str(VORTEX_JSON).expect("VortexFragments.json must parse")
    });
    match kind {
        RecipeKind::Elastic => &ELASTIC,
        RecipeKind::Surface => &SURFACE,
        RecipeKind::Vortex => &VORTEX,
    }
}

#[derive(Clone)]
struct Target {
    scope_path: Vec<NodeId>,
    node_id: NodeId,
}

fn parse_f32(node: &EffectGraphNode, key: &str) -> Option<f32> {
    match node.params.get(key) {
        Some(SerializedParamValue::Float { value }) if value.is_finite() => Some(*value),
        _ => None,
    }
}

fn scene_radius(def: &EffectGraphDef) -> Option<f32> {
    if let Some((min, max)) = def.preset_metadata.as_ref().and_then(|m| m.scene_bounds) {
        let e = [
            (max[0] - min[0]).abs() * 0.5,
            (max[1] - min[1]).abs() * 0.5,
            (max[2] - min[2]).abs() * 0.5,
        ];
        let r = (e[0] * e[0] + e[1] * e[1] + e[2] * e[2]).sqrt();
        if r.is_finite() && r > 0.0 {
            return Some(r);
        }
    }
    fn walk(nodes: &[EffectGraphNode]) -> Option<f32> {
        nodes
            .iter()
            .filter_map(|n| {
                let own = (n.type_id == "node.gltf_mesh_source"
                    || n.type_id == "node.gltf_skinned_mesh_source")
                    .then(|| parse_f32(n, "source_bbox_radius"))
                    .flatten()
                    .filter(|v| *v > 0.0);
                own.or_else(|| n.group.as_deref().and_then(|g| walk(&g.nodes)))
            })
            .fold(None, |best, v| Some(best.map_or(v, |b: f32| b.max(v))))
    }
    walk(&def.nodes)
}

fn object_targets(def: &EffectGraphDef, scene_id: u32) -> Option<Vec<Target>> {
    let scene_node_id = def
        .nodes
        .iter()
        .find(|n| n.id == scene_id && n.type_id == "node.render_scene")?
        .node_id
        .clone();
    let flat = flatten_groups(def).ok()?;
    let flat_scene = flat
        .nodes
        .iter()
        .find(|n| n.node_id == scene_node_id && n.type_id == "node.render_scene")?;
    let mut flat_objects = Vec::new();
    for w in &flat.wires {
        if w.to_node != flat_scene.id || !w.to_port.starts_with("object_") {
            continue;
        }
        let node = flat
            .nodes
            .iter()
            .find(|n| n.id == w.from_node && n.type_id == "node.scene_object")?;
        flat_objects.push(node.node_id.clone());
    }
    if flat_objects.is_empty() {
        return None;
    }
    fn locate(nodes: &[EffectGraphNode], path: &[NodeId], wanted: &NodeId, out: &mut Vec<Target>) {
        for node in nodes {
            if node.type_id == "node.scene_object" && &node.node_id == wanted {
                out.push(Target {
                    scope_path: path.to_vec(),
                    node_id: node.node_id.clone(),
                });
            }
            if let Some(group) = node.group.as_deref() {
                let mut child_path = path.to_vec();
                child_path.push(node.node_id.clone());
                locate(&group.nodes, &child_path, wanted, out);
            }
        }
    }
    let mut out = Vec::new();
    for wanted in flat_objects {
        let mut matches = Vec::new();
        locate(&def.nodes, &[], &wanted, &mut matches);
        if matches.len() != 1 {
            return None;
        }
        out.push(matches.remove(0));
    }
    Some(out)
}

fn level<'a>(
    def: &'a EffectGraphDef,
    path: &[NodeId],
) -> Option<(&'a [EffectGraphNode], &'a [EffectGraphWire])> {
    fn descend<'a>(
        nodes: &'a [EffectGraphNode],
        wires: &'a [EffectGraphWire],
        path: &[NodeId],
    ) -> Option<(&'a [EffectGraphNode], &'a [EffectGraphWire])> {
        let Some((first, rest)) = path.split_first() else {
            return Some((nodes, wires));
        };
        let group = nodes
            .iter()
            .find(|n| n.node_id == *first)?
            .group
            .as_deref()?;
        descend(&group.nodes, &group.wires, rest)
    }
    descend(&def.nodes, &def.wires, path)
}

fn transform_offset(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    target: u32,
) -> Option<[f32; 3]> {
    let source = wires
        .iter()
        .find(|w| w.to_node == target && w.to_port == "transform")?;
    let node = nodes
        .iter()
        .find(|n| n.id == source.from_node && n.type_id == "node.transform_3d")?;
    Some([
        parse_f32(node, "pos_x")?,
        parse_f32(node, "pos_y")?,
        parse_f32(node, "pos_z")?,
    ])
}

fn max_id(nodes: &[EffectGraphNode]) -> u32 {
    nodes
        .iter()
        .map(|n| n.id.max(n.group.as_deref().map_or(0, |g| max_id(&g.nodes))))
        .max()
        .unwrap_or(0)
}

fn remap_stage(
    stage: &mut EffectGraphNode,
    kind: RecipeKind,
    target: &NodeId,
) -> BTreeMap<String, NodeId> {
    let mut map = BTreeMap::new();
    fn visit(node: &mut EffectGraphNode, prefix: &str, map: &mut BTreeMap<String, NodeId>) {
        let old = node.node_id.clone();
        if !old.is_empty() {
            let new = NodeId::new(format!("{prefix}{old}"));
            map.insert(old.to_string(), new.clone());
            node.node_id = new;
        }
        if let Some(h) = node.handle.as_mut() {
            *h = format!("{prefix}{h}").replace('/', "_");
        }
        if let Some(g) = node.group.as_mut() {
            for child in &mut g.nodes {
                visit(child, prefix, map);
            }
        }
    }
    let prefix = format!("photoscan/{}/{}/", kind.kind_id(), target);
    if let Some(group) = stage.group.as_mut() {
        for child in &mut group.nodes {
            visit(child, &prefix, &mut map);
        }
    }
    map
}

fn stage_for(
    kind: RecipeKind,
    target: &Target,
    offset: [f32; 3],
    radius: f32,
    id: u32,
) -> Option<(EffectGraphNode, BTreeMap<String, NodeId>)> {
    let source = recipe(kind)
        .nodes
        .iter()
        .find(|n| n.type_id == GROUP_TYPE_ID)?
        .clone();
    let mut stage = source;
    stage.id = id;
    stage.node_id = NodeId::new(format!(
        "photoscan/{}/{}/stage",
        kind.kind_id(),
        target.node_id
    ));
    stage.handle =
        Some(format!("Photoscan_{}_{}_stage", kind.kind_id(), target.node_id).replace('/', "_"));
    let remap = remap_stage(&mut stage, kind, &target.node_id);
    if let Some(body) = stage.group.as_mut() {
        for n in &mut body.nodes {
            let is_shear = n.type_id == "node.wave_shear_mesh";
            let is_patch = n.type_id == "node.transform_mesh_patches";
            if is_shear || is_patch {
                f32_param(&mut n.params, "scale", radius);
                if is_shear {
                    for (name, value) in [
                        ("origin_x", -offset[0]),
                        ("origin_y", -offset[1]),
                        ("origin_z", -offset[2]),
                    ] {
                        f32_param(&mut n.params, name, value);
                    }
                } else {
                    for (name, value) in [
                        ("source_offset_x", offset[0]),
                        ("source_offset_y", offset[1]),
                        ("source_offset_z", offset[2]),
                    ] {
                        f32_param(&mut n.params, name, value);
                    }
                    f32_param(&mut n.params, "cell_size", 0.15);
                }
            }
        }
    }
    Some((stage, remap))
}

fn stage_reference(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    producer: (u32, String),
) -> (NodeId, String) {
    let mut current = producer;
    for _ in 0..64 {
        let Some(node) = nodes.iter().find(|n| n.id == current.0) else {
            break;
        };
        if node.type_id != GROUP_TYPE_ID || !node.node_id.as_str().starts_with("photoscan/") {
            break;
        }
        if let Some(w) = wires
            .iter()
            .find(|w| w.to_node == node.id && w.to_port == "reference")
        {
            current = (w.from_node, w.from_port.clone());
        } else {
            break;
        }
    }
    (
        nodes
            .iter()
            .find(|n| n.id == current.0)
            .map(|n| n.node_id.clone())
            .unwrap_or_default(),
        current.1,
    )
}

fn build(kind: RecipeKind, def: &EffectGraphDef, scene_id: u32) -> Option<SceneModifierPlan> {
    // Removal is allowed to proceed from the stamped stage identity even when
    // its source object was deleted. Recover those stages before consulting
    // bounds or live target applicability.
    let owned_stages = collect_owned_stages(kind, def);
    if !owned_stages.is_empty() {
        return finish_plan(kind, def, owned_stages);
    }
    let targets = object_targets(def, scene_id)?;
    let radius = scene_radius(def)?;
    let mut stages = Vec::new();
    let mut next = max_id(&def.nodes).saturating_add(1);
    for target in &targets {
        let (nodes, wires) = level(def, &target.scope_path)?;
        let target_doc = nodes
            .iter()
            .find(|n| n.node_id == target.node_id && n.type_id == "node.scene_object")?;
        let producer = wires
            .iter()
            .filter(|w| w.to_node == target_doc.id && w.to_port == "vertices")
            .collect::<Vec<_>>();
        if producer.len() != 1 {
            return None;
        }
        let offset = transform_offset(nodes, wires, target_doc.id)?;
        let (stage, _remap) = stage_for(kind, target, offset, radius, next)?;
        next = next.saturating_add(1);
        let reference = stage_reference(
            nodes,
            wires,
            (producer[0].from_node, producer[0].from_port.clone()),
        );
        if reference.0.is_empty() {
            return None;
        }
        stages.push(MeshStageSplice {
            scope_path: target.scope_path.clone(),
            target_node_id: target.node_id.clone(),
            stage,
            reference_source: reference,
        });
    }
    if stages.is_empty() {
        return None;
    }
    finish_plan(kind, def, stages)
}

fn finish_plan(kind: RecipeKind, def: &EffectGraphDef, stages: Vec<MeshStageSplice>) -> Option<SceneModifierPlan> {
    let mut links = Vec::new();
    for stage in &stages {
        links.extend(stage_links(kind, &stage.stage));
    }
    let mut controls = Vec::new();
    let stage_max = stages.iter().map(|stage| stage.stage.id).max().unwrap_or(0);
    let mut next = max_id(&def.nodes).max(stage_max).saturating_add(1);
    for spec in recipe(kind).preset_metadata.as_ref()?.params.iter() {
        let id = NodeId::new(format!("photoscan/{}/{}", kind.kind_id(), spec.id));
        let mut params = BTreeMap::new();
        f32_param(&mut params, "value", spec.default_value);
        let mut control = mint_node(next, id.as_str(), "node.value", params);
        // Stable IDs may contain slashes; graph handles reserve them for
        // paths introduced by flattening nested groups.
        control.handle = Some(id.as_str().replace('/', "_"));
        controls.push(control);
        next = next.saturating_add(1);
    }
    let mut skeleton = plan_skeleton(descriptor(kind), &controls, Vec::new());
    for exposure in &mut skeleton.exposures {
        let Some(spec) = recipe(kind)
            .preset_metadata
            .as_ref()?
            .params
            .iter()
            .find(|s| {
                s.id == exposure
                    .node_id
                    .as_str()
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
            })
        else {
            continue;
        };
        if let Some(meta) = exposure.metadata.first_mut() {
            meta.label = spec.name.clone();
            meta.min = spec.min;
            meta.max = spec.max;
            meta.default_value = SerializedParamValue::Float {
                value: spec.default_value,
            };
            meta.is_toggle = spec.is_toggle;
            meta.is_angle = spec.is_angle;
            meta.wraps = spec.wraps;
        }
    }
    Some(SceneModifierPlan {
        kind_id: kind.kind_id().into(),
        display_name: kind.display_name().into(),
        trace: skeleton.trace,
        new_nodes: controls,
        new_wires: Vec::new(),
        group_splices: Vec::new(),
        mesh_stages: stages,
        repoints: Vec::new(),
        exposures: skeleton.exposures,
        shared_params: links,
        enable: EnablePlan {
            toggle: ToggleDecl::ValueAtom {
                node_id: NodeId::new(format!("photoscan/{}/enabled", kind.kind_id())),
            },
            extra_nodes: Vec::new(),
            extra_wires: Vec::new(),
        },
    })
}

fn collect_owned_stages(kind: RecipeKind, def: &EffectGraphDef) -> Vec<MeshStageSplice> {
    fn walk(
        nodes: &[EffectGraphNode],
        wires: &[EffectGraphWire],
        path: &[NodeId],
        kind: RecipeKind,
        out: &mut Vec<MeshStageSplice>,
    ) {
        for node in nodes {
            if node.type_id == GROUP_TYPE_ID
                && node
                    .node_id
                    .as_str()
                    .starts_with(&format!("photoscan/{}/", kind.kind_id()))
            {
                let target = wires
                    .iter()
                    .find(|w| w.from_node == node.id && w.from_port == "vertices")
                    .and_then(|w| {
                        nodes
                            .iter()
                            .find(|n| n.id == w.to_node && n.type_id == "node.scene_object")
                    })
                    .map(|n| n.node_id.clone())
                    .unwrap_or_else(|| NodeId::new("orphan_target"));
                let reference = wires
                    .iter()
                    .find(|w| w.to_node == node.id && w.to_port == "reference")
                    .and_then(|w| {
                        nodes
                            .iter()
                            .find(|n| n.id == w.from_node)
                            .map(|n| (n.node_id.clone(), w.from_port.clone()))
                    })
                    .unwrap_or_else(|| (NodeId::new("orphan_reference"), "out".into()));
                out.push(MeshStageSplice {
                    scope_path: path.to_vec(),
                    target_node_id: target,
                    stage: node.clone(),
                    reference_source: reference,
                });
            }
            if let Some(g) = node.group.as_deref() {
                let mut p = path.to_vec();
                p.push(node.node_id.clone());
                walk(&g.nodes, &g.wires, &p, kind, out);
            }
        }
    }
    let mut stages = Vec::new();
    walk(&def.nodes, &def.wires, &[], kind, &mut stages);
    stages
}

fn stage_links(kind: RecipeKind, stage: &EffectGraphNode) -> Vec<SharedParamBinding> {
    fn visit_nodes<'a>(nodes: &'a [EffectGraphNode], out: &mut Vec<&'a EffectGraphNode>) {
        for n in nodes {
            out.push(n);
            if let Some(g) = n.group.as_deref() {
                visit_nodes(&g.nodes, out);
            }
        }
    }
    let mut all = Vec::new();
    if let Some(g) = stage.group.as_deref() {
        visit_nodes(&g.nodes, &mut all);
    }
    let Some(meta) = recipe(kind).preset_metadata.as_ref() else {
        return Vec::new();
    };
    let mut links = Vec::new();
    for binding in &meta.bindings {
        let BindingTarget::Node { node_id, param } = &binding.target else {
            continue;
        };
        let Some(inner) = all
            .iter()
            .find(|n| n.node_id.as_str().ends_with(&format!("/{node_id}")))
        else {
            continue;
        };
        links.push(SharedParamBinding {
            source: BindingTarget::Node {
                node_id: NodeId::new(format!("photoscan/{}/{}", kind.kind_id(), binding.id)),
                param: "value".into(),
            },
            target: BindingTarget::Node {
                node_id: inner.node_id.clone(),
                param: param.clone(),
            },
        });
    }
    links
}

fn descriptor(kind: RecipeKind) -> &'static SceneModifierDescriptor {
    match kind {
        RecipeKind::Elastic => &ELASTIC_SCULPTURE_DESCRIPTOR,
        RecipeKind::Surface => &SURFACE_PEEL_DESCRIPTOR,
        RecipeKind::Vortex => &VORTEX_FRAGMENTS_DESCRIPTOR,
    }
}
fn build_elastic_plan(def: &EffectGraphDef, scene: u32) -> Option<SceneModifierPlan> {
    build(RecipeKind::Elastic, def, scene)
}
fn build_surface_plan(def: &EffectGraphDef, scene: u32) -> Option<SceneModifierPlan> {
    build(RecipeKind::Surface, def, scene)
}
fn build_vortex_plan(def: &EffectGraphDef, scene: u32) -> Option<SceneModifierPlan> {
    build(RecipeKind::Vortex, def, scene)
}
fn applicable(def: &EffectGraphDef, scene: u32, kind: RecipeKind) -> bool {
    let Some(targets) = object_targets(def, scene) else {
        return false;
    };
    scene_radius(def).is_some()
        && !targets.is_empty()
        && targets.iter().all(|t| {
            level(def, &t.scope_path)
                .and_then(|(nodes, wires)| {
                    let n = nodes.iter().find(|n| n.node_id == t.node_id)?;
                    let p = wires
                        .iter()
                        .filter(|w| w.to_node == n.id && w.to_port == "vertices")
                        .count();
                    (p == 1 && transform_offset(nodes, wires, n.id).is_some()).then_some(())
                })
                .is_some()
        })
        && !super::trace_modifier(descriptor(kind), &def.nodes).applied(descriptor(kind))
}
