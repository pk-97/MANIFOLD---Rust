//! Transactional GPU resource replacement for a live preset runtime.

use super::*;
use super::core::GRAPH_FORMAT;
use crate::node_graph::Backend;

/// Opaque, fully prepared runtime resize.  Preparation allocates replacement
/// GPU resources while the live executor and graph remain untouched; commit is
/// an infallible publication step.
pub struct PreparedRuntimeResize {
    width: u32,
    height: u32,
    backend: Option<crate::node_graph::PreparedMetalBackendResize>,
    io: Option<PresetIo>,
    math_views: Vec<super::math_view::PreparedMathViewResize>,
}

impl PresetRuntime {
    /// Prepare a resize without changing the live graph, executor, or GPU
    /// bindings.  The returned value is safe to discard on allocation failure.
    pub fn prepare_resize(
        &self,
        device: &GpuDevice,
        width: u32,
        height: u32,
    ) -> Result<PreparedRuntimeResize, JsonGeneratorLoadError> {
        self.prepare_resize_with_overrides(device, width, height, &[])
    }

    pub(super) fn prepare_resize_with_overrides(
        &self,
        device: &GpuDevice,
        width: u32,
        height: u32,
        array_overrides: &[(ResourceId, manifold_gpu::GpuBuffer)],
    ) -> Result<PreparedRuntimeResize, JsonGeneratorLoadError> {
        let backend = self
            .executor
            .backend()
            .as_any()
            .and_then(|any| any.downcast_ref::<MetalBackend>())
            .map(|metal| {
                let mut prepared = metal
                    .prepare_resize(&self.plan, device, width, height)
                    .map_err(JsonGeneratorLoadError::Resize)?;
                let candidate = prepared.candidate_mut();
                for (resource, buffer) in array_overrides {
                    candidate.pre_bind_array(*resource, buffer.clone());
                }
                crate::node_graph::pre_allocate_resources(
                    &self.graph,
                    &self.plan,
                    device,
                    candidate,
                )
                .map_err(|error| JsonGeneratorLoadError::Resize(error.to_string()))?;
                candidate.prune_unbound_array_buffers();
                Ok::<_, JsonGeneratorLoadError>(prepared)
            })
            .transpose()?;
        let math_views = if let Some(prepared) = &backend {
            let parent = prepared.candidate();
            self.math_views
                .iter()
                .map(|view| view.prepare_resize(parent, device, width, height, self.target_format.unwrap_or(GRAPH_FORMAT)))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let io = backend.as_ref().map(|prepared| {
            let candidate = prepared.candidate();
            match self.io {
                PresetIo::Generate { generator_input_id, final_output_input_resource, .. } => {
                    PresetIo::Generate {
                        generator_input_id, final_output_input_resource,
                        final_output_slot: candidate.slot_for(final_output_input_resource),
                    }
                }
                PresetIo::Transform { source_slot, .. } => {
                    let old = self.executor.backend();
                    let source = (0..self.plan.resource_count())
                        .map(|id| ResourceId(id as u32))
                        .find(|&id| old.slot_for(id) == Some(source_slot))
                        .expect("transform source resource");
                    let output = self.plan.steps().iter()
                        .find(|step| self.graph.get_node(step.node)
                            .is_some_and(|node| node.node.type_id().as_str() == FINAL_OUTPUT_TYPE_ID))
                        .and_then(|step| step.inputs.first()).map(|(_, id)| *id)
                        .expect("transform final output input");
                    PresetIo::Transform {
                        source_slot: candidate.slot_for(source).expect("prepared source"),
                        output_slot: candidate.slot_for(output).expect("prepared output"),
                    }
                }
            }
        });
        Ok(PreparedRuntimeResize { width, height, backend, io, math_views })
    }

    /// Publish a previously prepared resize.  All fallible work has already
    /// completed, so this method only swaps owned state and resets simulations.
    pub fn commit_resize(&mut self, prepared: PreparedRuntimeResize) {
        self.width = prepared.width;
        self.height = prepared.height;
        if let Some(prepared_backend) = prepared.backend {
            let metal = self
                .executor
                .backend_mut()
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<MetalBackend>())
                .expect("prepared resize belongs to a MetalBackend");
            metal.commit_resize(prepared_backend);
        }
        if let Some(io) = prepared.io { self.io = io; }
        self.executor.reset_after_resource_replacement();
        for inst in self.graph.nodes_mut() {
            inst.node.clear_state();
        }
        self.state_store.cleanup_all();
        self.pending_trigger_baseline = None;
        let backend = self.executor.backend();
        for (view, prepared_view) in self.math_views.iter_mut().zip(prepared.math_views) {
            view.commit_resize(prepared_view, backend);
        }
    }

    /// Compatibility wrapper for callers that do not need to split prepare
    /// and commit.  Allocation errors are returned before live state changes.
    pub fn resize(
        &mut self,
        device: &GpuDevice,
        width: u32,
        height: u32,
    ) -> Result<(), JsonGeneratorLoadError> {
        let prepared = self.prepare_resize(device, width, height)?;
        self.commit_resize(prepared);
        Ok(())
    }

}
