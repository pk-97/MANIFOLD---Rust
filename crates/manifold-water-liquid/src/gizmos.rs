//! The setup shapes a GPU liquid simulates on a frame, for the Scene panel's
//! gizmo overlay (`docs/REALTIME_3D_DESIGN.md` D7a): its bodies and regions
//! from the rows the solver took, and its forces from the field it sampled.

use manifold_core::fluid_domain::{FluidDomainLayout, FluidDomainSnapshot, FluidDomainState};
use manifold_core::physics_gizmo::{PhysicsGizmo, PhysicsGizmoFamily, PhysicsGizmoShape};
use manifold_physics::{FieldLeaf, FieldValue};

use crate::bodies::LiquidBodies;

/// A uniform force's arrow, as a share of the domain's shortest side; it
/// shows direction, not strength.
const UNIFORM_ARROW_SHARE: f32 = 0.25;

/// A liquid domain's snapshot: Ready once a setup was accepted and the
/// liquid is neither failed nor holding on a pending input.
pub fn domain_snapshot(
    accepted: Option<FluidDomainLayout>,
    failed: bool,
    pending: bool,
    epoch: u64,
    fit: manifold_core::liquid_setup::SetupFit,
    refusal: Option<std::sync::Arc<str>>,
) -> FluidDomainSnapshot {
    let state = match (accepted, failed, pending) {
        (_, true, _) => FluidDomainState::Failed,
        (None, ..) => FluidDomainState::Initializing,
        (_, _, true) => FluidDomainState::PendingInputs,
        _ => FluidDomainState::Ready,
    };
    FluidDomainSnapshot { epoch, state, accepted_layout: accepted, fit, refusal }
}

/// Bodies, regions and forces into `out`. A sampled grid field has no
/// position of its own and draws nothing.
pub fn write_liquid_gizmos(
    layout: &FluidDomainLayout,
    bodies: &LiquidBodies,
    field: Option<&FieldValue>,
    out: &mut Vec<PhysicsGizmo>,
) {
    bodies.write_gizmos(out);
    let Some(field) = field else { return };
    let centre: [f32; 3] = std::array::from_fn(|i| layout.min[i] + 0.5 * layout.size[i]);
    let arrow = UNIFORM_ARROW_SHARE * layout.size.iter().copied().fold(f32::INFINITY, f32::min);
    let force = |shape| PhysicsGizmo { family: PhysicsGizmoFamily::Force, shape };
    field.for_each_leaf(|leaf, scale| match leaf {
        FieldLeaf::Uniform(uniform) => {
            let v = uniform.vector().map(|c| c * scale);
            let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            if length > 0.0 {
                let to = std::array::from_fn(|i| centre[i] + v[i] / length * arrow);
                out.push(force(PhysicsGizmoShape::Arrow { from: centre, to }));
            }
        }
        FieldLeaf::Radial(radial) => {
            out.push(force(PhysicsGizmoShape::Sphere { center: radial.center(), radius: radial.radius() }));
        }
        FieldLeaf::Vortex(vortex) => {
            let (center, radius) = (vortex.center(), vortex.radius());
            out.push(force(PhysicsGizmoShape::Sphere { center, radius }));
            if scale != 0.0 {
                let axis = vortex.axis();
                let to = std::array::from_fn(|i| center[i] + axis[i] * radius * scale.signum());
                out.push(force(PhysicsGizmoShape::Arrow { from: center, to }));
            }
        }
        FieldLeaf::Sampled(_) => {}
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wind, an attractor and a reversed vortex, summed as the scene's
    /// force nodes sum them: the wind an arrow from the domain's centre, the
    /// attractor its sphere, the vortex its sphere and its spin axis flipped
    /// by the negative strength.
    #[test]
    fn forces_draw_where_the_field_acts() {
        let layout = FluidDomainLayout { min: [-2.0, 0.0, -2.0], size: [4.0, 2.0, 4.0], cells: [32, 16, 32], cell_size: 0.125 };
        let wind = FieldValue::uniform([0.0, 0.0, 3.0]).unwrap();
        let attractor = FieldValue::radial([1.0, 1.0, 0.0], 0.75, 1.0).unwrap().scaled(-4.0).unwrap();
        let vortex = FieldValue::vortex([0.0, 0.5, 0.0], [0.0, 1.0, 0.0], 1.5, 1.0).unwrap().scaled(-2.0).unwrap();
        let field = wind.sum(&attractor).unwrap().sum(&vortex).unwrap();
        let mut out = Vec::new();
        write_liquid_gizmos(&layout, &LiquidBodies::default(), Some(&field), &mut out);
        let shapes: Vec<_> = out.iter().inspect(|gizmo| assert_eq!(gizmo.family, PhysicsGizmoFamily::Force)).map(|gizmo| gizmo.shape).collect();
        assert_eq!(shapes, [
            PhysicsGizmoShape::Arrow { from: [0.0, 1.0, 0.0], to: [0.0, 1.0, 0.5] },
            PhysicsGizmoShape::Sphere { center: [1.0, 1.0, 0.0], radius: 0.75 },
            PhysicsGizmoShape::Sphere { center: [0.0, 0.5, 0.0], radius: 1.5 },
            PhysicsGizmoShape::Arrow { from: [0.0, 0.5, 0.0], to: [0.0, -1.0, 0.0] },
        ]);
        out.clear();
        write_liquid_gizmos(&layout, &LiquidBodies::default(), None, &mut out);
        assert!(out.is_empty(), "no field, no force gizmos");
    }

    #[test]
    fn a_domain_is_ready_only_once_accepted_and_running() {
        let layout = Some(FluidDomainLayout { min: [0.0; 3], size: [1.0; 3], cells: [8; 3], cell_size: 0.125 });
        let state = |accepted, failed, pending| domain_snapshot(accepted, failed, pending, 1, Default::default(), None).state;
        assert_eq!(state(layout, false, false), FluidDomainState::Ready);
        assert_eq!(state(None, false, false), FluidDomainState::Initializing);
        assert_eq!(state(layout, false, true), FluidDomainState::PendingInputs);
        assert_eq!(state(layout, true, false), FluidDomainState::Failed);
    }
}
