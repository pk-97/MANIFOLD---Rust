//! Retained owner-side exchange between FLIP and the rigid-body backend.
//!
//! A coupling owns the fixed collider/body topology. Rebuild the fluid world
//! and this adapter together when that topology changes. The adapter currently
//! bridges the two worlds with a translation only: body positions and centres
//! of mass are shifted by `origin`, while rotations, velocities, moments and
//! world-space inverse inertia remain unchanged.

use std::collections::HashSet;

use manifold_physics::{
    BodyHandle, BodyImpulse, BodyPose, FieldInput, PhysicsWorld, Seconds, stepping::SubstepExchange,
};

use crate::{FluidError, FluidFrame, FluidWorld, FrameStats, MeshHandle, MeshRole};

use super::RigidBodyState;

#[derive(Clone, Copy, Debug)]
struct Binding {
    collider: MeshHandle,
    body: BodyHandle,
}

/// Owns the fixed fluid collider/body topology and retained exchange storage.
///
/// `prepare` must be repeated if either world's topology is rebuilt. A frame
/// borrows the coupling and fluid world exclusively until `finish` succeeds.
pub struct RigidFluidCoupling {
    bindings: Vec<Binding>,
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
    /// changed topology requires rebuilding the fluid world and adapter.
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

        let mut bodies = HashSet::with_capacity(bindings.len());
        let mut states = Vec::with_capacity(bindings.len());
        for &(_, body) in bindings {
            if !bodies.insert(body) {
                return Err(FluidError::input(
                    "rigid fluid coupling bindings must use distinct bodies",
                ));
            }
            states.push(read_state(rigid, body, origin)?);
        }

        fluid.prepare_rigid_coupling(&colliders, density)?;

        Ok(Self {
            bindings: bindings
                .iter()
                .map(|&(collider, body)| Binding { collider, body })
                .collect(),
            origin,
            states,
            impulses: Vec::with_capacity(bindings.len()),
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
        for binding in &self.bindings {
            fluid
                .mesh_state
                .validate_handle(binding.collider, Some(MeshRole::Collider))?;
        }
        let frame = fluid.begin_frame_with_fields(duration, fields)?;
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

        for (index, binding) in self.coupling.bindings.iter().enumerate() {
            let current = read_state(rigid, binding.body, self.coupling.origin)?;
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
            if reactions.len() != self.coupling.bindings.len() {
                return Err(FluidError::native("rigid reaction count changed"));
            }
            for (binding, reaction) in self.coupling.bindings.iter().zip(reactions) {
                self.coupling
                    .impulses
                    .push(reaction.body_impulse(binding.body)?);
            }
        }
        rigid
            .apply_impulses(&self.coupling.impulses)
            .map_err(|error| FluidError::input(format!("applying rigid reaction: {error}")))?;
        self.failed = false;
        Ok(())
    }

    fn finish(self) -> Result<(), Self::Error> {
        if self.failed {
            return Err(FluidError::native(
                "coupled frame failed; rebuild both worlds",
            ));
        }
        let stats = self.frame.finish()?;
        self.coupling.last_stats = Some(stats);
        Ok(())
    }
}

fn refresh_states(
    coupling: &mut RigidFluidCoupling,
    rigid: &PhysicsWorld,
) -> Result<(), FluidError> {
    for (state, binding) in coupling.states.iter_mut().zip(&coupling.bindings) {
        *state = read_state(rigid, binding.body, coupling.origin)?;
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
