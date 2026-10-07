//! Real-device construction and generator-device installation for [`PresetRuntime`].

use super::*;

impl PresetRuntime {
    /// Parse + compile + wire to a real [`MetalBackend`] for production
    /// rendering. Pre-binds a 1×1 placeholder at the FinalOutput-source slot so
    /// per-frame `render()` only swaps the borrowed texture (no hot-path alloc).
    pub fn from_json_str_with_device(
        json: &str,
        registry: &PrimitiveRegistry,
        device: std::sync::Arc<GpuDevice>,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        manifest: Option<&ParamManifest>,
    ) -> Result<Self, JsonGeneratorLoadError> {
        let doc: EffectGraphDef = serde_json::from_str(json)?;
        Self::from_def_with_device(doc, registry, device, width, height, format, manifest)
    }

    /// Same as [`Self::from_json_str_with_device`] but skips the JSON parse.
    /// `manifest` follows the [`Self::from_def`] contract: the live per-instance
    /// [`ParamManifest`] on a project-generator rebuild, `None` standalone.
    pub fn from_def_with_device(
        doc: EffectGraphDef,
        registry: &PrimitiveRegistry,
        device: std::sync::Arc<GpuDevice>,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        manifest: Option<&ParamManifest>,
    ) -> Result<Self, JsonGeneratorLoadError> {
        Self::from_def(doc, registry, manifest)?
            .with_generator_device(device, width, height, format)
    }

    pub(crate) fn with_generator_device(
        mut self,
        device: std::sync::Arc<GpuDevice>,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
    ) -> Result<Self, JsonGeneratorLoadError> {
        self.install_generator_device(device, width, height, format)?;
        Ok(self)
    }

    pub(super) fn install_generator_device(
        &mut self,
        device: std::sync::Arc<GpuDevice>,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
    ) -> Result<(), JsonGeneratorLoadError> {
        let g = self;
        g.width = width;
        g.height = height;
        let mut backend = MetalBackend::new(std::sync::Arc::clone(&device), width, height, format);
        let PresetIo::Generate {
            final_output_input_resource,
            ..
        } = g.io
        else {
            unreachable!("from_def always produces Generate IO");
        };
        // Pre-bind a 1×1 placeholder at the FinalOutput-source slot so the slot
        // exists across frames; `install_target` swaps in the host's real target
        // via `replace_texture_2d` each render call.
        let placeholder = RenderTarget::new(&device, 1, 1, format, "preset_runtime_target_owner");
        let slot = backend.pre_bind_texture_2d(final_output_input_resource, placeholder);
        if let PresetIo::Generate {
            final_output_slot, ..
        } = &mut g.io
        {
            *final_output_slot = Some(slot);
        }
        g.target_format = Some(format);

        // Pre-allocate every Array<T> buffer + Texture3D volume the compiled
        // plan declares, then run the post-allocation audit — the same shared
        // pipeline the effect chain uses.
        for (resource, buffer) in &g.shared_arrays {
            backend.pre_bind_array(*resource, buffer.clone());
        }
        crate::load::graph_loader::pre_allocate_resources(&mut g.graph, &g.plan, &device, &mut backend)
            .map_err(super::modifier_runtime::generator_error_from_prealloc)?;

        g.executor = Executor::new(Box::new(backend));
        g.install_math_views(device, width, height, format)?;
        Ok(())
    }
}
