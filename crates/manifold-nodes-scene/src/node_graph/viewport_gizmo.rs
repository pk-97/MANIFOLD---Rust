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
//! **Picking is origin-based, not an ID-buffer pass.** [`pick_target`]
//! projects each pickable target's origin through the camera the frame was
//! rendered with and takes the nearest within [`PICK_RADIUS_PX`]. Two
//! targets overlapping on screen resolve to the nearer origin, not the one
//! drawn on top.
//!
//! A handle whose param is wired reports locked: the host refuses the drag
//! and the handle draws in [`LOCKED_COLOR`]. The viewport never fights the
//! graph.
//!
//! **One composition entry draws every host's chrome** ([`compose_lines`]):
//! the ground grid, then what physics did (light markers, accepted domain
//! boxes, solver-reported shapes, all from the host's frame report), then
//! the selected shape's handles from its declaration.

use std::sync::LazyLock;

use manifold_core::NodeId;
use manifold_core::fluid_domain::{FluidDomainSnapshot, FluidDomainState};
use manifold_core::physics_gizmo::PhysicsGizmo;
use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{ParamTriple, SceneShape};
use manifold_node_engine::scene::camera::Camera;

use crate::node_graph::scene_vm::{FluidRole, SceneShapeVm, SceneVm, ShapeOwner, ShapeParamVm, ShapeWrite};
use crate::node_graph::viewport_overlay::{
    PHYSICS_GIZMO_LINES, ViewportOverlayConfig, WorldLine, fluid_domain_lines, grid_lines, light_billboard_lines,
    physics_gizmo_lines,
};

/// Click/handle hit-test radius, screen pixels.
pub const PICK_RADIUS_PX: f32 = 22.0;
/// Axis-handle hit-test tolerance, screen pixels.
pub const AXIS_PICK_TOLERANCE_PX: f32 = 10.0;
/// World-space length of a gizmo axis handle.
pub const GIZMO_HANDLE_LEN: f32 = 1.25;
/// The smallest size or radius a scale handle writes.
const MIN_EXTENT: f32 = 0.01;

const X_COLOR: [u8; 4] = [225, 70, 70, 255];
const Y_COLOR: [u8; 4] = [90, 210, 110, 255];
const Z_COLOR: [u8; 4] = [90, 140, 235, 255];
const UNIFORM_COLOR: [u8; 4] = [235, 235, 235, 255];
const DIRECTION_COLOR: [u8; 4] = [240, 200, 80, 255];
const LOCKED_COLOR: [u8; 4] = [130, 130, 130, 255];
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
    Inflow,
    Drain,
    Collider,
}

impl GizmoTargetKind {
    pub fn of(owner: &ShapeOwner) -> Self {
        match owner {
            ShapeOwner::Object(_) => Self::Object,
            ShapeOwner::LiquidDomain(_) => Self::FluidDomain,
            ShapeOwner::RoleSource { role, .. } => match role {
                FluidRole::Inflow => Self::Inflow,
                FluidRole::Outflow => Self::Drain,
                FluidRole::Collider => Self::Collider,
            },
            ShapeOwner::Force(_) => Self::Force,
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
}

impl HandleWrite {
    pub fn params(&self) -> &[ShapeParamVm] {
        match self {
            HandleWrite::Linear { param, .. } | HandleWrite::Angle(param) => std::slice::from_ref(param),
            HandleWrite::Direction { params, .. } => params,
        }
    }

    /// A wire feeds a param this handle writes.
    pub fn locked(&self) -> bool {
        self.params().iter().any(|param| param.wired)
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
    pub row: Option<u32>,
    /// The declared direction, drawn as a line from the origin.
    pub direction: Option<[f32; 3]>,
    pub handles: Vec<GizmoHandle>,
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
                for (axis, id) in GizmoAxis::XYZ.into_iter().zip(size) {
                    if let Some(param) = param(id) {
                        let write = HandleWrite::Linear { param, limit: extent_limit };
                        handles.push(GizmoHandle { mode: GizmoMode::Scale, axis, write });
                    }
                }
            }
            SceneShape::Sphere { center, radius } | SceneShape::PointRadius { center, radius } => {
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
            SceneShape::Box { .. } | SceneShape::Sphere { .. } | SceneShape::Plane { .. } | SceneShape::PointRadius { .. } | SceneShape::Axis { .. } => [0.0; 3],
        });
        let (origin, world_axes) = match shape.frame {
            Some(m) => (
                std::array::from_fn(|i| m[3][i] + (0..3).map(|j| m[j][i] * local[j]).sum::<f32>()),
                std::array::from_fn(|j| [m[j][0], m[j][1], m[j][2]]),
            ),
            None => (local, IDENTITY_AXES),
        };
        Self {
            id: GizmoTargetId { owner: shape.owner.clone(), shape: shape.index },
            kind: GizmoTargetKind::of(&shape.owner),
            origin,
            world_axes,
            unplaced: shape.params.iter().any(|param| param.write == ShapeWrite::Unplaced),
            pickable: shape.pickable,
            row: shape.row,
            direction,
            handles,
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

    fn world_point(&self, point: [f32; 3]) -> [f32; 3] {
        let d = [point[0] - self.origin[0], point[1] - self.origin[1], point[2] - self.origin[2]];
        std::array::from_fn(|i| self.origin[i] + (0..3).map(|j| self.world_axes[j][i] * d[j]).sum::<f32>())
    }

    pub fn projected_drag_delta(&self, axis: GizmoAxis, cam: &Camera, width: u32, height: u32, delta: (f32, f32)) -> Option<f32> {
        direction_drag_delta(self.origin, self.axis_direction(axis), cam, width, height, delta)
    }

    /// The world point a pointer grabs `mode`'s handle on `axis` by: a
    /// linear handle's tip, or a ring an eighth of a turn round, where no
    /// other ring passes. [`pick_axis`] returns that handle there unless
    /// another ring stands edge-on over it.
    pub fn grip(&self, mode: GizmoMode, axis: GizmoAxis) -> Option<[f32; 3]> {
        self.handle(mode, axis)?;
        match mode {
            GizmoMode::Move | GizmoMode::Scale => Some(offset(self.origin, self.axis_direction(axis), GIZMO_HANDLE_LEN)),
            GizmoMode::Rotate => ring_lines(self.origin, axis, GIZMO_HANDLE_LEN, [0; 4]).nth(3).map(|line| self.world_point(line.a)),
        }
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

/// The pickable target whose origin projects nearest `click`, within
/// [`PICK_RADIUS_PX`]. `None` clears the selection.
pub fn pick_target(scene: &SceneVm, cam: &Camera, width: u32, height: u32, click: (f32, f32)) -> Option<GizmoTargetId> {
    let mut best: Option<(GizmoTargetId, f32)> = None;
    for target in gizmo_targets(scene).filter(|target| target.pickable) {
        let Some(proj) = cam.project_to_pixel(target.origin, width, height) else { continue };
        let d = dist2(click, (proj.px, proj.py));
        if d <= PICK_RADIUS_PX * PICK_RADIUS_PX && best.as_ref().is_none_or(|(_, bd)| d < *bd) {
            best = Some((target.id, d));
        }
    }
    best.map(|(id, _)| id)
}

fn dist2(a: (f32, f32), b: (f32, f32)) -> f32 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    dx * dx + dy * dy
}

/// What one host frame shows (REALTIME_3D_DESIGN.md D7a). What physics did
/// comes from the host's frame report; handles come from the selected
/// shape's declaration.
#[derive(Clone, Copy)]
pub struct GizmoChrome<'a> {
    pub lights: &'a [[f32; 3]],
    pub domains: &'a [(NodeId, FluidDomainSnapshot)],
    pub physics: &'a [PhysicsGizmo],
    pub selected: Option<&'a GizmoTarget>,
    pub mode: GizmoMode,
    /// An effect after the scene layer moves the picture off the scene, so
    /// the handles fade (D-69).
    pub fade_handles: bool,
}

/// Every line a host draws for one frame, into `out` (cleared first): the
/// ground grid, a marker per light, a box per accepted liquid domain, each
/// solver-reported physics shape, then the selected shape's handles. The
/// one composition entry every host calls.
pub fn compose_lines(chrome: &GizmoChrome<'_>, out: &mut Vec<WorldLine>) {
    out.clear();
    out.extend_from_slice(&GROUND_GRID);
    let size = ViewportOverlayConfig::default().light_billboard_size;
    for &pos in chrome.lights {
        out.extend(light_billboard_lines(pos, size));
    }
    let accepted = chrome
        .domains
        .iter()
        .filter(|(_, snapshot)| snapshot.state == FluidDomainState::Ready)
        .filter_map(|(_, snapshot)| snapshot.accepted_layout);
    for layout in accepted {
        out.extend(fluid_domain_lines(layout));
    }
    let mut shape = [WorldLine { a: [0.0; 3], b: [0.0; 3], color: [0; 4] }; PHYSICS_GIZMO_LINES];
    for gizmo in chrome.physics {
        let count = physics_gizmo_lines(gizmo, &mut shape);
        out.extend_from_slice(&shape[..count]);
    }
    if let Some(target) = chrome.selected {
        let start = out.len();
        gizmo_lines(chrome.mode, target, out);
        if chrome.fade_handles {
            for line in &mut out[start..] {
                line.color[3] = line.color[3].min(FADED_HANDLE_ALPHA);
            }
        }
    }
}

/// World-space handle geometry for `mode` at `target`, plus the declared
/// direction when the shape has one, appended to `lines`. Locked handles
/// draw in [`LOCKED_COLOR`].
pub fn gizmo_lines(mode: GizmoMode, target: &GizmoTarget, lines: &mut Vec<WorldLine>) {
    let origin = target.origin;
    if let Some(direction) = target.direction.and_then(normalized) {
        lines.push(WorldLine { a: origin, b: offset(origin, direction, GIZMO_HANDLE_LEN * 1.4), color: DIRECTION_COLOR });
    }
    for handle in target.handles.iter().filter(|handle| handle.mode == mode) {
        let color = if handle.write.locked() { LOCKED_COLOR } else { handle.axis.color() };
        match mode {
            GizmoMode::Move => {
                let tip = offset(origin, target.axis_direction(handle.axis), GIZMO_HANDLE_LEN);
                lines.push(WorldLine { a: origin, b: tip, color });
            }
            GizmoMode::Scale => {
                let u = target.axis_direction(handle.axis);
                let tip = offset(origin, u, GIZMO_HANDLE_LEN);
                // A short tick at the tip tells scale from move.
                let perp = perpendicular(u);
                let tick = GIZMO_HANDLE_LEN * 0.12;
                lines.push(WorldLine { a: origin, b: tip, color });
                lines.push(WorldLine { a: offset(tip, perp, tick), b: offset(tip, perp, -tick), color });
            }
            GizmoMode::Rotate => {
                lines.extend(ring_lines(origin, handle.axis, GIZMO_HANDLE_LEN, color).map(|line| WorldLine {
                    a: target.world_point(line.a),
                    b: target.world_point(line.b),
                    color: line.color,
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
    const SEGMENTS: usize = 24;
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
        WorldLine { a: point(t0), b: point(t1), color }
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

/// The handle of `mode` under `click`, or `None` when no handle lies within
/// [`AXIS_PICK_TOLERANCE_PX`]. A locked handle is returned too; the host
/// decides to refuse the drag.
pub fn pick_axis(mode: GizmoMode, target: &GizmoTarget, cam: &Camera, width: u32, height: u32, click: (f32, f32)) -> Option<GizmoAxis> {
    let origin = target.origin;
    let origin_px = cam.project_to_pixel(origin, width, height)?;
    let mut best: Option<(GizmoAxis, f32)> = None;
    for handle in target.handles.iter().filter(|handle| handle.mode == mode) {
        let d = match mode {
            GizmoMode::Move | GizmoMode::Scale => {
                let tip = offset(origin, target.axis_direction(handle.axis), GIZMO_HANDLE_LEN);
                let Some(tip_px) = cam.project_to_pixel(tip, width, height) else { continue };
                point_segment_dist(click, (origin_px.px, origin_px.py), (tip_px.px, tip_px.py))
            }
            GizmoMode::Rotate => ring_lines(origin, handle.axis, GIZMO_HANDLE_LEN, [0; 4])
                .filter_map(|seg| {
                    let a = cam.project_to_pixel(target.world_point(seg.a), width, height)?;
                    let b = cam.project_to_pixel(target.world_point(seg.b), width, height)?;
                    Some(point_segment_dist(click, (a.px, a.py), (b.px, b.py)))
                })
                .fold(f32::INFINITY, f32::min),
        };
        if d <= AXIS_PICK_TOLERANCE_PX && best.is_none_or(|(_, bd)| d < bd) {
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
    start: (f32, f32),
    last: (f32, f32),
    angle: f32,
}

impl GizmoDrag {
    /// `None` when the target has no such handle, the handle is locked, or
    /// the target is not placed yet.
    pub fn begin(target: GizmoTarget, mode: GizmoMode, axis: GizmoAxis, at: (f32, f32)) -> Option<Self> {
        let handle = target.handle(mode, axis)?;
        if handle.write.locked() || target.unplaced {
            return None;
        }
        Some(Self { target, mode, axis, start: at, last: at, angle: 0.0 })
    }

    pub fn handle(&self) -> &GizmoHandle {
        self.target.handle(self.mode, self.axis).expect("begin checked the handle")
    }

    /// The handle's values with the pointer at `cur` (render pixels).
    pub fn update(&mut self, cam: &Camera, width: u32, height: u32, cur: (f32, f32)) -> Option<HandleValues> {
        let origin = self.target.origin;
        let turned = rotate_drag_delta(origin, cam, width, height, self.last, cur);
        self.last = cur;
        let write = self.handle().write.clone();
        let values = match &write {
            HandleWrite::Linear { param, limit } => {
                let delta = self.target.projected_drag_delta(self.axis, cam, width, height, (cur.0 - self.start.0, cur.1 - self.start.1))?;
                // A handle has no slider range: only the declared limit stops it.
                HandleValues::One(limit.clamp_input(param.value + delta, (f32::NEG_INFINITY, f32::INFINITY)))
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

fn direction_drag_delta(origin: [f32; 3], direction: [f32; 3], cam: &Camera, width: u32, height: u32, mouse_delta: (f32, f32)) -> Option<f32> {
    const EPS: f32 = 1.0;
    let p0 = cam.project_to_pixel(origin, width, height)?;
    let p1 = cam.project_to_pixel(offset(origin, direction, EPS), width, height)?;
    let dir = (p1.px - p0.px, p1.py - p0.py);
    let px_per_unit = (dir.0 * dir.0 + dir.1 * dir.1).sqrt() / EPS;
    if px_per_unit < 1e-4 {
        return None;
    }
    let dir_n = (dir.0 / (px_per_unit * EPS), dir.1 / (px_per_unit * EPS));
    let along_px = mouse_delta.0 * dir_n.0 + mouse_delta.1 * dir_n.1;
    Some(along_px / px_per_unit)
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
