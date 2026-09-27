//! Rigid-body exchange at the existing native FLIP substep boundary.

use std::ffi::c_void;

use manifold_physics::{BodyDynamics, BodyHandle, BodyImpulse, BodyKind, BodyPose};

use crate::{FluidError, FluidFrame, FluidWorld, MeshHandle, MeshRole, native_result};

mod owner;
pub use owner::{CoupledFluidFrame, RigidFluidCoupling};

/// A collision pose and dynamics snapshot in the fluid simulation's coordinate
/// frame, in metres and seconds. The centre of mass must match this pose.
/// Queued external acceleration predicts the boundary velocity at each native
/// substep. The rigid backend remains responsible for integrating those forces
/// exactly once when it advances after accepting the fluid reaction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RigidBodyState {
    pub pose: BodyPose,
    pub dynamics: BodyDynamics,
}

/// The accepted fluid reaction for one prepared body, in preparation order.
/// Angular impulse is about the uploaded centre of mass. Solved velocity changes
/// are retained separately so the owner can verify the backend's response.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RigidReaction {
    pub linear: [f64; 3],
    pub angular: [f64; 3],
    pub delta_linear: [f64; 3],
    pub delta_angular: [f64; 3],
}

impl RigidReaction {
    pub fn body_impulse(self, body: BodyHandle) -> Result<BodyImpulse, FluidError> {
        let linear = self.linear.map(|value| value as f32);
        let angular = self.angular.map(|value| value as f32);
        if !linear
            .iter()
            .chain(angular.iter())
            .all(|value| value.is_finite())
        {
            return Err(FluidError::native(
                "rigid reaction cannot be represented by Box3D",
            ));
        }
        Ok(BodyImpulse {
            body,
            linear,
            angular,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NativeRigidBodyInput {
    pose: [f32; 7],
    center: [f32; 3],
    linear_velocity: [f32; 3],
    angular_velocity: [f32; 3],
    external_linear_acceleration: [f32; 3],
    external_angular_acceleration: [f32; 3],
    inverse_mass: f32,
    inverse_inertia: [f32; 9],
    enabled: u32,
}

pub(super) struct RigidCouplingState {
    inputs: Vec<NativeRigidBodyInput>,
    reactions: Vec<RigidReaction>,
}

unsafe extern "C" {
    fn manifold_fluids_world_prepare_rigid_coupling(
        world: *mut c_void,
        slots: *const u32,
        body_indices: *const u32,
        collider_count: usize,
        body_count: usize,
        density: f64,
    ) -> i32;
    fn manifold_fluids_world_set_rigid_bodies(
        world: *mut c_void,
        inputs: *const NativeRigidBodyInput,
        count: usize,
    ) -> i32;
    fn manifold_fluids_world_rigid_reactions(
        world: *mut c_void,
        output: *mut RigidReaction,
        capacity: usize,
        count: *mut usize,
    ) -> i32;
}

impl FluidWorld {
    /// Prepare two-way coupling once during world construction. Each collider
    /// represents one rigid body with one collider mesh. Use
    /// `prepare_rigid_coupling_groups` for compound proxies. Rebuild the world
    /// when this topology changes.
    ///
    /// Density is kg/m³. The owner must use `begin_frame` and exchange body state
    /// and reactions at every substep; `step` is unavailable for coupled worlds.
    pub fn prepare_rigid_coupling(
        &mut self,
        colliders: &[MeshHandle],
        density: f64,
    ) -> Result<(), FluidError> {
        let groups: Vec<_> = colliders.iter().map(std::slice::from_ref).collect();
        self.prepare_rigid_coupling_groups(&groups, density)
    }

    /// Prepare one nonempty collider group per rigid body. Each mesh must be
    /// in that body's local frame. Meshes retain their union geometry; all
    /// hulls in a group share the same mass/inertia, pose and reaction. State
    /// uploads and reaction reads use group order, not flattened mesh order.
    /// A collider can appear only once across all groups.
    pub fn prepare_rigid_coupling_groups(
        &mut self,
        groups: &[&[MeshHandle]],
        density: f64,
    ) -> Result<(), FluidError> {
        if self.rigid_coupling.is_some() {
            return Err(FluidError::input(
                "rigid coupling is already prepared; rebuild the world",
            ));
        }
        if groups.is_empty()
            || groups.iter().any(|group| group.is_empty())
            || !density.is_finite()
            || density <= 0.0
        {
            return Err(FluidError::input(
                "rigid coupling needs nonempty collider groups and positive finite density",
            ));
        }
        let count = groups
            .iter()
            .try_fold(0usize, |count, group| count.checked_add(group.len()))
            .ok_or_else(|| FluidError::input("rigid collider count overflow"))?;
        let mut slots = Vec::with_capacity(count);
        let mut body_indices = Vec::with_capacity(count);
        for (body, group) in groups.iter().enumerate() {
            let body =
                u32::try_from(body).map_err(|_| FluidError::input("rigid body index overflow"))?;
            for &handle in *group {
                let slot = self
                    .mesh_state
                    .validate_handle(handle, Some(MeshRole::Collider))?;
                slots.push(
                    u32::try_from(slot).map_err(|_| FluidError::input("mesh slot overflow"))?,
                );
                body_indices.push(body);
            }
        }
        let state = RigidCouplingState {
            inputs: vec![NativeRigidBodyInput::default(); groups.len()],
            reactions: vec![RigidReaction::default(); groups.len()],
        };
        let ok = unsafe {
            manifold_fluids_world_prepare_rigid_coupling(
                self.native,
                slots.as_ptr(),
                body_indices.as_ptr(),
                slots.len(),
                groups.len(),
                density,
            )
        };
        native_result(ok, "preparing rigid fluid coupling")?;
        self.rigid_coupling = Some(state);
        Ok(())
    }
}

impl FluidFrame<'_> {
    /// Upload all bound bodies before requesting the next native timestep.
    /// An outstanding offer prevents geometry changes. The bridge checks the
    /// whole batch before applying any pose. Buffers are retained across ticks.
    pub fn set_rigid_bodies(&mut self, bodies: &[RigidBodyState]) -> Result<(), FluidError> {
        let coupling = self
            .world
            .rigid_coupling
            .as_mut()
            .ok_or_else(|| FluidError::input("rigid fluid coupling is not prepared"))?;
        if bodies.len() != coupling.inputs.len() {
            return Err(FluidError::input(
                "rigid body count must match prepared collider groups",
            ));
        }
        for (input, body) in coupling.inputs.iter_mut().zip(bodies) {
            input.pose[..3].copy_from_slice(&body.pose.position);
            input.pose[3..].copy_from_slice(&body.pose.rotation);
            input.center = body.dynamics.center_of_mass;
            input.linear_velocity = body.dynamics.linear_velocity;
            input.angular_velocity = body.dynamics.angular_velocity;
            let responds = body.dynamics.enabled && body.dynamics.kind == BodyKind::Dynamic;
            input.external_linear_acceleration = if responds {
                body.dynamics.external_linear_acceleration
            } else {
                [0.0; 3]
            };
            input.external_angular_acceleration = if responds {
                body.dynamics.external_angular_acceleration
            } else {
                [0.0; 3]
            };
            input.inverse_mass = if responds {
                body.dynamics.inverse_mass
            } else {
                0.0
            };
            for row in 0..3 {
                for column in 0..3 {
                    input.inverse_inertia[row * 3 + column] = if responds {
                        body.dynamics.inverse_inertia[row][column]
                    } else {
                        0.0
                    };
                }
            }
            input.enabled = u32::from(body.dynamics.enabled);
        }
        let ok = unsafe {
            manifold_fluids_world_set_rigid_bodies(
                self.world.native,
                coupling.inputs.as_ptr(),
                coupling.inputs.len(),
            )
        };
        native_result(ok, "uploading rigid fluid bodies")
    }

    /// Read reactions only after an accepted substep, before uploading the next
    /// input. The returned slice uses retained storage and allocates nothing.
    pub fn rigid_reactions(&mut self) -> Result<&[RigidReaction], FluidError> {
        let coupling = self
            .world
            .rigid_coupling
            .as_mut()
            .ok_or_else(|| FluidError::input("rigid fluid coupling is not prepared"))?;
        let mut count = 0;
        let ok = unsafe {
            manifold_fluids_world_rigid_reactions(
                self.world.native,
                coupling.reactions.as_mut_ptr(),
                coupling.reactions.len(),
                &mut count,
            )
        };
        native_result(ok, "reading rigid fluid reactions")?;
        if count != coupling.reactions.len() {
            return Err(FluidError::native("rigid fluid reaction count changed"));
        }
        Ok(&coupling.reactions)
    }
}

#[cfg(test)]
mod tests;
