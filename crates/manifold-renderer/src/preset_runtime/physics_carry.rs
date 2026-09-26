//! Preserve native physics through compatible generator presentation rebuilds.
use super::*;
use crate::node_graph::PortType;

#[cfg(test)]
#[path = "physics_carry_tests.rs"]
mod tests;

impl PresetRuntime {
    pub(crate) fn carry_generator_state_from(&mut self, prior: &mut Self) {
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
            (&self.physics_sample_steps, &prior.physics_sample_steps)
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
                "node.fluid_surface"
                    | "node.physics_world"
                    | "node.rigid_body"
                    | "node.fluid_role_source"
            ) {
                let old = prior.graph.get_node_mut(old_id).expect("compiled node");
                std::mem::swap(&mut new.node, &mut old.node);
            }
        }
        if let (Some(inputs), Some(old_inputs)) = (
            &mut self.physics_input_snapshot,
            &prior.physics_input_snapshot,
        ) {
            inputs.carry_from(old_inputs, &steps);
        }
        self.last_physics_frame_time = prior.last_physics_frame_time;

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
                    PortType::RigidBody => carry!(rigid_body, set_rigid_body),
                    PortType::FluidRole => carry!(fluid_role, set_fluid_role),
                    PortType::VectorField => carry!(vector_field, set_vector_field),
                    PortType::MeshSource => carry!(mesh_source, set_mesh_source),
                    _ => {}
                }
            }
        }
    }
}
