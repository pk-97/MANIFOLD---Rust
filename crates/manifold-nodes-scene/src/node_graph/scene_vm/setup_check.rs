//! The setup check: every liquid's setup shapes against its domain, read
//! from the declared shapes at the authored values the knob shows
//! (SCENE_SETUP_PROGRAMME.md section 3 B3, BUG-gm927 part 1). One list per
//! scene, built with the `SceneVm`; the card's warning line and the red
//! outline in the viewport both read it, and the rule is
//! `manifold_core::liquid_setup`, the same fit the solvers fill from.

use manifold_core::fluid_domain::FluidDomainLayout;
use manifold_core::NodeId;
use manifold_core::fluid_domain::FluidDomainSnapshot;
use manifold_core::liquid_setup::{AxisFit, BoxFit, ColumnOutcome, FillFit, LATTICE_PADDING_CELLS, axis_fits, fit_box, fit_fill_height};
use manifold_foundation::scene_surface::SceneShape;

use super::{FluidRole, ParamAddr, SceneShapeVm, ShapeOwner, ShapeWrite};

/// What does not fit. Each names the defined behaviour the solver applies,
/// so the warning says what was changed.
#[derive(Debug, Clone, PartialEq)]
pub enum SetupProblemKind {
    /// The domain box itself is not one the solvers take; the water does not
    /// run. Carries the solver's reason.
    DomainRefused(String),
    /// Fill Height is at or past the domain's top; the pool fills to the top.
    FillCapped,
    /// The initial column reaches past a wall; the part inside fills.
    ColumnClipped,
    /// The initial column is wholly outside; it fills nothing.
    ColumnOutside,
    /// An Inflow or Drain reaches past a wall; the part outside does nothing.
    SourceClipped(FluidRole),
    /// A source or collider is wholly outside; it does nothing.
    SourceOutside(FluidRole),
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
    /// The shape as authored, as a world box. The viewport outlines it in
    /// red; the fill is a flat box on the domain's top.
    pub bounds: ([f32; 3], [f32; 3]),
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
                    bounds,
                });
            }
            return;
        }
    };
    if let Some((addr, height)) = fill
        && let Ok(FillFit::Capped(_)) = fit_fill_height(layout, height)
    {
        let y = layout.min[1] + layout.size[1];
        out.push(SetupProblem {
            owner: domain.clone(),
            kind: SetupProblemKind::FillCapped,
            params: vec![addr],
            bounds: ([layout.min[0], y, layout.min[2]], [layout.min[0] + layout.size[0], y, layout.min[2] + layout.size[2]]),
        });
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
            out.push(SetupProblem { owner: domain.clone(), kind, params: placing_params(shape, axes_past(layout, lo, hi).into_iter()), bounds: (lo, hi) });
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
    out.push(SetupProblem { owner: owner.clone(), kind, params: placing_params(shape, axes_past(layout, lo, hi).into_iter()), bounds: (lo, hi) });
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
        assert_eq!(clipped.bounds, ([1.75, 2.75, -0.25], [2.25, 3.25, 0.25]));
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

/// The scene's authored problems, then what each domain's solver reports it
/// did on the values it ran (`FluidDomainSnapshot::fit` and `refusal`): a
/// driven, modulated or wired fill or column is capped or clipped the same
/// as an authored one, and shows the same way. A problem the authored list
/// already names for that domain is not added twice. `out` is cleared first.
pub fn problems_with_runtime(
    anchors: &[SetupAnchor],
    authored: &[SetupProblem],
    domains: &[(NodeId, FluidDomainSnapshot)],
    out: &mut Vec<SetupProblem>,
) {
    out.clear();
    out.extend_from_slice(authored);
    let has = |out: &[SetupProblem], owner: &ShapeOwner, kind: &SetupProblemKind| {
        out.iter().any(|problem| &problem.owner == owner && std::mem::discriminant(&problem.kind) == std::mem::discriminant(kind))
    };
    for (id, snapshot) in domains {
        let Some(anchor) = anchors.iter().find(|anchor| matches!(&anchor.owner, ShapeOwner::LiquidDomain(node) if node == id)) else {
            continue;
        };
        let layout_box = snapshot.accepted_layout.map(|layout| {
            (layout.min, std::array::from_fn(|d| layout.min[d] + layout.size[d]))
        });
        let add = |out: &mut Vec<SetupProblem>, kind: SetupProblemKind, params: Vec<ParamAddr>, bounds: Option<([f32; 3], [f32; 3])>| {
            if has(out, &anchor.owner, &kind) {
                return;
            }
            if let Some(bounds) = bounds {
                out.push(SetupProblem { owner: anchor.owner.clone(), kind, params, bounds });
            }
        };
        if snapshot.fit.fill_capped {
            let top = layout_box.map(|(min, max)| ([min[0], max[1], min[2]], max));
            add(out, SetupProblemKind::FillCapped, vec![anchor.fill.clone()], top);
        }
        let column = match snapshot.fit.column {
            ColumnOutcome::Fits => None,
            ColumnOutcome::Clipped => Some(SetupProblemKind::ColumnClipped),
            ColumnOutcome::Outside => Some(SetupProblemKind::ColumnOutside),
        };
        if let Some(kind) = column {
            add(out, kind, anchor.column.clone(), anchor.column_bounds.or(layout_box));
        }
        if let Some(reason) = snapshot.refusal.as_deref() {
            let params = if anchor.domain.is_empty() { vec![anchor.fill.clone()] } else { anchor.domain.clone() };
            add(out, SetupProblemKind::DomainRefused(reason.to_owned()), params, anchor.domain_bounds.or(layout_box));
        }
    }
}
