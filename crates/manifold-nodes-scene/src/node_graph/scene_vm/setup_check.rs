//! The setup check: every liquid's setup shapes against its domain, read
//! from the declared shapes at the authored values the knob shows
//! (SCENE_SETUP_PROGRAMME.md section 3 B3, BUG-gm927 part 1). One list per
//! scene, built with the `SceneVm`; the card's warning line and the red
//! outline in the viewport both read it, and the rule is
//! `manifold_core::liquid_setup`, the same fit the solvers fill from. A
//! Physics World's copies its pool could not make warn on the same cards,
//! under the copies bodies' Count rows ([`copies_problems`]).

use manifold_core::NodeId;
use manifold_core::physics_members::BodyRefusalRow;
use manifold_core::copies_body::{PHYSICS_WORLD_TYPE_ID, RIGID_BODY_TYPE_ID, copy_group_of_input};
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};
use manifold_core::fluid_domain::{FluidDomainLayout, FluidDomainSnapshot};
use manifold_core::liquid_setup::{
    AxisFit, BoxFit, ColumnOutcome, FillFit, SetupFit, axis_fits, fit_box, fit_fill_height,
};
use manifold_core::scene_doc::{ItemFields, SceneDoc, SourceRole, param_id};
use manifold_core::scene_schema::DOMAIN_ROLE;
use manifold_foundation::scene_surface::SceneShape;

use super::{FluidRole, ParamAddr, SceneShapeVm, ShapeOwner, ShapeWrite};

/// A copies body's Count param.
const COPY_COUNT: &str = "copy_count";

/// What does not fit. Each names the defined behaviour the solver applies,
/// so the warning says what was changed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SetupProblemKind {
    /// The domain box itself is not one the solvers take; the water does not
    /// run. Carries the solver's reason.
    DomainRefused(String),
    /// Fill Height is past the domain's top; the pool fills to the top.
    FillCapped,
    /// The initial column reaches past a wall; the part inside fills.
    ColumnClipped,
    /// The initial column is wholly outside; it fills nothing.
    ColumnOutside,
    /// An Inflow or Drain reaches past a wall; the part outside does nothing.
    SourceClipped(FluidRole),
    /// A source or collider is wholly outside; it does nothing.
    SourceOutside(FluidRole),
    /// The coupled world has more bodies than the liquid holds; the water
    /// neither pushes nor flows round this many.
    BodiesNotCoupled(u32),
    /// A Physics World's copy groups ask for more copies than its pool
    /// holds; the groups past the cap make fewer.
    CopiesNotMade(u32),
    /// The particle pool is full and an inflow wants more: the inflow is
    /// held back until water drains. Only the solver sees it.
    PoolFull,
    /// The world left a body or copy group out. Carries the world's reason.
    BodyRefused(String),
}

impl SetupProblemKind {
    /// The warning line, saying what was changed.
    pub fn message(&self) -> String {
        match self {
            Self::DomainRefused(reason) => reason.strip_prefix("Fluid: ").unwrap_or(reason).to_owned(),
            Self::FillCapped => "Fill is above the top; capped at the top".to_owned(),
            Self::ColumnClipped => "Column leaves the domain; clipped to it".to_owned(),
            Self::ColumnOutside => "Column is outside the domain; no water".to_owned(),
            Self::SourceClipped(role) => format!("{} leaves the domain; the rest is unused", role.title()),
            Self::SourceOutside(role) => format!("{} is outside the domain; it does nothing", role.title()),
            Self::BodiesNotCoupled(1) => "1 body not coupled; water marks each cell with one byte, so it takes 255 bodies and passes through the rest".to_owned(),
            Self::BodiesNotCoupled(count) => format!("{count} bodies not coupled; water marks each cell with one byte, so it takes 255 bodies and passes through the rest"),
            Self::CopiesNotMade(1) => "1 copy not made; the world's copy pool is full".to_owned(),
            Self::CopiesNotMade(count) => format!("{count} copies not made; the world's copy pool is full"),
            Self::PoolFull => "Water pool is full; the inflow is held back. Add a Drain or lower Resolution".to_owned(),
            Self::BodyRefused(reason) => sentence(reason),
        }
    }
}

/// `reason` as a card line: its first letter capitalised.
fn sentence(reason: &str) -> String {
    let mut chars = reason.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

impl FluidRole {
    fn title(self) -> &'static str {
        match self {
            Self::InitialFill => "Initial Fill",
            Self::Inflow => "Inflow",
            Self::Outflow => "Drain",
        }
    }
}

/// One shape that does not fit its domain.
#[derive(Debug, Clone, PartialEq)]
pub struct SetupProblem {
    /// Who declares the shape: the domain for its box, fill and column, the
    /// role source for a placed source.
    pub owner: ShapeOwner,
    pub kind: SetupProblemKind,
    /// The node params that place the shape, most telling first: the card
    /// puts the warning on the first one a card row shows.
    pub params: Vec<ParamAddr>,
    /// The shape as a world box: as authored, or as the solver ran it. The
    /// viewport outlines it in red; a capped fill is a flat box on the
    /// domain's top. `None` when nothing places it (a refused domain sized
    /// by its params alone): the card still warns, the viewport draws no box.
    pub bounds: Option<([f32; 3], [f32; 3])>,
}

impl SetupProblem {
    /// What the card shows of it: who, what and the rows it sits under,
    /// never where. The planner keys the cards on this alone, so a driven
    /// shape moving inside its warning never re-projects them.
    pub fn hash_card(&self, state: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        self.owner.hash(state);
        self.kind.hash(state);
        self.params.hash(state);
    }

    /// Where the viewport outlines it this frame: a fill or column problem on
    /// a domain the frame reports reads that domain's live box, so a driven
    /// column is outlined where it is now without rebuilding the list.
    /// `live_columns[i]`, when given, is `domains[i]`'s column box held apart
    /// from its snapshot.
    pub fn live_bounds(&self, domains: &[(NodeId, FluidDomainSnapshot)], live_columns: &[Option<([f32; 3], [f32; 3])>]) -> Option<([f32; 3], [f32; 3])> {
        let ShapeOwner::LiquidDomain(node) = &self.owner else { return self.bounds };
        let Some(at) = domains.iter().position(|(id, _)| id == node) else { return self.bounds };
        let snapshot = &domains[at].1;
        let live = match self.kind {
            SetupProblemKind::FillCapped => snapshot.accepted_layout.as_ref().map(fill_top),
            SetupProblemKind::ColumnClipped | SetupProblemKind::ColumnOutside => live_columns.get(at).copied().flatten().or(snapshot.column_box),
            _ => None,
        };
        live.or(self.bounds)
    }
}

/// The flat box a capped fill outlines: the domain's top face.
fn fill_top(layout: &FluidDomainLayout) -> ([f32; 3], [f32; 3]) {
    let y = layout.min[1] + layout.size[1];
    ([layout.min[0], y, layout.min[2]], [layout.min[0] + layout.size[0], y, layout.min[2] + layout.size[2]])
}

/// A domain whose setup is read: the layout the solver takes, or the reason
/// it refuses the box.
pub(super) enum DomainState<'a> {
    Layout(&'a FluidDomainLayout),
    Refused(&'a str),
}

/// A shape's declared box in world space as `(min, max)`, with the param
/// ids of its center and size per axis. `None` when the shape is not a box,
/// names a param it lacks, or a wire drives any of them (a wired value is
/// not the authored one).
pub(super) fn declared_box(shape: &SceneShapeVm) -> Option<([f32; 3], [f32; 3])> {
    let SceneShape::Box { center, rotation, size } = &shape.decl.shape else { return None };
    let param = |id: &str| shape.params.iter().find(|param| param.id == id);
    let read = |ids: &[std::borrow::Cow<'static, str>; 3]| -> Option<[f32; 3]> {
        let mut out = [0.0; 3];
        for (slot, id) in out.iter_mut().zip(ids) {
            let param = param(id)?;
            if param.wired {
                return None;
            }
            *slot = param.value;
        }
        Some(out)
    };
    let (centre, size) = (read(center)?, read(size)?);
    let turn = match rotation {
        Some(ids) => read(ids)?,
        None => [0.0; 3],
    };
    if turn.iter().any(|angle| angle.abs() > 1e-6) {
        return Some(turned_bounds(centre, turn, size));
    }
    Some((std::array::from_fn(|d| centre[d] - 0.5 * size[d]), std::array::from_fn(|d| centre[d] + 0.5 * size[d])))
}

fn turned_bounds(centre: [f32; 3], turn: [f32; 3], size: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let m = crate::node_graph::primitives::render_scene::model_matrix(centre, turn, size);
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for corner in 0..8u8 {
        let local = [0, 1, 2].map(|d| if corner >> d & 1 == 1 { 0.5 } else { -0.5 });
        for d in 0..3 {
            let v = m[3][d] + (0..3).map(|j| m[j][d] * local[j]).sum::<f32>();
            min[d] = min[d].min(v);
            max[d] = max[d].max(v);
        }
    }
    (min, max)
}

/// The card rows of `shape` that place it along `axes` first (size, then
/// centre), then every other it names.
fn placing_params(shape: &SceneShapeVm, axes: impl Iterator<Item = usize> + Clone) -> Vec<ParamAddr> {
    let SceneShape::Box { center, size, .. } = &shape.decl.shape else { return Vec::new() };
    let addr = |id: &str| {
        shape.params.iter().find(|param| param.id == id).and_then(|param| match &param.write {
            ShapeWrite::Card { id, .. } => Some(ParamAddr { scope_path: Vec::new(), node_doc_id: 0, param_id: id.clone() }),
            ShapeWrite::Unplaced => None,
        })
    };
    let ids: Vec<&str> = axes.clone().map(|d| size[d].as_ref()).chain(axes.map(|d| center[d].as_ref())).collect();
    ids.into_iter().filter_map(addr).collect()
}

/// The axes on which `[lo, hi]` does not sit inside the layout, by the same
/// test the fit uses; every axis when none leaves.
fn axes_past(layout: &FluidDomainLayout, lo: [f32; 3], hi: [f32; 3]) -> Vec<usize> {
    let past: Vec<usize> = (0..3).filter(|&d| axis_fits(layout, lo, hi)[d] != AxisFit::Inside).collect();
    if past.is_empty() { vec![0, 1, 2] } else { past }
}

/// The problems of one liquid domain's setup, appended to `out`.
/// `fill` is the domain's Fill Height param address and its authored value,
/// `None` when a wire drives it.
pub(super) fn check_domain(
    domain: &ShapeOwner,
    state: DomainState<'_>,
    box_shape: Option<&SceneShapeVm>,
    column: Option<&SceneShapeVm>,
    fill: Option<(ParamAddr, f32)>,
    out: &mut Vec<SetupProblem>,
) {
    let layout = match state {
        DomainState::Layout(layout) => layout,
        DomainState::Refused(reason) => {
            if let Some(shape) = box_shape
                && let Some(bounds) = declared_box(shape)
            {
                out.push(SetupProblem {
                    owner: domain.clone(),
                    kind: SetupProblemKind::DomainRefused(reason.to_owned()),
                    params: placing_params(shape, 0..3),
                    bounds: Some(bounds),
                });
            }
            return;
        }
    };
    if let Some((addr, height)) = fill
        && let Ok(FillFit::Capped(_)) = fit_fill_height(layout, height)
    {
        out.push(SetupProblem { owner: domain.clone(), kind: SetupProblemKind::FillCapped, params: vec![addr], bounds: Some(fill_top(layout)) });
    }
    if let Some(shape) = column
        && let Some((lo, hi)) = declared_box(shape)
    {
        let kind = match fit_box(layout, lo, hi) {
            BoxFit::Inside => None,
            BoxFit::Clipped { .. } => Some(SetupProblemKind::ColumnClipped),
            BoxFit::Outside => Some(SetupProblemKind::ColumnOutside),
        };
        if let Some(kind) = kind {
            out.push(SetupProblem { owner: domain.clone(), kind, params: placing_params(shape, axes_past(layout, lo, hi).into_iter()), bounds: Some((lo, hi)) });
        }
    }
}

/// The problem of one placed source against its domain's layout, if any.
pub(super) fn check_source(
    layout: &FluidDomainLayout,
    owner: &ShapeOwner,
    role: FluidRole,
    shape: &SceneShapeVm,
    out: &mut Vec<SetupProblem>,
) {
    let Some((lo, hi)) = declared_box(shape) else { return };
    let kind = match fit_box(layout, lo, hi) {
        BoxFit::Inside => return,
        BoxFit::Clipped { .. } => SetupProblemKind::SourceClipped(role),
        BoxFit::Outside => SetupProblemKind::SourceOutside(role),
    };
    out.push(SetupProblem { owner: owner.clone(), kind, params: placing_params(shape, axes_past(layout, lo, hi).into_iter()), bounds: Some((lo, hi)) });
}

/// Where a liquid's setup shapes sit in the graph, whether or not a wire
/// drives them: what a problem the solver reports at run time hangs its card
/// line and outline on.
#[derive(Debug, Clone, PartialEq)]
pub struct SetupAnchor {
    pub owner: ShapeOwner,
    pub fill: ParamAddr,
    pub column: Vec<ParamAddr>,
    pub column_bounds: Option<([f32; 3], [f32; 3])>,
    pub domain: Vec<ParamAddr>,
    pub domain_bounds: Option<([f32; 3], [f32; 3])>,
}

impl SetupAnchor {
    pub(super) fn new(owner: ShapeOwner, fill: ParamAddr, box_shape: Option<&SceneShapeVm>, column: Option<&SceneShapeVm>) -> Self {
        Self {
            owner,
            fill,
            column: column.map_or_else(Vec::new, |shape| placing_params(shape, 0..3)),
            column_bounds: column.and_then(declared_box),
            domain: box_shape.map_or_else(Vec::new, |shape| placing_params(shape, 0..3)),
            domain_bounds: box_shape.and_then(declared_box),
        }
    }
}

/// What one liquid domain's solver, or one Physics World's copy pool,
/// reported about the setup it ran: the cards read it from the content
/// thread's fit report, the gizmo hosts from the frame's domain snapshots.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeFit<'a> {
    pub node: &'a NodeId,
    pub fit: SetupFit,
    pub refusal: Option<&'a str>,
    /// The domain box the solver accepted, where a capped fill outlines.
    pub layout: Option<&'a FluidDomainLayout>,
    /// The initial column as it ran, when it did not fit.
    pub column_box: Option<([f32; 3], [f32; 3])>,
    /// Coupled-world bodies past what the liquid holds.
    pub uncoupled_bodies: u32,
    /// A world's copies past what its pool holds.
    pub copies_not_made: u32,
}

impl<'a> RuntimeFit<'a> {
    pub fn of((node, snapshot): &'a (NodeId, FluidDomainSnapshot)) -> Self {
        Self {
            node,
            fit: snapshot.fit,
            refusal: snapshot.refusal.as_deref(),
            layout: snapshot.accepted_layout.as_ref(),
            column_box: snapshot.column_box,
            uncoupled_bodies: snapshot.uncoupled_bodies,
            copies_not_made: 0,
        }
    }
}

/// A Physics World that copies bodies, and the rows its copy shortfall
/// warns under: each copies body's Count, last group first. The pool fills
/// groups in port order, so the last ones go short.
#[derive(Debug, Clone, PartialEq)]
pub struct CopiesAnchor {
    pub world: NodeId,
    pub counts: Vec<ParamAddr>,
}

/// Every world in `def` that copies a body, at any group depth. Reads wires
/// alone, so a value change never moves one.
pub fn copies_anchors(def: &EffectGraphDef) -> Vec<CopiesAnchor> {
    let mut out = Vec::new();
    collect_copies_anchors(&def.nodes, &def.wires, &mut Vec::new(), &mut out);
    out
}

fn collect_copies_anchors(nodes: &[EffectGraphNode], wires: &[EffectGraphWire], scope: &mut Vec<u32>, out: &mut Vec<CopiesAnchor>) {
    for world in nodes.iter().filter(|n| n.type_id == PHYSICS_WORLD_TYPE_ID) {
        let mut groups: Vec<(usize, u32)> = wires
            .iter()
            .filter(|w| w.to_node == world.id)
            .filter_map(|w| Some((copy_group_of_input(&w.to_port)?, w.from_node)))
            .filter(|(_, body)| nodes.iter().any(|n| n.id == *body && n.type_id == RIGID_BODY_TYPE_ID))
            .collect();
        if groups.is_empty() {
            continue;
        }
        groups.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        let counts = groups
            .into_iter()
            .map(|(_, body)| ParamAddr { scope_path: scope.clone(), node_doc_id: body, param_id: COPY_COUNT.to_owned() })
            .collect();
        out.push(CopiesAnchor { world: world.node_id.clone(), counts });
    }
    for node in nodes {
        if let Some(group) = &node.group {
            scope.push(node.id);
            collect_copies_anchors(&group.nodes, &group.wires, scope, out);
            scope.pop();
        }
    }
}

/// Each world's copies its pool could not make, as one warning under its
/// Count rows, appended to `out`. A world declares no shape, so the
/// viewport outlines nothing.
pub fn copies_problems<'a>(anchors: &[CopiesAnchor], runtime: impl IntoIterator<Item = RuntimeFit<'a>>, out: &mut Vec<SetupProblem>) {
    for report in runtime.into_iter().filter(|report| report.copies_not_made > 0) {
        let Some(anchor) = anchors.iter().find(|anchor| &anchor.world == report.node) else { continue };
        out.push(SetupProblem {
            owner: ShapeOwner::World(anchor.world.clone()),
            kind: SetupProblemKind::CopiesNotMade(report.copies_not_made),
            params: anchor.counts.clone(),
            bounds: None,
        });
    }
}

/// The rows a refusal no control is named for warns under, most telling
/// first: what the body bounces and weighs with, or a copy group's Count.
const BODY_ROWS: [&str; 4] = ["bounce", "mass", "friction", COPY_COUNT];

/// The transform params a refusal can concern: they sit on the node wired
/// into the body's `transform`, not on the body.
const TRANSFORM_PREFIXES: [&str; 3] = ["pos_", "rot_", "scale_"];

/// A Physics World and the body wired into each of its inputs (`body_k`,
/// `copies_k`), for the rows a refused body warns under.
#[derive(Debug, Clone, PartialEq)]
pub struct BodyAnchor {
    pub world: NodeId,
    /// (world input, body node doc id).
    pub inputs: Vec<(String, u32)>,
    /// (body node doc id, the node wired into its `transform`).
    pub transforms: Vec<(u32, u32)>,
    pub scope_path: Vec<u32>,
}

/// Every world in `def` with a body wired in, at any group depth. Reads
/// wires alone, so a value change never moves one.
pub fn body_anchors(def: &EffectGraphDef) -> Vec<BodyAnchor> {
    let mut out = Vec::new();
    collect_body_anchors(&def.nodes, &def.wires, &mut Vec::new(), &mut out);
    out
}

fn collect_body_anchors(nodes: &[EffectGraphNode], wires: &[EffectGraphWire], scope: &mut Vec<u32>, out: &mut Vec<BodyAnchor>) {
    for world in nodes.iter().filter(|n| n.type_id == PHYSICS_WORLD_TYPE_ID) {
        let inputs: Vec<(String, u32)> = wires
            .iter()
            .filter(|w| w.to_node == world.id && manifold_core::physics_members::member_input(&w.to_port).is_some())
            .map(|w| (w.to_port.clone(), w.from_node))
            .collect();
        let transforms = inputs
            .iter()
            .filter_map(|(_, body)| wires.iter().find(|w| w.to_node == *body && w.to_port == "transform").map(|w| (*body, w.from_node)))
            .collect();
        if !inputs.is_empty() {
            out.push(BodyAnchor { world: world.node_id.clone(), inputs, transforms, scope_path: scope.clone() });
        }
    }
    for node in nodes {
        if let Some(group) = &node.group {
            scope.push(node.id);
            collect_body_anchors(&group.nodes, &group.wires, scope, out);
            scope.pop();
        }
    }
}

/// Each body or copy group a world left out, as a warning on the control it
/// concerns, then under the body's own rows: `refused` is (world, refusal).
pub fn body_problems<'a>(anchors: &[BodyAnchor], refused: impl IntoIterator<Item = (&'a NodeId, &'a BodyRefusalRow)>, out: &mut Vec<SetupProblem>) {
    for (world, row) in refused {
        let Some(anchor) = anchors.iter().find(|anchor| &anchor.world == world) else { continue };
        let Some(&(_, body)) = anchor.inputs.iter().find(|(port, _)| **port == *row.input) else { continue };
        let at = |node_doc_id: u32, param: &str| ParamAddr { scope_path: anchor.scope_path.clone(), node_doc_id, param_id: param.to_owned() };
        let concerned = row.param.and_then(|param| {
            if !TRANSFORM_PREFIXES.iter().any(|prefix| param.starts_with(prefix)) {
                return Some(at(body, param));
            }
            let &(_, transform) = anchor.transforms.iter().find(|(of, _)| *of == body)?;
            Some(at(transform, param))
        });
        let params = concerned.into_iter().chain(BODY_ROWS.iter().map(|param| at(body, param))).collect();
        out.push(SetupProblem {
            owner: ShapeOwner::World(anchor.world.clone()),
            kind: SetupProblemKind::BodyRefused(row.reason.to_string()),
            params,
            bounds: None,
        });
    }
}

/// Two problems a card shows in one place: the fill, the column (clipped or
/// outside), the uncoupled bodies whatever their count, or one refusal reason.
fn same_slot(a: &SetupProblemKind, b: &SetupProblemKind) -> bool {
    use SetupProblemKind::{BodiesNotCoupled, ColumnClipped, ColumnOutside};
    matches!(
        (a, b),
        (ColumnClipped | ColumnOutside, ColumnClipped | ColumnOutside) | (BodiesNotCoupled(_), BodiesNotCoupled(_))
    ) || a == b
}

/// The scene's authored problems, then what each domain's solver reported
/// it did on the values it ran: a driven, modulated or wired fill or column
/// is capped or clipped the same as an authored one, and shows the same
/// way. Where both name one slot, what ran wins and the authored rows keep
/// their order. `out` is cleared first.
pub fn problems_with_runtime<'a>(
    anchors: &[SetupAnchor],
    authored: &[SetupProblem],
    runtime: impl IntoIterator<Item = RuntimeFit<'a>>,
    out: &mut Vec<SetupProblem>,
) {
    out.clear();
    out.extend_from_slice(authored);
    for report in runtime {
        let Some(anchor) = anchors.iter().find(|anchor| matches!(&anchor.owner, ShapeOwner::LiquidDomain(node) if node == report.node)) else {
            continue;
        };
        let mut add = |kind: SetupProblemKind, params: &[ParamAddr], bounds: Option<([f32; 3], [f32; 3])>| {
            match out.iter_mut().find(|problem| problem.owner == anchor.owner && same_slot(&problem.kind, &kind)) {
                Some(problem) => {
                    problem.kind = kind;
                    problem.bounds = bounds.or(problem.bounds);
                }
                None => out.push(SetupProblem { owner: anchor.owner.clone(), kind, params: params.to_vec(), bounds }),
            }
        };
        if report.fit.fill_capped {
            add(SetupProblemKind::FillCapped, std::slice::from_ref(&anchor.fill), report.layout.map(fill_top));
        }
        let column = match report.fit.column {
            ColumnOutcome::Fits => None,
            ColumnOutcome::Clipped => Some(SetupProblemKind::ColumnClipped),
            ColumnOutcome::Outside => Some(SetupProblemKind::ColumnOutside),
        };
        if let Some(kind) = column {
            add(kind, &anchor.column, report.column_box.or(anchor.column_bounds));
        }
        let layout_box = report.layout.map(|layout| (layout.min, std::array::from_fn(|d| layout.min[d] + layout.size[d])));
        if let Some(reason) = report.refusal {
            let params = if anchor.domain.is_empty() { std::slice::from_ref(&anchor.fill) } else { &anchor.domain[..] };
            add(SetupProblemKind::DomainRefused(reason.to_owned()), params, anchor.domain_bounds.or(layout_box));
        }
        if report.uncoupled_bodies > 0 {
            // On the water's own rows; the bodies it skips are not one shape.
            let params = if anchor.domain.is_empty() { std::slice::from_ref(&anchor.fill) } else { &anchor.domain[..] };
            add(SetupProblemKind::BodiesNotCoupled(report.uncoupled_bodies), params, None);
        }
        if report.fit.pool_full {
            // The pool holds a fixed number of sites, so a coarser lattice
            // holds more water; Resolution is the Water's, beside Fill Height.
            let item = anchor.fill.param_id.strip_suffix("fill_height").unwrap_or_default();
            let resolution = ParamAddr { param_id: format!("{item}resolution"), ..anchor.fill.clone() };
            add(SetupProblemKind::PoolFull, &[resolution, anchor.fill.clone()], layout_box.or(anchor.domain_bounds));
        }
    }
}

/// How a liquid domain's box is laid out: the Water's Transform rows place
/// the box, and its Resolution sets the cells.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SiteLayout {
    pub(super) transform: Box<super::TransformVm>,
    pub(super) resolution: (ParamAddr, f32),
}

/// One liquid domain's setup as the check reads it: the shapes it checks (by
/// index into `SceneVm::shapes`) and every param that places them, so the
/// check re-runs at new param values without rebuilding the scene model.
#[derive(Debug, Clone, PartialEq)]
pub struct SetupSite {
    pub(super) owner: ShapeOwner,
    pub(super) layout: SiteLayout,
    pub(super) box_shape: Option<usize>,
    pub(super) column: Option<usize>,
    /// The Fill Height param, read by the check only when no wire drives it.
    pub(super) fill: ParamAddr,
    pub(super) fill_authored: Option<f32>,
    pub(super) sources: Vec<(FluidRole, usize)>,
}

/// Where a value lookup overrides the authored value of a param, else the
/// authored one.
pub type SetupValues<'a> = &'a dyn Fn(&ParamAddr) -> Option<f32>;

fn at_value(value: SetupValues<'_>, (addr, authored): &(ParamAddr, f32)) -> f32 {
    value(addr).unwrap_or(*authored)
}

impl SetupSite {
    /// Check the site at `value`, appending its problems and its anchor. With
    /// no override this is the check the scene model was built with.
    pub(super) fn check(&self, shapes: &[SceneShapeVm], value: SetupValues<'_>, problems: &mut Vec<SetupProblem>, anchors: &mut Vec<SetupAnchor>) {
        let at = |index: Option<usize>| index.map(|i| &shapes[i]);
        let (box_shape, column) = (at(self.box_shape), at(self.column));
        let fill = self.fill_authored.map(|authored| (self.fill.clone(), value(&self.fill).unwrap_or(authored)));
        anchors.push(SetupAnchor::new(self.owner.clone(), self.fill.clone(), box_shape, column));
        let layout = {
            let SiteLayout { transform, resolution } = &self.layout;
            let triple = |addr: &(ParamAddr, ParamAddr, ParamAddr), v: (f32, f32, f32)| {
                [at_value(value, &(addr.0.clone(), v.0)), at_value(value, &(addr.1.clone(), v.1)), at_value(value, &(addr.2.clone(), v.2))]
            };
            let pose = manifold_core::fluid_domain::DomainBox {
                pos: triple(&transform.pos_addr, transform.pos_value),
                rot_euler: triple(&transform.rot_addr, transform.rot_value),
                scale: triple(&transform.scale_addr, transform.scale_value),
                billboard: false,
            };
            // The box sets the size; the cube size is unread beside it.
            manifold_core::fluid_domain::domain_layout(Some(pose), 0.0, at_value(value, resolution).round() as u32)
        };
        let state = match &layout {
            Ok(layout) => DomainState::Layout(layout),
            Err(reason) => DomainState::Refused(reason),
        };
        check_domain(&self.owner, state, box_shape, column, fill, problems);
        if let Ok(layout) = &layout {
            for &(role, index) in &self.sources {
                let shape = &shapes[index];
                check_source(layout, &shape.owner, role, shape, problems);
            }
        }
    }
}

/// Every Water's setup site, its domain box and its sources' boxes pushed
/// onto `shapes` over their card rows. A Fill is the initial column; Inflows
/// and Drains are checked against the domain.
pub(super) fn water_sites(doc: &SceneDoc, values: &dyn Fn(&str) -> Option<f32>, shapes: &mut Vec<SceneShapeVm>) -> Vec<SetupSite> {
    use crate::node_graph::scene_surface::{INITIAL_FILL_SHAPES, LIQUID_DOMAIN_SHAPES, WATER_SOURCE_SHAPES};
    use super::shapes::{card_shape, resting};
    let mut sites = Vec::new();
    doc.walk(|item, _| {
        if !matches!(item.fields, ItemFields::Water(_)) {
            return;
        }
        let owner = ShapeOwner::LiquidDomain(NodeId::new(format!("{}:{DOMAIN_ROLE}", item.id)));
        let box_shape = shapes.len();
        shapes.push(card_shape(owner.clone(), item, &LIQUID_DOMAIN_SHAPES[0], values));
        let (mut column, mut sources) = (None, Vec::new());
        for child in &item.children {
            let ItemFields::WaterSource(source) = &child.fields else { continue };
            let role = match source.role {
                SourceRole::Fill => FluidRole::InitialFill,
                SourceRole::Inflow => FluidRole::Inflow,
                SourceRole::Drain => FluidRole::Outflow,
            };
            let decl = if role == FluidRole::InitialFill { &INITIAL_FILL_SHAPES[0] } else { &WATER_SOURCE_SHAPES[0] };
            let node = NodeId::new(format!("{}:role", child.id));
            let index = shapes.len();
            shapes.push(card_shape(ShapeOwner::RoleSource { node, role }, child, decl, values));
            match role {
                FluidRole::InitialFill => column = column.or(Some(index)),
                _ => sources.push((role, index)),
            }
        }
        let card = |property: &str| ParamAddr { scope_path: Vec::new(), node_doc_id: 0, param_id: param_id(&item.id, property) };
        let at = |property: &str| values(&param_id(&item.id, property)).unwrap_or_else(|| resting(item, property));
        let triple = |axes: [&str; 3]| (card(axes[0]), card(axes[1]), card(axes[2]));
        let value = |axes: [&str; 3]| (at(axes[0]), at(axes[1]), at(axes[2]));
        let (pos, rot, scale) = (["pos_x", "pos_y", "pos_z"], ["rot_x", "rot_y", "rot_z"], ["scale_x", "scale_y", "scale_z"]);
        let transform = super::TransformVm {
            node_doc_id: 0,
            node: NodeId::new(format!("{}:transform", item.id)),
            pos_addr: triple(pos),
            pos_value: value(pos),
            pos_driven: (false, false, false),
            rot_addr: triple(rot),
            rot_value: value(rot),
            rot_driven: (false, false, false),
            scale_addr: triple(scale),
            scale_value: value(scale),
            scale_driven: (false, false, false),
        };
        sites.push(SetupSite {
            owner,
            layout: SiteLayout { transform: Box::new(transform), resolution: (card("resolution"), at("resolution")) },
            box_shape: Some(box_shape),
            column,
            fill: card("fill_height"),
            fill_authored: None,
            sources,
        });
    });
    sites
}

/// The card param a setup param names when it is a scene item's own row:
/// a document scene addresses its rows by card id, with no graph node.
pub fn card_param(addr: &ParamAddr) -> Option<&str> {
    (addr.scope_path.is_empty() && addr.node_doc_id == 0).then_some(addr.param_id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::scene_vm::ShapeParamVm;
    use manifold_core::NodeId;

    fn layout() -> FluidDomainLayout {
        FluidDomainLayout { min: [-2.0, 0.0, -2.0], size: [4.0, 4.0, 4.0], cells: [32; 3], cell_size: 0.125 }
    }

    /// A placed source's box at `centre` of `size`, unturned.
    fn source(centre: [f32; 3], size: [f32; 3], wired: &[&str]) -> SceneShapeVm {
        let decl = crate::node_graph::scene_surface::WATER_SOURCE_SHAPES[0].clone();
        let value = |id: &str| match id {
            "pos_x" => centre[0],
            "pos_y" => centre[1],
            "pos_z" => centre[2],
            "scale_x" => size[0],
            "scale_y" => size[1],
            "scale_z" => size[2],
            _ => 0.0,
        };
        let params = decl
            .shape
            .param_ids()
            .map(|id| ShapeParamVm {
                id: id.to_owned(),
                write: ShapeWrite::Card { id: id.to_owned(), scale: 1.0, offset: 0.0 },
                value: value(id),
                wired: wired.contains(&id),
            })
            .collect();
        SceneShapeVm {
            owner: ShapeOwner::RoleSource { node: NodeId::new("inflow"), role: FluidRole::Inflow },
            index: 0,
            decl,
            params,
            frame: None,
            anchor: [0.0; 3],
            row: None,
            pickable: true,
        }
    }

    fn problem(role: FluidRole, shape: &SceneShapeVm) -> Option<SetupProblem> {
        let mut out = Vec::new();
        check_source(&layout(), &shape.owner, role, shape, &mut out);
        out.pop()
    }

    #[test]
    fn an_inflow_is_inside_clipped_or_outside() {
        let inside = source([0.0, 3.0, 0.0], [0.5; 3], &[]);
        assert_eq!(problem(FluidRole::Inflow, &inside), None);
        let past = source([2.0, 3.0, 0.0], [0.5; 3], &[]);
        let clipped = problem(FluidRole::Inflow, &past).expect("a problem");
        assert_eq!(clipped.kind, SetupProblemKind::SourceClipped(FluidRole::Inflow));
        assert_eq!(clipped.kind.message(), "Inflow leaves the domain; the rest is unused");
        assert_eq!(clipped.bounds, Some(([1.75, 2.75, -0.25], [2.25, 3.25, 0.25])));
        // The x params lead: they are the ones that took it past the wall.
        assert_eq!(clipped.params.first().map(|addr| addr.param_id.as_str()), Some("scale_x"));
        let away = source([9.0, 3.0, 0.0], [0.5; 3], &[]);
        assert_eq!(problem(FluidRole::Inflow, &away).expect("a problem").kind, SetupProblemKind::SourceOutside(FluidRole::Inflow));
    }

    #[test]
    fn a_wired_param_leaves_the_shape_unchecked() {
        let driven = source([9.0, 3.0, 0.0], [0.5; 3], &["pos_x"]);
        assert_eq!(problem(FluidRole::Inflow, &driven), None, "a wire, not the authored value, places it");
    }

    #[test]
    fn a_turned_source_is_checked_by_its_turned_extent() {
        // A thin slab turned a quarter about Z stands on its end and its
        // long side reaches the +X wall.
        let mut slab = source([1.5, 2.0, 0.0], [2.0, 0.2, 0.5], &[]);
        for param in &mut slab.params {
            if param.id == "rot_z" {
                param.value = std::f32::consts::FRAC_PI_2;
            }
        }
        let (lo, hi) = declared_box(&slab).expect("a box");
        assert!(hi[1] - lo[1] > 1.9 && hi[0] - lo[0] < 0.3, "turned about Z: {lo:?} {hi:?}");
    }

    fn anchor() -> SetupAnchor {
        let addr = |param: &str| ParamAddr { scope_path: vec![1], node_doc_id: 3, param_id: param.to_owned() };
        SetupAnchor {
            owner: ShapeOwner::LiquidDomain(NodeId::new("water")),
            fill: addr("fill_height"),
            column: vec![addr("scale_x"), addr("pos_x")],
            column_bounds: None,
            domain: Vec::new(),
            domain_bounds: None,
        }
    }

    fn runtime(anchors: &[SetupAnchor], authored: &[SetupProblem], report: RuntimeFit<'_>) -> Vec<SetupProblem> {
        let mut out = Vec::new();
        problems_with_runtime(anchors, authored, [report], &mut out);
        out
    }

    /// Bodies the liquid could not take warn on the water's rows with their
    /// count, one line however the count moves.
    #[test]
    fn uncoupled_bodies_warn_on_the_water_with_their_count() {
        let anchors = [anchor()];
        let node = NodeId::new("water");
        let report = |count| RuntimeFit {
            node: &node,
            fit: SetupFit::default(),
            refusal: None,
            layout: None,
            column_box: None,
            uncoupled_bodies: count,
            copies_not_made: 0,
        };
        assert!(runtime(&anchors, &[], report(0)).is_empty());
        let problems = runtime(&anchors, &[], report(12));
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].kind, SetupProblemKind::BodiesNotCoupled(12));
        assert_eq!(problems[0].kind.message(), "12 bodies not coupled; water marks each cell with one byte, so it takes 255 bodies and passes through the rest");
        assert_eq!(problems[0].params, vec![anchors[0].fill.clone()], "on the water's own rows");
        assert_eq!(SetupProblemKind::BodiesNotCoupled(1).message(), "1 body not coupled; water marks each cell with one byte, so it takes 255 bodies and passes through the rest");
        let mut out = Vec::new();
        problems_with_runtime(&anchors, &[], [report(12), report(3)], &mut out);
        assert_eq!(out.iter().map(|p| p.kind.clone()).collect::<Vec<_>>(), [SetupProblemKind::BodiesNotCoupled(3)], "one line");
    }

    #[test]
    fn a_wired_column_outlines_where_it_ran() {
        let anchors = [anchor()];
        let node = NodeId::new("water");
        let ran = ([2.5, 0.0, -0.5], [3.5, 1.0, 0.5]);
        let report = RuntimeFit {
            node: &node,
            fit: SetupFit { fill_capped: false, column: ColumnOutcome::Outside, pool_full: false },
            refusal: None,
            layout: Some(&layout()),
            column_box: Some(ran),
            uncoupled_bodies: 0,
            copies_not_made: 0,
        };
        let problems = runtime(&anchors, &[], report);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].kind, SetupProblemKind::ColumnOutside);
        assert_eq!(problems[0].bounds, Some(ran), "the live column, not the whole domain");
    }

    #[test]
    fn a_refusal_with_nothing_to_outline_still_warns_the_card() {
        let anchors = [anchor()];
        let node = NodeId::new("water");
        let report = RuntimeFit { node: &node, fit: SetupFit::default(), refusal: Some("Fluid: too big"), layout: None, column_box: None, uncoupled_bodies: 0, copies_not_made: 0 };
        let problems = runtime(&anchors, &[], report);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].kind, SetupProblemKind::DomainRefused("Fluid: too big".to_owned()));
        assert_eq!(problems[0].bounds, None);
        assert_eq!(problems[0].params, vec![anchors[0].fill.clone()], "no domain rows: the warning sits on the fill row");
    }

    #[test]
    fn what_ran_replaces_the_authored_column_problem() {
        let anchors = [anchor()];
        let node = NodeId::new("water");
        // The knob's base value clips; a modulator took the column wholly out.
        let authored = SetupProblem {
            owner: anchors[0].owner.clone(),
            kind: SetupProblemKind::ColumnClipped,
            params: vec![anchors[0].column[1].clone()],
            bounds: Some(([1.5; 3], [2.5; 3])),
        };
        let ran = ([4.0; 3], [5.0; 3]);
        let report =
            RuntimeFit { node: &node, fit: SetupFit { fill_capped: false, column: ColumnOutcome::Outside, pool_full: false }, refusal: None, layout: None, column_box: Some(ran), uncoupled_bodies: 0, copies_not_made: 0 };
        let problems = runtime(&anchors, std::slice::from_ref(&authored), report);
        assert_eq!(problems.len(), 1, "one column, one warning: {problems:?}");
        assert_eq!(problems[0].kind, SetupProblemKind::ColumnOutside);
        assert_eq!(problems[0].bounds, Some(ran));
        assert_eq!(problems[0].params, authored.params, "the authored row order stays");
    }

    /// A world short of copies warns under its copies bodies' Count rows,
    /// last group first, inside the group that holds it.
    #[test]
    fn copies_not_made_warn_under_the_count_rows_last_group_first() {
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 2,
            "nodes": [{"id": 9, "nodeId": "boxes", "typeId": "group", "group": {
                "interface": {"inputs": [], "outputs": []},
                "nodes": [
                    {"id": 40, "nodeId": "world", "typeId": "node.physics_world"},
                    {"id": 121, "nodeId": "a", "typeId": "node.rigid_body"},
                    {"id": 131, "nodeId": "b", "typeId": "node.rigid_body"},
                    {"id": 5, "nodeId": "lfo", "typeId": "node.lfo"}
                ],
                "wires": [
                    {"fromNode": 121, "fromPort": "body", "toNode": 40, "toPort": "copies_0"},
                    {"fromNode": 131, "fromPort": "body", "toNode": 40, "toPort": "copies_1"},
                    {"fromNode": 5, "fromPort": "out", "toNode": 40, "toPort": "copies_2"}
                ]
            }}],
            "wires": []
        }))
        .expect("a world in a group");
        let anchors = copies_anchors(&def);
        let count = |body: u32| ParamAddr { scope_path: vec![9], node_doc_id: body, param_id: "copy_count".to_owned() };
        assert_eq!(anchors, [CopiesAnchor { world: NodeId::new("world"), counts: vec![count(131), count(121)] }]);

        let world = NodeId::new("world");
        let report = |copies_not_made| RuntimeFit {
            node: &world,
            fit: SetupFit::default(),
            refusal: None,
            layout: None,
            column_box: None,
            uncoupled_bodies: 0,
            copies_not_made,
        };
        let mut out = Vec::new();
        copies_problems(&anchors, [report(0)], &mut out);
        assert!(out.is_empty());
        copies_problems(&anchors, [report(250)], &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].owner, ShapeOwner::World(world.clone()));
        assert_eq!(out[0].params, anchors[0].counts);
        assert_eq!((out[0].kind.message(), out[0].bounds), ("250 copies not made; the world's copy pool is full".to_owned(), None));
        assert_eq!(SetupProblemKind::CopiesNotMade(1).message(), "1 copy not made; the world's copy pool is full");
    }

    /// A refusal warns on the control it concerns: a transform param on the
    /// node feeding the body's transform, a body param on the body, and
    /// with none named, under the body's own rows. The line is the reason.
    #[test]
    fn a_refusal_warns_on_the_control_it_concerns() {
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 2,
            "nodes": [
                {"id": 40, "nodeId": "world", "typeId": "node.physics_world"},
                {"id": 121, "nodeId": "ground:copies", "typeId": "node.rigid_body"},
                {"id": 7, "nodeId": "ground:transform", "typeId": "node.transform_3d"}
            ],
            "wires": [
                {"fromNode": 7, "fromPort": "transform", "toNode": 121, "toPort": "transform"},
                {"fromNode": 121, "fromPort": "body", "toNode": 40, "toPort": "copies_0"}
            ]
        }))
        .expect("a world with one copies body");
        let anchors = body_anchors(&def);
        let world = NodeId::new("world");
        let at = |node_doc_id: u32, param: &str| ParamAddr { scope_path: Vec::new(), node_doc_id, param_id: param.to_owned() };
        let refusal = |param: Option<&'static str>, reason: &str| BodyRefusalRow { input: "copies_0".into(), param, reason: reason.into() };
        let rows = [
            refusal(Some("scale_x"), "scale must be finite and not zero on any axis"),
            refusal(Some("copy_size_spacing"), "Spacing must be at least 0.01"),
            refusal(None, "Box3D refused the body"),
        ];
        let mut out = Vec::new();
        body_problems(&anchors, rows.iter().map(|row| (&world, row)), &mut out);
        let first: Vec<&ParamAddr> = out.iter().map(|problem| &problem.params[0]).collect();
        assert_eq!(first, [&at(7, "scale_x"), &at(121, "copy_size_spacing"), &at(121, "bounce")]);
        assert_eq!(out[0].kind.message(), "Scale must be finite and not zero on any axis");
    }

    /// BUG-3zq4r (inflow runs out): a pool too full for an inflow is never
    /// silent. It warns on Resolution (then Fill Height) and outlines the
    /// domain the solver ran.
    #[test]
    fn a_full_pool_warns_on_resolution() {
        let anchors = [anchor()];
        let node = NodeId::new("water");
        let fit = SetupFit { pool_full: true, ..SetupFit::default() };
        let report = RuntimeFit { node: &node, fit, refusal: None, layout: Some(&layout()), column_box: None, uncoupled_bodies: 0, copies_not_made: 0 };
        let problems = runtime(&anchors, &[], report);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].kind, SetupProblemKind::PoolFull);
        assert_eq!(problems[0].params[0].param_id, "resolution");
        assert_eq!(problems[0].params[1], anchors[0].fill);
        let layout = layout();
        assert_eq!(problems[0].bounds, Some((layout.min, std::array::from_fn(|d| layout.min[d] + layout.size[d]))));
        assert!(problems[0].kind.message().contains("inflow is held back"));
    }
}
