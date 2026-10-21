//! Rigid bodies coupled to a liquid domain: resolved inputs, the visible
//! rigid frame, and the body layout shared by every in-thread liquid.

use manifold_core::scene_impulse::RigidImpulseTargets;
use manifold_node_engine::scene::transform::{Transform, quat_to_render_scene_euler};
use manifold_physics::{BodyHandle, PhysicsWorld, TickStamp};

use crate::physics::{RigidSceneInputs, RigidSimulation};

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
        for index in self.colliders.bodies() {
            if self.scene.bodies.get(index).is_none_or(Option::is_none) {
                return Err(format!("Fluid coupling: body {index} is not connected"));
            }
        }
        for group in self.colliders.groups() {
            if self.scene.groups.get(group).is_none_or(Option::is_none) {
                return Err(format!("Fluid coupling: copy group {group} has no copies body"));
            }
        }
        Ok(())
    }
}

/// An immutable visible rigid result, accepted with the liquid surface at the
/// same completed boundary. Tick zero is the prepared, unstepped scene.
#[derive(Debug)]
pub struct CoupledRigidFrame {
    pub stamp: TickStamp,
    /// One per body port.
    pub poses: Vec<Transform>,
    /// Every copy group's copies, group after group.
    pub copies: Vec<Transform>,
    /// How many of `copies` each copy group owns.
    pub group_copies: Vec<usize>,
}

impl Default for CoupledRigidFrame {
    fn default() -> Self {
        Self {
            stamp: TickStamp { epoch: 0, tick: 0 },
            poses: Vec::new(),
            copies: Vec::new(),
            group_copies: Vec::new(),
        }
    }
}

impl Clone for CoupledRigidFrame {
    fn clone(&self) -> Self {
        Self {
            stamp: self.stamp,
            poses: self.poses.clone(),
            copies: self.copies.clone(),
            group_copies: self.group_copies.clone(),
        }
    }

    fn clone_from(&mut self, source: &Self) {
        self.stamp = source.stamp;
        self.poses.clone_from(&source.poses);
        self.copies.clone_from(&source.copies);
        self.group_copies.clone_from(&source.group_copies);
    }
}

impl CoupledRigidFrame {
    /// Copy group `group`'s copies.
    pub fn group(&self, group: usize) -> &[Transform] {
        let Some(&count) = self.group_copies.get(group) else {
            return &[];
        };
        let start: usize = self.group_copies[..group].iter().sum();
        self.copies.get(start..start + count).unwrap_or(&[])
    }
}

/// How a prepared rigid world's bodies map onto a [`CoupledRigidFrame`]:
/// shared by every liquid that owns a rigid world in-thread.
pub struct CoupledRigidLayout {
    bodies: Vec<Option<BodyHandle>>,
    fragment_parents: Vec<Option<usize>>,
    authored: Vec<Transform>,
    copies: Vec<BodyHandle>,
    group_copies: Vec<usize>,
    group_scales: Vec<[f32; 3]>,
}

impl CoupledRigidLayout {
    /// The layout of `rigid`, prepared from `initial`.
    pub fn new(rigid: &RigidSimulation, initial: &RigidSceneInputs) -> Self {
        let groups = initial.groups.len();
        let mut copies = Vec::new();
        let mut group_copies = Vec::with_capacity(groups);
        for group in 0..groups {
            let before = copies.len();
            copies.extend(rigid.native_copy_handles(group).iter().copied().flatten());
            group_copies.push(copies.len() - before);
        }
        Self {
            bodies: rigid.native_handles().to_vec(),
            fragment_parents: initial
                .bodies
                .iter()
                .map(|body| body.as_ref().and_then(|body| body.fragment_parent))
                .collect(),
            authored: initial
                .bodies
                .iter()
                .map(|body| body.as_ref().map_or(Transform::default(), |body| body.transform))
                .collect(),
            copies,
            group_copies,
            group_scales: initial
                .groups
                .iter()
                .map(|body| body.as_ref().map_or([1.0; 3], |body| body.transform.scale))
                .collect(),
        }
    }

    /// Size `output`'s storage for this layout, before a tick captures into it.
    pub fn prepare_output(&self, output: &mut CoupledRigidFrame) {
        output.poses.resize(self.bodies.len(), Transform::default());
        output
            .copies
            .resize(self.copies.len(), Transform::default());
        output.group_copies.clone_from(&self.group_copies);
    }

    pub fn capture(
        &self,
        world: &PhysicsWorld,
        output: &mut CoupledRigidFrame,
    ) -> Result<(), String> {
        if output.poses.len() != self.bodies.len()
            || output.copies.len() != self.copies.len()
            || output.group_copies != self.group_copies
        {
            return Err("Fluid coupling: output storage was not prepared".into());
        }
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
        let mut next = 0;
        for (&count, &scale) in self.group_copies.iter().zip(&self.group_scales) {
            for (&handle, destination) in self.copies[next..next + count]
                .iter()
                .zip(&mut output.copies[next..next + count])
            {
                let pose = world.pose(handle).map_err(|error| error.to_string())?;
                *destination = Transform {
                    pos: pose.position,
                    rot_euler: quat_to_render_scene_euler(pose.rotation),
                    scale,
                    billboard: false,
                };
            }
            next += count;
        }
        Ok(())
    }
}
