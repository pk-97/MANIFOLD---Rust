//! Preserve native physics through compatible generator presentation rebuilds.
use crate::runtime::*;
use crate::ports::PortType;
use crate::water::physics::RigidBody;
use crate::water::fluid_role::FluidRole;
use manifold_physics::FieldValue;

#[cfg(test)]
mod tests;

impl PresetRuntime {
    pub fn carry_generator_state_from(&mut self, prior: &mut Self) {
        self.carry_physics_state_from(prior);
        self.carry_modifier_control_state_from(prior);
    }

    fn carry_physics_state_from(&mut self, prior: &mut Self) {
        let (Some(slot), Some(old_slot)) = (self.effect_nodes.first(), prior.effect_nodes.first())
        else {
            return;
        };
        if self.type_id.is_none()
            || self.type_id != prior.type_id
            || self.width != prior.width
            || self.height != prior.height
            || slot.def_content_key == 0
            || slot.def_content_key != old_slot.def_content_key
        {
            return;
        }
        let (Some(mask), Some(old_mask)) =
            (&self.water.sample_steps, &prior.water.sample_steps)
        else {
            return;
        };
        // Validate the entire CPU ancestry before moving any world. Fusion is
        // permitted around it, but a missing/retyped input cannot inherit time.
        let mut steps = Vec::new();
        for (index, step) in self
            .plan
            .steps()
            .iter()
            .enumerate()
            .filter(|(i, _)| mask[*i])
        {
            let node = self.graph.get_node(step.node).expect("compiled node");
            let Some((old_index, _)) = prior.plan.steps().iter().enumerate().find(|(i, old)| {
                old_mask[*i]
                    && prior.graph.get_node(old.node).is_some_and(|old| {
                        old.node_id == node.node_id
                            && old.node.type_id() == node.node.type_id()
                            && old.params.len() == node.params.len()
                            && old.params.keys().all(|key| node.params.contains_key(key))
                    })
            }) else {
                return;
            };
            steps.push((index, old_index));
        }
        if steps.is_empty() || steps.len() != old_mask.iter().filter(|&&v| v).count() {
            return;
        }
        for &(new_index, old_index) in &steps {
            let new_id = self.plan.steps()[new_index].node;
            let old_id = prior.plan.steps()[old_index].node;
            let new = self.graph.get_node_mut(new_id).expect("compiled node");
            // These native owners retain CPU snapshots and republish them to
            // fresh destinations. Their upload caches include buffer identity.
            // Do not move arbitrary GPU primitives or copy backend slot IDs.
            if matches!(
                new.node.type_id().as_str(),
                "node.physics_world"
                    | "node.rigid_body"
                    | "node.fluid_role_source"
            ) || (cfg!(feature = "gpu-proofs")
                && new.node.type_id().as_str() == manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID) {
                let old = prior.graph.get_node_mut(old_id).expect("compiled node");
                std::mem::swap(&mut new.node, &mut old.node);
            }
        }
        if let (Some(inputs), Some(old_inputs)) = (
            &mut self.water.input_snapshot,
            &prior.water.input_snapshot,
        ) {
            inputs.carry_from(old_inputs, &steps);
        }
        self.water.last_frame_time = prior.water.last_frame_time;
        self.water.project_tempo.clone_from(&prior.water.project_tempo);
        #[cfg(feature = "gpu-proofs")]
        self.carry_physics_source_controls_from(prior);
        #[cfg(feature = "gpu-proofs")]
        self.install_physics_source_identities();

        // Setup and event wires are intentionally outside historical sampling.
        // Retain their last available CPU values under the new resource IDs so
        // the first historical interval sees the same inputs as before rebuild.
        // Pending or GPU-backed storage is never transferred.
        for &(new_index, old_index) in &steps {
            let new_step = &self.plan.steps()[new_index];
            let old_step = &prior.plan.steps()[old_index];
            for &(port, resource) in &new_step.inputs {
                let Some(&(_, old_resource)) =
                    old_step.inputs.iter().find(|(name, _)| *name == port)
                else {
                    continue;
                };
                if prior.executor.mesh_pending_of(old_resource) {
                    continue;
                }
                let Some(ty) = self.plan.resource_type(resource) else {
                    continue;
                };
                if prior.plan.resource_type(old_resource) != Some(ty) {
                    continue;
                }
                let old_backend = prior.executor.backend();
                let Some(old_slot) = old_backend.slot_for(old_resource) else {
                    continue;
                };
                let backend = self.executor.backend_mut();
                macro_rules! carry {
                    ($payload:ty) => {
                        if let Some(value) = old_backend.cpu_values().get::<$payload>(old_slot) {
                            let slot = backend.acquire(resource, ty, None, (0, 0));
                            backend.cpu_values_mut().set(slot, value);
                        }
                    };
                    ($read:ident, $write:ident) => {
                        if let Some(value) = old_backend.$read(old_slot) {
                            let slot = backend.acquire(resource, ty, None, (0, 0));
                            backend.$write(slot, value);
                        }
                    };
                }
                match ty {
                    PortType::Scalar(_) => carry!(scalar, set_scalar),
                    PortType::Transform => carry!(transform, set_transform),
                    PortType::RigidBody => carry!(RigidBody),
                    PortType::FluidRole => carry!(FluidRole),
                    PortType::VectorField => carry!(FieldValue),
                    PortType::MeshSource => carry!(mesh_source, set_mesh_source),
                    _ => {}
                }
            }
        }
    }
}
