//! Move, rotate and scale gizmos (`docs/REALTIME_3D_DESIGN.md` D7, D7a, D8).
//!
//! Pure CPU functions, no GPU: picking and handle geometry project through
//! the same `Camera::project_to_pixel` the overlay uses, so handles and the
//! rendered scene cannot disagree. Every host (the graph editor's viewport,
//! the main output preview) calls this one core.
//!
//! **Every target comes from a shape declaration**
//! (PHYSICS_ENGINE_BOUNDARY_DESIGN.md section 5.2 (One scene-object model)):
//! a target is one declared shape on one owner, read from
//! `SceneVm::shapes`. Its handles, the modes it supports and the limits on
//! its values follow the shape kind alone; no target kind has handle code
//! of its own.
//!
//! **Only what draws no pixels is picked here** (SCENE_DOCUMENT_DESIGN.md
//! section 7.14): lamps, forces, water domains and sources. Everything that
//! draws is picked by the scene's item-id buffer (`render_scene/item_pick.rs`),
//! and its shape only carries the selected item's handles. [`pick_target`]
//! projects each pickable target through the camera the frame was rendered
//! with and takes the nearest: its own drawn edges within
//! [`EDGE_PICK_TOLERANCE_PT`], or its origin within [`PICK_RADIUS_PT`]. A
//! domain box is picked by its edges, not only at its centre, where its
//! fill column sits too. A lamp has no edges, so it is picked at its origin.
//!
//! A handle whose param is wired reports locked: the host refuses the drag
//! and the handle draws in [`LOCKED_COLOR`]. The viewport never fights the
//! graph.
//!
//! **Selection is a set** ([`GizmoSelection`]), as in Blender, Cinema 4D
//! and Maya: every helper shape draws faintly so it can be clicked, the
//! selected ones draw bright, and one handle set shows, on the lone
//! selected shape or at the selection's centre ([`GizmoTarget::selection`]).
//!
//! **One composition entry draws every host's chrome** ([`compose_lines`]):
//! the ground grid, then what physics did (light markers, accepted domain
//! boxes, solver-reported shapes, all from the host's frame report), then
//! every helper shape's outline from its declaration, then the handles.

use std::sync::LazyLock;

use manifold_core::NodeId;
use manifold_core::fluid_domain::{FluidDomainSnapshot, FluidDomainState};
use manifold_core::physics_gizmo::{PhysicsGizmo, PhysicsGizmoFamily};
use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{ParamTriple, SceneShape};
use manifold_node_engine::scene::camera::{Camera, CameraMode};

use crate::node_graph::primitives::render_scene::model_matrix;
use crate::node_graph::scene_vm::{
    FluidRole, SceneShapeVm, SceneVm, SetupProblem, ShapeOwner, ShapeParamVm, ShapeWrite,
};
use crate::node_graph::viewport_overlay::{
    DOMAIN_BOX_COLOR, PHYSICS_GIZMO_LINES, ViewportOverlayConfig, WorldLine, box_lines, clip_to_near, fluid_domain_lines, fluid_lattice_lines,
    grid_lines, light_billboard_lines, physics_gizmo_color, physics_gizmo_lines,
};

mod selection;
pub use selection::{GizmoSelection, SelectionWrite, selection_writes};

/// Hit-test radius around a target's origin, screen points.
pub const PICK_RADIUS_PT: f32 = 22.0;
/// Hit-test tolerance around a shape's drawn edges, screen points.
pub const EDGE_PICK_TOLERANCE_PT: f32 = 8.0;
/// Alpha of a shape that is not selected: faint, but there to click.
pub const UNSELECTED_ALPHA: u8 = 90;
/// Segments per circle of a shape outline: round at any size on screen.
const OUTLINE_SEGMENTS: usize = 96;
/// Segments per rotate ring of a handle.
const HANDLE_RING_SEGMENTS: usize = 64;
/// A force's falloff shells: the strengths they mark, and how much of the
/// outline's alpha each keeps.
const SHELLS: [(f32, f32); 2] = [(0.5, 0.6), (0.1, 0.35)];
/// A swarm's forces box.
const SWARM_COLOR: [u8; 4] = [160, 150, 255, 255];
/// Handle hit-test tolerance, screen points.
pub const AXIS_PICK_TOLERANCE_PT: f32 = 10.0;
/// A Move or Scale handle whose axis lies within about 8 degrees of the
/// line of sight stands end-on: no drag along it can follow the cursor, so
/// it takes no press.
const END_ON_COS: f32 = 0.99;
/// A handle's length as a share of the view's height. Handles keep this
/// size on screen at any camera distance or field of view, as Blender, Unity
/// and Unreal scale theirs (D-102).
pub const HANDLE_VIEW_FRACTION: f32 = 0.12;
/// The smallest size or radius a scale handle writes.
const MIN_EXTENT: f32 = 0.01;

const X_COLOR: [u8; 4] = [225, 70, 70, 255];
const Y_COLOR: [u8; 4] = [90, 210, 110, 255];
const Z_COLOR: [u8; 4] = [90, 140, 235, 255];
const UNIFORM_COLOR: [u8; 4] = [235, 235, 235, 255];
const DIRECTION_COLOR: [u8; 4] = [240, 200, 80, 255];
const LOCKED_COLOR: [u8; 4] = [130, 130, 130, 255];
/// The outline of a setup shape that does not fit its domain.
const PROBLEM_COLOR: [u8; 4] = [255, 50, 50, 255];
/// Handle alpha while an effect after the scene layer moves the picture
/// off the scene (D-69).
pub const FADED_HANDLE_ALPHA: u8 = 80;

/// The ground grid every host draws: ten units each way, one-unit steps.
static GROUND_GRID: LazyLock<Vec<WorldLine>> = LazyLock::new(|| grid_lines(10.0, 1.0));

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GizmoMode {
    #[default]
    Move,
    Rotate,
    Scale,
}

/// A handle's axis. `Uniform` is the one scale handle of a radius.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GizmoAxis {
    X,
    Y,
    Z,
    Uniform,
}

impl GizmoAxis {
    const XYZ: [GizmoAxis; 3] = [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z];

    pub fn unit(self) -> [f32; 3] {
        match self {
            GizmoAxis::X => [1.0, 0.0, 0.0],
            GizmoAxis::Y => [0.0, 1.0, 0.0],
            GizmoAxis::Z => [0.0, 0.0, 1.0],
            GizmoAxis::Uniform => {
                let c = 1.0 / 3.0_f32.sqrt();
                [c, c, c]
            }
        }
    }

    fn color(self) -> [u8; 4] {
        match self {
            GizmoAxis::X => X_COLOR,
            GizmoAxis::Y => Y_COLOR,
            GizmoAxis::Z => Z_COLOR,
            GizmoAxis::Uniform => UNIFORM_COLOR,
        }
    }
}

/// What a target is, from its declaring owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GizmoTargetKind {
    Object,
    FluidDomain,
    Force,
    InitialFill,
    Inflow,
    Drain,
    Swarm,
}

impl GizmoTargetKind {
    pub fn of(owner: &ShapeOwner) -> Self {
        match owner {
            // A world declares no shape, so no target is ever one.
            ShapeOwner::Item(_) | ShapeOwner::World(_) => Self::Object,
            ShapeOwner::LiquidDomain(_) => Self::FluidDomain,
            ShapeOwner::RoleSource { role, .. } => match role {
                FluidRole::InitialFill => Self::InitialFill,
                FluidRole::Inflow => Self::Inflow,
                FluidRole::Outflow => Self::Drain,
            },
            ShapeOwner::Force(_) => Self::Force,
            ShapeOwner::Swarm(_) => Self::Swarm,
        }
    }
}

/// A target's identity: its owner and which of the owner's declared shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GizmoTargetId {
    pub owner: ShapeOwner,
    pub shape: usize,
}

/// What dragging one handle writes.
#[derive(Debug, Clone, PartialEq)]
pub enum HandleWrite {
    /// One param moved along the handle: a position, a size or a radius.
    /// `limit` stops the value as the param's knob stops at its hard limit.
    Linear { param: ShapeParamVm, limit: HardLimit },
    /// One angle param, turned by the ring.
    Angle(ShapeParamVm),
    /// A direction's three components, turned by the ring about the
    /// handle's axis. `normalize` writes a unit vector; otherwise the
    /// length is kept.
    Direction { params: [ShapeParamVm; 3], normalize: bool },
    /// One scale handle of a box whose three size params are one uniform
    /// scale (`SceneShapeDecl::uniform_scale`): the drag's ratio multiplies
    /// the first param's value and writes the result to all three, so the
    /// three never differ.
    Uniform { params: [ShapeParamVm; 3], limit: HardLimit },
    /// A handle of the selection's set ([`GizmoTarget::selection`]): one
    /// amount (a distance, a factor or an angle) that [`selection_writes`]
    /// turns into each member's params. `locked` when no member can take it.
    Selection { locked: bool },
}

impl HandleWrite {
    pub fn params(&self) -> &[ShapeParamVm] {
        match self {
            HandleWrite::Linear { param, .. } | HandleWrite::Angle(param) => std::slice::from_ref(param),
            HandleWrite::Direction { params, .. } | HandleWrite::Uniform { params, .. } => params,
            HandleWrite::Selection { .. } => &[],
        }
    }

    /// A wire feeds a param this handle writes.
    pub fn locked(&self) -> bool {
        match self {
            HandleWrite::Selection { locked } => *locked,
            HandleWrite::Linear { .. } | HandleWrite::Angle(_) | HandleWrite::Direction { .. } | HandleWrite::Uniform { .. } => {
                self.params().iter().any(|param| param.wired)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GizmoHandle {
    pub mode: GizmoMode,
    pub axis: GizmoAxis,
    pub write: HandleWrite,
}

/// One declared shape of one owner, resolved to world space.
#[derive(Debug, Clone, PartialEq)]
pub struct GizmoTarget {
    pub id: GizmoTargetId,
    pub kind: GizmoTargetKind,
    /// World-space point the handles sit at.
    pub origin: [f32; 3],
    /// Parent-space axes in world coordinates, scale included.
    pub world_axes: [[f32; 3]; 3],
    /// The shape's `via` port has no transform yet: the host creates one
    /// before the first drag (D8, "an unwired `transform_n` gets its atom
    /// created on first drag").
    pub unplaced: bool,
    pub pickable: bool,
    /// The scene panel row this target selects, if any.
    pub row: Option<manifold_foundation::ItemId>,
    /// The declared direction, drawn as a line from the origin.
    pub direction: Option<[f32; 3]>,
    /// The shape's own edges in world space: what draws and what is picked.
    pub outline: ShapeOutline,
    pub handles: Vec<GizmoHandle>,
}

/// A shape's edges in world space, drawn faint or bright and hit-tested by
/// [`pick_target`]. An object has none: its declared box is the transform,
/// not the mesh, so it shows by its selection outline instead.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShapeOutline {
    None,
    /// A box's eight corners; two corners that differ in one index bit
    /// share an edge.
    Box([[f32; 3]; 8]),
    /// A ball: three great circles.
    Ball { center: [f32; 3], radius: f32 },
    /// A direction from a point, with the ring of its reach around it.
    Ray { from: [f32; 3], dir: [f32; 3], length: f32, reach: Option<f32> },
    /// A radial force: the ball of its radius, and the balls where its
    /// strength falls to each of [`SHELLS`] (none where it never does).
    Field { center: [f32; 3], radius: f32, shells: [Option<f32>; 2], sign: f32 },
    /// A vortex: a cylinder of its radius round its unit axis, as tall as
    /// it is wide, and the axis through it.
    Vortex { center: [f32; 3], axis: [f32; 3], radius: f32, sign: f32 },
}

/// A circle of [`OUTLINE_SEGMENTS`] round `center` in the plane of unit `u`
/// and `v`.
fn circle(center: [f32; 3], u: [f32; 3], v: [f32; 3], r: f32, edge: &mut impl FnMut([f32; 3], [f32; 3])) {
    let point = |k: usize| -> [f32; 3] {
        let (s, c) = (k as f32 / OUTLINE_SEGMENTS as f32 * std::f32::consts::TAU).sin_cos();
        std::array::from_fn(|i| center[i] + r * (c * u[i] + s * v[i]))
    };
    for k in 0..OUTLINE_SEGMENTS {
        edge(point(k), point(k + 1));
    }
}

/// A ball as three great circles.
fn ball(center: [f32; 3], r: f32, edge: &mut impl FnMut([f32; 3], [f32; 3])) {
    for (u, v) in [(0, 1), (1, 2), (0, 2)] {
        circle(center, IDENTITY_AXES[u], IDENTITY_AXES[v], r, edge);
    }
}

/// Where a radial force's strength falls to `strength` (0 to 1) of full,
/// for radius `r`, power `p` and curve `curve` (0 Edge, 1 Inverse), solving
/// `FalloffCurve::magnitude` (manifold-physics): Edge (1 − d/r)^p, Inverse
/// (r/d)^p past r. `None` where the strength never falls: power 0 holds it
/// at full.
fn shell_distance(strength: f32, r: f32, p: f32, curve: f32) -> Option<f32> {
    if p <= 0.0 || r <= 0.0 {
        return None;
    }
    let d = if curve.round() >= 1.0 { r * strength.powf(-1.0 / p) } else { r * (1.0 - strength.powf(1.0 / p)) };
    d.is_finite().then_some(d)
}

impl ShapeOutline {
    /// Each edge as a world segment: what draws at full strength and what
    /// a click picks.
    pub fn for_each_edge(&self, mut edge: impl FnMut([f32; 3], [f32; 3])) {
        match *self {
            ShapeOutline::None => {}
            ShapeOutline::Box(corners) => {
                for k in 0..8 {
                    for axis in 0..3 {
                        if k >> axis & 1 == 0 {
                            edge(corners[k], corners[k | 1 << axis]);
                        }
                    }
                }
            }
            ShapeOutline::Ball { center, radius } | ShapeOutline::Field { center, radius, .. } => ball(center, radius, &mut edge),
            ShapeOutline::Ray { from, dir, length, reach } => {
                edge(from, offset(from, dir, length));
                if let Some(r) = reach {
                    let u = normalized(perpendicular(dir)).unwrap_or(IDENTITY_AXES[0]);
                    let v = cross3(dir, u);
                    circle(from, u, v, r, &mut edge);
                }
            }
            ShapeOutline::Vortex { center, axis, radius, .. } => {
                let u = normalized(perpendicular(axis)).unwrap_or(IDENTITY_AXES[0]);
                let v = cross3(axis, u);
                let [top, bottom] = [radius, -radius].map(|h| offset(center, axis, h));
                circle(top, u, v, radius, &mut edge);
                circle(bottom, u, v, radius, &mut edge);
                for side in [u, v, u.map(|c| -c), v.map(|c| -c)] {
                    edge(offset(bottom, side, radius), offset(top, side, radius));
                }
                edge(offset(center, axis, -1.5 * radius), offset(center, axis, 1.5 * radius));
            }
        }
    }

    /// Each falloff shell's edges, with the share of the outline's alpha it
    /// draws at. Shells draw only; a click picks the outline.
    pub fn for_each_shell(&self, mut edge: impl FnMut([f32; 3], [f32; 3], f32)) {
        if let ShapeOutline::Field { center, shells, .. } = *self {
            for (shell, (_, alpha)) in shells.into_iter().zip(SHELLS) {
                if let Some(r) = shell {
                    ball(center, r, &mut |a, b| edge(a, b, alpha));
                }
            }
        }
    }

    /// Small directional arrows showing push/pull (radial) and spin (vortex).
    /// Drawn, never picked: `for_each_edge` stays the pick set.
    pub fn for_each_arrow(&self, mut edge: impl FnMut([f32; 3], [f32; 3])) {
        match *self {
            ShapeOutline::Field { center, radius, sign, .. } if radius > MIN_EXTENT => {
                for dir in [[1.0_f32, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, -1.0]] {
                    let p_in = offset(center, dir, 0.35 * radius);
                    let p_out = offset(center, dir, 0.85 * radius);
                    let perp = perpendicular(dir);
                    if sign >= 0.0 {
                        edge(p_in, p_out);
                        let barb = offset(p_out, dir, -0.12 * radius);
                        edge(p_out, offset(barb, perp, 0.08 * radius));
                        edge(p_out, offset(barb, perp.map(|c| -c), 0.08 * radius));
                    } else {
                        edge(p_out, p_in);
                        let barb = offset(p_in, dir, 0.12 * radius);
                        edge(p_in, offset(barb, perp, 0.08 * radius));
                        edge(p_in, offset(barb, perp.map(|c| -c), 0.08 * radius));
                    }
                }
            }
            ShapeOutline::Vortex { center, axis, radius, sign } if radius > MIN_EXTENT => {
                let u = normalized(perpendicular(axis)).unwrap_or(IDENTITY_AXES[0]);
                let v = cross3(axis, u);
                let bases = [(u, v), (v, u.map(|c| -c)), (u.map(|c| -c), v.map(|c| -c)), (v.map(|c| -c), u)];
                for (radial, tangent) in bases {
                    let tip = offset(center, radial, radius);
                    let t = tangent.map(|c| c * sign);
                    let back = offset(tip, t.map(|c| -c), 0.18 * radius);
                    edge(tip, offset(back, radial, 0.10 * radius));
                    edge(tip, offset(back, radial.map(|c| -c), 0.10 * radius));
                }
            }
            _ => {}
        }
    }
}

const IDENTITY_AXES: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

impl GizmoTarget {
    /// Build the target `shape` declares. Handles, mode support and limits
    /// come from the shape kind and the params it names.
    pub fn from_shape(shape: &SceneShapeVm) -> Self {
        let param = |id: &str| shape.params.iter().find(|param| param.id == id).cloned();
        let triple = |ids: &ParamTriple| -> Option<[ShapeParamVm; 3]> {
            Some([param(&ids[0])?, param(&ids[1])?, param(&ids[2])?])
        };
        let mut handles = Vec::new();
        let mut local = None;
        let mut direction = None;
        let extent_limit = shape.decl.extent_limit.unwrap_or(HardLimit::AtLeast(MIN_EXTENT));
        let mut place = |center: &ParamTriple, handles: &mut Vec<GizmoHandle>| {
            if let Some(center) = triple(center) {
                local = Some(center.each_ref().map(|param| param.value));
                for (axis, param) in GizmoAxis::XYZ.into_iter().zip(center) {
                    handles.push(GizmoHandle { mode: GizmoMode::Move, axis, write: HandleWrite::Linear { param, limit: HardLimit::Unlimited } });
                }
            }
        };
        let extent = |id: &str, handles: &mut Vec<GizmoHandle>| {
            if let Some(param) = param(id) {
                let write = HandleWrite::Linear { param, limit: extent_limit };
                handles.push(GizmoHandle { mode: GizmoMode::Scale, axis: GizmoAxis::Uniform, write });
            }
        };
        let mut turn = |ids: &ParamTriple, normalize: bool, handles: &mut Vec<GizmoHandle>| {
            if let Some(params) = triple(ids) {
                direction = Some(params.each_ref().map(|param| param.value));
                for axis in GizmoAxis::XYZ {
                    let write = HandleWrite::Direction { params: params.clone(), normalize };
                    handles.push(GizmoHandle { mode: GizmoMode::Rotate, axis, write });
                }
            }
        };
        match &shape.decl.shape {
            SceneShape::Box { center, rotation, size } => {
                place(center, &mut handles);
                for (axis, id) in GizmoAxis::XYZ.into_iter().zip(rotation.iter().flatten()) {
                    if let Some(param) = param(id) {
                        handles.push(GizmoHandle { mode: GizmoMode::Rotate, axis, write: HandleWrite::Angle(param) });
                    }
                }
                match triple(size) {
                    // Three params that are one scale: one handle writes all three.
                    Some(params) if shape.decl.uniform_scale && params.iter().all(|param| param.write != ShapeWrite::Unplaced) => {
                        let write = HandleWrite::Uniform { params, limit: extent_limit };
                        handles.push(GizmoHandle { mode: GizmoMode::Scale, axis: GizmoAxis::Uniform, write });
                    }
                    // A cube's sides are one param: it scales uniformly.
                    Some([side, y, z]) if side.write != ShapeWrite::Unplaced && side.write == y.write && side.write == z.write => {
                        let write = HandleWrite::Linear { param: side, limit: extent_limit };
                        handles.push(GizmoHandle { mode: GizmoMode::Scale, axis: GizmoAxis::Uniform, write });
                    }
                    _ => {
                        for (axis, id) in GizmoAxis::XYZ.into_iter().zip(size) {
                            if let Some(param) = param(id) {
                                let write = HandleWrite::Linear { param, limit: extent_limit };
                                handles.push(GizmoHandle { mode: GizmoMode::Scale, axis, write });
                            }
                        }
                    }
                }
            }
            SceneShape::Sphere { center, radius } | SceneShape::PointRadius { center, radius } | SceneShape::Field { center, radius, .. } => {
                place(center, &mut handles);
                extent(radius, &mut handles);
            }
            SceneShape::Plane { center, normal } => {
                place(center, &mut handles);
                turn(normal, true, &mut handles);
            }
            SceneShape::Axis { origin, direction, radius } => {
                if let Some(origin) = origin {
                    place(origin, &mut handles);
                }
                turn(direction, false, &mut handles);
                if let Some(radius) = radius {
                    extent(radius, &mut handles);
                }
            }
        }
        let local = local.unwrap_or(match &shape.decl.shape {
            SceneShape::Axis { origin: None, .. } => shape.anchor,
            SceneShape::Box { .. }
            | SceneShape::Sphere { .. }
            | SceneShape::Plane { .. }
            | SceneShape::PointRadius { .. }
            | SceneShape::Axis { .. }
            | SceneShape::Field { .. } => [0.0; 3],
        });
        let to_world = |p: [f32; 3]| -> [f32; 3] {
            match shape.frame {
                Some(m) => std::array::from_fn(|i| m[3][i] + (0..3).map(|j| m[j][i] * p[j]).sum::<f32>()),
                None => p,
            }
        };
        let origin = to_world(local);
        let world_axes = shape.frame.map_or(IDENTITY_AXES, |m| std::array::from_fn(|j| [m[j][0], m[j][1], m[j][2]]));
        let kind = GizmoTargetKind::of(&shape.owner);
        Self {
            id: GizmoTargetId { owner: shape.owner.clone(), shape: shape.index },
            kind,
            origin,
            world_axes,
            unplaced: shape.params.iter().any(|param| param.write == ShapeWrite::Unplaced),
            pickable: shape.pickable,
            row: shape.row.clone(),
            direction,
            outline: outline(shape, kind, origin, to_world),
            handles,
        }
    }

    /// The lines of this shape's outline in `color`, appended to `out`.
    #[cfg(test)]
    fn outline_lines(&self, color: [u8; 4], out: &mut Vec<WorldLine>) {
        self.outline_lines_with_shells(color, true, out);
    }

    /// Like [`Self::outline_lines`], but `show_shells` controls whether the
    /// falloff shells are drawn (unselected forces skip them).
    pub fn outline_lines_with_shells(&self, color: [u8; 4], show_shells: bool, out: &mut Vec<WorldLine>) {
        // A box's edges meet at its corners; a curve's pieces join end to end.
        let caps = matches!(self.outline, ShapeOutline::Box(_));
        self.outline.for_each_edge(|a, b| out.push(WorldLine { a, b, color, caps }));
        if show_shells {
            self.outline.for_each_shell(|a, b, alpha| {
                let [r, g, b8, a8] = color;
                // Tint falloff shells toward warm amber when selected so Radius (magenta)
                // and Falloff (amber) are immediately distinguishable.
                let (r, g, b8) = if a8 == 255 { (255, g.saturating_add(95), b8 / 3) } else { (r, g, b8) };
                out.push(WorldLine { a, b, color: [r, g, b8, (f32::from(a8) * alpha).round() as u8], caps: false });
            });
        }
        self.outline.for_each_arrow(|a, b| out.push(WorldLine { a, b, color, caps: false }));
    }

    /// The color this shape's outline draws in, by what it is: the families
    /// the solver's own report draws in.
    fn outline_color(&self) -> [u8; 4] {
        match self.kind {
            GizmoTargetKind::FluidDomain => DOMAIN_BOX_COLOR,
            GizmoTargetKind::InitialFill => physics_gizmo_color(PhysicsGizmoFamily::Fill),
            GizmoTargetKind::Inflow => physics_gizmo_color(PhysicsGizmoFamily::Source),
            GizmoTargetKind::Drain => physics_gizmo_color(PhysicsGizmoFamily::Drain),
            GizmoTargetKind::Force => physics_gizmo_color(PhysicsGizmoFamily::Force),
            GizmoTargetKind::Swarm => SWARM_COLOR,
            GizmoTargetKind::Object => UNIFORM_COLOR,
        }
    }

    pub fn supports_mode(&self, mode: GizmoMode) -> bool {
        self.handles.iter().any(|handle| handle.mode == mode)
    }

    pub fn handle(&self, mode: GizmoMode, axis: GizmoAxis) -> Option<&GizmoHandle> {
        self.handles.iter().find(|handle| handle.mode == mode && handle.axis == axis)
    }

    /// The world direction a Move or Scale handle on `axis` drags along.
    fn axis_direction(&self, axis: GizmoAxis) -> [f32; 3] {
        match axis {
            GizmoAxis::X => self.world_axes[0],
            GizmoAxis::Y => self.world_axes[1],
            GizmoAxis::Z => self.world_axes[2],
            GizmoAxis::Uniform => GizmoAxis::Uniform.unit(),
        }
    }

    /// The unit world direction a Move or Scale handle on `axis` draws
    /// along: handles keep their fixed length whatever the parent's scale.
    fn handle_direction(&self, axis: GizmoAxis) -> [f32; 3] {
        let u = self.axis_direction(axis);
        normalized(u).unwrap_or(u)
    }

    /// `point`, an offset from the origin in handle space, in world space:
    /// along the parent's axes turned to unit length, so rings keep the
    /// fixed size too.
    fn world_point(&self, point: [f32; 3]) -> [f32; 3] {
        let d = [point[0] - self.origin[0], point[1] - self.origin[1], point[2] - self.origin[2]];
        let axes = self.world_axes.map(|axis| normalized(axis).unwrap_or(axis));
        std::array::from_fn(|i| self.origin[i] + (0..3).map(|j| axes[j][i] * d[j]).sum::<f32>())
    }

    /// Where along `axis` the pointer at render pixel `p` of `view` holds
    /// the handle, in the axis's param units: the closest point between the
    /// pointer ray and the axis line, as Blender and Unity drag a handle, so
    /// the grabbed point stays under the cursor. `None` while the axis lies
    /// along the line of sight or that point is not in front of the camera.
    fn axis_param(&self, axis: GizmoAxis, view: &GizmoView, p: (f32, f32)) -> Option<f32> {
        ray_axis_param(self.origin, self.axis_direction(axis), view, p)
    }

    /// A Move or Scale handle on `axis` points along the line of sight.
    fn end_on(&self, mode: GizmoMode, axis: GizmoAxis, cam: &Camera) -> bool {
        if mode == GizmoMode::Rotate {
            return false;
        }
        let sight = match cam.mode {
            CameraMode::Perspective { .. } => sub3(self.origin, cam.pos),
            CameraMode::Orthographic { .. } => cam.fwd,
        };
        match (normalized(self.axis_direction(axis)), normalized(sight)) {
            (Some(u), Some(s)) => dot3(u, s).abs() > END_ON_COS,
            _ => true,
        }
    }

    /// The world point a pointer grabs `mode`'s handle on `axis` by in
    /// `view`: a linear handle's tip, or a ring an eighth of a turn round,
    /// where no other ring passes. [`pick_axis`] returns that handle there
    /// unless another ring stands edge-on over it. `None` for an end-on
    /// handle, which takes no press.
    pub fn grip(&self, mode: GizmoMode, axis: GizmoAxis, view: &GizmoView) -> Option<[f32; 3]> {
        self.handle(mode, axis)?;
        if self.end_on(mode, axis, &view.camera) {
            return None;
        }
        let len = handle_length(self.origin, &view.camera, view.height);
        match mode {
            GizmoMode::Move | GizmoMode::Scale => Some(offset(self.origin, self.handle_direction(axis), len)),
            GizmoMode::Rotate => ring_lines(self.origin, axis, len, [0; 4]).nth(HANDLE_RING_SEGMENTS / 8).map(|line| self.world_point(line.a)),
        }
    }
}

/// The world outline `shape` declares, sitting at `origin`. Box params are
/// in the shape's parent space; `to_world` maps them out.
fn outline(shape: &SceneShapeVm, kind: GizmoTargetKind, origin: [f32; 3], to_world: impl Fn([f32; 3]) -> [f32; 3]) -> ShapeOutline {
    let value = |id: &str| shape.params.iter().find(|param| param.id == id).map(|param| param.value);
    let values = |ids: &ParamTriple| Some([value(&ids[0])?, value(&ids[1])?, value(&ids[2])?]);
    let outline = match (kind, &shape.decl.shape) {
        (GizmoTargetKind::Object, _) => None,
        (_, SceneShape::Box { center, rotation, size }) => values(center).zip(values(size)).map(|(center, size)| {
            let turn = rotation.as_ref().and_then(values).unwrap_or([0.0; 3]);
            let m = model_matrix(center, turn, size);
            ShapeOutline::Box(std::array::from_fn(|k| {
                let local: [f32; 3] = std::array::from_fn(|i| if k >> i & 1 == 1 { 0.5 } else { -0.5 });
                to_world(std::array::from_fn(|i| m[3][i] + (0..3).map(|j| m[j][i] * local[j]).sum::<f32>()))
            }))
        }),
        (_, SceneShape::Sphere { radius, .. } | SceneShape::PointRadius { radius, .. }) => {
            value(radius).map(|radius| ShapeOutline::Ball { center: origin, radius: radius.abs() })
        }
        (_, SceneShape::Field { radius, falloff, curve, axis, strength, .. }) => value(radius).map(|radius| {
            let radius = radius.abs();
            let sign = if strength.as_deref().and_then(&value).unwrap_or(1.0) < 0.0 { -1.0 } else { 1.0 };
            match axis.as_ref().map(|axis| values(axis).and_then(normalized)) {
                Some(Some(axis)) => ShapeOutline::Vortex { center: origin, axis, radius, sign },
                // A vortex with no axis swirls nowhere: its reach only.
                Some(None) => ShapeOutline::Ball { center: origin, radius },
                None => {
                    let (power, curve) = (value(falloff).unwrap_or(1.0), value(curve).unwrap_or(0.0));
                    let shells = SHELLS.map(|(strength, _)| shell_distance(strength, radius, power, curve));
                    ShapeOutline::Field { center: origin, radius, shells, sign }
                }
            }
        }),
        (_, SceneShape::Axis { direction, radius, .. }) => values(direction).and_then(normalized).map(|dir| {
            let reach = radius.as_deref().and_then(value).map(f32::abs);
            ShapeOutline::Ray { from: origin, dir, length: reach.filter(|r| *r > MIN_EXTENT).unwrap_or(1.0), reach }
        }),
        (_, SceneShape::Plane { .. }) => None,
    };
    outline.unwrap_or(ShapeOutline::None)
}

/// The picture a host shows: the camera it was rendered through, the
/// render's size, and how many render pixels one screen point spans there.
/// Pointer points are in the render's pixels; hit tolerances are in points.
#[derive(Clone, Copy, Debug)]
pub struct GizmoView {
    pub camera: Camera,
    pub width: u32,
    pub height: u32,
    pub px_per_point: f32,
}

impl GizmoView {
    /// A view whose render pixels are screen points; a host sets the real
    /// scale with [`Self::on_screen`] where it maps the pointer.
    pub fn new(camera: Camera, width: u32, height: u32) -> Self {
        Self { camera, width, height, px_per_point: 1.0 }
    }

    /// This view shown `points_high` screen points tall.
    pub fn on_screen(self, points_high: f32) -> Self {
        let scale = self.height as f32 / points_high;
        Self { px_per_point: if scale.is_finite() && scale > 0.0 { scale } else { 1.0 }, ..self }
    }
}

/// The parameter `t` of the point `origin + t * u` nearest the pointer ray
/// through render pixel `p`. `None` when the axis and the ray are within
/// the end-on angle of parallel, or when that point is not in front of the
/// camera: past a receding axis's vanishing point the lines meet behind the
/// eye.
fn ray_axis_param(origin: [f32; 3], u: [f32; 3], view: &GizmoView, p: (f32, f32)) -> Option<f32> {
    let (a, d) = view.camera.pixel_ray(p.0, p.1, view.width, view.height);
    let w0 = sub3(origin, a);
    let (uu, ud, dd) = (dot3(u, u), dot3(u, d), dot3(d, d));
    let denom = uu * dd - ud * ud;
    // denom / (uu * dd) is the squared sine of the angle between them.
    if denom <= uu * dd * (1.0 - END_ON_COS * END_ON_COS) {
        return None;
    }
    let (uw, dw) = (dot3(u, w0), dot3(d, w0));
    // How far along the ray the nearest point sits.
    let s = (uu * dw - ud * uw) / denom;
    if s.is_nan() || s <= view.camera.near {
        return None;
    }
    let t = (ud * dw - dd * uw) / denom;
    t.is_finite().then_some(t)
}

/// The world length a handle at `origin` takes so it spans
/// [`HANDLE_VIEW_FRACTION`] of a `height`-pixel view through `cam`. Draw,
/// grip and hit-test all read this one length.
pub fn handle_length(origin: [f32; 3], cam: &Camera, height: u32) -> f32 {
    HANDLE_VIEW_FRACTION * height as f32 * world_per_pixel(origin, cam, height)
}

/// World units one pixel spans at `point`'s depth in a `height`-pixel view.
/// A point at or behind the near plane takes the near plane's scale.
fn world_per_pixel(point: [f32; 3], cam: &Camera, height: u32) -> f32 {
    let height = height.max(1) as f32;
    match cam.mode {
        CameraMode::Perspective { fov_y } => {
            let d = [point[0] - cam.pos[0], point[1] - cam.pos[1], point[2] - cam.pos[2]];
            let depth = (d[0] * cam.fwd[0] + d[1] * cam.fwd[1] + d[2] * cam.fwd[2]).max(cam.near.max(1e-4));
            2.0 * depth * (fov_y * 0.5).tan() / height
        }
        CameraMode::Orthographic { half_height } => 2.0 * half_height / height,
    }
}

/// Every declared shape in `scene` as a gizmo target.
pub fn gizmo_targets(scene: &SceneVm) -> impl Iterator<Item = GizmoTarget> + '_ {
    scene.shapes.iter().map(GizmoTarget::from_shape)
}

/// The target `id` names in `scene`, or `None` when it no longer exists,
/// so callers drop stale gizmo geometry.
pub fn gizmo_target_for(scene: &SceneVm, id: &GizmoTargetId) -> Option<GizmoTarget> {
    scene
        .shapes
        .iter()
        .find(|shape| shape.owner == id.owner && shape.index == id.shape)
        .map(GizmoTarget::from_shape)
}

/// The pickable target nearest `click` (render pixels of `view`): by its
/// drawn edges within [`EDGE_PICK_TOLERANCE_PT`], or by its origin within
/// [`PICK_RADIUS_PT`]. `None` is a click on empty space.
pub fn pick_target(scene: &SceneVm, view: &GizmoView, click: (f32, f32)) -> Option<GizmoTargetId> {
    let mut best: Option<(GizmoTargetId, f32)> = None;
    for target in gizmo_targets(scene).filter(|target| target.pickable) {
        if let Some(d) = target_distance(&target, view, click)
            && best.as_ref().is_none_or(|(_, bd)| d < *bd)
        {
            best = Some((target.id, d));
        }
    }
    best.map(|(id, _)| id)
}

/// How far `click` lies from `target` on screen, in render pixels: the
/// nearer of its origin (within [`PICK_RADIUS_PT`]) and its edges (within
/// [`EDGE_PICK_TOLERANCE_PT`]). `None` when neither is in reach.
fn target_distance(target: &GizmoTarget, view: &GizmoView, click: (f32, f32)) -> Option<f32> {
    let project = |p: [f32; 3]| view.camera.project_to_pixel(p, view.width, view.height).map(|p| (p.px, p.py));
    let radius = PICK_RADIUS_PT * view.px_per_point;
    let tolerance = EDGE_PICK_TOLERANCE_PT * view.px_per_point;
    let mut best = project(target.origin).map(|o| dist2(click, o).sqrt()).filter(|d| *d <= radius);
    target.outline.for_each_edge(|a, b| {
        // A camera inside a domain box still reaches the edges in front of it.
        let Some((a, b)) = clip_to_near(&view.camera, a, b) else { return };
        if let (Some(a), Some(b)) = (project(a), project(b)) {
            let d = point_segment_dist(click, a, b);
            if d <= tolerance && best.is_none_or(|bd| d < bd) {
                best = Some(d);
            }
        }
    });
    best
}

fn dist2(a: (f32, f32), b: (f32, f32)) -> f32 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    dx * dx + dy * dy
}

/// What one host frame shows (REALTIME_3D_DESIGN.md D7a). What physics did
/// comes from the host's frame report; outlines and handles come from the
/// shape declarations.
#[derive(Clone, Copy)]
pub struct GizmoChrome<'a> {
    /// The picture the lines draw over; handles size to it.
    pub view: GizmoView,
    pub lights: &'a [[f32; 3]],
    pub domains: &'a [(NodeId, FluidDomainSnapshot)],
    /// The live column box of `domains[i]` when the host carries it apart
    /// from the snapshot (so a moving column re-shares nothing); empty
    /// otherwise.
    pub live_columns: &'a [Option<([f32; 3], [f32; 3])>],
    pub physics: &'a [PhysicsGizmo],
    /// The scene's setup check: each shape that does not fit its domain
    /// draws a red box.
    pub problems: &'a [SetupProblem],
    /// Every shape in the scene, which are selected, and the one handle set.
    pub selection: &'a GizmoSelection,
    pub mode: GizmoMode,
    /// An effect after the scene layer moves the picture off the scene, so
    /// the handles fade (D-69).
    pub fade_handles: bool,
}

/// `color` drawn faint unless `bright`.
fn shown(mut color: [u8; 4], bright: bool) -> [u8; 4] {
    if !bright {
        color[3] = color[3].min(UNSELECTED_ALPHA);
    }
    color
}

/// Every line a host draws for one frame, into `out` (cleared first): the
/// ground grid, a marker per light, each solver-reported physics shape
/// (faint), a box per accepted liquid domain, every pickable helper shape's
/// outline (faint, or bright when selected), the selected domains' cell
/// grids, a red box on every shape the setup check finds outside its domain
/// whatever is selected, then the one handle set. The one composition entry
/// every host calls.
pub fn compose_lines(chrome: &GizmoChrome<'_>, out: &mut Vec<WorldLine>) {
    out.clear();
    out.extend_from_slice(&GROUND_GRID);
    let size = ViewportOverlayConfig::default().light_billboard_size;
    for &pos in chrome.lights {
        out.extend(light_billboard_lines(pos, size));
    }
    // What physics did carries no owner, so it never draws bright; the
    // declared outline over it does.
    let mut shape = [WorldLine { a: [0.0; 3], b: [0.0; 3], color: [0; 4], caps: false }; PHYSICS_GIZMO_LINES];
    for gizmo in chrome.physics {
        let count = physics_gizmo_lines(gizmo, &mut shape);
        out.extend(shape[..count].iter().map(|line| WorldLine { color: shown(line.color, false), ..*line }));
    }
    let selection = chrome.selection;
    let accepted = |node: &NodeId| {
        chrome
            .domains
            .iter()
            .find(|(id, snapshot)| id == node && snapshot.state == FluidDomainState::Ready)
            .and_then(|(_, snapshot)| snapshot.accepted_layout)
    };
    // A domain box the solver accepted draws where the solver put it (its
    // size snaps to whole cells), in place of its declared outline.
    for (node, snapshot) in chrome.domains.iter().filter(|(_, snapshot)| snapshot.state == FluidDomainState::Ready) {
        let Some(layout) = snapshot.accepted_layout else { continue };
        let bright = selection.selected.iter().any(|target| is_domain_box(target, node));
        out.extend(fluid_domain_lines(layout, shown(DOMAIN_BOX_COLOR, bright)));
    }
    for target in selection.targets.iter().filter(|target| target.pickable) {
        if let ShapeOwner::LiquidDomain(node) = &target.id.owner
            && is_domain_box(target, node)
            && accepted(node).is_some()
        {
            continue;
        }
        let bright = selection.is_selected(&target.id);
        target.outline_lines_with_shells(shown(target.outline_color(), bright), bright, out);
    }
    // The cell grid shows only for a selected domain, from what its solver
    // accepted (D7a); handles and cards read the declaration instead.
    let view = &chrome.view;
    let on_screen = |a: [f32; 3], b: [f32; 3]| {
        let a = view.camera.project_to_pixel(a, view.width, view.height)?;
        let b = view.camera.project_to_pixel(b, view.width, view.height)?;
        Some((a.px - b.px).hypot(a.py - b.py) / view.px_per_point)
    };
    for (index, target) in selection.selected.iter().enumerate() {
        let ShapeOwner::LiquidDomain(node) = &target.id.owner else { continue };
        let first = !selection.selected[..index].iter().any(|other| other.id.owner == target.id.owner);
        if let Some(layout) = accepted(node).filter(|_| first) {
            fluid_lattice_lines(layout, view.camera.pos, view.camera.fwd, on_screen, out);
        }
    }
    for (lo, hi) in chrome.problems.iter().filter_map(|problem| problem.live_bounds(chrome.domains, chrome.live_columns)) {
        out.extend(box_lines(lo, hi, PROBLEM_COLOR));
    }
    if let Some(target) = selection.handles.as_ref() {
        let start = out.len();
        let len = handle_length(target.origin, &chrome.view.camera, chrome.view.height);
        gizmo_lines(chrome.mode, target, len, out);
        if chrome.fade_handles {
            for line in &mut out[start..] {
                line.color[3] = line.color[3].min(FADED_HANDLE_ALPHA);
            }
        }
    }
}

/// `target` is the box of the liquid domain `node`: its first declared
/// shape (`LIQUID_DOMAIN_SHAPES`).
fn is_domain_box(target: &GizmoTarget, node: &NodeId) -> bool {
    target.id.shape == 0 && matches!(&target.id.owner, ShapeOwner::LiquidDomain(owner) if owner == node)
}

/// World-space handle geometry for `mode` at `target`, plus the declared
/// direction when the shape has one, appended to `lines`, with handles
/// `len` long ([`handle_length`]). Locked handles draw in [`LOCKED_COLOR`].
pub fn gizmo_lines(mode: GizmoMode, target: &GizmoTarget, len: f32, lines: &mut Vec<WorldLine>) {
    let origin = target.origin;
    if let Some(direction) = target.direction.and_then(normalized) {
        lines.push(WorldLine { a: origin, b: offset(origin, direction, len * 1.4), color: DIRECTION_COLOR, caps: false });
    }
    for handle in target.handles.iter().filter(|handle| handle.mode == mode) {
        let color = if handle.write.locked() { LOCKED_COLOR } else { handle.axis.color() };
        match mode {
            GizmoMode::Move => {
                let tip = offset(origin, target.handle_direction(handle.axis), len);
                lines.push(WorldLine { a: origin, b: tip, color, caps: false });
            }
            GizmoMode::Scale => {
                let u = target.handle_direction(handle.axis);
                let tip = offset(origin, u, len);
                // A short tick at the tip tells scale from move.
                let perp = perpendicular(u);
                let tick = len * 0.12;
                lines.push(WorldLine { a: origin, b: tip, color, caps: false });
                lines.push(WorldLine { a: offset(tip, perp, tick), b: offset(tip, perp, -tick), color, caps: false });
            }
            GizmoMode::Rotate => {
                lines.extend(ring_lines(origin, handle.axis, len, color).map(|line| WorldLine {
                    a: target.world_point(line.a),
                    b: target.world_point(line.b),
                    color: line.color,
                    caps: line.caps,
                }));
            }
        }
    }
}

fn offset(p: [f32; 3], dir: [f32; 3], len: f32) -> [f32; 3] {
    [p[0] + dir[0] * len, p[1] + dir[1] * len, p[2] + dir[2] * len]
}

fn normalized(v: [f32; 3]) -> Option<[f32; 3]> {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    (len.is_finite() && len > 1e-6).then(|| v.map(|c| c / len))
}

/// Any unit vector perpendicular to `u`, for the decorative scale tick.
fn perpendicular(u: [f32; 3]) -> [f32; 3] {
    if u[0].abs() < 0.9 { [0.0, -u[2], u[1]] } else { [-u[1], u[0], 0.0] }
}

/// A ring of segments in the plane perpendicular to `axis` around `origin`.
/// `Uniform` has no ring.
fn ring_lines(origin: [f32; 3], axis: GizmoAxis, r: f32, color: [u8; 4]) -> impl Iterator<Item = WorldLine> {
    const SEGMENTS: usize = HANDLE_RING_SEGMENTS;
    let ((a, b), segments) = match axis {
        GizmoAxis::X => (([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]), SEGMENTS),
        GizmoAxis::Y => (([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]), SEGMENTS),
        GizmoAxis::Z => (([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]), SEGMENTS),
        GizmoAxis::Uniform => (([0.0; 3], [0.0; 3]), 0),
    };
    let point = move |t: f32| -> [f32; 3] {
        let (s, c) = t.sin_cos();
        std::array::from_fn(|i| origin[i] + r * (c * a[i] + s * b[i]))
    };
    (0..segments).map(move |i| {
        let t0 = (i as f32 / SEGMENTS as f32) * std::f32::consts::TAU;
        let t1 = ((i + 1) as f32 / SEGMENTS as f32) * std::f32::consts::TAU;
        WorldLine { a: point(t0), b: point(t1), color, caps: false }
    })
}

/// Screen-space point-to-segment distance, for handle hit-testing.
fn point_segment_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let len2 = abx * abx + aby * aby;
    if len2 < 1e-6 {
        return dist2(p, a).sqrt();
    }
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len2).clamp(0.0, 1.0);
    let proj = (a.0 + abx * t, a.1 + aby * t);
    dist2(p, proj).sqrt()
}

/// The handle of `mode` under `click` (render pixels of `view`), or `None`
/// when no handle lies within [`AXIS_PICK_TOLERANCE_PT`]. A locked handle
/// is returned too; the host decides to refuse the drag. An end-on handle
/// is never returned.
pub fn pick_axis(mode: GizmoMode, target: &GizmoTarget, view: &GizmoView, click: (f32, f32)) -> Option<GizmoAxis> {
    let (cam, width, height) = (&view.camera, view.width, view.height);
    let tolerance = AXIS_PICK_TOLERANCE_PT * view.px_per_point;
    let origin = target.origin;
    let origin_px = cam.project_to_pixel(origin, width, height)?;
    let len = handle_length(origin, cam, height);
    let mut best: Option<(GizmoAxis, f32)> = None;
    for handle in target.handles.iter().filter(|handle| handle.mode == mode && !target.end_on(mode, handle.axis, cam)) {
        let d = match mode {
            GizmoMode::Move | GizmoMode::Scale => {
                let tip = offset(origin, target.handle_direction(handle.axis), len);
                let Some(tip_px) = cam.project_to_pixel(tip, width, height) else { continue };
                point_segment_dist(click, (origin_px.px, origin_px.py), (tip_px.px, tip_px.py))
            }
            GizmoMode::Rotate => ring_lines(origin, handle.axis, len, [0; 4])
                .filter_map(|seg| {
                    let a = cam.project_to_pixel(target.world_point(seg.a), width, height)?;
                    let b = cam.project_to_pixel(target.world_point(seg.b), width, height)?;
                    Some(point_segment_dist(click, (a.px, a.py), (b.px, b.py)))
                })
                .fold(f32::INFINITY, f32::min),
        };
        if d <= tolerance && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((handle.axis, d));
        }
    }
    best.map(|(axis, _)| axis)
}

/// The values a handle drag writes, in its write's param order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HandleValues {
    One(f32),
    Three([f32; 3]),
}

/// One handle drag. The target is held as it was at the press, so every
/// update computes from the pressed values, not from whatever the graph
/// shows mid-drag.
#[derive(Debug, Clone, PartialEq)]
pub struct GizmoDrag {
    pub target: GizmoTarget,
    pub mode: GizmoMode,
    pub axis: GizmoAxis,
    /// Where along the axis the press held a Move or Scale handle, in param
    /// units. Fixed for the drag: a camera that moves mid-drag never shifts
    /// the grab, and each update finds the cursor through the camera on
    /// screen, so the grabbed point stays under it.
    grab: Option<f32>,
    last: (f32, f32),
    angle: f32,
}

impl GizmoDrag {
    /// A press at render pixel `at` of `view`. `None` when the target has
    /// no such handle, the handle is locked, the target is not placed yet,
    /// or a linear handle cannot be held there (end-on, behind the camera,
    /// or a Scale press on the origin itself).
    pub fn begin(target: GizmoTarget, mode: GizmoMode, axis: GizmoAxis, view: &GizmoView, at: (f32, f32)) -> Option<Self> {
        let handle = target.handle(mode, axis)?;
        if handle.write.locked() || target.unplaced {
            return None;
        }
        let held_linearly = match (&handle.write, mode) {
            (HandleWrite::Linear { .. } | HandleWrite::Uniform { .. }, _) | (HandleWrite::Selection { .. }, GizmoMode::Move | GizmoMode::Scale) => true,
            (HandleWrite::Angle(_) | HandleWrite::Direction { .. }, _) | (HandleWrite::Selection { .. }, GizmoMode::Rotate) => false,
        };
        let grab = if held_linearly {
            let grab = target.axis_param(axis, view, at)?;
            if mode == GizmoMode::Scale && grab.abs() < 1e-6 {
                return None;
            }
            Some(grab)
        } else {
            None
        };
        Some(Self { target, mode, axis, grab, last: at, angle: 0.0 })
    }

    pub fn handle(&self) -> &GizmoHandle {
        self.target.handle(self.mode, self.axis).expect("begin checked the handle")
    }

    /// The handle's values with the pointer at `cur` (render pixels of
    /// `view`).
    pub fn update(&mut self, view: &GizmoView, cur: (f32, f32)) -> Option<HandleValues> {
        let origin = self.target.origin;
        let turned = rotate_drag_delta(origin, &view.camera, view.width, view.height, self.last, cur);
        self.last = cur;
        let write = self.handle().write.clone();
        let values = match &write {
            HandleWrite::Linear { param, limit } => {
                let (grab, held) = (self.grab?, self.target.axis_param(self.axis, view, cur)?);
                // Move adds the distance dragged; Scale multiplies by the
                // ratio of the axis distances, as Blender and Unity scale, so
                // the same on-screen drag scales alike near and far.
                let value = match self.mode {
                    GizmoMode::Scale => param.value * held / grab,
                    GizmoMode::Move | GizmoMode::Rotate => param.value + (held - grab),
                };
                // A handle has no slider range: only the declared limit stops it.
                HandleValues::One(limit.clamp_input(value, (f32::NEG_INFINITY, f32::INFINITY)))
            }
            HandleWrite::Uniform { params, limit } => {
                let (grab, held) = (self.grab?, self.target.axis_param(self.axis, view, cur)?);
                let value = limit.clamp_input(params[0].value * held / grab, (f32::NEG_INFINITY, f32::INFINITY));
                HandleValues::Three([value; 3])
            }
            HandleWrite::Angle(param) => {
                self.angle += turned?;
                HandleValues::One(param.value + self.angle)
            }
            HandleWrite::Direction { params, normalize } => {
                self.angle += turned?;
                let base = params.each_ref().map(|param| param.value);
                let rotated = rotate_about(base, self.axis.unit(), self.angle);
                HandleValues::Three(if *normalize { normalized(rotated)? } else { rotated })
            }
            // The selection's amount: the distance moved, the factor scaled
            // (never through zero) or the angle turned.
            HandleWrite::Selection { .. } => HandleValues::One(match self.mode {
                GizmoMode::Move => self.target.axis_param(self.axis, view, cur)? - self.grab?,
                GizmoMode::Scale => (self.target.axis_param(self.axis, view, cur)? / self.grab?).max(MIN_EXTENT),
                GizmoMode::Rotate => {
                    self.angle += turned?;
                    self.angle
                }
            }),
        };
        match values {
            HandleValues::One(v) if v.is_finite() => Some(values),
            HandleValues::Three(v) if v.iter().all(|c| c.is_finite()) => Some(values),
            HandleValues::One(_) | HandleValues::Three(_) => None,
        }
    }
}

/// `v` turned by `angle` radians about the unit axis `k` (Rodrigues).
fn rotate_about(v: [f32; 3], k: [f32; 3], angle: f32) -> [f32; 3] {
    let (s, c) = angle.sin_cos();
    let cross = [k[1] * v[2] - k[2] * v[1], k[2] * v[0] - k[0] * v[2], k[0] * v[1] - k[1] * v[0]];
    let dot = k[0] * v[0] + k[1] * v[1] + k[2] * v[2];
    std::array::from_fn(|i| v[i] * c + cross[i] * s + k[i] * dot * (1.0 - c))
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// Rotation delta (radians) for dragging a ring: the screen-space angle
/// swept around the origin's projected point from `prev_mouse` to
/// `cur_mouse`. `None` when the origin is behind the camera or under the
/// pointer.
pub fn rotate_drag_delta(
    origin: [f32; 3],
    cam: &Camera,
    width: u32,
    height: u32,
    prev_mouse: (f32, f32),
    cur_mouse: (f32, f32),
) -> Option<f32> {
    let o = cam.project_to_pixel(origin, width, height)?;
    let v0 = (prev_mouse.0 - o.px, prev_mouse.1 - o.py);
    let v1 = (cur_mouse.0 - o.px, cur_mouse.1 - o.py);
    if v0.0.hypot(v0.1) < 1e-3 || v1.0.hypot(v1.1) < 1e-3 {
        return None;
    }
    let mut d = v1.1.atan2(v1.0) - v0.1.atan2(v0.0);
    // Wrap to (-PI, PI] so crossing the atan2 seam does not spike.
    while d > std::f32::consts::PI {
        d -= std::f32::consts::TAU;
    }
    while d < -std::f32::consts::PI {
        d += std::f32::consts::TAU;
    }
    Some(d)
}

#[cfg(test)]
mod tests;
