//! What every GPU liquid domain shares (`docs/LIQUID_SOLVER_SEAM_DESIGN.md`
//! D2): the fixed-tick clock, the padded lattice with its walls, the body
//! rows and their distance atlas, the rigid owner that couples Box3D one
//! settled tick at a time, the A/B frame ring of the particle-frame seam, and
//! the face grid layout every solver publishes. Solver rules (substep bounds,
//! reaction encodings, block sorting) stay with each solver.
//!
//! Sits on manifold-water-rigid. Never depends on a solver or mesher crate
//! (gpu-flip, gpu-mpm, whitewater, surface) or on manifold-nodes-water.

pub mod bodies;
pub mod body_buffers;
pub mod clock;
pub mod coupling;
pub mod display_cursor;
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod extent;
pub mod fields;
pub mod frame_history;
pub mod frame_ring;
pub mod gizmos;
pub mod grid;
pub mod lattice;
pub mod world_pose;

use crate::fluid_role::FluidRole;
use manifold_node_engine::bindings::NodeInputs;
use manifold_node_engine::exec::effect_node::ParamValues;
use manifold_node_engine::ports::{NodeInput, PortKind, PortType};

pub mod fluid_particles;
pub mod fluid_role;
pub mod primitives;
pub mod whitewater;
mod wire_values;

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit;

pub use manifold_core::fluid_domain::DEFAULT_ROLE_PORTS;

/// The `roles` param: how many role inputs (`role_0`, `role_1`, ...) a
/// liquid domain has. A scene Water has one per source.
pub fn role_count(params: &ParamValues) -> usize {
    params
        .get("roles")
        .and_then(|value| value.as_scalar())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map_or(DEFAULT_ROLE_PORTS, |value| value.round() as usize)
}

/// A liquid domain's role inputs, sized by its `roles` param: the node's
/// fixed inputs, then `role_0..role_<n>`, and the slot each reads into. Sized
/// when the param changes, never per frame.
#[derive(Default)]
pub struct RolePorts {
    inputs: Vec<NodeInput>,
    names: Vec<String>,
    roles: Vec<Option<FluidRole>>,
}

impl RolePorts {
    /// Size the role inputs to the `roles` param after `fixed`.
    pub fn configure(&mut self, fixed: &'static [NodeInput], params: &ParamValues) {
        let count = role_count(params);
        if !self.inputs.is_empty() && self.names.len() == count {
            return;
        }
        self.names = (0..count).map(|index| format!("role_{index}")).collect();
        self.inputs.clear();
        self.inputs.extend(fixed.iter().cloned());
        self.inputs.extend(self.names.iter().map(|name| NodeInput {
            name: std::borrow::Cow::Owned(name.clone()),
            ty: PortType::FluidRole,
            kind: PortKind::Input,
            required: false,
        }));
        self.roles.resize(count, None);
    }

    /// The node's inputs, once configured.
    pub fn inputs(&self) -> Option<&[NodeInput]> {
        (!self.inputs.is_empty()).then_some(self.inputs.as_slice())
    }

    /// Read every wired role input into its slot; true when a wired role is
    /// not ready yet.
    pub fn read(&mut self, inputs: &NodeInputs<'_>) -> bool {
        let mut pending = false;
        for (port, role) in self.names.iter().zip(self.roles.iter_mut()) {
            *role = None;
            if inputs.slot(port).is_some() {
                *role = inputs.cpu_value::<FluidRole>(port);
                pending |= role.is_none();
            }
        }
        pending
    }

    /// The roles the last [`Self::read`] found, in slot order. Taken and put
    /// back around a use that needs the node mutably, so it never allocates.
    pub fn take(&mut self) -> Vec<Option<FluidRole>> {
        std::mem::take(&mut self.roles)
    }

    pub fn put_back(&mut self, roles: Vec<Option<FluidRole>>) {
        self.roles = roles;
    }

    /// The role input names, in slot order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }
}

/// Rest density of water, kg/m³: every liquid solver's water weighs this.
pub const WATER_DENSITY: f32 = 1000.0;

pub mod substep_history;
