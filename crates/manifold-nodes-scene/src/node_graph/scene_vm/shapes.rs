//! Every declared shape in the scene, with the params it names resolved to
//! their write address, authored value and wired flag
//! (PHYSICS_ENGINE_BOUNDARY_DESIGN.md section 5.2 (One scene-object model)).
//! Node shapes come from the scene table (`scene_shape`), force shapes from
//! their preset manifest in the registry. The gizmo core builds every target from this list
//! alone; nothing here knows a handle.

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode, GROUP_INPUT_TYPE_ID};
use manifold_core::effects::apply_card_reshape;
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::scene_modifier_preset::{
    SceneModifierInstanceDef, SceneTargetSelection, is_force_recipe, scene_modifier_macro_lock_reason,
};
use manifold_foundation::scene_surface::{SceneShape, SceneShapeDecl};
use manifold_node_engine::load::catalog_source::preset_def;

use manifold_core::liquid_setup::DEFAULT_FILL_HEIGHT;

use super::setup_check::{SetupAnchor, SetupProblem, SetupSite, SiteLayout};
use super::{Level, ParamAddr, SceneObjectVm, TRANSFORM_3D_TYPE_ID, TransformVm, locate_node, param_f32, resolve_producer_through_group, trace_transform};
use crate::node_graph::scene_surface::{SWARM_FORCES_TYPE_ID, scene_shape};

const FLUID_ROLE_SOURCE_TYPE_ID: &str = "node.fluid_role_source";

/// The roles a placed fluid source plays (`node.fluid_role_source`'s
/// `role` values 1 to 3). Initial Fill (0) is refused by the GPU domains
/// and has no shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FluidRole {
    Inflow,
    Outflow,
    Collider,
}

/// Who declares a shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ShapeOwner {
    /// A scene object row, by its `object_node_id`.
    Object(u32),
    /// A liquid domain node.
    LiquidDomain(NodeId),
    /// A fluid role source node.
    RoleSource { node: NodeId, role: FluidRole },
    /// A force scene modifier instance.
    Force(NodeId),
    /// A swarm's forces node, whose box its forces are laid over.
    Swarm(NodeId),
    /// A Physics World. It declares no shape; it owns the warning for
    /// copies its pool could not make.
    World(NodeId),
}

/// Where a shape param is written.
#[derive(Debug, Clone, PartialEq)]
pub enum ShapeWrite {
    /// A graph node param. The host writes it through its card when a card
    /// binds it, as that card's knob does.
    Node(ParamAddr),
    /// A host card param: a scene modifier's control. `scale` and `offset`
    /// compose the host and modifier bindings; with the card's own range,
    /// curve and invert they map the card value to the shape value.
    Card { id: String, scale: f32, offset: f32 },
    /// The shape's `via` port has no node yet. The host creates it before
    /// the first write (D8, "an unwired `transform_n` gets its atom created
    /// on first drag"); the value is the node's identity until then.
    Unplaced,
}

/// One param a shape names.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapeParamVm {
    pub id: String,
    pub write: ShapeWrite,
    /// The value the shape is placed at, as the knob shows it after its
    /// binding's reshape.
    pub value: f32,
    /// A wire feeds the param: its handle shows locked.
    pub wired: bool,
}

/// One declared shape on one owner.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneShapeVm {
    pub owner: ShapeOwner,
    /// Position in the owner's declaration list.
    pub index: usize,
    pub decl: SceneShapeDecl,
    /// The declared params in `decl.shape.param_ids()` order. Empty when
    /// `decl.via` names a port with no transform wired yet; only an object
    /// shape is listed then, so its first drag can create the transform.
    pub params: Vec<ShapeParamVm>,
    /// The parent space the params are in, as a model matrix. `None` is
    /// world space.
    pub frame: Option<[[f32; 4]; 4]>,
    /// Where an `axis` with no origin sits: the centre of the one liquid
    /// domain it acts on, else the world origin.
    pub anchor: [f32; 3],
    /// The scene panel row this shape selects, if any.
    pub row: Option<u32>,
    pub pickable: bool,
}

use manifold_core::NodeId;

pub(super) fn trace_shapes(
    root: &Level<'_>,
    def: &EffectGraphDef,
    objects: &[SceneObjectVm],
) -> (Vec<SceneShapeVm>, Vec<SetupProblem>, Vec<SetupAnchor>, Vec<SetupSite>) {
    let rows = known_rows(objects);
    let mut out = object_shapes(objects);
    let mut problems = Vec::new();
    let mut anchors = Vec::new();
    let mut sites = Vec::new();
    // Setup sites index into `out`: shapes appended after these keep them valid.
    liquid_shapes(root, &rows, &mut out, &mut problems, &mut anchors, &mut sites);
    swarm_shapes(root, &rows, &mut out);
    for instance in &def.scene_modifiers {
        if let Some(shape) = force_shape(def, instance, &rows, root) {
            out.push(shape);
        }
    }
    (out, problems, anchors, sites)
}

fn known_rows(objects: &[SceneObjectVm]) -> Vec<&super::SceneObjectKnownRow> {
    objects
        .iter()
        .filter_map(|object| match object {
            SceneObjectVm::Known(row) => Some(row.as_ref()),
            SceneObjectVm::Custom { .. } => None,
        })
        .collect()
}

/// Each scene object row's declared shapes, over its traced transform.
pub(crate) fn object_shapes(objects: &[SceneObjectVm]) -> Vec<SceneShapeVm> {
    let rows = known_rows(objects);
    let mut out = Vec::new();
    for row in &rows {
        // The model says which rows move by their transform: a liquid-built
        // surface is placed by its domain, a water look by its water.
        if !row.capabilities.transform || (row.transform.is_none() && !row.transform_chain_parseable) {
            continue;
        }
        let frame = row.parent_group_id.and_then(|id| {
            rows.iter().find(|parent| parent.object_node_id == id).and_then(|parent| parent.transform.as_ref())
        });
        let frame = frame.map(|t| {
            crate::node_graph::primitives::render_scene::model_matrix(
                [t.pos_value.0, t.pos_value.1, t.pos_value.2],
                [t.rot_value.0, t.rot_value.1, t.rot_value.2],
                [t.scale_value.0, t.scale_value.1, t.scale_value.2],
            )
        });
        for (index, decl) in scene_shape(super::SCENE_OBJECT_TYPE_ID).iter().enumerate() {
            let params = match row.transform.as_ref() {
                Some(transform) => {
                    let Some(params) = transform_params(decl, transform) else { continue };
                    params
                }
                None => unplaced_params(decl),
            };
            out.push(SceneShapeVm {
                owner: ShapeOwner::Object(row.object_node_id),
                index,
                decl: decl.clone(),
                params,
                frame,
                anchor: [0.0; 3],
                row: Some(row.object_node_id),
                pickable: row.visible_value && row.parent_group_id.is_none(),
            });
        }
    }
    out
}

/// Each liquid domain's shapes and its fluid role sources' shapes.
fn liquid_shapes(
    root: &Level<'_>,
    rows: &[&super::SceneObjectKnownRow],
    out: &mut Vec<SceneShapeVm>,
    problems: &mut Vec<SetupProblem>,
    anchors: &mut Vec<SetupAnchor>,
    sites: &mut Vec<SetupSite>,
) {
    let mut seen = Vec::new();
    for row in rows {
        let Some(domain) = row.liquid_domain.as_ref() else { continue };
        if seen.contains(&domain.node) {
            continue;
        }
        seen.push(domain.node.clone());
        let Some(at) = locate_node(root, domain) else { continue };
        let pickable = row.visible_value && row.parent_group_id.is_none();
        let first = out.len();
        let owner = ShapeOwner::LiquidDomain(at.node.node_id.clone());
        node_shapes(&at, owner.clone(), Some(row.object_node_id), pickable, out);
        let param = |id: &str, default: f32| {
            (ParamAddr { scope_path: at.scope.clone(), node_doc_id: at.node.id, param_id: id.to_owned() }, param_f32(at.node, id, default))
        };
        // `trace_fluid_domain` leaves both unset exactly when a wire drives the box.
        let checked = row.fluid_domain.is_some() || row.fluid_domain_error.is_some();
        // With the `domain` port unwired the solver runs its default cube,
        // sized by Domain Size alone; the cube still takes clicks and scales.
        let cube = checked && row.fluid_domain_transform.is_none();
        if cube && let Some(shape) = cube_shape(&at, owner.clone(), Some(row.object_node_id), pickable) {
            out.insert(first, shape);
        }
        let declared = |via: &str| out[first..].iter().position(|shape| shape.decl.via.as_deref() == Some(via)).map(|at| first + at);
        // The setup check reads a cube's size off its layout, not a box shape.
        let (box_shape, column) = (declared("domain").filter(|_| !cube), declared("initial_volume"));
        let (fill, fill_value) = param("fill_height", DEFAULT_FILL_HEIGHT);
        let fill_authored = at.level.producer(at.node.id, "fill_height").is_none().then_some(fill_value);
        let (size, resolution) = (param("domain_size", 4.0), param("resolution", 24.0));
        let structure = row.fluid_domain_transform.as_ref().map(|transform| ParamAddr {
            scope_path: transform.pos_addr.0.scope_path.clone(),
            node_doc_id: transform.node_doc_id,
            param_id: "billboard".to_owned(),
        });
        let layout = match (checked, row.fluid_domain_transform.clone()) {
            (false, _) => SiteLayout::Unchecked,
            (true, None) => SiteLayout::Cube { size, resolution },
            (true, Some(transform)) => SiteLayout::Box { transform: Box::new(transform), size, resolution },
        };
        let mut site = SetupSite {
            owner: owner.clone(),
            layout,
            box_shape,
            column,
            fill,
            fill_authored,
            sources: Vec::new(),
            structure: structure.into_iter().collect(),
        };
        for k in 0..manifold_core::fluid_domain::MAX_FLUID_ROLES {
            let Some((role_level, crossed, role, _)) = resolve_producer_through_group(&at.level, at.node.id, &format!("role_{k}"))
            else {
                continue;
            };
            if role.type_id != FLUID_ROLE_SOURCE_TYPE_ID || role.node_id.is_empty() || seen.contains(&role.node_id) {
                continue;
            }
            if role_level.producer(role.id, "role").is_some() {
                continue;
            }
            let mut role_scope = at.scope.clone();
            role_scope.extend(crossed);
            site.structure.push(ParamAddr { scope_path: role_scope, node_doc_id: role.id, param_id: "role".to_owned() });
            let kind = match param_f32(role, "role", 1.0).round() as i32 {
                1 => FluidRole::Inflow,
                2 => FluidRole::Outflow,
                3 => FluidRole::Collider,
                _ => continue,
            };
            seen.push(role.node_id.clone());
            let mut scope = at.scope.clone();
            scope.extend(crossed);
            let parent = crossed.map(|group| (Level { nodes: at.level.nodes, wires: at.level.wires }, group));
            let role_at = NodeAt { level: role_level, scope, node: role, parent };
            let owner = ShapeOwner::RoleSource { node: role.node_id.clone(), role: kind };
            let first = out.len();
            node_shapes(&role_at, owner, Some(row.object_node_id), pickable, out);
            site.sources.extend((first..out.len()).map(|at| (kind, at)));
        }
        site.check(out, &|_| None, problems, anchors);
        sites.push(site);
    }
}

/// Each swarm's forces box, at any group depth. It selects the object row
/// the scene object model gives the swarm, if any.
fn swarm_shapes(root: &Level<'_>, rows: &[&super::SceneObjectKnownRow], out: &mut Vec<SceneShapeVm>) {
    let mut levels = vec![(Level { nodes: root.nodes, wires: root.wires }, Vec::new(), None)];
    while let Some((level, scope, parent)) = levels.pop() {
        for node in level.nodes {
            if let Some(body) = node.group.as_deref() {
                let mut inner = scope.clone();
                inner.push(node.id);
                let outer = Level { nodes: level.nodes, wires: level.wires };
                levels.push((Level { nodes: &body.nodes, wires: &body.wires }, inner, Some((outer, node.id))));
            }
        }
        for node in level.nodes.iter().filter(|node| node.type_id == SWARM_FORCES_TYPE_ID) {
            let row = rows.iter().find(|row| {
                row.swarm.as_ref().and_then(|swarm| locate_node(root, swarm)).is_some_and(|at| at.node.id == node.id && at.scope == scope)
            });
            let pickable = row.is_none_or(|row| row.visible_value && row.parent_group_id.is_none());
            let parent = parent.as_ref().map(|(outer, group): &(Level<'_>, u32)| (Level { nodes: outer.nodes, wires: outer.wires }, *group));
            let at = NodeAt { level: Level { nodes: level.nodes, wires: level.wires }, scope: scope.clone(), node, parent };
            node_shapes(&at, ShapeOwner::Swarm(node.node_id.clone()), row.map(|row| row.object_node_id), pickable, out);
        }
    }
}

/// A node found in the authored graph: the level holding it, the group doc
/// ids from the root down to that level, and the level holding that group
/// with the group's doc id, so an input wired in from outside resolves.
pub(super) struct NodeAt<'a> {
    pub level: Level<'a>,
    pub scope: Vec<u32>,
    pub node: &'a EffectGraphNode,
    pub parent: Option<(Level<'a>, u32)>,
}

fn node_shapes(at: &NodeAt<'_>, owner: ShapeOwner, row: Option<u32>, pickable: bool, out: &mut Vec<SceneShapeVm>) {
    for (index, decl) in scene_shape(&at.node.type_id).iter().enumerate() {
        let Some(port) = decl.via.as_deref() else { continue };
        let Some(transform) = via_transform(at, port) else { continue };
        let Some(params) = transform_params(decl, &transform) else { continue };
        out.push(SceneShapeVm { owner: owner.clone(), index, decl: decl.clone(), params, frame: None, anchor: [0.0; 3], row, pickable });
    }
}

/// The `node.transform_3d` feeding `at.node`'s `port`, through a group
/// boundary in either direction.
fn via_transform(at: &NodeAt<'_>, port: &str) -> Option<TransformVm> {
    let (level, crossed, source, source_port) = resolve_producer_through_group(&at.level, at.node.id, port)?;
    if source.type_id == GROUP_INPUT_TYPE_ID {
        let (outer, group_id) = at.parent.as_ref()?;
        let (id, _) = outer.producer(*group_id, source_port)?;
        let outer_scope = &at.scope[..at.scope.len().checked_sub(1)?];
        return (outer.node(id)?.type_id == TRANSFORM_3D_TYPE_ID).then(|| trace_transform(outer, outer_scope.to_vec(), id));
    }
    let mut scope = at.scope.clone();
    scope.extend(crossed);
    (source.type_id == TRANSFORM_3D_TYPE_ID).then(|| trace_transform(&level, scope, source.id))
}

/// The declared params read off a traced transform, by name. `None` when
/// the declaration names a param the transform does not have.
fn transform_params(decl: &SceneShapeDecl, t: &TransformVm) -> Option<Vec<ShapeParamVm>> {
    let entries = [
        (&t.pos_addr.0, t.pos_value.0, t.pos_driven.0),
        (&t.pos_addr.1, t.pos_value.1, t.pos_driven.1),
        (&t.pos_addr.2, t.pos_value.2, t.pos_driven.2),
        (&t.rot_addr.0, t.rot_value.0, t.rot_driven.0),
        (&t.rot_addr.1, t.rot_value.1, t.rot_driven.1),
        (&t.rot_addr.2, t.rot_value.2, t.rot_driven.2),
        (&t.scale_addr.0, t.scale_value.0, t.scale_driven.0),
        (&t.scale_addr.1, t.scale_value.1, t.scale_driven.1),
        (&t.scale_addr.2, t.scale_value.2, t.scale_driven.2),
    ];
    decl.shape
        .param_ids()
        .map(|id| {
            let (addr, value, wired) = entries.iter().find(|(addr, _, _)| addr.param_id == id)?;
            Some(ShapeParamVm { id: id.to_string(), write: ShapeWrite::Node((*addr).clone()), value: *value, wired: *wired })
        })
        .collect()
}

/// The domain's default cube (`domain_layout` with no box), in place of the
/// box its `domain` transform would declare: it sits on the floor at the
/// world origin, Domain Size on every side. Each side writes Domain Size,
/// so the box scales uniformly; no param positions it, so its centre reads
/// as driven and does not move.
fn cube_shape(at: &NodeAt<'_>, owner: ShapeOwner, row: Option<u32>, pickable: bool) -> Option<SceneShapeVm> {
    let (index, decl) = scene_shape(&at.node.type_id).iter().enumerate().find(|(_, decl)| decl.via.as_deref() == Some("domain"))?;
    let SceneShape::Box { center, size: sides, .. } = &decl.shape else { return None };
    let size = ParamAddr { scope_path: at.scope.clone(), node_doc_id: at.node.id, param_id: "domain_size".to_owned() };
    let value = param_f32(at.node, "domain_size", 4.0);
    let driven = at.level.producer(at.node.id, "domain_size").is_some();
    let centre = [0.0, value * 0.5, 0.0];
    let params = center
        .iter()
        .zip(centre)
        .map(|(id, value)| (id, value, true))
        .chain(sides.iter().map(|id| (id, value, driven)))
        .map(|(id, value, wired)| ShapeParamVm { id: id.to_string(), write: ShapeWrite::Node(size.clone()), value, wired })
        .collect();
    Some(SceneShapeVm { owner, index, decl: decl.clone(), params, frame: None, anchor: [0.0; 3], row, pickable })
}

/// The params of a shape whose `via` node does not exist yet, at the
/// identity transform: no offset or turn, unit size.
fn unplaced_params(decl: &SceneShapeDecl) -> Vec<ShapeParamVm> {
    let unit = match &decl.shape {
        SceneShape::Box { size, .. } => Some(size),
        SceneShape::Sphere { .. } | SceneShape::Plane { .. } | SceneShape::PointRadius { .. } | SceneShape::Axis { .. } => None,
    };
    decl.shape
        .param_ids()
        .map(|id| ShapeParamVm {
            id: id.to_string(),
            write: ShapeWrite::Unplaced,
            value: if unit.is_some_and(|size| size.iter().any(|unit| unit == id)) { 1.0 } else { 0.0 },
            wired: false,
        })
        .collect()
}

/// A force's declared shape over the host's card params. `None` when the
/// modifier is not a force, declares no shape, or a named param has no
/// host card.
fn force_shape(
    def: &EffectGraphDef,
    instance: &SceneModifierInstanceDef,
    rows: &[&super::SceneObjectKnownRow],
    root: &Level<'_>,
) -> Option<SceneShapeVm> {
    if !is_force_recipe(&instance.graph) {
        return None;
    }
    let local = instance.graph.preset_metadata.as_ref()?;
    let decl = declared_force_shape(&local.id)?;
    let host = def.preset_metadata.as_ref()?;
    let params = decl
        .shape
        .param_ids()
        .map(|id| {
            let binding = host.bindings.iter().find(|binding| {
                matches!(&binding.target, BindingTarget::SceneModifier { modifier_id, param_id }
                    if modifier_id == &instance.id && param_id == id)
            })?;
            let spec = host.params.iter().find(|spec| spec.id == binding.id)?;
            let (local_scale, local_offset) = local
                .bindings
                .iter()
                .find(|local| local.id == id)
                .map_or((1.0, 0.0), |local| (local.scale, local.offset));
            let scale = binding.scale * local_scale;
            let offset = binding.offset * local_scale + local_offset;
            let value = apply_card_reshape(binding.default_value, spec.min, spec.max, spec.invert, spec.curve, scale, offset);
            // The press refuses a macro-locked card, so its handle draws locked.
            let wired = scene_modifier_macro_lock_reason(def, &binding.id).is_some();
            Some(ShapeParamVm { id: id.to_string(), write: ShapeWrite::Card { id: binding.id.clone(), scale, offset }, value, wired })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(SceneShapeVm {
        owner: ShapeOwner::Force(instance.id.clone()),
        index: 0,
        decl,
        params,
        frame: None,
        anchor: force_anchor(&instance.targets, rows, root),
        row: None,
        pickable: true,
    })
}

/// The shape a force preset declares, read from the preset registry by its
/// type. The declaration is preset data: an instance's saved graph copy may
/// predate it, so the copy is never read for it.
fn declared_force_shape(preset: &PresetTypeId) -> Option<SceneShapeDecl> {
    preset_def(preset)?.preset_metadata.as_ref()?.shape.clone()
}

/// D-78: the centre of the one liquid domain a force acts on, else the
/// world origin.
fn force_anchor(targets: &SceneTargetSelection, rows: &[&super::SceneObjectKnownRow], root: &Level<'_>) -> [f32; 3] {
    let mut domains = Vec::new();
    for row in rows {
        let (Some(domain), Some(layout)) = (row.liquid_domain.as_ref(), row.fluid_domain) else { continue };
        let targeted = match targets {
            SceneTargetSelection::AllObjects => true,
            SceneTargetSelection::Explicit { objects } => objects.iter().any(|target| {
                target.node == row.object
                    || row.group_node_id.and_then(|id| root.node(id)).is_some_and(|group| group.node_id == target.node)
            }),
        };
        if targeted && !domains.iter().any(|(node, _)| node == &domain.node) {
            domains.push((domain.node.clone(), layout.transform().pos));
        }
    }
    match domains.as_slice() {
        [(_, centre)] => *centre,
        _ => [0.0; 3],
    }
}
