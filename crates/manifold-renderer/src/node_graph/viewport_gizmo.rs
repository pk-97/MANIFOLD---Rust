//! Viewport Tier 2 — move/rotate/scale gizmos
//! (`docs/REALTIME_3D_DESIGN.md` D7/D8, P6).
//!
//! Pure CPU functions, no GPU: picking and gizmo-handle geometry ride the
//! SAME `Camera::project_to_pixel` oracle `viewport_overlay.rs` already
//! trusts for the P5 grid/frustum/light overlay, so gizmo placement and the
//! scene's own on-GPU projection can never silently disagree. This mirrors
//! `viewport_overlay`'s own documented scope choice: gizmo chrome is 2D
//! editor UI drawn straight onto the tonemapped readback, never a GPU pass —
//! it structurally cannot touch a shadow map, a fusion boundary, or the
//! no-monolith rules.
//!
//! **Object-center picking, not an ID-buffer pass (documented deviation from
//! D7's literal wording, same class of pivot P5 took with its throwaway
//! runtime):** D7 names "ID-buffer picking" as the Tier 2 mechanism. Building
//! a real per-pixel object-id G-buffer output on `render_scene` would need a
//! new lazy output (alongside `depth`/`world_normal`) PLUS a second readback
//! path — real scope, and it still wouldn't beat analytic picking for a flat
//! scene with `OBJECT_SLIDER_MAX = 64` objects. [`pick_object`] instead
//! projects each known object's transform origin (or the origin for an
//! unwired/identity transform) through the SAME editor camera the frame was
//! rendered with and picks the nearest projected point within
//! [`PICK_RADIUS_PX`] pixels of the click — origin-only, not silhouette-
//! accurate, so two objects that overlap on screen resolve to whichever
//! origin is closer to the click point rather than whichever is drawn on
//! top. Acceptable for v1's flat, un-occluded object list; a true ID pass is
//! the natural P7+ upgrade if dense/overlapping scenes make this bite.
//!
//! Gizmo target resolution follows D8's amended semantics: a gizmo drags one
//! of the object's `node.transform_3d` atom's nine scalar params (found via
//! [`crate::node_graph::scene_vm::SceneVm`]'s existing transform trace); a
//! wired axis (`TransformVm::pos_driven`/`rot_driven`/`scale_driven`) is
//! reported to the caller so it can refuse the drag and render that one axis
//! in [`LOCKED_COLOR`] instead of its normal per-axis color — the viewport
//! never fights the graph.

use crate::node_graph::camera::Camera;
use crate::node_graph::scene_vm::{ParamAddr, SceneObjectVm, SceneVm, TransformVm};
use crate::node_graph::viewport_overlay::WorldLine;

/// Click/handle hit-test radius, screen pixels.
pub const PICK_RADIUS_PX: f32 = 22.0;
/// Axis-handle hit-test tolerance, screen pixels (a line is thin; give it a
/// forgiving grab area, same idea as a DCC's gizmo hit box).
pub const AXIS_PICK_TOLERANCE_PX: f32 = 10.0;
/// World-space length of a gizmo axis handle — small enough not to swamp a
/// typically-scaled scene object, large enough to grab comfortably.
pub const GIZMO_HANDLE_LEN: f32 = 1.25;

const X_COLOR: [u8; 4] = [225, 70, 70, 255];
const Y_COLOR: [u8; 4] = [90, 210, 110, 255];
const Z_COLOR: [u8; 4] = [90, 140, 235, 255];
const LOCKED_COLOR: [u8; 4] = [130, 130, 130, 255];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GizmoMode {
    #[default]
    Move,
    Rotate,
    Scale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GizmoAxis {
    X,
    Y,
    Z,
}

impl GizmoAxis {
    pub fn unit(self) -> [f32; 3] {
        match self {
            GizmoAxis::X => [1.0, 0.0, 0.0],
            GizmoAxis::Y => [0.0, 1.0, 0.0],
            GizmoAxis::Z => [0.0, 0.0, 1.0],
        }
    }

    fn color(self) -> [u8; 4] {
        match self {
            GizmoAxis::X => X_COLOR,
            GizmoAxis::Y => Y_COLOR,
            GizmoAxis::Z => Z_COLOR,
        }
    }

    /// The write address + driven flag for this axis on `t`, per `mode`.
    fn addr_and_driven(self, mode: GizmoMode, t: &TransformVm) -> (ParamAddr, bool) {
        let (addrs, driven) = match mode {
            GizmoMode::Move => (&t.pos_addr, t.pos_driven),
            GizmoMode::Rotate => (&t.rot_addr, t.rot_driven),
            GizmoMode::Scale => (&t.scale_addr, t.scale_driven),
        };
        match self {
            GizmoAxis::X => (addrs.0.clone(), driven.0),
            GizmoAxis::Y => (addrs.1.clone(), driven.1),
            GizmoAxis::Z => (addrs.2.clone(), driven.2),
        }
    }

    fn current_value(self, mode: GizmoMode, t: &TransformVm) -> f32 {
        let v = match mode {
            GizmoMode::Move => t.pos_value,
            GizmoMode::Rotate => t.rot_value,
            GizmoMode::Scale => t.scale_value,
        };
        match self {
            GizmoAxis::X => v.0,
            GizmoAxis::Y => v.1,
            GizmoAxis::Z => v.2,
        }
    }
}

/// A resolved gizmo target: one selected, known scene object plus (if its
/// `transform` port is wired to a `node.transform_3d` atom — D8's "follow
/// the `transform_n` wire") the write surface for a drag.
#[derive(Debug, Clone)]
pub struct GizmoTarget {
    pub object_node_id: u32,
    /// World-space origin to draw the gizmo at and pick axes against.
    /// `[0,0,0]` (identity) when `transform` is `None` — an unwired object's
    /// scene_object still renders at the identity transform (D8/SCENE_BUILD
    /// P2's "unwired = identity" contract), so the gizmo has a well-defined
    /// place to appear even before the user has dragged anything.
    pub origin: [f32; 3],
    /// `Some` when the object's `transform` port already resolves to a
    /// `node.transform_3d` atom — the direct-drag case. `None` means P6's
    /// "unwired `transform_n` → gizmo offers to create the atom" entry
    /// state: the first axis drag must go through
    /// `manifold_editing::commands::graph::AddObjectTransformCommand`
    /// before any `SetGraphNodeParamCommand` can target it.
    pub transform: Option<TransformVm>,
}

/// Find the selected object (`object_node_id`) in `scene` and resolve its
/// gizmo target. `None` if the id isn't a `Known` object in this scene this
/// frame (e.g. it was just deleted) — the caller drops the gizmo/selection
/// rather than drawing stale geometry (no-silent-fallbacks).
pub fn gizmo_target_for(scene: &SceneVm, object_node_id: u32) -> Option<GizmoTarget> {
    scene.objects.iter().find_map(|o| match o {
        SceneObjectVm::Known(row) if row.object_node_id == object_node_id => {
            let origin = row.transform.as_ref().map(|t| [t.pos_value.0, t.pos_value.1, t.pos_value.2]).unwrap_or([0.0, 0.0, 0.0]);
            Some(GizmoTarget { object_node_id, origin, transform: row.transform.clone() })
        }
        _ => None,
    })
}

/// Object-center pick (see module docs for why this isn't an ID-buffer
/// pass): the nearest `Known` object whose origin projects within
/// [`PICK_RADIUS_PX`] of `click`, or `None` if nothing in `scene` qualifies
/// (empty scene, everything behind the camera, or nothing within range —
/// the caller should clear selection, not leave it stale).
pub fn pick_object(scene: &SceneVm, cam: &Camera, width: u32, height: u32, click: (f32, f32)) -> Option<u32> {
    let mut best: Option<(u32, f32)> = None;
    for obj in &scene.objects {
        let SceneObjectVm::Known(row) = obj else { continue };
        let origin = row.transform.as_ref().map(|t| [t.pos_value.0, t.pos_value.1, t.pos_value.2]).unwrap_or([0.0, 0.0, 0.0]);
        let Some(proj) = cam.project_to_pixel(origin, width, height) else { continue };
        let d = dist2(click, (proj.px, proj.py));
        if d <= PICK_RADIUS_PX * PICK_RADIUS_PX && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((row.object_node_id, d));
        }
    }
    best.map(|(id, _)| id)
}

fn dist2(a: (f32, f32), b: (f32, f32)) -> f32 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    dx * dx + dy * dy
}

/// Build the gizmo's world-space handle geometry for `mode` at `target`'s
/// origin. Locked axes (per `target.transform`'s `_driven` flags — always
/// unlocked when `transform` is `None`, since there's nothing wired yet)
/// draw in [`LOCKED_COLOR`] instead of their normal per-axis color, per D8:
/// "the viewport never fights the graph."
pub fn gizmo_lines(mode: GizmoMode, target: &GizmoTarget) -> Vec<WorldLine> {
    let origin = target.origin;
    let driven = |axis: GizmoAxis| -> bool {
        target.transform.as_ref().is_some_and(|t| {
            let d = match mode {
                GizmoMode::Move => t.pos_driven,
                GizmoMode::Rotate => t.rot_driven,
                GizmoMode::Scale => t.scale_driven,
            };
            match axis {
                GizmoAxis::X => d.0,
                GizmoAxis::Y => d.1,
                GizmoAxis::Z => d.2,
            }
        })
    };
    let color = |axis: GizmoAxis| if driven(axis) { LOCKED_COLOR } else { axis.color() };

    match mode {
        GizmoMode::Move => [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z]
            .into_iter()
            .map(|axis| {
                let u = axis.unit();
                let tip = offset(origin, u, GIZMO_HANDLE_LEN);
                WorldLine { a: origin, b: tip, color: color(axis) }
            })
            .collect(),
        GizmoMode::Scale => [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z]
            .into_iter()
            .flat_map(|axis| {
                let u = axis.unit();
                let tip = offset(origin, u, GIZMO_HANDLE_LEN);
                let c = color(axis);
                // A small perpendicular tick at the tip distinguishes the
                // scale handle from move's bare line, cheaply (two extra
                // segments, no new geometry kind).
                let perp = perpendicular(u);
                let tick_len = GIZMO_HANDLE_LEN * 0.12;
                vec![
                    WorldLine { a: origin, b: tip, color: c },
                    WorldLine { a: offset(tip, perp, tick_len), b: offset(tip, perp, -tick_len), color: c },
                ]
            })
            .collect(),
        GizmoMode::Rotate => [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z]
            .into_iter()
            .flat_map(|axis| ring_lines(origin, axis, GIZMO_HANDLE_LEN, color(axis)))
            .collect(),
    }
}

fn offset(p: [f32; 3], dir: [f32; 3], len: f32) -> [f32; 3] {
    [p[0] + dir[0] * len, p[1] + dir[1] * len, p[2] + dir[2] * len]
}

/// Any unit vector perpendicular to `u` (up to sign/rotation — only used for
/// a short decorative tick, not a physically meaningful basis).
fn perpendicular(u: [f32; 3]) -> [f32; 3] {
    if u[0].abs() < 0.9 { [0.0, -u[2], u[1]] } else { [-u[1], u[0], 0.0] }
}

/// A ring of line segments in the plane perpendicular to `axis`, centered at
/// `origin` with radius `r` — the rotate-mode handle for that axis.
fn ring_lines(origin: [f32; 3], axis: GizmoAxis, r: f32, color: [u8; 4]) -> Vec<WorldLine> {
    const SEGMENTS: usize = 24;
    let (a, b) = match axis {
        GizmoAxis::X => ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        GizmoAxis::Y => ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        GizmoAxis::Z => ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
    };
    let point = |t: f32| -> [f32; 3] {
        let (s, c) = t.sin_cos();
        [
            origin[0] + r * (c * a[0] + s * b[0]),
            origin[1] + r * (c * a[1] + s * b[1]),
            origin[2] + r * (c * a[2] + s * b[2]),
        ]
    };
    (0..SEGMENTS)
        .map(|i| {
            let t0 = (i as f32 / SEGMENTS as f32) * std::f32::consts::TAU;
            let t1 = ((i + 1) as f32 / SEGMENTS as f32) * std::f32::consts::TAU;
            WorldLine { a: point(t0), b: point(t1), color }
        })
        .collect()
}

/// Screen-space point-to-segment distance, for axis hit-testing.
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

/// Pick which axis handle `click` lands on, for `mode` at `target`'s origin,
/// projected through `cam`. `None` if the click isn't within
/// [`AXIS_PICK_TOLERANCE_PX`] of any handle, or the origin is behind the
/// camera. Returns a locked axis too (the caller decides whether to refuse
/// the drag) — the gizmo still highlights what you grabbed.
pub fn pick_axis(
    mode: GizmoMode,
    target: &GizmoTarget,
    cam: &Camera,
    width: u32,
    height: u32,
    click: (f32, f32),
) -> Option<GizmoAxis> {
    let origin = target.origin;
    let origin_px = cam.project_to_pixel(origin, width, height)?;
    let mut best: Option<(GizmoAxis, f32)> = None;
    for axis in [GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z] {
        let d = match mode {
            GizmoMode::Move | GizmoMode::Scale => {
                let tip = offset(origin, axis.unit(), GIZMO_HANDLE_LEN);
                let Some(tip_px) = cam.project_to_pixel(tip, width, height) else { continue };
                point_segment_dist(click, (origin_px.px, origin_px.py), (tip_px.px, tip_px.py))
            }
            GizmoMode::Rotate => {
                // The rotate handle is a ring, not a line to a tip — test
                // against its projected segments and keep the nearest one.
                let ring = ring_lines(origin, axis, GIZMO_HANDLE_LEN, [0, 0, 0, 0]);
                let mut nearest = f32::INFINITY;
                for seg in &ring {
                    let (Some(a), Some(b)) =
                        (cam.project_to_pixel(seg.a, width, height), cam.project_to_pixel(seg.b, width, height))
                    else {
                        continue;
                    };
                    let d = point_segment_dist(click, (a.px, a.py), (b.px, b.py));
                    nearest = nearest.min(d);
                }
                nearest
            }
        };
        if d <= AXIS_PICK_TOLERANCE_PX && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((axis, d));
        }
    }
    best.map(|(axis, _)| axis)
}

/// The write address + current value + driven flag for dragging `axis` in
/// `mode` on `target`. `None` if `target.transform` is `None` (P6 entry
/// state: the caller must create the `node.transform_3d` atom first — see
/// module docs).
pub fn drag_write(mode: GizmoMode, axis: GizmoAxis, target: &GizmoTarget) -> Option<(ParamAddr, f32, bool)> {
    let t = target.transform.as_ref()?;
    let (addr, driven) = axis.addr_and_driven(mode, t);
    let current = axis.current_value(mode, t);
    Some((addr, current, driven))
}

/// World-space (Move) / raw multiplier (Scale) delta along `axis` for a
/// screen-space mouse delta, computed by projecting the origin and a small
/// offset along the axis through `cam` and taking the mouse delta's
/// component along the resulting screen-space direction, scaled by the
/// projected pixels-per-world-unit — the standard translate-gizmo drag
/// technique (equivalent to intersecting the drag with the view-aligned
/// plane containing the axis, without the extra ray-plane math). `None` if
/// the origin or its axis-offset point falls behind the camera.
pub fn move_drag_delta(
    origin: [f32; 3],
    axis: GizmoAxis,
    cam: &Camera,
    width: u32,
    height: u32,
    mouse_delta: (f32, f32),
) -> Option<f32> {
    const EPS: f32 = 1.0;
    let p0 = cam.project_to_pixel(origin, width, height)?;
    let p1 = cam.project_to_pixel(offset(origin, axis.unit(), EPS), width, height)?;
    let dir = (p1.px - p0.px, p1.py - p0.py);
    let px_per_unit = (dir.0 * dir.0 + dir.1 * dir.1).sqrt() / EPS;
    if px_per_unit < 1e-4 {
        return None;
    }
    let dir_n = (dir.0 / (px_per_unit * EPS), dir.1 / (px_per_unit * EPS));
    let along_px = mouse_delta.0 * dir_n.0 + mouse_delta.1 * dir_n.1;
    Some(along_px / px_per_unit)
}

/// Scale delta for `axis` — same projection technique as
/// [`move_drag_delta`], multiplied by [`SCALE_SENSITIVITY`] so a full
/// [`GIZMO_HANDLE_LEN`]-length drag reads as roughly a 1x change (feels
/// comparable to Move rather than needing a much longer drag).
pub const SCALE_SENSITIVITY: f32 = 1.0;
pub fn scale_drag_delta(
    origin: [f32; 3],
    axis: GizmoAxis,
    cam: &Camera,
    width: u32,
    height: u32,
    mouse_delta: (f32, f32),
) -> Option<f32> {
    move_drag_delta(origin, axis, cam, width, height, mouse_delta).map(|d| d * SCALE_SENSITIVITY)
}

/// Radians per screen pixel of horizontal+vertical drag, for
/// [`rotate_drag_delta`]'s screen-space approximation.
pub const ROTATE_SENSITIVITY: f32 = 0.012;

/// Rotation delta (radians) for dragging `axis`'s ring: the screen-space
/// angle swept around the origin's projected point from `prev_mouse` to
/// `cur_mouse` — the accurate technique for a ring handle (you're dragging
/// AROUND the circle, not along a line, so this reads correctly regardless
/// of view angle, unlike a fixed-sensitivity linear mapping). `None` if the
/// origin is behind the camera or coincides with the click point.
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
    let a0 = v0.1.atan2(v0.0);
    let a1 = v1.1.atan2(v1.0);
    let mut d = a1 - a0;
    // Wrap to (-PI, PI] so a crossing of the atan2 seam doesn't spike.
    while d > std::f32::consts::PI {
        d -= std::f32::consts::TAU;
    }
    while d < -std::f32::consts::PI {
        d += std::f32::consts::TAU;
    }
    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::scene_vm::SceneHeaderVm;

    fn cam_looking_down_neg_z(pos: [f32; 3]) -> Camera {
        Camera::look_at(pos, [pos[0], pos[1], pos[2] - 1.0], [0.0, 1.0, 0.0], 0.9, 0.1, 100.0)
    }

    fn known_object(id: u32, pos: (f32, f32, f32), driven: (bool, bool, bool)) -> SceneObjectVm {
        let addr = |n: &str| ParamAddr { scope_path: Vec::new(), node_doc_id: 99, param_id: n.to_string() };
        SceneObjectVm::Known(Box::new(crate::node_graph::scene_vm::SceneObjectKnownRow {
            index: 0,
            object_node_id: id,
            group_node_id: None,
            name: "Obj".to_string(),
            tint: None,
            visible_addr: addr("visible"),
            visible_value: true,
            visible_driven: false,
            transform: Some(TransformVm {
                node_doc_id: 99,
                pos_addr: (addr("pos_x"), addr("pos_y"), addr("pos_z")),
                pos_value: pos,
                pos_driven: driven,
                rot_addr: (addr("rot_x"), addr("rot_y"), addr("rot_z")),
                rot_value: (0.0, 0.0, 0.0),
                rot_driven: (false, false, false),
                scale_addr: (addr("scale_x"), addr("scale_y"), addr("scale_z")),
                scale_value: (1.0, 1.0, 1.0),
                scale_driven: (false, false, false),
            }),
            material: crate::node_graph::scene_vm::MaterialVm::None,
            modifier_chain: Vec::new(),
            modifier_chain_parseable: true,
            maps_present: Default::default(),
        }))
    }

    fn scene_with(objects: Vec<SceneObjectVm>) -> SceneVm {
        SceneVm {
            scene_root_node_id: 0,
            multiple_scenes: false,
            header: SceneHeaderVm::default(),
            objects,
            lights: Vec::new(),
            camera: crate::node_graph::scene_vm::CameraVm::None,
            environment: crate::node_graph::scene_vm::EnvironmentVm::None,
            atmosphere: crate::node_graph::scene_vm::AtmosphereVm::None,
        }
    }

    #[test]
    fn pick_object_finds_the_object_under_the_click() {
        let scene = scene_with(vec![known_object(1, (0.0, 0.0, -5.0), (false, false, false))]);
        let cam = cam_looking_down_neg_z([0.0, 0.0, 0.0]);
        let proj = cam.project_to_pixel([0.0, 0.0, -5.0], 640, 480).unwrap();
        let picked = pick_object(&scene, &cam, 640, 480, (proj.px, proj.py));
        assert_eq!(picked, Some(1));
    }

    #[test]
    fn pick_object_none_when_click_far_from_every_object() {
        let scene = scene_with(vec![known_object(1, (0.0, 0.0, -5.0), (false, false, false))]);
        let cam = cam_looking_down_neg_z([0.0, 0.0, 0.0]);
        let picked = pick_object(&scene, &cam, 640, 480, (5.0, 5.0));
        assert_eq!(picked, None);
    }

    #[test]
    fn gizmo_target_for_resolves_known_object_and_origin() {
        let scene = scene_with(vec![known_object(7, (1.0, 2.0, 3.0), (false, false, false))]);
        let target = gizmo_target_for(&scene, 7).expect("object 7 exists");
        assert_eq!(target.origin, [1.0, 2.0, 3.0]);
        assert!(target.transform.is_some());
    }

    #[test]
    fn gizmo_target_for_missing_object_is_none() {
        let scene = scene_with(vec![known_object(7, (0.0, 0.0, 0.0), (false, false, false))]);
        assert!(gizmo_target_for(&scene, 42).is_none());
    }

    #[test]
    fn move_gizmo_lines_locks_driven_axis_to_gray() {
        let scene = scene_with(vec![known_object(1, (0.0, 0.0, 0.0), (true, false, false))]);
        let target = gizmo_target_for(&scene, 1).unwrap();
        let lines = gizmo_lines(GizmoMode::Move, &target);
        assert_eq!(lines.len(), 3);
        let x_line = lines.iter().find(|l| (l.b[0] - l.a[0]).abs() > 0.5).unwrap();
        assert_eq!(x_line.color, LOCKED_COLOR, "driven pos_x axis should draw locked-gray");
        let y_line = lines.iter().find(|l| (l.b[1] - l.a[1]).abs() > 0.5).unwrap();
        assert_eq!(y_line.color, Y_COLOR, "undriven pos_y axis keeps its normal color");
    }

    #[test]
    fn unwired_transform_gizmo_target_has_identity_origin_and_no_transform() {
        let addr = |n: &str| ParamAddr { scope_path: Vec::new(), node_doc_id: 5, param_id: n.to_string() };
        let row = SceneObjectVm::Known(Box::new(crate::node_graph::scene_vm::SceneObjectKnownRow {
            index: 0,
            object_node_id: 5,
            group_node_id: None,
            name: "Bare".to_string(),
            tint: None,
            visible_addr: addr("visible"),
            visible_value: true,
            visible_driven: false,
            transform: None,
            material: crate::node_graph::scene_vm::MaterialVm::None,
            modifier_chain: Vec::new(),
            modifier_chain_parseable: true,
            maps_present: Default::default(),
        }));
        let scene = scene_with(vec![row]);
        let target = gizmo_target_for(&scene, 5).unwrap();
        assert_eq!(target.origin, [0.0, 0.0, 0.0]);
        assert!(target.transform.is_none());
        assert!(drag_write(GizmoMode::Move, GizmoAxis::X, &target).is_none());
    }

    #[test]
    fn pick_axis_returns_the_axis_under_the_click() {
        let scene = scene_with(vec![known_object(1, (0.0, 0.0, -5.0), (false, false, false))]);
        let target = gizmo_target_for(&scene, 1).unwrap();
        let cam = cam_looking_down_neg_z([0.0, 0.0, 0.0]);
        let tip = offset(target.origin, GizmoAxis::X.unit(), GIZMO_HANDLE_LEN);
        let tip_px = cam.project_to_pixel(tip, 640, 480).unwrap();
        let axis = pick_axis(GizmoMode::Move, &target, &cam, 640, 480, (tip_px.px, tip_px.py));
        assert_eq!(axis, Some(GizmoAxis::X));
    }

    #[test]
    fn pick_axis_none_far_from_every_handle() {
        let scene = scene_with(vec![known_object(1, (0.0, 0.0, -5.0), (false, false, false))]);
        let target = gizmo_target_for(&scene, 1).unwrap();
        let cam = cam_looking_down_neg_z([0.0, 0.0, 0.0]);
        assert_eq!(pick_axis(GizmoMode::Move, &target, &cam, 640, 480, (0.0, 0.0)), None);
    }

    #[test]
    fn move_drag_delta_along_axis_is_positive_when_dragging_toward_projected_axis_direction() {
        let cam = cam_looking_down_neg_z([0.0, 0.0, 0.0]);
        let origin = [0.0, 0.0, -5.0];
        // Looking down -Z with up=+Y, the camera's `right` is +X, so a
        // rightward screen drag should read as a positive X-axis delta.
        let d = move_drag_delta(origin, GizmoAxis::X, &cam, 640, 480, (10.0, 0.0)).unwrap();
        assert!(d > 0.0, "rightward drag should move +X, got {d}");
    }

    #[test]
    fn move_drag_delta_is_zero_for_zero_mouse_delta() {
        let cam = cam_looking_down_neg_z([0.0, 0.0, 0.0]);
        let d = move_drag_delta([0.0, 0.0, -5.0], GizmoAxis::Y, &cam, 640, 480, (0.0, 0.0)).unwrap();
        assert!(d.abs() < 1e-4);
    }

    #[test]
    fn rotate_drag_delta_quarter_turn_is_roughly_half_pi() {
        let cam = cam_looking_down_neg_z([0.0, 0.0, 0.0]);
        let origin = [0.0, 0.0, -5.0];
        let o = cam.project_to_pixel(origin, 640, 480).unwrap();
        let r = 100.0;
        let p0 = (o.px + r, o.py);
        let p1 = (o.px, o.py - r);
        let d = rotate_drag_delta(origin, &cam, 640, 480, p0, p1).unwrap();
        assert!((d.abs() - std::f32::consts::FRAC_PI_2).abs() < 0.05, "got {d}");
    }

    #[test]
    fn ring_lines_are_closed_loop_of_24_segments() {
        let lines = ring_lines([0.0, 0.0, 0.0], GizmoAxis::Y, 1.0, Y_COLOR);
        assert_eq!(lines.len(), 24);
        // Closed: the last segment's `b` should equal the first's `a`.
        let first_a = lines[0].a;
        let last_b = lines[23].b;
        for i in 0..3 {
            assert!((first_a[i] - last_b[i]).abs() < 1e-4);
        }
    }

    #[test]
    fn scale_gizmo_lines_has_tick_marks_doubling_line_count_vs_move() {
        let scene = scene_with(vec![known_object(1, (0.0, 0.0, 0.0), (false, false, false))]);
        let target = gizmo_target_for(&scene, 1).unwrap();
        let move_lines = gizmo_lines(GizmoMode::Move, &target);
        let scale_lines = gizmo_lines(GizmoMode::Scale, &target);
        assert_eq!(move_lines.len(), 3);
        assert_eq!(scale_lines.len(), 6);
    }
}
