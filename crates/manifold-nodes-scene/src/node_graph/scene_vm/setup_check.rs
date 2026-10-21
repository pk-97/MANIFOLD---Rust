//! The setup check: every liquid's setup shapes against its domain, read
//! from the declared shapes at the authored values the knob shows
//! (SCENE_SETUP_PROGRAMME.md section 3 B3, BUG-gm927 part 1). One list per
//! scene, built with the `SceneVm`; the card's warning line and the red
//! outline in the viewport both read it, and the rule is
//! `manifold_core::liquid_setup`, the same fit the solvers fill from. A
//! Physics World's copies its pool could not make warn on the same cards,
//! under the copies bodies' Count rows ([`copies_problems`]).

use manifold_core::NodeId;
use manifold_core::copies_body::{PHYSICS_WORLD_TYPE_ID, RIGID_BODY_TYPE_ID, copy_group_of_input};
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};
use manifold_core::fluid_domain::{FluidDomainLayout, FluidDomainSnapshot};
use manifold_core::liquid_setup::{
    AxisFit, BoxFit, ColumnOutcome, FillFit, LATTICE_PADDING_CELLS, SetupFit, axis_fits, fit_box, fit_fill_height,
};
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
            Self::BodiesNotCoupled(1) => "1 body not coupled; the water passes through it".to_owned(),
            Self::BodiesNotCoupled(count) => format!("{count} bodies not coupled; the water passes through them"),
            Self::CopiesNotMade(1) => "1 copy not made; the world's copy pool is full".to_owned(),
            Self::CopiesNotMade(count) => format!("{count} copies not made; the world's copy pool is full"),
            Self::PoolFull => "Water pool is full; the inflow is held back. Add a Drain or lower Resolution".to_owned(),
        }
    }
}

impl FluidRole {
    fn title(self) -> &'static str {
        match self {
            Self::Inflow => "Inflow",
            Self::Outflow => "Drain",
            Self::Collider => "Collider",
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

/// The node params of `shape` that place it along `axes` first (size, then
/// centre), then every other it names.
fn placing_params(shape: &SceneShapeVm, axes: impl Iterator<Item = usize> + Clone) -> Vec<ParamAddr> {
    let SceneShape::Box { center, size, .. } = &shape.decl.shape else { return Vec::new() };
    let addr = |id: &str| {
        shape.params.iter().find(|param| param.id == id).and_then(|param| match &param.write {
            ShapeWrite::Node(addr) => Some(addr.clone()),
            ShapeWrite::Card { .. } | ShapeWrite::Unplaced => None,
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

/// The problem of one placed source against its domain's layout, if any. A
/// collider bigger than the domain (a floor, a scanned room) is ordinary, so
/// it is a problem only when it lies beyond the lattice's reach altogether.
pub(super) fn check_source(
    layout: &FluidDomainLayout,
    owner: &ShapeOwner,
    role: FluidRole,
    shape: &SceneShapeVm,
    out: &mut Vec<SetupProblem>,
) {
    let Some((lo, hi)) = declared_box(shape) else { return };
    let kind = match role {
        FluidRole::Collider => {
            let reach = (layout.cell_size * f64::from(LATTICE_PADDING_CELLS)) as f32;
            let reached = FluidDomainLayout {
                min: layout.min.map(|v| v - reach),
                size: layout.size.map(|v| v + 2.0 * reach),
                ..*layout
            };
            match fit_box(&reached, lo, hi) {
                BoxFit::Outside => SetupProblemKind::SourceOutside(role),
                BoxFit::Inside | BoxFit::Clipped { .. } => return,
            }
        }
        FluidRole::Inflow | FluidRole::Outflow => match fit_box(layout, lo, hi) {
            BoxFit::Inside => return,
            BoxFit::Clipped { .. } => SetupProblemKind::SourceClipped(role),
            BoxFit::Outside => SetupProblemKind::SourceOutside(role),
        },
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
            // holds more water; Resolution is the domain's, beside Fill Height.
            let resolution = ParamAddr { param_id: "resolution".to_owned(), ..anchor.fill.clone() };
            add(SetupProblemKind::PoolFull, &[resolution, anchor.fill.clone()], layout_box.or(anchor.domain_bounds));
        }
    }
}

/// How a liquid domain's box is laid out, with the params that size it.
/// Which form it takes depends on wires alone, so a value change never moves
/// a domain between forms.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum SiteLayout {
    /// A wire drives the domain's size, transform or billboard: no stable
    /// box to check against.
    Unchecked,
    /// A cube of Domain Size standing on the floor.
    Cube { size: (ParamAddr, f32), resolution: (ParamAddr, f32) },
    /// The box a Transform 3D places.
    Box { transform: Box<super::TransformVm>, size: (ParamAddr, f32), resolution: (ParamAddr, f32) },
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
    /// Params the site's form depends on (the domain transform's billboard,
    /// each source's role): a change to one needs a rebuild, not a re-check.
    pub(super) structure: Vec<ParamAddr>,
}

/// Where a value lookup overrides the authored value of a param, else the
/// authored one.
pub type SetupValues<'a> = &'a dyn Fn(&ParamAddr) -> Option<f32>;

fn at_value(value: SetupValues<'_>, (addr, authored): &(ParamAddr, f32)) -> f32 {
    value(addr).unwrap_or(*authored)
}

/// `shape` with each node param it names at `value`, borrowed when none moved.
fn shape_at<'a>(shape: &'a SceneShapeVm, value: SetupValues<'_>) -> std::borrow::Cow<'a, SceneShapeVm> {
    let moved = |param: &super::ShapeParamVm| match &param.write {
        ShapeWrite::Node(addr) => value(addr).filter(|v| v.to_bits() != param.value.to_bits()),
        ShapeWrite::Card { .. } | ShapeWrite::Unplaced => None,
    };
    if !shape.params.iter().any(|param| moved(param).is_some()) {
        return std::borrow::Cow::Borrowed(shape);
    }
    let mut shape = shape.clone();
    for param in &mut shape.params {
        if let Some(v) = moved(param) {
            param.value = v;
        }
    }
    std::borrow::Cow::Owned(shape)
}

impl SetupSite {
    /// Check the site at `value`, appending its problems and its anchor. With
    /// no override this is the check the scene model was built with.
    pub(super) fn check(&self, shapes: &[SceneShapeVm], value: SetupValues<'_>, problems: &mut Vec<SetupProblem>, anchors: &mut Vec<SetupAnchor>) {
        let at = |index: Option<usize>| index.map(|i| shape_at(&shapes[i], value));
        let (box_shape, column) = (at(self.box_shape), at(self.column));
        let fill = self.fill_authored.map(|authored| (self.fill.clone(), value(&self.fill).unwrap_or(authored)));
        anchors.push(SetupAnchor::new(self.owner.clone(), self.fill.clone(), box_shape.as_deref(), column.as_deref()));
        let layout = match &self.layout {
            SiteLayout::Unchecked => return,
            SiteLayout::Cube { size, resolution } => {
                manifold_core::fluid_domain::domain_layout(None, at_value(value, size), at_value(value, resolution).round() as u32)
            }
            SiteLayout::Box { transform, size, resolution } => {
                let triple = |addr: &(ParamAddr, ParamAddr, ParamAddr), v: (f32, f32, f32)| {
                    [at_value(value, &(addr.0.clone(), v.0)), at_value(value, &(addr.1.clone(), v.1)), at_value(value, &(addr.2.clone(), v.2))]
                };
                let pose = manifold_core::fluid_domain::DomainBox {
                    pos: triple(&transform.pos_addr, transform.pos_value),
                    rot_euler: triple(&transform.rot_addr, transform.rot_value),
                    scale: triple(&transform.scale_addr, transform.scale_value),
                    billboard: false,
                };
                manifold_core::fluid_domain::domain_layout(Some(pose), at_value(value, size), at_value(value, resolution).round() as u32)
            }
        };
        let state = match &layout {
            Ok(layout) => DomainState::Layout(layout),
            Err(reason) => DomainState::Refused(reason),
        };
        check_domain(&self.owner, state, box_shape.as_deref(), column.as_deref(), fill, problems);
        if let Ok(layout) = &layout {
            for &(role, index) in &self.sources {
                let shape = shape_at(&shapes[index], value);
                check_source(layout, &shape.owner, role, &shape, problems);
            }
        }
    }

    /// Every param the check reads, with whether a change to it needs a
    /// rebuild rather than a re-check.
    pub(super) fn params<'a>(&'a self, shapes: &'a [SceneShapeVm]) -> impl Iterator<Item = (&'a ParamAddr, bool)> + 'a {
        let layout: Vec<&ParamAddr> = match &self.layout {
            SiteLayout::Unchecked => Vec::new(),
            SiteLayout::Cube { size, resolution } => vec![&size.0, &resolution.0],
            SiteLayout::Box { transform, size, resolution } => {
                let t = transform.as_ref();
                vec![
                    &size.0, &resolution.0, &t.pos_addr.0, &t.pos_addr.1, &t.pos_addr.2, &t.rot_addr.0, &t.rot_addr.1, &t.rot_addr.2,
                    &t.scale_addr.0, &t.scale_addr.1, &t.scale_addr.2,
                ]
            }
        };
        let shape_params = self.box_shape.into_iter().chain(self.column).chain(self.sources.iter().map(|&(_, i)| i)).flat_map(move |i| {
            shapes[i].params.iter().filter_map(|param| match &param.write {
                ShapeWrite::Node(addr) => Some(addr),
                ShapeWrite::Card { .. } | ShapeWrite::Unplaced => None,
            })
        });
        self.fill_authored
            .map(|_| &self.fill)
            .into_iter()
            .chain(layout)
            .chain(shape_params)
            .map(|addr| (addr, false))
            .chain(self.structure.iter().map(|addr| (addr, true)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::scene_surface::scene_shape;
    use crate::node_graph::scene_vm::ShapeParamVm;
    use manifold_core::NodeId;

    fn layout() -> FluidDomainLayout {
        FluidDomainLayout { min: [-2.0, 0.0, -2.0], size: [4.0, 4.0, 4.0], cells: [32; 3], cell_size: 0.125 }
    }

    /// A placed source's box at `centre` of `size`, unturned.
    fn source(centre: [f32; 3], size: [f32; 3], wired: &[&str]) -> SceneShapeVm {
        let decl = scene_shape("node.fluid_role_source")[0].clone();
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
                write: ShapeWrite::Node(ParamAddr { scope_path: vec![1], node_doc_id: 7, param_id: id.to_owned() }),
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
    fn a_collider_bigger_than_the_domain_is_ordinary() {
        let floor = source([0.0, -0.05, 0.0], [10.0, 0.1, 10.0], &[]);
        assert_eq!(problem(FluidRole::Collider, &floor), None, "a floor past every wall is no problem");
        let away = source([9.0, 3.0, 0.0], [0.5; 3], &[]);
        assert_eq!(problem(FluidRole::Collider, &away).expect("a problem").kind, SetupProblemKind::SourceOutside(FluidRole::Collider));
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
        assert_eq!(problems[0].kind.message(), "12 bodies not coupled; the water passes through them");
        assert_eq!(problems[0].params, vec![anchors[0].fill.clone()], "on the water's own rows");
        assert_eq!(SetupProblemKind::BodiesNotCoupled(1).message(), "1 body not coupled; the water passes through it");
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
