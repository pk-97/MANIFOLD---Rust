//! Host access to scene snapshots and events supplied by runtime extensions.

use manifold_core::{NodeId, id::EffectId};

use super::{ModifierPreviewContext, PresetRuntime};
use crate::{
    exec::effect_node::FrameTime,
    scene::{fluid_domain::FluidDomainSnapshot, impulse::SceneImpulseDiagnostics},
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

    pub fn write_fluid_domains_watched(&self, output: &mut Vec<(NodeId, FluidDomainSnapshot)>) {
        if let Some(slot) = self.extension_slots().first() {
            self.write_fluid_domains(slot.effect_id, output);
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
                let snapshot = output[read].1;
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

    pub fn fire_scene_impulse(
        &mut self,
        param: &str,
        source: FrameTime,
        next_sequence: &mut u64,
    ) -> Result<bool, String> {
        let (extensions, mut context) = self.extension_context();
        for extension in extensions {
            if extension.fire_scene_impulse(&mut context, param, source, next_sequence)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn drain_scene_impulse_diagnostics(&mut self, diagnostics: &mut SceneImpulseDiagnostics) {
        self.for_each_extension(|extension, context| {
            extension.drain_scene_impulse_diagnostics(context, diagnostics);
        });
    }
}
