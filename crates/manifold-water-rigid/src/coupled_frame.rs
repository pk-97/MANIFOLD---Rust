//! Rigid bodies coupled to a liquid domain: resolved inputs, the visible
//! rigid frame, and the body layout shared by every in-thread liquid.

use manifold_core::scene_impulse::RigidImpulseTargets;
use manifold_node_engine::scene::transform::{Transform, quat_to_render_scene_euler};
use manifold_physics::{BodyHandle, PhysicsWorld, TickStamp};

use crate::physics::{MAX_BODIES, RigidSceneInputs, RigidSimulation};

/// Resolved rigid inputs for one fluid domain. The same rigid world supplies
/// contacts for all its bodies; `colliders` selects those exchanging momentum
/// with this domain. Density is physical liquid density in kg/m³.
#[derive(Clone, Copy)]
pub struct CoupledRigidInputs<'a> {
    pub scene: &'a RigidSceneInputs,
    pub colliders: RigidImpulseTargets,
    pub density: f64,
}

impl CoupledRigidInputs<'_> {
    pub fn validate(self) -> Result<(), String> {
        if !self.density.is_finite() || self.density <= 0.0 {
            return Err("Fluid coupling: density must be finite and positive".into());
        }
        for index in 0..MAX_BODIES {
            if self.colliders.contains_body(index) && self.scene.bodies[index].is_none() {
                return Err(format!("Fluid coupling: body {index} is not connected"));
            }
        }
        if self.colliders.copies && self.scene.prototype.is_none() {
            return Err("Fluid coupling: copies require a connected prototype".into());
        }
        Ok(())
    }
}

/// An immutable visible rigid result, accepted with the liquid surface at the
/// same completed boundary. Tick zero is the prepared, unstepped scene.
#[derive(Clone, Debug)]
pub struct CoupledRigidFrame {
    pub stamp: TickStamp,
    pub poses: [Transform; MAX_BODIES],
    pub copies: Vec<Transform>,
}

impl Default for CoupledRigidFrame {
    fn default() -> Self {
        Self {
            stamp: TickStamp { epoch: 0, tick: 0 },
            poses: [Transform::default(); MAX_BODIES],
            copies: Vec::new(),
        }
    }
}

/// How a prepared rigid world's bodies map onto a [`CoupledRigidFrame`]:
/// shared by every liquid that owns a rigid world in-thread.
pub struct CoupledRigidLayout {
    bodies: [Option<BodyHandle>; MAX_BODIES],
    fragment_parents: [Option<usize>; MAX_BODIES],
    authored: [Transform; MAX_BODIES],
    copies: Vec<BodyHandle>,
    copy_scale: [f32; 3],
}

impl CoupledRigidLayout {
    /// The layout of `rigid`, prepared from `initial`.
    pub fn new(rigid: &RigidSimulation, initial: &RigidSceneInputs) -> Self {
        let (bodies, copies) = rigid.native_handles();
        Self {
            bodies: *bodies,
            fragment_parents: std::array::from_fn(|index| {
                initial.bodies[index]
                    .as_ref()
                    .and_then(|body| body.fragment_parent)
            }),
            authored: std::array::from_fn(|index| {
                initial.bodies[index]
                    .as_ref()
                    .map_or(Transform::default(), |body| body.transform)
            }),
            copies: copies.iter().copied().flatten().collect(),
            copy_scale: initial
                .prototype
                .as_ref()
                .map_or([1.0; 3], |body| body.transform.scale),
        }
    }

    /// Size `output`'s copy storage for this layout, before a tick captures into it.
    pub fn prepare_output(&self, output: &mut CoupledRigidFrame) {
        output
            .copies
            .resize(self.copies.len(), Transform::default());
    }

    pub fn capture(
        &self,
        world: &PhysicsWorld,
        output: &mut CoupledRigidFrame,
    ) -> Result<(), String> {
        for (index, destination) in output.poses.iter_mut().enumerate() {
            let Some(mut handle) = self.bodies[index] else {
                *destination = self.authored[index];
                continue;
            };
            // Prepared, inactive fragment geometry follows its intact parent,
            // exactly as in the ordinary rigid output path.
            if let Some(parent) = self.fragment_parents[index]
                && !world
                    .dynamics(handle)
                    .map_err(|error| error.to_string())?
                    .enabled
            {
                handle = self.bodies[parent].ok_or("Fluid coupling: fragment parent is absent")?;
            }
            let pose = world.pose(handle).map_err(|error| error.to_string())?;
            *destination = Transform {
                pos: pose.position,
                rot_euler: quat_to_render_scene_euler(pose.rotation),
                ..self.authored[index]
            };
        }
        if output.copies.len() != self.copies.len() {
            return Err("Fluid coupling: output copy storage was not prepared".into());
        }
        for (&handle, destination) in self.copies.iter().zip(&mut output.copies) {
            let pose = world.pose(handle).map_err(|error| error.to_string())?;
            *destination = Transform {
                pos: pose.position,
                rot_euler: quat_to_render_scene_euler(pose.rotation),
                scale: self.copy_scale,
                billboard: false,
            };
        }
        Ok(())
    }
}

