//! One scene-object model (PHYSICS_ENGINE_BOUNDARY_DESIGN.md section 5.2 (One
//! scene-object model)): the object rows a scene has, who owns each, its
//! parent, its kind, the nodes whose controls it owns and the commands it
//! accepts. The Scene tab tree, the cards, the gizmos and the editing commands
//! read this one analysis. Derived data: never serialized, never an engine API.
//!
//! Water is recognised by the liquid domain inside its group, never by how its
//! objects are wired (BUG-4lfm (GPU-surface water not recognised as water)):
//! the nearest group around an object whose body holds exactly one liquid
//! domain owns a Water family ([`water_family`]). Grouping alone makes no
//! family: a group holding two domains keeps its objects independent, and an
//! object the simulation does not feed stays an ordinary object.
//!
//! The model reads topology and type ids only, never a param value, so a
//! cache keyed on the owner's structure version is exact.
//!
//! The walk is tolerant like the panel it feeds: a slot whose producer is not
//! a scene object has no row (the panel shows it as custom), and nothing here
//! needs the flat index except water built outside a family group, which
//! [`liquid_domain_of`] finds from the object's surface.

use crate::NodeId;
use crate::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GroupDef, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID,
    GROUP_TYPE_ID, RENDER_SCENE_TYPE_ID,
};
use crate::fluid_domain::MAX_FLUID_ROLES;
use crate::liquid_domain::{is_liquid_domain, liquid_domain_of};
use crate::scene_graph_edit::SceneGraphEditError;
use crate::scene_index::FlatSceneIndex;
use crate::scene_modifier_preset::SceneNodeRef;
pub use crate::scene_surface::{SceneObjectCapabilities, SceneObjectRowKind};

const SCENE_OBJECT_TYPE_ID: &str = "node.scene_object";
const FLUID_ROLE_SOURCE_TYPE_ID: &str = "node.fluid_role_source";
const TRANSFORM_3D_TYPE_ID: &str = "node.transform_3d";
const SWITCH_MESH_TYPE_ID: &str = "node.switch_mesh";
const SWITCH_INDICES_TYPE_ID: &str = "node.switch_indices";
const SWITCH_TYPE_IDS: [&str; 4] = ["node.switch_value", SWITCH_MESH_TYPE_ID, SWITCH_INDICES_TYPE_ID, "node.switch_instances"];
const LOOK_MESH_TYPE_ID: &str = "node.platonic_solid_mesh";
/// A swarm's forces node: its clock and the box its forces are laid over.
pub const SWARM_FORCES_TYPE_ID: &str = "node.swarm_forces";

/// One object row of a scene.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneObjectModel {
    /// The row's own node: a scene object, an imported model's group, or a
    /// placed fluid source's role node.
    pub object: SceneNodeRef,
    /// The node whose lifetime the row follows: the object's group, else the
    /// object itself.
    pub owner: SceneNodeRef,
    /// The row this one sits under in the tree.
    pub parent: Option<SceneNodeRef>,
    pub kind: SceneObjectRowKind,
    /// Nodes whose controls this row owns beyond its own object, transform,
    /// material and modifier chain: its simulation, roles, looks and
    /// switches, in the order the card lists them.
    pub controls: Vec<SceneNodeRef>,
    pub capabilities: SceneObjectCapabilities,
    /// The render slot the object fills; `None` for an imported model's group
    /// and a placed source.
    pub slot: Option<u32>,
    /// The liquid domain this row simulates: Water's real object, or an
    /// object whose surface a domain builds.
    pub liquid_domain: Option<SceneNodeRef>,
    /// The swarm whose copies this row places: the forces node behind its
    /// `instances`.
    pub swarm: Option<SceneNodeRef>,
    /// The same row addressed by document ids, root first, ending with the
    /// object's own id: the form panel rows and editing commands use.
    pub doc_path: Vec<u32>,
}

/// A Water family inside one group body: the liquid domain (or the nested
/// group holding it), the real Water object, and its looks in output order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaterFamily {
    /// Document id, in the body, of the domain or of the group holding it.
    pub domain: u32,
    pub parent: u32,
    pub children: Vec<u32>,
    /// Whether the parent's surface is built from the domain.
    pub surface: bool,
}

/// The Water family `group` owns, if its body holds exactly one liquid domain
/// and that domain feeds one of the group's object outputs. The parent is the
/// output object whose surface the domain builds, else the domain-fed object
/// behind the earliest object output; the other domain-fed output objects are
/// its looks.
pub fn water_family(group: &GroupDef) -> Option<WaterFamily> {
    let mut domains = crate::liquid_domain::liquid_domains_in(&group.nodes).into_iter();
    let domain = domains.next()?;
    if domains.next().is_some() {
        return None;
    }
    let anchor = domain.groups.first().copied().unwrap_or(domain.node);
    let body = Level { nodes: &group.nodes, wires: &group.wires };
    let mut members: Vec<(usize, u32)> = Vec::new();
    for output in group.nodes.iter().filter(|node| node.type_id == GROUP_OUTPUT_TYPE_ID) {
        for wire in group.wires.iter().filter(|wire| wire.to_node == output.id) {
            let Some(object) = body.node(wire.from_node).filter(|node| node.type_id == SCENE_OBJECT_TYPE_ID) else {
                continue;
            };
            let order = group.interface.outputs.iter().position(|port| port.name == wire.to_port).unwrap_or(usize::MAX);
            match members.iter_mut().find(|(_, id)| *id == object.id) {
                Some(member) => member.0 = member.0.min(order),
                None => members.push((order, object.id)),
            }
        }
    }
    members.sort_unstable();
    let fed: Vec<u32> = members
        .into_iter()
        .map(|(_, id)| id)
        .filter(|&id| upstream(&body, &[id]).contains(&anchor))
        .collect();
    let surface_built = fed.iter().copied().find(|&id| {
        body.producer(id, "vertices").is_some_and(|(from, _)| from == anchor || upstream(&body, &[from]).contains(&anchor))
    });
    let parent = surface_built.or_else(|| fed.first().copied())?;
    Some(WaterFamily {
        domain: anchor,
        parent,
        children: fed.into_iter().filter(|&id| id != parent).collect(),
        surface: surface_built.is_some(),
    })
}

/// The scene object feeding `output`'s `port` in one level: one wire, not a
/// walk.
pub fn scene_object_at_output(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    output: u32,
    port: &str,
) -> Option<u32> {
    let mut feeding = wires.iter().filter(|wire| wire.to_node == output && wire.to_port == port);
    let wire = feeding.next()?;
    if feeding.next().is_some() {
        return None;
    }
    nodes.iter().find(|node| node.id == wire.from_node && node.type_id == SCENE_OBJECT_TYPE_ID).map(|node| node.id)
}

/// The rows of `scene`, a `node.render_scene`, in tree order: each top-level
/// row is followed by its children.
pub fn scene_object_models(
    graph: &EffectGraphDef,
    scene: &SceneNodeRef,
) -> Result<Vec<SceneObjectModel>, SceneGraphEditError> {
    scene_object_models_in(graph, scene, FlatSceneIndex::build(graph).ok().as_ref())
}

/// [`scene_object_models`] over a flat index the caller already built
/// (`None` when the graph has none).
pub fn scene_object_models_in(
    graph: &EffectGraphDef,
    scene: &SceneNodeRef,
    index: Option<&FlatSceneIndex>,
) -> Result<Vec<SceneObjectModel>, SceneGraphEditError> {
    let error = |message: &str| SceneGraphEditError { at: Some(scene.clone()), message: message.into() };
    let scene_at = Path::locate(graph, scene).ok_or_else(|| error("render scene not found"))?;
    let scene_node = scene_at.node;
    if scene_node.type_id != RENDER_SCENE_TYPE_ID {
        return Err(error("target is not a render scene"));
    }

    // Every wired render slot that resolves to a scene object.
    let mut slots: Vec<(u32, At<'_>)> = Vec::new();
    for wire in scene_at.path.level().wires.iter().filter(|wire| wire.to_node == scene_node.id) {
        let Some(slot) = wire.to_port.strip_prefix("object_").and_then(|n| n.parse::<u32>().ok()) else { continue };
        let Some(at) = scene_at.path.resolve_inward(wire.from_node, &wire.from_port) else { continue };
        if at.node.type_id == SCENE_OBJECT_TYPE_ID && !slots.iter().any(|(taken, _)| *taken == slot) {
            slots.push((slot, at));
        }
    }
    slots.sort_by_key(|(slot, _)| *slot);

    let mut rows: Vec<Row<'_>> = Vec::new();
    for (slot, at) in &slots {
        rows.push(Row::object(*slot, at.clone()));
    }
    // Water families and imported models, by the object's nearest groups.
    let mut parents: Vec<Parent<'_>> = Vec::new();
    for row in &mut rows {
        let at = row.at.clone();
        if let Some(owner) = family_owner(&at) {
            let group_body = owner.node.group.as_deref().expect("family owner is a group");
            let family = water_family(group_body).expect("family owner has a family");
            let member = at.path.groups.len() == owner.path.groups.len() + 1;
            if member && at.node.id == family.parent {
                row.kind = SceneObjectRowKind::PhysicsParent;
                row.family = Some((owner.clone(), family.clone()));
            } else if member && family.children.contains(&at.node.id) {
                row.kind = SceneObjectRowKind::LookChild;
                row.family = Some((owner.clone(), family.clone()));
            }
            continue;
        }
        let Some(group) = at.path.groups.last().copied() else { continue };
        if at.path.level().producer(at.node.id, "parent_transform").is_none() {
            continue;
        }
        let group_at = at.path.parent_at(group);
        if !parents.iter().any(|parent| parent.at.node.id == group.id && parent.at.path.doc_scope() == group_at.path.doc_scope()) {
            parents.push(Parent { at: group_at.clone() });
        }
        row.compound = Some(group_at);
    }

    // Liquids, then each row's controls.
    for row in &mut rows {
        row.liquid = match (&row.kind, &row.family) {
            (SceneObjectRowKind::LookChild, _) => None,
            (SceneObjectRowKind::PhysicsParent, Some((owner, family))) => {
                let surface = index.and_then(|index| liquid_domain_of(index, &row.at.reference()).ok().flatten());
                match surface {
                    Some(domain) => row.at.path.root_locate(&domain).map(|at| (at, true)),
                    None => family_domain_at(owner, family).map(|at| (at, family.surface)),
                }
            }
            _ => index
                .and_then(|index| liquid_domain_of(index, &row.at.reference()).ok().flatten())
                .and_then(|domain| row.at.path.root_locate(&domain))
                .map(|at| (at, true)),
        };
    }

    let mut out: Vec<SceneObjectModel> = Vec::new();
    let mut placed_seen: Vec<SceneNodeRef> = Vec::new();
    let mut emitted_parents: Vec<usize> = Vec::new();
    for position in 0..rows.len() {
        let row = &rows[position];
        if row.kind == SceneObjectRowKind::LookChild && rows.iter().any(|other| other.is_parent_of(row)) {
            continue; // emitted under its parent
        }
        if let Some(group_at) = &row.compound {
            let parent_index = parents.iter().position(|parent| parent.at.node.id == group_at.node.id
                && parent.at.path.doc_scope() == group_at.path.doc_scope()).expect("compound parent listed");
            if emitted_parents.contains(&parent_index) {
                continue;
            }
            emitted_parents.push(parent_index);
            let parent_at = &parents[parent_index].at;
            let parent_ref = parent_at.reference();
            out.push(SceneObjectModel {
                object: parent_ref.clone(),
                owner: parent_ref.clone(),
                parent: None,
                kind: SceneObjectRowKind::PhysicsParent,
                controls: group_role_sources(parent_at, &[]),
                capabilities: SceneObjectCapabilities {
                    rename: true,
                    delete: true,
                    duplicate: true,
                    transform: true,
                    enable_physics: true,
                    fluid_role: true,
                    ..SceneObjectCapabilities::default()
                },
                slot: None,
                liquid_domain: None,
                swarm: None,
                doc_path: parent_at.doc_path(),
            });
            for child in rows.iter().filter(|other| other.compound.as_ref().is_some_and(|at| at.node.id == group_at.node.id
                && at.path.doc_scope() == group_at.path.doc_scope()))
            {
                let mut model = child.identity(&rows);
                model.parent = Some(parent_ref.clone());
                model.capabilities.enable_physics = false;
                model.capabilities.fluid_role = false;
                out.push(model);
            }
            continue;
        }
        let placed = row.placed_sources();
        let mut model = row.model(&rows, &placed);
        if row.kind == SceneObjectRowKind::PhysicsParent {
            for child in row.children(&rows) {
                for port in ["visible", "vertices", "indices", "instances"] {
                    if let Some((id, _)) = child.at.path.level().producer(child.at.node.id, port)
                        && let Some(switch) = child.at.path.level().node(id).filter(|node| SWITCH_TYPE_IDS.contains(&node.type_id.as_str()))
                    {
                        push_unique(&mut model.controls, child.at.path.reference(switch));
                    }
                }
            }
        }
        let object_ref = model.object.clone();
        out.push(model);
        if row.kind == SceneObjectRowKind::PhysicsParent {
            for child in row.children(&rows) {
                out.push(child.model(&rows, &[]));
            }
        }
        // Two rows drawing one water list its sources once, under the first.
        for source in placed {
            let reference = source.role.reference();
            if placed_seen.contains(&reference) {
                continue;
            }
            placed_seen.push(reference.clone());
            out.push(SceneObjectModel {
                object: reference,
                owner: source.group.reference(),
                parent: Some(object_ref.clone()),
                kind: SceneObjectRowKind::Object,
                controls: source.owned(),
                capabilities: SceneObjectCapabilities {
                    delete: true,
                    transform: true,
                    fluid_role: true,
                    ..SceneObjectCapabilities::default()
                },
                slot: None,
                liquid_domain: None,
                swarm: None,
                doc_path: source.role.doc_path(),
            });
        }
    }
    Ok(out)
}

/// One graph level: a body's nodes and wires.
#[derive(Clone, Copy)]
struct Level<'a> {
    nodes: &'a [EffectGraphNode],
    wires: &'a [EffectGraphWire],
}

impl<'a> Level<'a> {
    fn node(&self, id: u32) -> Option<&'a EffectGraphNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    fn producer(&self, to_node: u32, to_port: &str) -> Option<(u32, &'a str)> {
        self.wires
            .iter()
            .find(|wire| wire.to_node == to_node && wire.to_port == to_port)
            .map(|wire| (wire.from_node, wire.from_port.as_str()))
    }
}

/// Every node upstream of `start` (exclusive) in one level.
fn upstream(level: &Level<'_>, start: &[u32]) -> Vec<u32> {
    let mut seen: Vec<u32> = Vec::new();
    let mut pending: Vec<u32> = start.to_vec();
    while let Some(id) = pending.pop() {
        for wire in level.wires.iter().filter(|wire| wire.to_node == id) {
            if !seen.contains(&wire.from_node) {
                seen.push(wire.from_node);
                pending.push(wire.from_node);
            }
        }
    }
    seen
}

/// The groups entered from the root, outermost first, and the level each
/// opens; `levels[0]` is the root.
#[derive(Clone)]
struct Path<'a> {
    groups: Vec<&'a EffectGraphNode>,
    levels: Vec<Level<'a>>,
}

/// A node and the path to the level holding it.
#[derive(Clone)]
struct At<'a> {
    path: Path<'a>,
    node: &'a EffectGraphNode,
}

impl<'a> Path<'a> {
    fn root(graph: &'a EffectGraphDef) -> Self {
        Self { groups: Vec::new(), levels: vec![Level { nodes: &graph.nodes, wires: &graph.wires }] }
    }

    fn level(&self) -> Level<'a> {
        *self.levels.last().expect("a path has its root level")
    }

    fn enter(&self, group: &'a EffectGraphNode) -> Option<Self> {
        let body = group.group.as_deref()?;
        let mut path = self.clone();
        path.groups.push(group);
        path.levels.push(Level { nodes: &body.nodes, wires: &body.wires });
        Some(path)
    }

    fn leave(&self) -> Option<(Self, &'a EffectGraphNode)> {
        let mut path = self.clone();
        let group = path.groups.pop()?;
        path.levels.pop();
        Some((path, group))
    }

    fn doc_scope(&self) -> Vec<u32> {
        self.groups.iter().map(|group| group.id).collect()
    }

    fn reference(&self, node: &EffectGraphNode) -> SceneNodeRef {
        SceneNodeRef { scope: self.groups.iter().map(|group| group.node_id.clone()).collect(), node: node.node_id.clone() }
    }

    /// The node `reference` names, from this path's root.
    fn root_locate(&self, reference: &SceneNodeRef) -> Option<At<'a>> {
        let mut path = Path { groups: Vec::new(), levels: vec![self.levels[0]] };
        for group in &reference.scope {
            let node = path.level().nodes.iter().find(|node| &node.node_id == group)?;
            path = path.enter(node)?;
        }
        let node = path.level().nodes.iter().find(|node| node.node_id == reference.node)?;
        Some(At { path, node })
    }

    fn locate(graph: &'a EffectGraphDef, reference: &SceneNodeRef) -> Option<At<'a>> {
        Path::root(graph).root_locate(reference)
    }

    /// The node feeding `node`'s `port` at this level, entering groups
    /// through their outputs.
    fn producer_inward(&self, node: u32, port: &str) -> Option<At<'a>> {
        let (from, from_port) = self.level().producer(node, port)?;
        self.resolve_inward(from, from_port)
    }

    fn resolve_inward(&self, from: u32, from_port: &str) -> Option<At<'a>> {
        let mut path = self.clone();
        let mut node = path.level().node(from)?;
        let mut port = from_port;
        for _ in 0..64 {
            if node.type_id != GROUP_TYPE_ID {
                return Some(At { path, node });
            }
            let inner = path.enter(node)?;
            // An imported group may carry one output sentinel per port.
            let (id, inner_port) = inner
                .level()
                .nodes
                .iter()
                .filter(|candidate| candidate.type_id == GROUP_OUTPUT_TYPE_ID)
                .find_map(|output| inner.level().producer(output.id, port))?;
            node = inner.level().node(id)?;
            port = inner_port;
            path = inner;
        }
        None
    }

    /// The node feeding `node`'s `port`, entering a group through its output
    /// or leaving the level through its group input.
    fn producer_either(&self, node: u32, port: &str) -> Option<At<'a>> {
        let (from, from_port) = self.level().producer(node, port)?;
        let source = self.level().node(from)?;
        if source.type_id == GROUP_INPUT_TYPE_ID {
            let (outer, group) = self.leave()?;
            return outer.producer_inward(group.id, from_port);
        }
        self.resolve_inward(from, from_port)
    }

    /// `group`, a node of this level, located with the path to it.
    fn parent_at(&self, group: &'a EffectGraphNode) -> At<'a> {
        let (outer, _) = self.leave().expect("a grouped node has an enclosing level");
        At { path: outer, node: group }
    }
}

impl<'a> At<'a> {
    fn reference(&self) -> SceneNodeRef {
        self.path.reference(self.node)
    }

    fn doc_path(&self) -> Vec<u32> {
        let mut path = self.path.doc_scope();
        path.push(self.node.id);
        path
    }
}

/// The nearest enclosing group of `at` whose body holds a liquid domain, when
/// that group owns a Water family.
fn family_owner<'a>(at: &At<'a>) -> Option<At<'a>> {
    let mut path = at.path.clone();
    while let Some((outer, group)) = path.leave() {
        let body = group.group.as_deref()?;
        if !crate::liquid_domain::liquid_domains_in(&body.nodes).is_empty() {
            water_family(body)?;
            return Some(At { path: outer, node: group });
        }
        path = outer;
    }
    None
}

/// The family's domain node, located under its owner.
fn family_domain_at<'a>(owner: &At<'a>, family: &WaterFamily) -> Option<At<'a>> {
    let mut path = owner.path.enter(owner.node)?;
    let mut node = path.level().node(family.domain)?;
    while !is_liquid_domain(&node.type_id) {
        path = path.enter(node)?;
        node = path.level().nodes.iter().find(|candidate| {
            is_liquid_domain(&candidate.type_id)
                || candidate.group.as_deref().is_some_and(|body| !crate::liquid_domain::liquid_domains_in(&body.nodes).is_empty())
        })?;
    }
    Some(At { path, node })
}

/// Every fluid role source anywhere inside `group`, except `skip`.
fn group_role_sources(group: &At<'_>, skip: &[SceneNodeRef]) -> Vec<SceneNodeRef> {
    fn collect(path: &Path<'_>, out: &mut Vec<SceneNodeRef>) {
        for node in path.level().nodes {
            if node.type_id == FLUID_ROLE_SOURCE_TYPE_ID && !node.node_id.is_empty() {
                out.push(path.reference(node));
            }
            if node.group.is_some()
                && let Some(inner) = path.enter(node)
            {
                collect(&inner, out);
            }
        }
    }
    let mut out = Vec::new();
    if let Some(body) = group.path.enter(group.node) {
        collect(&body, &mut out);
    }
    out.retain(|reference| !skip.contains(reference));
    out
}

fn push_unique(controls: &mut Vec<SceneNodeRef>, reference: SceneNodeRef) {
    if !reference.node.is_empty() && !controls.contains(&reference) {
        controls.push(reference);
    }
}

struct Parent<'a> {
    at: At<'a>,
}

/// A placed Inflow, Drain or Collider: its role node and the group holding it.
struct Placed<'a> {
    role: At<'a>,
    group: At<'a>,
}

impl Placed<'_> {
    fn owned(&self) -> Vec<SceneNodeRef> {
        let Some(body) = self.group.path.enter(self.group.node) else { return Vec::new() };
        body.level()
            .nodes
            .iter()
            .filter(|node| !node.node_id.is_empty() && node.type_id != GROUP_OUTPUT_TYPE_ID && node.type_id != GROUP_INPUT_TYPE_ID)
            .map(|node| body.reference(node))
            .collect()
    }
}

struct Row<'a> {
    slot: u32,
    at: At<'a>,
    kind: SceneObjectRowKind,
    family: Option<(At<'a>, WaterFamily)>,
    compound: Option<At<'a>>,
    /// The domain and whether it builds this row's surface.
    liquid: Option<(At<'a>, bool)>,
}

impl<'a> Row<'a> {
    fn object(slot: u32, at: At<'a>) -> Self {
        Self { slot, at, kind: SceneObjectRowKind::Object, family: None, compound: None, liquid: None }
    }

    fn is_parent_of(&self, child: &Row<'_>) -> bool {
        self.kind == SceneObjectRowKind::PhysicsParent
            && matches!((&self.family, &child.family), (Some((owner, family)), Some((child_owner, _)))
                if owner.node.id == child_owner.node.id && owner.path.doc_scope() == child_owner.path.doc_scope()
                    && family.children.contains(&child.at.node.id))
    }

    /// This water's look rows in the family's output order, not render slot
    /// order: the tree lists Foam, Spray, Bubbles, then the dots.
    fn children<'r>(&self, rows: &'r [Row<'a>]) -> Vec<&'r Row<'a>> {
        let Some((_, family)) = &self.family else { return Vec::new() };
        family
            .children
            .iter()
            .filter_map(|id| rows.iter().find(|row| row.at.node.id == *id && row.kind == SceneObjectRowKind::LookChild && self.is_parent_of(row)))
            .collect()
    }

    /// The standalone source groups feeding this row's domain.
    fn placed_sources(&self) -> Vec<Placed<'a>> {
        let Some((domain, _)) = &self.liquid else { return Vec::new() };
        let mut out = Vec::new();
        for k in 0..MAX_FLUID_ROLES {
            let Some(role) = domain.path.producer_inward(domain.node.id, &format!("role_{k}")) else { continue };
            if role.node.type_id != FLUID_ROLE_SOURCE_TYPE_ID || role.path.groups.len() <= domain.path.groups.len() {
                continue;
            }
            let group = domain.path.groups.len();
            let group_node = role.path.groups[group];
            let mut group_path = role.path.clone();
            group_path.groups.truncate(group);
            group_path.levels.truncate(group + 1);
            let group_at = At { path: group_path, node: group_node };
            let body = group_at.path.enter(group_node);
            if body.is_none_or(|body| body.level().nodes.iter().any(|node| node.type_id == SCENE_OBJECT_TYPE_ID)) {
                continue;
            }
            if !out.iter().any(|placed: &Placed<'_>| placed.role.node.id == role.node.id) {
                out.push(Placed { role, group: group_at });
            }
        }
        out
    }

    fn model(&self, rows: &[Row<'_>], placed: &[Placed<'_>]) -> SceneObjectModel {
        let mut model = self.identity(rows);
        model.controls = self.controls(placed);
        model
    }

    /// The row with no controls: an imported model's part leaves them to
    /// its model row, so it never walks for them.
    fn identity(&self, rows: &[Row<'_>]) -> SceneObjectModel {
        let object = self.at.reference();
        let owner = match (&self.family, self.at.path.groups.last()) {
            (Some((family_owner, _)), _) => family_owner.reference(),
            (None, Some(group)) => self.at.path.parent_at(group).reference(),
            (None, None) => object.clone(),
        };
        let parent = match self.kind {
            SceneObjectRowKind::LookChild => rows.iter().find(|row| row.is_parent_of(self)).map(|row| row.at.reference()),
            _ => None,
        };
        let liquid = self.liquid.is_some();
        let capabilities = match self.kind {
            SceneObjectRowKind::LookChild => SceneObjectCapabilities { rename: true, hide: true, ..SceneObjectCapabilities::default() },
            SceneObjectRowKind::PhysicsParent => SceneObjectCapabilities {
                rename: true,
                delete: true,
                modifiers: true,
                ..SceneObjectCapabilities::default()
            },
            SceneObjectRowKind::Object => SceneObjectCapabilities {
                rename: true,
                delete: true,
                duplicate: true,
                transform: !liquid,
                enable_physics: !liquid,
                fluid_role: !liquid,
                modifiers: true,
                hide: false,
            },
        };
        SceneObjectModel {
            object,
            owner,
            parent,
            kind: self.kind,
            controls: Vec::new(),
            capabilities,
            slot: Some(self.slot),
            liquid_domain: self.liquid.as_ref().map(|(domain, _)| domain.reference()),
            swarm: self.swarm().map(|swarm| swarm.reference()),
            doc_path: self.at.doc_path(),
        }
    }

    /// The swarm an ordinary object places copies of.
    fn swarm(&self) -> Option<At<'a>> {
        (self.kind == SceneObjectRowKind::Object && self.liquid.is_none()).then(|| swarm_of(&self.at)).flatten()
    }

    fn controls(&self, placed: &[Placed<'_>]) -> Vec<SceneNodeRef> {
        let liquid = self.liquid.is_some();
        let mut controls = Vec::new();
        if self.kind == SceneObjectRowKind::LookChild {
            controls.extend(look_mesh(&self.at));
        } else {
            let placed_refs: Vec<SceneNodeRef> = placed.iter().flat_map(Placed::owned).collect();
            if let Some((domain, surface)) = &self.liquid {
                liquid_controls(domain, &mut controls);
                if *surface {
                    surface_controls(&self.at, &mut controls);
                }
            }
            // A grouped object owns the role sources beside it.
            for node in self.at.path.level().nodes.iter().filter(|node| node.type_id == FLUID_ROLE_SOURCE_TYPE_ID) {
                if !self.at.path.groups.is_empty() {
                    push_unique(&mut controls, self.at.path.reference(node));
                }
            }
            if let Some(swarm) = self.swarm() {
                swarm_controls(&swarm, &mut controls);
            }
            if liquid {
                for switch in chain_switches(&self.at) {
                    push_unique(&mut controls, switch);
                }
            }
            if let Some(group) = self.at.path.groups.last() {
                let owner_at = match &self.family {
                    Some((family_owner, _)) => family_owner.clone(),
                    None => self.at.path.parent_at(group),
                };
                for reference in group_role_sources(&owner_at, &placed_refs) {
                    push_unique(&mut controls, reference);
                }
            }
        }
        controls
    }
}

/// The swarm forces node upstream of the object's `instances`, on the
/// level that places them.
fn swarm_of<'a>(object: &At<'a>) -> Option<At<'a>> {
    let start = object.path.producer_inward(object.node.id, "instances")?;
    let level = start.path.level();
    let node = std::iter::once(start.node.id)
        .chain(upstream(&level, &[start.node.id]))
        .filter_map(|id| level.node(id))
        .find(|node| node.type_id == SWARM_FORCES_TYPE_ID)?;
    Some(At { path: start.path, node })
}

/// A swarm's forces node and the transform placing its box.
fn swarm_controls(swarm: &At<'_>, controls: &mut Vec<SceneNodeRef>) {
    push_unique(controls, swarm.reference());
    if let Some(bounds) = swarm.path.producer_either(swarm.node.id, "bounds")
        && bounds.node.type_id == TRANSFORM_3D_TYPE_ID
    {
        push_unique(controls, bounds.reference());
    }
}

/// A look's platonic mesh behind its `vertices`, through nested groups, a
/// mesh switch or a single-input modifier.
fn look_mesh(object: &At<'_>) -> Option<SceneNodeRef> {
    let mut at = object.path.producer_inward(object.node.id, "vertices")?;
    for _ in 0..64 {
        if at.node.type_id == LOOK_MESH_TYPE_ID {
            return Some(at.reference());
        }
        let next = if at.node.type_id == SWITCH_MESH_TYPE_ID {
            ["in_0", "in_1"].into_iter().find_map(|port| at.path.producer_inward(at.node.id, port))
        } else {
            at.path.producer_inward(at.node.id, "in")
        };
        at = next?;
    }
    None
}

/// The domain, the transforms placing it, the whitewater stepping on its
/// clock, the shared values feeding them, and the role sources inside the
/// water that no visible object or standalone source group owns.
fn liquid_controls(domain: &At<'_>, controls: &mut Vec<SceneNodeRef>) {
    let level = domain.path.level();
    push_unique(controls, domain.reference());
    for port in ["domain", "emitter", "initial_volume"] {
        if let Some(source) = domain.path.producer_either(domain.node.id, port)
            && source.node.type_id == TRANSFORM_3D_TYPE_ID
        {
            push_unique(controls, source.reference());
        }
    }
    let mut water_nodes = vec![domain.node.id];
    for whitewater in level.nodes.iter().filter(|node| node.type_id == "node.whitewater_step") {
        if ["ticks", "epoch"].iter().any(|port| level.producer(whitewater.id, port).is_some_and(|(from, _)| from == domain.node.id)) {
            push_unique(controls, domain.path.reference(whitewater));
            water_nodes.push(whitewater.id);
        }
    }
    for wire in level.wires.iter().filter(|wire| water_nodes.contains(&wire.to_node)) {
        if let Some(value) = level.node(wire.from_node).filter(|node| node.type_id == "node.value") {
            push_unique(controls, domain.path.reference(value));
        }
    }
    for k in 0..MAX_FLUID_ROLES {
        let Some(role) = domain.path.producer_inward(domain.node.id, &format!("role_{k}")) else { continue };
        if role.node.type_id != FLUID_ROLE_SOURCE_TYPE_ID {
            continue;
        }
        // A source group is its own row, and a visible source object owns
        // its role.
        if role.path.groups.len() > domain.path.groups.len() {
            continue;
        }
        push_unique(controls, role.reference());
        for port in ["transform", "source_transform"] {
            if let Some(source) = role.path.producer_inward(role.node.id, port)
                && source.node.type_id == TRANSFORM_3D_TYPE_ID
                // The box a collider role follows is the object's that draws
                // the same transform.
                && !source.path.level().nodes.iter().any(|object| {
                    object.type_id == SCENE_OBJECT_TYPE_ID
                        && source.path.level().producer(object.id, "transform").is_some_and(|(from, _)| from == source.node.id)
                })
            {
                push_unique(controls, source.reference());
            }
        }
    }
}

/// The surface chain behind the object's `vertices`: every node upstream of
/// its mesh in that level, stopping at group inputs and liquid domains.
fn surface_controls(object: &At<'_>, controls: &mut Vec<SceneNodeRef>) {
    let Some(mut at) = object.path.producer_inward(object.node.id, "vertices") else { return };
    for _ in 0..64 {
        if at.node.type_id != SWITCH_MESH_TYPE_ID {
            break;
        }
        let Some(next) = ["in_0", "in_1"].into_iter().find_map(|port| at.path.producer_inward(at.node.id, port)) else { return };
        at = next;
    }
    let level = at.path.level();
    let mut pending = vec![at.node.id];
    let mut seen: Vec<u32> = Vec::new();
    while let Some(id) = pending.pop() {
        if seen.contains(&id) {
            continue;
        }
        seen.push(id);
        let Some(node) = level.node(id) else { continue };
        if node.type_id == GROUP_INPUT_TYPE_ID || is_liquid_domain(&node.type_id) {
            continue;
        }
        push_unique(controls, at.path.reference(node));
        pending.extend(level.wires.iter().filter(|wire| wire.to_node == id).map(|wire| wire.from_node));
    }
}

/// The switches between an object and its surface: an index switch on
/// `indices` and every mesh switch along `vertices`.
fn chain_switches(object: &At<'_>) -> Vec<SceneNodeRef> {
    let mut out = Vec::new();
    if let Some(switch) = object.path.producer_inward(object.node.id, "indices")
        && switch.node.type_id == SWITCH_INDICES_TYPE_ID
    {
        out.push(switch.reference());
    }
    let mut cursor = object.path.producer_inward(object.node.id, "vertices");
    for _ in 0..64 {
        let Some(at) = cursor else { break };
        if at.node.type_id != SWITCH_MESH_TYPE_ID {
            break;
        }
        out.push(at.reference());
        cursor = ["in_0", "in_1"].into_iter().find_map(|port| at.path.producer_inward(at.node.id, port));
    }
    out
}

/// Every stable id inside `node`, the node itself included.
fn collect_node_ids(node: &EffectGraphNode, out: &mut Vec<NodeId>) {
    if !node.node_id.is_empty() {
        out.push(node.node_id.clone());
    }
    for inner in node.group.as_deref().map_or(&[][..], |body| body.nodes.as_slice()) {
        collect_node_ids(inner, out);
    }
}

/// Remove a placed Inflow, Drain or Collider: its group, the wire into its
/// water's role input, and every card row bound inside it.
pub fn remove_placed_source(
    graph: &EffectGraphDef,
    source: &SceneNodeRef,
) -> Result<crate::scene_graph_edit::SceneGraphEdit, SceneGraphEditError> {
    let error = |message: &str| SceneGraphEditError { at: Some(source.clone()), message: message.into() };
    let role = Path::locate(graph, source).ok_or_else(|| error("placed source not found"))?;
    if role.node.type_id != FLUID_ROLE_SOURCE_TYPE_ID {
        return Err(error("not a fluid source"));
    }
    // The group the tree lists: the outermost one wired into a domain's role
    // input, however deep the role node sits inside it.
    let (depth, group) = role
        .path
        .groups
        .iter()
        .enumerate()
        .find(|(depth, group)| {
            let outer = role.path.levels[*depth];
            outer.wires.iter().any(|wire| wire.from_node == group.id
                && wire.to_port.starts_with("role_")
                && outer.node(wire.to_node).is_some_and(|node| is_liquid_domain(&node.type_id)))
        })
        .map(|(depth, group)| (depth, *group))
        .ok_or_else(|| error("the source feeds no water"))?;
    let body = group.group.as_deref().ok_or_else(|| error("source group has no body"))?;
    if body.nodes.iter().any(|node| node.type_id == SCENE_OBJECT_TYPE_ID) {
        return Err(error("a source drawn by an object is removed with that object"));
    }
    let mut removed_nodes = Vec::new();
    collect_node_ids(group, &mut removed_nodes);
    let mut graph = graph.clone();
    let scope: Vec<NodeId> = role.path.groups[..depth].iter().map(|group| group.node_id.clone()).collect();
    let group_id = group.id;
    let (nodes, wires) = level_mut(&mut graph, &scope).ok_or_else(|| error("source scope not found"))?;
    nodes.retain(|node| node.id != group_id);
    wires.retain(|wire| wire.from_node != group_id && wire.to_node != group_id);
    let mut removed_param_ids = Vec::new();
    if let Some(metadata) = graph.preset_metadata.as_mut() {
        let inside = |target: &crate::effect_graph_def::BindingTarget| {
            matches!(target, crate::effect_graph_def::BindingTarget::Node { node_id, .. } if removed_nodes.contains(node_id))
        };
        let dropped: Vec<String> = metadata.bindings.iter().filter(|binding| inside(&binding.target)).map(|binding| binding.id.clone()).collect();
        metadata.bindings.retain(|binding| !inside(&binding.target));
        metadata.string_bindings.retain(|binding| !inside(&binding.target));
        // A control fanned out to a node outside the source keeps its row.
        metadata.params.retain(|spec| {
            let keep = !dropped.contains(&spec.id) || metadata.bindings.iter().any(|binding| binding.id == spec.id);
            if !keep {
                removed_param_ids.push(spec.id.clone());
            }
            keep
        });
    }
    Ok(crate::scene_graph_edit::SceneGraphEdit { graph, removed_param_ids, parameter_id_remaps: Vec::new() })
}

fn level_mut<'a>(
    graph: &'a mut EffectGraphDef,
    scope: &[NodeId],
) -> Option<(&'a mut Vec<EffectGraphNode>, &'a mut Vec<EffectGraphWire>)> {
    let (mut nodes, mut wires) = (&mut graph.nodes, &mut graph.wires);
    for group in scope {
        let body = nodes.iter_mut().find(|node| &node.node_id == group)?.group.as_deref_mut()?;
        nodes = &mut body.nodes;
        wires = &mut body.wires;
    }
    Some((nodes, wires))
}

#[cfg(test)]
#[path = "scene_object_model_tests.rs"]
mod tests;
