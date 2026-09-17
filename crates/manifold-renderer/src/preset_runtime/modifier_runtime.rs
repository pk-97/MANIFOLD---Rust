//! Scene-modifier runtime admission, event state, and generator preparation.
//!
//! These methods are a facet of [`PresetRuntime`], kept here so the core frame
//! loop remains within its source-size contract. Their signatures and behavior
//! remain part of the same runtime type.

use ahash::AHashMap;
use manifold_core::effects::PresetInstance;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::params::ParamManifest;

use super::{FrameContextInputs, JsonGeneratorLoadError, PresetIo, PresetRuntime};
use crate::node_graph::{ParamValue, PrimitiveRegistry};

/// Map resource-preparation failures into the public generator-load error.
pub(super) fn generator_error_from_prealloc(
    e: crate::node_graph::PreAllocationError,
) -> JsonGeneratorLoadError {
    use crate::node_graph::PreAllocationError as P;
    match e {
        P::ModifierAdmission(error) => JsonGeneratorLoadError::SceneModifier(error),
        P::ModifierMemoryUnavailable => JsonGeneratorLoadError::SceneModifier(
            crate::node_graph::scene_modifier_expand::SceneModifierExpandError::CapacityExceeded {
                path: "modifierBufferBudget".into(),
                detail: "the GPU did not expose current allocated size and working-set capacity"
                    .into(),
            },
        ),
        P::UnsizedArrayOutput {
            node_type, port, ..
        } => JsonGeneratorLoadError::UnsizedArrayOutput { node_type, port },
        P::UnsizedTexture3DOutput {
            node_type, port, ..
        } => JsonGeneratorLoadError::UnsizedTexture3DOutput { node_type, port },
        P::UnboundArrayResource {
            producer_handle,
            producer_node_type,
            producer_port,
            cause,
        } => JsonGeneratorLoadError::UnboundArrayResource {
            producer_handle,
            producer_node_type,
            producer_port,
            cause,
        },
    }
}

impl PresetRuntime {
    /// Shared structural entry for watched, standalone and fused generators.
    pub(crate) fn from_def_for_render(
        doc: EffectGraphDef,
        registry: &PrimitiveRegistry,
        manifest: Option<&ParamManifest>,
        render_fused: bool,
    ) -> Result<Self, JsonGeneratorLoadError> {
        if !doc.scene_modifiers.iter().any(|modifier|
            manifold_core::scene_modifier_math_view::has_math_view_controls(&modifier.graph)) {
            return Self::from_def_for_render_view(doc, registry, manifest, render_fused, None);
        }
        let mut runtime = Self::from_def_for_render_view(doc.clone(), registry, manifest, render_fused, None)?;
        runtime.math_views = super::math_view::prepare_views(&doc, registry, manifest, render_fused, &runtime)?;
        Ok(runtime)
    }

    pub(super) fn from_def_for_render_view(
        doc: EffectGraphDef,
        registry: &PrimitiveRegistry,
        manifest: Option<&ParamManifest>,
        render_fused: bool,
        math_view: Option<(&manifold_core::NodeId, crate::node_graph::scene_modifier_expand::MathViewScope)>,
    ) -> Result<Self, JsonGeneratorLoadError> {
        let (render_def, authoring) =
            if manifold_core::scene_modifier_preset::has_scene_modifier_data(&doc)
                || crate::node_graph::scene_modifier_expand::contains_fragments(&doc)
            {
                let prepared = match math_view {
                    Some((modifier_id, scope)) => crate::node_graph::scene_modifier_expand::prepare_scene_modifier_math_view(&doc, registry, modifier_id, scope)?,
                    None => crate::node_graph::scene_modifier_expand::prepare_scene_modifiers(&doc, registry)?,
                };
                // The generator resolver drops Composite bindings. Keep provenance
                // in the same order before installing the resolved binding list.
                let sources = prepared
                    .def
                    .preset_metadata
                    .as_ref()
                    .map(|metadata| {
                        metadata
                            .bindings
                            .iter()
                            .zip(prepared.binding_sources)
                            .filter_map(|(binding, source)| {
                                matches!(binding.target, BindingTarget::Node { .. })
                                    .then_some(source)
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let guards = crate::node_graph::scene_modifier_expand::PreparedModifierParameterGuards::prepare(&doc)?;
                (
                    prepared.def,
                    Some((doc, prepared.routes, sources, guards, prepared.event_routes)),
                )
            } else {
                (doc, None)
            };
        let fused = if render_fused {
            crate::node_graph::freeze::install::fused_generator_view_for(&render_def)
        } else {
            None
        };
        // Design §3.3: the fused view's mesh-rule sidecar describes the
        // fused def's generated node ids, so it must ride into
        // `from_render_def` alongside `view.def`. An empty map is correct
        // only when fusion did not occur — an unfused render_def has no
        // sidecar by construction.
        let mesh_rules = fused
            .as_ref()
            .map_or_else(crate::node_graph::mesh_change::PreparedMeshRules::default, |view| {
                view.mesh_rules.clone()
            });
        let render_def = match &fused {
            Some(view) => (*view.def).clone(),
            None => render_def,
        };
        let mut runtime = Self::from_render_def(render_def, registry, manifest, &mesh_rules)?;
        if let Some(view) = &fused {
            runtime.effect_nodes[0].bound.fused_retarget = view.retarget.clone();
        }
        if let Some((canonical, routes, sources, guards, event_routes)) = authoring {
            crate::node_graph::scene_modifier_expand::validate_modifier_runtime(
                &canonical,
                &runtime.graph,
            )?;
            let empty_members = ahash::AHashMap::default();
            let members = fused
                .as_ref()
                .map_or(&empty_members, |view| &view.node_retarget);
            let budget =
                crate::node_graph::scene_modifier_expand::PreparedModifierBufferBudget::prepare(
                    &canonical,
                    &routes,
                    &runtime.graph,
                    members,
                )?;
            runtime.graph.set_modifier_buffer_budget(budget);
            guards.install(&mut runtime.graph)?;
            runtime.modifier_events = Some(
                crate::node_graph::scene_modifier_expand::PreparedModifierEvents::prepare(
                    &canonical,
                    &event_routes,
                    &runtime.graph,
                )?,
            );
            runtime.modifier_control_state = Some(crate::node_graph::scene_modifier_expand::PreparedModifierControlState::prepare_with_fusion(
                &canonical, &routes, &runtime.graph, members,
            )?);
            let segment = &mut runtime.effect_nodes[0];
            let writes =
                crate::node_graph::scene_modifier_expand::PreparedGraphValueWrites::prepare(
                    &canonical,
                    &routes,
                    &runtime.graph,
                    &segment.bound.fused_retarget,
                )?;
            segment.bound.install_prepared_routes(writes, sources)?;
            segment.group_preview_map =
                manifold_core::flatten::group_output_producer_map(&canonical);
            runtime.modifier_preview_routes = routes;
        }
        Ok(runtime)
    }
}

impl PresetRuntime {
    /// Check the same prepared-array plan used by native allocation, without
    /// creating GPU resources. Structural admission calls this before publishing
    /// an edited owner so an oversized stack cannot replace a working scene.
    pub fn prepared_modifier_buffer_usage(
        &self,
        canvas: (u32, u32),
    ) -> Result<
        Option<crate::node_graph::scene_modifier_expand::ModifierBufferUsage>,
        crate::node_graph::PreAllocationError,
    > {
        let Some(budget) = self.graph.modifier_buffer_budget() else {
            return Ok(None);
        };
        let allocation = crate::node_graph::resource_allocation::plan_array_allocations(
            &self.graph,
            &self.plan,
            canvas,
            &AHashMap::default(),
        )?;
        let mut usage = budget.account(&allocation)
            .map_err(crate::node_graph::PreAllocationError::ModifierAdmission)?;
        let add = |left: u64, right: u64| left.checked_add(right).ok_or_else(||
            crate::node_graph::PreAllocationError::ModifierAdmission(
                crate::node_graph::scene_modifier_expand::SceneModifierExpandError::CapacityExceeded {
                    path: "mathViewBuffers".into(), detail: "prepared byte count overflow".into(),
                }));
        for view in &self.math_views {
            for variant in &view.variants {
                if let Some(extra) = variant.prepared_modifier_buffer_usage(canvas)? {
                    usage.candidate_bytes = add(usage.candidate_bytes, extra.candidate_bytes)?;
                    usage.baseline_bytes = add(usage.baseline_bytes, extra.baseline_bytes)?;
                    for (scene, bytes) in extra.modifier_bytes {
                        let entry = usage.modifier_bytes.entry(scene).or_default();
                        *entry = add(*entry, bytes)?;
                    }
                }
            }
        }
        Ok(Some(usage))
    }

    /// Account and admit a prepared-array candidate against a captured GPU
    /// memory snapshot. The snapshot belongs to the caller's admission
    /// boundary; this method does no device query and performs no allocation.
    pub fn prepared_modifier_buffer_usage_with_snapshot(
        &self,
        canvas: (u32, u32),
        snapshot: Option<manifold_gpu::GpuMemorySnapshot>,
    ) -> Result<
        Option<crate::node_graph::scene_modifier_expand::ModifierBufferUsage>,
        crate::node_graph::PreAllocationError,
    > {
        let Some(usage) = self.prepared_modifier_buffer_usage(canvas)? else {
            return Ok(None);
        };
        crate::node_graph::scene_modifier_expand::admit_candidate_bytes(snapshot, usage.candidate_bytes)
            .map_err(crate::node_graph::PreAllocationError::ModifierAdmission)?;
        Ok(Some(usage))
    }

    pub(crate) fn is_modifier_trigger_param(&self, param: &str) -> bool {
        self.modifier_events
            .as_ref()
            .is_some_and(|events| events.is_modifier_param(param))
    }

    #[cfg(test)]
    pub(crate) fn note_modifier_audio_event(&mut self, param: &str) -> bool {
        for view in &mut self.math_views {
            for variant in &mut view.variants { variant.note_modifier_audio_event(param); }
        }
        self.modifier_events
            .as_mut()
            .is_some_and(|events| events.note_audio(param))
    }

    pub(crate) fn note_modifier_audio_key(&mut self, param_key: u64) -> bool {
        for view in &mut self.math_views {
            for variant in &mut view.variants { variant.note_modifier_audio_key(param_key); }
        }
        self.modifier_events
            .as_mut()
            .is_some_and(|events| events.note_audio_key(param_key))
    }

    pub(crate) fn note_modifier_clip_event(&mut self, host: Option<&PresetInstance>) {
        for view in &mut self.math_views {
            for variant in &mut view.variants { variant.note_modifier_clip_event(host); }
        }
        if let Some(events) = &mut self.modifier_events {
            events.note_clip(|param| {
                host.is_none_or(|host| {
                    host.clip_edge_enabled_matching(|candidate| candidate == param)
                })
            });
        }
    }

    pub(super) fn consume_trigger_markers(&mut self) {
        self.pending_trigger_baseline = None;
        if let Some(events) = &mut self.modifier_events {
            events.consume_pending();
        }
    }

    /// Called by the event owner before incrementing its clip/audio counter.
    /// Multiple events before an evaluation preserve the earliest baseline.
    pub fn note_trigger_event(&mut self, previous_count: u32) {
        self.pending_trigger_baseline.get_or_insert(previous_count);
        for view in &mut self.math_views {
            for variant in &mut view.variants { variant.note_trigger_event(previous_count); }
        }
    }

    pub(crate) fn carry_pending_trigger_from(&mut self, prior: &Self) {
        self.pending_trigger_baseline = prior.pending_trigger_baseline;
    }

    pub(crate) fn carry_modifier_control_state_from(&mut self, prior: &mut Self) {
        for view in &mut self.math_views {
            if let Some(previous) = prior.math_views.iter_mut().find(|previous| previous.modifier_id == view.modifier_id) {
                view.events.carry_from(&previous.events);
                for (variant, previous) in view.variants.iter_mut().zip(&mut previous.variants) {
                    variant.carry_modifier_control_state_from(previous);
                }
            }
        }
        self.carry_pending_trigger_from(prior);
        if let (Some(current), Some(previous)) = (&mut self.modifier_events, &prior.modifier_events)
        {
            current.carry_from(previous);
        }
        if let (Some(current), Some(previous)) =
            (&self.modifier_control_state, &prior.modifier_control_state)
        {
            current.harvest_from(
                previous,
                &mut self.graph,
                &mut prior.graph,
                &mut self.state_store,
                &mut prior.state_store,
            );
        }
    }

    /// Update the `system.generator_input` node's per-frame context. No-op on
    /// an effect-chain runtime.
    pub fn set_frame_context(&mut self, fc: FrameContextInputs) {
        if let Some(events) = &self.modifier_events {
            events.write_context(&mut self.graph);
        }
        let FrameContextInputs {
            time,
            beat,
            aspect,
            trigger_count,
            anim_progress,
            output_width,
            output_height,
        } = fc;
        let PresetIo::Generate {
            generator_input_id, ..
        } = self.io
        else {
            return;
        };
        let id = generator_input_id;
        let _ = self.graph.set_param(id, "time", ParamValue::Float(time));
        let _ = self.graph.set_param(id, "beat", ParamValue::Float(beat));
        let _ = self
            .graph
            .set_param(id, "aspect", ParamValue::Float(aspect));
        let _ = self
            .graph
            .set_param(id, "trigger_count", ParamValue::Float(trigger_count));
        let baseline = self
            .pending_trigger_baseline
            .map_or(trigger_count, |count| count as f32);
        let _ = self
            .graph
            .set_param(id, "trigger_baseline", ParamValue::Float(baseline));
        let _ = self
            .graph
            .set_param(id, "anim_progress", ParamValue::Float(anim_progress));
        let _ = self
            .graph
            .set_param(id, "output_width", ParamValue::Float(output_width));
        let _ = self
            .graph
            .set_param(id, "output_height", ParamValue::Float(output_height));
    }

    /// Push the host's slider values through the preset's bindings to the
    /// matching inner-node params (generator path). Each binding reads its value
    /// from the id-keyed `params` manifest by `source_id`; an empty manifest
    /// leaves every binding at its declared default. No per-frame allocation —
    /// the manifest is borrowed directly, no float-bus wrapping.
    pub fn apply_param_values(&mut self, params: &ParamManifest) {
        if let Some(seg) = self.effect_nodes.first_mut() {
            seg.bound.apply(&mut self.graph, params);
        }
    }

    /// Explicit local preview targets. Per-object copies remain distinct so
    /// the editor can request an object rather than silently selecting one.
    pub fn modifier_node_copies(
        &self,
        modifier: &manifold_core::NodeId,
        local: &manifold_core::scene_modifier_preset::SceneNodeRef,
    ) -> Option<&[crate::node_graph::scene_modifier_expand::SceneModifierNodeCopy]> {
        self.modifier_preview_routes
            .iter()
            .find(|route| &route.modifier_id == modifier && &route.local == local)
            .map(|route| route.copies.as_slice())
    }
}
