//! Persistent sparse evaluations of authored scene-modifier graphs.
//!
//! The normal runtime keeps its original graph and resources. Each presentation
//! evaluates the same prepared bindings against bounded sample geometry.

use super::*;
use crate::node_graph::primitives::standalone_pipeline::dispatch_standalone_2d;
use crate::node_graph::scene_modifier_expand::{MathViewScope, SceneModifierExpandError};

pub(super) struct MathViewRuntime {
    pub modifier_id: NodeId,
    mode_node: NodeInstanceId,
    scope_node: NodeInstanceId,
    pub variants: [PresetRuntime; 2],
    presentation: Option<Presentation>,
    last_active_scope: Option<usize>,
    pub(super) events: super::math_view_events::MathEvents,
    shared_resources: [Vec<(ResourceId, ResourceId)>; 2],
    pub(super) shared_depth: [Vec<(ResourceId, ResourceId)>; 2],
}

struct Presentation {
    diagram: RenderTarget,
    background: RenderTarget,
    mix: manifold_gpu::GpuComputePipeline,
    sampler: manifold_gpu::GpuSampler,
}

impl PresetRuntime {
    pub(super) fn install_math_views(
        &mut self,
        device: std::sync::Arc<GpuDevice>,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
    ) -> Result<(), JsonGeneratorLoadError> {
        self.pin_math_view_depth(&device, width, height);
        for view in &mut self.math_views {
            view.install_device(
                self.executor.backend(),
                std::sync::Arc::clone(&device),
                width,
                height,
                format,
            )?;
        }
        Ok(())
    }

    /// An export sink keeps the scene producer live, but its texture must also
    /// survive the parent's final step so a separate view executor can borrow it.
    pub(super) fn pin_math_view_depth(&mut self, device: &GpuDevice, width: u32, height: u32) {
        let Some(backend) = self.executor.backend_mut().as_any_mut()
            .and_then(|any| any.downcast_mut::<MetalBackend>()) else { return; };
        for view in &self.math_views {
            for &(source, _) in view.shared_depth.iter().flatten() {
                if let Some(slot) = crate::node_graph::Backend::slot_for(backend, source) {
                    backend.bind_resource_to_slot(source, slot);
                } else {
                    let (w, h) = crate::node_graph::execution::resolve_dims(
                        &self.plan, source, (width, height),
                    );
                    backend.pre_bind_texture_2d(source, RenderTarget::new(
                        device, w, h, GpuTextureFormat::R32Float, "math_view.scene_depth",
                    ));
                }
            }
        }
    }

    pub(super) fn tick_math_view_events(&mut self, beat: Beats) {
        for view in &mut self.math_views {
            let (count, baseline) = self
                .modifier_events
                .as_ref()
                .and_then(|events| events.counts(&view.modifier_id))
                .expect("prepared Math View event stream");
            view.events
                .tick(&mut self.graph, &mut view.variants, count, baseline, beat);
        }
    }

    pub(super) fn render_math_views(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        target: &GpuTexture,
        ctx: &PresetContext,
        params: &ParamManifest,
    ) {
        for view in &mut self.math_views {
            if view.resources_ready(&self.executor) {
                if view.mode(&self.graph) != 0 {
                    view.borrow_scene_depth(self.executor.backend());
                }
                view.render(&self.graph, gpu, target, ctx, params);
            }
        }
    }
}

pub(super) fn invalid(detail: impl Into<String>) -> JsonGeneratorLoadError {
    JsonGeneratorLoadError::SceneModifier(SceneModifierExpandError::InvalidRecipe {
        path: "mathView".into(),
        detail: detail.into(),
    })
}

pub(super) fn prepare_views(
    owner: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    manifest: Option<&ParamManifest>,
    fused: bool,
    parent: &PresetRuntime,
) -> Result<Vec<MathViewRuntime>, JsonGeneratorLoadError> {
    let mut views = Vec::new();
    for modifier in &owner.scene_modifiers {
        if !manifold_core::scene_modifier_math_view::has_math_view_controls(&modifier.graph) {
            continue;
        }
        let control = |suffix: &str| {
            let local = manifold_core::scene_modifier_preset::SceneNodeRef {
                scope: Vec::new(),
                node: NodeId::new(format!("__math_view_{suffix}")),
            };
            let copies = parent
                .modifier_node_copies(&modifier.id, &local)
                .ok_or_else(|| {
                    invalid(format!(
                        "missing {suffix} control route for {}",
                        modifier.id
                    ))
                })?;
            if copies.len() != 1 || copies[0].object.is_some() {
                return Err(invalid(format!("{suffix} must be a shared control")));
            }
            parent
                .graph
                .instance_by_node_id(&copies[0].node_id)
                .ok_or_else(|| invalid(format!("missing prepared {suffix} control")))
        };
        let mode_node = control("mode")?;
        let scope_node = control("scope")?;
        let isolated = PresetRuntime::from_def_for_render_view(
            owner.clone(),
            registry,
            manifest,
            fused,
            Some((&modifier.id, MathViewScope::ThisModifier)),
        )?;
        let chained = PresetRuntime::from_def_for_render_view(
            owner.clone(),
            registry,
            manifest,
            fused,
            Some((&modifier.id, MathViewScope::WithinChain)),
        )?;
        let controls = manifold_core::scene_modifier_math_view::CONTROLS
            .iter()
            .map(|(name, _, default, _, _)| control(name).map(|node| (*name, node, *default)))
            .collect::<Result<Vec<_>, _>>()?;
        let variants = [isolated, chained];
        let events =
            super::math_view_events::MathEvents::prepare(modifier, parent, &variants, controls)?;
        let shared_depth = [
            shared_resources(parent, &variants[0], modifier, &["depth"])?,
            shared_resources(parent, &variants[1], modifier, &["depth"])?,
        ];
        let shared_resources = [
            shared_resources(parent, &variants[0], modifier, &["vertices", "weights"])?,
            shared_resources(parent, &variants[1], modifier, &["vertices", "weights"])?,
        ];
        views.push(MathViewRuntime {
            modifier_id: modifier.id.clone(),
            mode_node,
            scope_node,
            variants,
            presentation: None,
            last_active_scope: None,
            events,
            shared_resources,
            shared_depth,
        });
    }
    Ok(views)
}

impl MathViewRuntime {
    fn control(graph: &Graph, node: NodeInstanceId) -> f32 {
        graph
            .get_node(node)
            .and_then(|node| node.params.get("value"))
            .and_then(ParamValue::as_scalar)
            .expect("Math View control resolved at preparation")
    }

    pub fn mode(&self, graph: &Graph) -> u32 {
        Self::control(graph, self.mode_node).round().clamp(0.0, 2.0) as u32
    }

    pub fn install_device(
        &mut self,
        parent: &dyn crate::node_graph::Backend,
        device: std::sync::Arc<GpuDevice>,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
    ) -> Result<(), JsonGeneratorLoadError> {
        crate::node_graph::primitives::RenderMeshDiagram::prewarm_pipelines(&device);
        self.bind_shared_resources(parent)?;
        for variant in &mut self.variants {
            variant.install_generator_device(
                std::sync::Arc::clone(&device),
                width,
                height,
                GpuTextureFormat::Rgba16Float,
            )?;
        }
        self.install_depth_inputs(&device);
        self.presentation = Some(Presentation::new(&device, width, height, format)?);
        Ok(())
    }

    pub fn resize(
        &mut self,
        parent: &dyn crate::node_graph::Backend,
        device: &GpuDevice,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
    ) {
        self.bind_shared_resources(parent)
            .expect("previously admitted shared mesh resources");
        self.events.clear();
        for variant in &mut self.variants {
            variant.resize(device, width, height);
        }
        self.install_depth_inputs(device);
        self.presentation = Some(
            Presentation::new(device, width, height, format)
                .expect("previously admitted Math View output format"),
        );
        self.last_active_scope = None;
    }

    pub fn resources_ready(&self, parent: &crate::node_graph::execution::Executor) -> bool {
        self.shared_resources
            .iter()
            .flatten()
            .chain(self.shared_depth.iter().flatten())
            .all(|(source, _)| parent.resource_content_ready(*source))
    }

    pub fn render(
        &mut self,
        graph: &Graph,
        gpu: &mut GpuEncoder<'_>,
        target: &GpuTexture,
        ctx: &PresetContext,
        params: &ParamManifest,
    ) {
        let mode = self.mode(graph);
        if mode == 0 {
            self.last_active_scope = None;
            return;
        }
        let scope = usize::from(Self::control(graph, self.scope_node) >= 0.5);
        if self.last_active_scope != Some(scope) {
            self.variants[scope].clear_state();
        }
        self.last_active_scope = Some(scope);
        let presentation = self
            .presentation
            .as_ref()
            .expect("Math View device installed");
        self.variants[scope].render(gpu, &presentation.diagram.texture, ctx, params);
        // Keep source and destination disjoint for Metal storage writes.
        if mode == 2 {
            gpu.copy_texture_to_texture(
                target,
                &presentation.background.texture,
                target.width,
                target.height,
            );
        }
        let background = if mode == 1 {
            &presentation.diagram.texture
        } else {
            &presentation.background.texture
        };
        let uniforms = MixUniforms {
            amount: 1.0,
            mode: if mode == 1 { 0 } else { 2 },
            padding: [0; 2],
        };
        dispatch_standalone_2d(
            gpu,
            &presentation.mix,
            bytemuck::bytes_of(&uniforms),
            &[background, &presentation.diagram.texture],
            Some(&presentation.sampler),
            target,
            "math_view.composite",
        );
    }
}

impl MathViewRuntime {
    /// Pin tiny owned placeholders, then borrow the live parent's textures each
    /// frame. Borrowed handles must never enter the view's owned texture pool.
    fn install_depth_inputs(&mut self, device: &GpuDevice) {
        for (variant, resources) in self.variants.iter_mut().zip(&self.shared_depth) {
            let backend = variant.executor.backend_mut().as_any_mut()
                .and_then(|any| any.downcast_mut::<MetalBackend>())
                .expect("Math View installed on Metal backend");
            for &(_, destination) in resources {
                backend.pre_bind_texture_2d(destination, RenderTarget::new(
                    device, 1, 1, GpuTextureFormat::R32Float, "math_view.depth_input",
                ));
            }
        }
    }

    fn borrow_scene_depth(&mut self, parent: &dyn crate::node_graph::Backend) {
        for (variant, resources) in self.variants.iter_mut().zip(&self.shared_depth) {
            let backend = variant.executor.backend_mut().as_any_mut()
                .and_then(|any| any.downcast_mut::<MetalBackend>())
                .expect("Math View installed on Metal backend");
            for &(source, destination) in resources {
                let depth = parent.slot_for(source)
                    .and_then(|slot| parent.texture_2d(slot))
                    .expect("ready parent scene depth");
                let slot = crate::node_graph::Backend::slot_for(backend, destination)
                    .expect("pinned Math View depth input");
                assert!(backend.replace_texture_2d(slot, depth.clone()));
            }
        }
    }

    fn bind_shared_resources(
        &mut self,
        parent: &dyn crate::node_graph::Backend,
    ) -> Result<(), JsonGeneratorLoadError> {
        for (variant, resources) in self.variants.iter_mut().zip(&self.shared_resources) {
            variant.shared_arrays.clear();
            for &(source, destination) in resources {
                let buffer = parent
                    .slot_for(source)
                    .and_then(|slot| parent.array_buffer(slot))
                    .ok_or_else(|| invalid("parent mesh export buffer is absent"))?;
                variant.shared_arrays.push((destination, buffer.clone()));
            }
        }
        Ok(())
    }
}

fn shared_resources(
    parent: &PresetRuntime,
    variant: &PresetRuntime,
    modifier: &manifold_core::scene_modifier_preset::SceneModifierInstanceDef,
    ports: &[&str],
) -> Result<Vec<(ResourceId, ResourceId)>, JsonGeneratorLoadError> {
    let mut resources = Vec::new();
    for frame in &modifier.mesh_frames {
        let export_id = crate::node_graph::scene_modifier_expand::math_resource_node_id(
            &modifier.id,
            &frame.target,
            "export",
        );
        let export = parent
            .graph
            .instance_by_node_id(&export_id)
            .ok_or_else(|| invalid("parent mesh export node is absent"))?;
        // Seed nodes retain the first slice's stable generated identity.
        let input = variant
            .graph
            .instance_by_node_id(&sample_node_id(&modifier.id, &frame.target))
            .filter(|id| {
                variant
                    .graph
                    .get_node(*id)
                    .is_some_and(|node| node.node.type_id().as_str() == "system.mesh_input")
            })
            .ok_or_else(|| invalid("view mesh input node is absent"))?;
        for &port in ports {
            let source = parent
                .plan
                .steps()
                .iter()
                .find(|s| s.node == export)
                .and_then(|s| s.inputs.iter().find(|(name, _)| *name == port))
                .map(|(_, r)| *r)
                .ok_or_else(|| invalid("parent export resource is absent"))?;
            let destination = variant
                .plan
                .steps()
                .iter()
                .find(|s| s.node == input)
                .and_then(|s| s.outputs.iter().find(|(name, _)| *name == port))
                .map(|(_, r)| *r)
                .ok_or_else(|| invalid("view input resource is absent"))?;
            resources.push((source, destination));
        }
    }
    Ok(resources)
}

fn sample_node_id(
    modifier: &NodeId,
    target: &manifold_core::scene_modifier_preset::SceneNodeRef,
) -> NodeId {
    crate::node_graph::scene_modifier_expand::math_sample_node_id(modifier, target)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MixUniforms {
    amount: f32,
    mode: u32,
    padding: [u32; 2],
}

impl Presentation {
    fn new(
        device: &GpuDevice,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
    ) -> Result<Self, JsonGeneratorLoadError> {
        let storage = match format {
            GpuTextureFormat::Rgba16Float => "rgba16float",
            GpuTextureFormat::Rgba8Unorm => "rgba8unorm",
            _ => {
                return Err(invalid(format!(
                    "unsupported Math View output format {format:?}"
                )));
            }
        };
        // The composition operation is the existing Mix atom's generated
        // kernel, including its alpha contract; no parallel blend equation.
        let shader = crate::node_graph::freeze::codegen::standalone_for_spec::<Mix>()
            .map_err(|error| invalid(format!("Math View Mix codegen: {error:?}")))?
            .replace("rgba16float", storage);
        Ok(Self {
            diagram: RenderTarget::new(
                device,
                width,
                height,
                GpuTextureFormat::Rgba16Float,
                "math_view.diagram",
            ),
            background: RenderTarget::new(device, width, height, format, "math_view.background"),
            mix: device.create_compute_pipeline(
                &shader,
                crate::node_graph::freeze::codegen::ENTRY,
                "math_view.composite",
            ),
            sampler: device.create_sampler(&manifold_gpu::GpuSamplerDesc::default()),
        })
    }
}
