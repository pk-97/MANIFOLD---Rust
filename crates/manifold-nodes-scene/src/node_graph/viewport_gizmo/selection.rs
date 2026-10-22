//! The selection as a set, as Blender, Cinema 4D and Maya keep it: a click
//! selects one shape, Shift-click adds or removes one, and several selected
//! shapes share one handle set at their centre that moves, turns and scales
//! them together. Pure CPU, like the rest of the core: the host turns each
//! [`SelectionWrite`] into the knob scrub of its param.

use manifold_node_engine::scene::transform::rotation_to_render_scene_euler;

use super::{
    GizmoAxis, GizmoHandle, GizmoMode, GizmoTarget, GizmoTargetId, HandleWrite, IDENTITY_AXES, ShapeOutline, cross3, dot3,
    gizmo_targets, model_matrix, normalized, rotate_about, sub3,
};
use crate::node_graph::scene_vm::{SceneVm, ShapeParamVm};

/// Every shape a host shows, which of them are selected, and the one
/// handle set.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GizmoSelection {
    /// Every declared shape in the scene.
    pub targets: Vec<GizmoTarget>,
    /// The selected shapes, in the order they were selected.
    pub selected: Vec<GizmoTarget>,
    /// The one handle set: the lone selected shape's own, or the
    /// selection's about its centre.
    pub handles: Option<GizmoTarget>,
}

impl GizmoSelection {
    pub fn new(scene: &SceneVm, selected: Vec<GizmoTarget>) -> Self {
        let handles = match selected.as_slice() {
            [] => None,
            [one] => Some(one.clone()),
            many => GizmoTarget::selection(many),
        };
        Self { targets: gizmo_targets(scene).collect(), selected, handles }
    }

    pub fn is_selected(&self, id: &GizmoTargetId) -> bool {
        self.selected.iter().any(|target| &target.id == id)
    }
}

impl GizmoTarget {
    /// The one handle set of several selected shapes, at their centre
    /// (Blender's median point) along the world axes. Move, Rotate and
    /// Scale (per axis and uniform) each draw locked when no placed member
    /// can take them. Its id and kind are its first member's; it is never
    /// picked or looked up by id.
    pub fn selection(members: &[GizmoTarget]) -> Option<GizmoTarget> {
        let first = members.first()?;
        let takes = |mode: GizmoMode| {
            members.iter().filter(|member| !member.unplaced).any(|member| {
                // Every placed member's centre moves, turns and scales about the
                // selection's centre, so a free move handle takes all three.
                member.handles.iter().any(|handle| !handle.write.locked() && (handle.mode == GizmoMode::Move || handle.mode == mode))
            })
        };
        let mut handles = Vec::new();
        for (mode, axes) in [
            (GizmoMode::Move, &GizmoAxis::XYZ[..]),
            (GizmoMode::Rotate, &GizmoAxis::XYZ[..]),
            (GizmoMode::Scale, &[GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z, GizmoAxis::Uniform][..]),
        ] {
            let locked = !takes(mode);
            handles.extend(axes.iter().map(|&axis| GizmoHandle { mode, axis, write: HandleWrite::Selection { locked } }));
        }
        Some(GizmoTarget {
            id: first.id.clone(),
            kind: first.kind,
            origin: centre(members),
            world_axes: IDENTITY_AXES,
            unplaced: false,
            pickable: false,
            row: None,
            direction: None,
            outline: ShapeOutline::None,
            handles,
        })
    }
}

/// The members' median point.
fn centre(members: &[GizmoTarget]) -> [f32; 3] {
    let n = members.len().max(1) as f32;
    std::array::from_fn(|i| members.iter().map(|member| member.origin[i]).sum::<f32>() / n)
}

/// One param a selection drag writes on one member.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectionWrite {
    /// The member's place in the selection.
    pub member: usize,
    /// The param as it was at the press.
    pub param: ShapeParamVm,
    pub value: f32,
}

/// What the selection's `mode` handle on `axis` writes when dragged by
/// `amount` (a distance, a factor or an angle: [`super::GizmoDrag::update`]),
/// into `out` (cleared first). Every member starts from its value at the
/// press: its centre moves, or turns or scales about the selection's
/// centre; a box turns its own rotation and a direction turns with it;
/// sizes scale along their axis and radii with the uniform handle. A wired
/// param and an unplaced member stay put.
///
/// A turned box scales along its own axes, not the world's (Blender shears
/// it instead).
pub fn selection_writes(members: &[GizmoTarget], mode: GizmoMode, axis: GizmoAxis, amount: f32, out: &mut Vec<SelectionWrite>) {
    out.clear();
    let pivot = centre(members);
    let unit = axis.unit();
    for (member, target) in members.iter().enumerate().filter(|(_, target)| !target.unplaced) {
        let mut write = |param: &ShapeParamVm, value: f32| {
            if !param.wired && value.is_finite() {
                out.push(SelectionWrite { member, param: param.clone(), value });
            }
        };
        let from = sub3(target.origin, pivot);
        let to = match mode {
            GizmoMode::Move => std::array::from_fn(|i| from[i] + unit[i] * amount),
            GizmoMode::Rotate => rotate_about(from, unit, amount),
            GizmoMode::Scale => std::array::from_fn(|i| match axis {
                GizmoAxis::Uniform => from[i] * amount,
                GizmoAxis::X | GizmoAxis::Y | GizmoAxis::Z => if unit[i] > 0.5 { from[i] * amount } else { from[i] },
            }),
        };
        if let Some(moved) = in_param_units(target.world_axes, sub3(to, from)) {
            for handle in target.handles.iter().filter(|handle| handle.mode == GizmoMode::Move) {
                let (HandleWrite::Linear { param, .. }, Some(k)) = (&handle.write, axis_index(handle.axis)) else { continue };
                if moved[k] != 0.0 {
                    write(param, param.value + moved[k]);
                }
            }
        }
        match mode {
            GizmoMode::Move => {}
            GizmoMode::Rotate => turn_with(target, unit, amount, &mut write),
            GizmoMode::Scale => {
                for handle in target.handles.iter().filter(|handle| handle.mode == GizmoMode::Scale) {
                    match &handle.write {
                        HandleWrite::Linear { param, limit } if axis == GizmoAxis::Uniform || handle.axis == axis => {
                            write(param, limit.clamp_input(param.value * amount, (f32::NEG_INFINITY, f32::INFINITY)));
                        }
                        // A uniform scale takes only the uniform handle, and
                        // all three params move to one value.
                        HandleWrite::Uniform { params, limit } if axis == GizmoAxis::Uniform => {
                            let value = limit.clamp_input(params[0].value * amount, (f32::NEG_INFINITY, f32::INFINITY));
                            for param in params {
                                write(param, value);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

fn axis_index(axis: GizmoAxis) -> Option<usize> {
    match axis {
        GizmoAxis::X => Some(0),
        GizmoAxis::Y => Some(1),
        GizmoAxis::Z => Some(2),
        GizmoAxis::Uniform => None,
    }
}

/// The param offsets along `axes` (a target's parent axes in world space)
/// that move its centre by the world offset `delta`. `None` for a parent
/// that flattens space.
fn in_param_units(axes: [[f32; 3]; 3], delta: [f32; 3]) -> Option<[f32; 3]> {
    let det = dot3(axes[0], cross3(axes[1], axes[2]));
    if det.abs() < 1e-12 {
        return None;
    }
    Some([
        dot3(delta, cross3(axes[1], axes[2])) / det,
        dot3(axes[0], cross3(delta, axes[2])) / det,
        dot3(axes[0], cross3(axes[1], delta)) / det,
    ])
}

/// Turn `target`'s own orientation by `angle` about the world `unit` axis:
/// a box's rotation angles, recomposed in `render_scene`'s Euler order and
/// kept within a half turn of where they were, or a declared direction.
fn turn_with(target: &GizmoTarget, unit: [f32; 3], angle: f32, write: &mut impl FnMut(&ShapeParamVm, f32)) {
    let angles: [Option<&ShapeParamVm>; 3] = std::array::from_fn(|k| {
        target.handles.iter().find_map(|handle| match (&handle.write, axis_index(handle.axis)) {
            (HandleWrite::Angle(param), Some(axis)) if axis == k => Some(param),
            _ => None,
        })
    });
    if angles.iter().any(Option::is_some) {
        let rot = angles.map(|param| param.map_or(0.0, |param| param.value));
        // Column-major: `m[col][row]`.
        let m = model_matrix([0.0; 3], rot, [1.0; 3]);
        let turn: [[f32; 3]; 3] = std::array::from_fn(|col| rotate_about(IDENTITY_AXES[col], unit, angle));
        let rows: [[f32; 3]; 3] = std::array::from_fn(|i| {
            std::array::from_fn(|j| (0..3).map(|k| turn[k][i] * m[j][k]).sum::<f32>())
        });
        let euler = rotation_to_render_scene_euler(rows);
        for (param, value) in angles.iter().zip(euler) {
            if let Some(param) = param {
                let tau = std::f32::consts::TAU;
                write(param, value + tau * ((param.value - value) / tau).round());
            }
        }
    }
    let direction = target.handles.iter().find_map(|handle| match &handle.write {
        HandleWrite::Direction { params, normalize } => Some((params, *normalize)),
        HandleWrite::Linear { .. } | HandleWrite::Angle(_) | HandleWrite::Uniform { .. } | HandleWrite::Selection { .. } => None,
    });
    if let Some((params, normalize)) = direction {
        let turned = rotate_about(params.each_ref().map(|param| param.value), unit, angle);
        if let Some(turned) = if normalize { normalized(turned) } else { Some(turned) } {
            for (param, value) in params.iter().zip(turned) {
                write(param, value);
            }
        }
    }
}
