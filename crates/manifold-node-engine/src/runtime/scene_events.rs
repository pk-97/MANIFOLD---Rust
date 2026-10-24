//! Host access to scene snapshots and events supplied by runtime extensions.

use manifold_core::{Beats, NodeId, PlayedTime, id::EffectId};

use super::{ModifierPreviewContext, PresetRuntime};
use crate::exec::effect_node::FrameTime;
use manifold_core::{
    fluid_domain::FluidDomainSnapshot,
    scene_impulse::{ImpulseFire, SceneImpulseDiagnostics},
};

impl PresetRuntime {
    /// Append accepted domains without polling workers. The caller reuses the buffer.
    pub fn write_fluid_domains(
        &self,
        effect_id: &EffectId,
        output: &mut Vec<(NodeId, FluidDomainSnapshot)>,
    ) {
        let Some(slot) = self
            .extension_slots()
            .iter()
            .find(|slot| slot.effect_id == effect_id)
        else {
            return;
        };
        for extension in &self.extensions {
            extension.write_fluid_domains(&self.graph, slot, output);
        }
    }

    /// The camera the scene last rendered through and the authored id of the
    /// node that rendered it, from the first node in authored order that
    /// reports one; that node's light positions are appended to `lights`.
    /// Read-only, for editor overlays and the preview's navigation view.
    pub fn write_rendered_view(
        &self,
        lights: &mut Vec<[f32; 3]>,
    ) -> Option<(NodeId, crate::scene::camera::Camera)> {
        let node = self
            .extension_slots()
            .first()?
            .node_map
            .iter()
            .find_map(|(id, instance)| {
                let node = &self.graph.get_node(*instance)?.node;
                node.rendered_camera().map(|camera| (id, node, camera))
            });
        let (id, node, camera) = node?;
        lights.extend_from_slice(node.rendered_lights());
        Some((id.clone(), camera))
    }

    pub fn write_fluid_domains_watched(&self, output: &mut Vec<(NodeId, FluidDomainSnapshot)>) {
        if let Some(slot) = self.extension_slots().first() {
            self.write_fluid_domains(slot.effect_id, output);
        }
    }

    /// Append the setup shapes of the same slot's simulations, for the
    /// gizmo overlay. The caller reuses the buffer.
    pub fn write_physics_gizmos_watched(
        &self,
        output: &mut Vec<manifold_core::physics_gizmo::PhysicsGizmo>,
    ) {
        let Some(slot) = self.extension_slots().first() else {
            return;
        };
        for extension in &self.extensions {
            extension.write_physics_gizmos(&self.graph, slot, output);
        }
    }

    /// Append the same slot's worlds that left copies unmade. The caller
    /// reuses the buffer.
    pub fn write_copies_not_made_watched(&self, output: &mut Vec<(NodeId, u32)>) {
        let Some(slot) = self.extension_slots().first() else {
            return;
        };
        for extension in &self.extensions {
            extension.write_copies_not_made(&self.graph, slot, output);
        }
    }

    /// Append the same slot's worlds' refused bodies. The caller reuses the
    /// buffer.
    pub fn write_body_refusals_watched(
        &self,
        output: &mut Vec<(NodeId, manifold_core::physics_members::BodyRefusalRow)>,
    ) {
        let Some(slot) = self.extension_slots().first() else {
            return;
        };
        for extension in &self.extensions {
            extension.write_body_refusals(&self.graph, slot, output);
        }
    }

    /// Translate the selected generated modifier copy back to authored node IDs.
    pub fn write_modifier_fluid_domains(
        &self,
        context: &ModifierPreviewContext,
        output: &mut Vec<(NodeId, FluidDomainSnapshot)>,
    ) {
        let start = output.len();
        self.write_fluid_domains_watched(output);
        let mut write = start;
        for read in start..output.len() {
            let authored = self
                .modifier_preview_local_node(context, output[read].0.as_str())
                .cloned();
            if let Some(authored) = authored {
                let snapshot = output[read].1.clone();
                output[write] = (authored, snapshot);
                write += 1;
            }
        }
        output.truncate(write);
    }

    pub fn is_scene_impulse_param(&self, param: &str) -> bool {
        self.extensions
            .iter()
            .any(|extension| extension.is_scene_impulse_param(param))
    }

    /// A Fire at transport `source`: it becomes played time with this frame's
    /// tempo shift here, as it arrives.
    pub fn fire_scene_impulse(
        &mut self,
        param: &str,
        source: FrameTime,
        next_sequence: &mut u64,
    ) -> Result<ImpulseFire, String> {
        let played = self.executor.sim_step().to_played(source.seconds);
        self.fire_scene_impulse_played(param, played, source.beats, next_sequence)
    }

    /// One extension owns each impulse param; the Fire goes to it alone, so
    /// its refusal is this Fire's answer and never stops another Fire.
    pub fn fire_scene_impulse_played(
        &mut self,
        param: &str,
        source: PlayedTime,
        beats: Beats,
        next_sequence: &mut u64,
    ) -> Result<ImpulseFire, String> {
        let (extensions, mut context) = self.extension_context();
        match extensions
            .iter_mut()
            .find(|extension| extension.is_scene_impulse_param(param))
        {
            Some(owner) => {
                owner.fire_scene_impulse(&mut context, param, source, beats, next_sequence)
            }
            None => Ok(ImpulseFire::NotImpulse),
        }
    }

    pub fn drain_scene_impulse_diagnostics(&mut self, diagnostics: &mut SceneImpulseDiagnostics) {
        self.for_each_extension(|extension, context| {
            extension.drain_scene_impulse_diagnostics(context, diagnostics);
        });
    }
}
