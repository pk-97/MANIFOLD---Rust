//! Retained owner-side exchange between FLIP and the rigid-body backend.
//!
//! A coupling owns the fixed collider/body topology. Rebuild the fluid world
//! and this adapter together when that topology changes. The adapter currently
//! bridges the two worlds with a translation only: body positions and centres
//! of mass are shifted by `origin`, while rotations, velocities, moments and
//! world-space inverse inertia remain unchanged.

use std::collections::HashMap;

use manifold_physics::{
    BodyHandle, BodyImpulse, BodyPose, FieldInput, PhysicsWorld, Seconds, stepping::SubstepExchange,
};

use crate::{FluidError, FluidFrame, FluidWorld, FrameStats, MeshHandle, MeshRole};

use super::RigidBodyState;

/// Owns the fixed fluid collider/body topology and retained exchange storage.
///
/// `prepare` must be repeated if either world's topology is rebuilt. A frame
/// borrows the coupling and fluid world exclusively until `finish` succeeds.
pub struct RigidFluidCoupling {
    colliders: Vec<MeshHandle>,
    bodies: Vec<BodyHandle>,
    origin: [f32; 3],
    states: Vec<RigidBodyState>,
    impulses: Vec<BodyImpulse>,
    last_stats: Option<FrameStats>,
}

/// An unpublished FLIP frame owned by the rigid-body tick loop.
#[must_use = "finish the coupled frame after all substeps"]
pub struct CoupledFluidFrame<'coupling, 'fluid> {
    coupling: &'coupling mut RigidFluidCoupling,
    frame: FluidFrame<'fluid>,
    pending: Option<Seconds>,
    failed: bool,
}

impl RigidFluidCoupling {
    /// Prepare the retained topology and validate the initial rigid state.
    ///
    /// The fluid world must already contain each collider. Calling this for a
    /// changed topology requires rebuilding the fluid world and adapter. A
    /// body can own several colliders; its state and reaction are exchanged
    /// once, in first-occurrence body order. Collider meshes must already use
    /// their body's local coordinates, as returned by `PhysicsWorld::hull_meshes`.
    pub fn prepare(
        fluid: &mut FluidWorld,
        rigid: &PhysicsWorld,
        bindings: &[(MeshHandle, BodyHandle)],
        origin: [f32; 3],
        density: f64,
    ) -> Result<Self, FluidError> {
        if bindings.is_empty() {
            return Err(FluidError::input(
                "rigid fluid coupling needs at least one binding",
            ));
        }
        if !origin.iter().all(|value| value.is_finite()) {
            return Err(FluidError::input(
                "rigid fluid coupling origin must be finite",
            ));
        }

        let colliders: Vec<_> = bindings.iter().map(|&(collider, _)| collider).collect();
        for &collider in &colliders {
            fluid
                .mesh_state
                .validate_handle(collider, Some(MeshRole::Collider))?;
        }

        let mut body_indices = HashMap::with_capacity(bindings.len());
        let mut bodies = Vec::with_capacity(bindings.len());
        let mut groups: Vec<Vec<MeshHandle>> = Vec::new();
        let mut states = Vec::with_capacity(bindings.len());
        for &(collider, body) in bindings {
            let index = if let Some(&index) = body_indices.get(&body) {
                index
            } else {
                let index = bodies.len();
                states.push(read_state(rigid, body, origin)?);
                bodies.push(body);
                groups.push(Vec::new());
                body_indices.insert(body, index);
                index
            };
            groups[index].push(collider);
        }
        let group_refs: Vec<_> = groups.iter().map(Vec::as_slice).collect();
        fluid.prepare_rigid_coupling_groups(&group_refs, density)?;
        let body_count = bodies.len();

        Ok(Self {
            colliders,
            bodies,
            origin,
            states,
            impulses: Vec::with_capacity(body_count),
            last_stats: None,
        })
    }

    /// Begin one fluid frame. The supplied fields use the existing FLIP input
    /// preparation and remain owned by the caller after this method returns.
    pub fn begin_frame<'coupling, 'fluid>(
        &'coupling mut self,
        fluid: &'fluid mut FluidWorld,
        duration: Seconds,
        fields: &[FieldInput<'_>],
    ) -> Result<CoupledFluidFrame<'coupling, 'fluid>, FluidError> {
        self.last_stats = None;
        for &collider in &self.colliders {
            fluid
                .mesh_state
                .validate_handle(collider, Some(MeshRole::Collider))?;
        }
        let frame = fluid.begin_frame_with_fields(duration, fields)?;
        Ok(CoupledFluidFrame {
            coupling: self,
            frame,
            pending: None,
            failed: false,
        })
    }

    /// Begin a live coupled frame whose final native substep consumes the
    /// remaining interval after the stability cap.
    pub fn begin_live_frame<'coupling, 'fluid>(
        &'coupling mut self,
        fluid: &'fluid mut FluidWorld,
        duration: Seconds,
        fields: &[FieldInput<'_>],
    ) -> Result<CoupledFluidFrame<'coupling, 'fluid>, FluidError> {
        self.last_stats = None;
        for &collider in &self.colliders {
            fluid
                .mesh_state
                .validate_handle(collider, Some(MeshRole::Collider))?;
        }
        let frame = fluid.begin_live_frame_with_fields(duration, fields)?;
        Ok(CoupledFluidFrame {
            coupling: self,
            frame,
            pending: None,
            failed: false,
        })
    }

    pub fn last_stats(&self) -> Option<FrameStats> {
        self.last_stats
    }

    /// The paired rigid state captured at the last successfully finished fluid
    /// frame, in scene/world coordinates and first-occurrence body order.
    /// Beginning another frame invalidates this view before reusing its input
    /// storage. A publisher must copy these values with that frame's surface.
    pub fn completed_bodies(
        &self,
    ) -> Option<impl ExactSizeIterator<Item = (BodyHandle, &RigidBodyState)> + '_> {
        self.last_stats
            .map(|_| self.bodies.iter().copied().zip(&self.states))
    }
}

impl SubstepExchange for CoupledFluidFrame<'_, '_> {
    type Error = FluidError;

    fn next_substep(
        &mut self,
        rigid: &PhysicsWorld,
        maximum: Seconds,
    ) -> Result<Seconds, Self::Error> {
        if self.failed {
            return Err(FluidError::native(
                "coupled frame failed; rebuild both worlds",
            ));
        }
        if !maximum.0.is_finite() || maximum.0 <= 0.0 {
            return Err(FluidError::input(
                "maximum coupled substep must be finite and positive",
            ));
        }
        if self.pending.is_some() {
            return Err(FluidError::input(
                "the previous coupled substep must be exchanged first",
            ));
        }

        refresh_states(self.coupling, rigid)?;
        self.frame.set_rigid_bodies(&self.coupling.states)?;
        let native = self
            .frame
            .next_substep()?
            .ok_or_else(|| FluidError::native("fluid frame has no substep remaining"))?;
        if !native.0.is_finite() || native.0 <= 0.0 {
            return Err(FluidError::native(
                "native fluid substep duration must be finite and positive",
            ));
        }
        let selected = Seconds(native.0.min(maximum.0));
        self.pending = Some(selected);
        Ok(selected)
    }

    fn exchange(&mut self, rigid: &mut PhysicsWorld, duration: Seconds) -> Result<(), Self::Error> {
        let offered = self
            .pending
            .ok_or_else(|| FluidError::input("coupled substep exchange has no pending duration"))?;
        if duration != offered {
            return Err(FluidError::input(
                "coupled substep duration does not match the offered duration",
            ));
        }

        for (index, &body) in self.coupling.bodies.iter().enumerate() {
            let current = read_state(rigid, body, self.coupling.origin)?;
            if current != self.coupling.states[index] {
                return Err(FluidError::input(
                    "rigid body state changed after the coupled substep offer",
                ));
            }
        }

        // The offer is consumed before native mutation. Any subsequent error
        // leaves this frame unusable rather than permitting a second exchange.
        self.pending = None;
        self.failed = true;
        self.frame.advance(duration)?;

        self.coupling.impulses.clear();
        {
            let reactions = self.frame.rigid_reactions()?;
            if reactions.len() != self.coupling.bodies.len() {
                return Err(FluidError::native("rigid reaction count changed"));
            }
            for (&body, reaction) in self.coupling.bodies.iter().zip(reactions) {
                self.coupling.impulses.push(reaction.body_impulse(body)?);
            }
        }
        rigid
            .apply_impulses(&self.coupling.impulses)
            .map_err(|error| FluidError::input(format!("applying rigid reaction: {error}")))?;
        self.failed = false;
        Ok(())
    }

    fn finish(self, rigid: &PhysicsWorld) -> Result<(), Self::Error> {
        if self.failed {
            return Err(FluidError::native(
                "coupled frame failed; rebuild both worlds",
            ));
        }
        // Read after the owner's final Box3D step, before any later authored
        // teleport or release. Reuse the upload scratch, now in scene space.
        // A failed read drops the unpublished fluid guard and invalidates it.
        for (state, &body) in self.coupling.states.iter_mut().zip(&self.coupling.bodies) {
            *state = read_state(rigid, body, [0.0; 3])?;
        }
        let stats = self.frame.finish()?;
        self.coupling.last_stats = Some(stats);
        Ok(())
    }
}

impl CoupledFluidFrame<'_, '_> {
    /// Replace the copied force fields for the next accepted live segment.
    pub fn set_fields(
        &mut self,
        dt: Seconds,
        fields: &[FieldInput<'_>],
    ) -> Result<(), FluidError> {
        self.frame.set_fields(dt, fields)
    }
}

fn refresh_states(
    coupling: &mut RigidFluidCoupling,
    rigid: &PhysicsWorld,
) -> Result<(), FluidError> {
    for (state, &body) in coupling.states.iter_mut().zip(&coupling.bodies) {
        *state = read_state(rigid, body, coupling.origin)?;
    }
    Ok(())
}

fn read_state(
    rigid: &PhysicsWorld,
    body: BodyHandle,
    origin: [f32; 3],
) -> Result<RigidBodyState, FluidError> {
    let mut pose = rigid
        .pose(body)
        .map_err(|error| FluidError::input(format!("reading rigid body pose: {error}")))?;
    let mut dynamics = rigid
        .dynamics(body)
        .map_err(|error| FluidError::input(format!("reading rigid body dynamics: {error}")))?;
    if !state_is_finite(pose, dynamics) {
        return Err(FluidError::input(
            "rigid body pose and dynamics must be finite",
        ));
    }
    for (axis, offset) in origin.into_iter().enumerate() {
        pose.position[axis] -= offset;
        dynamics.center_of_mass[axis] -= offset;
    }
    if !state_is_finite(pose, dynamics) {
        return Err(FluidError::input(
            "translated rigid body pose and dynamics must be finite",
        ));
    }
    Ok(RigidBodyState { pose, dynamics })
}

fn state_is_finite(pose: BodyPose, dynamics: manifold_physics::BodyDynamics) -> bool {
    pose.position
        .iter()
        .chain(pose.rotation.iter())
        .chain(dynamics.center_of_mass.iter())
        .chain(dynamics.linear_velocity.iter())
        .chain(dynamics.angular_velocity.iter())
        .chain(dynamics.external_linear_acceleration.iter())
        .chain(dynamics.external_angular_acceleration.iter())
        .all(|value| value.is_finite())
        && dynamics
            .inverse_inertia
            .iter()
            .flatten()
            .all(|value| value.is_finite())
        && dynamics.inverse_mass.is_finite()
}

#[cfg(test)]
mod tests;
