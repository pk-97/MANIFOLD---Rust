//! HAL compute pipeline builder — creates pipelines from WGSL source via naga.
//!
//! Mirrors wgpu-hal's `load_shader` + `create_compute_pipeline` path exactly,
//! using identical naga MSL options to ensure shader output matches.

#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
mod inner {
    use wgpu::hal::{self, Device as HalDevice};

    use crate::hal_context::HalContext;

    type MetalApi = hal::api::Metal;

    /// A hal compute pipeline with its layout and bind group layout.
    /// Created once per effect/generator at init time.
    pub struct HalComputePipeline {
        pub pipeline: <MetalApi as hal::Api>::ComputePipeline,
        pub pipeline_layout: <MetalApi as hal::Api>::PipelineLayout,
        pub bind_group_layout: <MetalApi as hal::Api>::BindGroupLayout,
    }

    /// Create a hal compute pipeline from WGSL source.
    ///
    /// This mirrors wgpu-hal's internal compilation path:
    /// 1. Parse WGSL → naga Module
    /// 2. Validate → ModuleInfo
    /// 3. Create BGL + PipelineLayout (builds Metal argument index mapping)
    /// 4. Compile WGSL → MSL via naga (using pipeline layout's per_stage_map)
    /// 5. Create MTLLibrary → MTLFunction → MTLComputePipelineState
    /// 6. Package into hal ComputePipeline
    pub fn create_compute_pipeline(
        hal_ctx: &HalContext,
        wgsl_source: &str,
        entry_point: &str,
        bind_group_entries: &[wgpu::wgt::BindGroupLayoutEntry],
        label: &str,
    ) -> HalComputePipeline {
        // Step 1: Parse WGSL
        let module = naga::front::wgsl::parse_str(wgsl_source)
            .unwrap_or_else(|e| panic!("{label}: WGSL parse error: {e}"));

        // Step 2: Validate
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{label}: WGSL validation error: {e}"));

        // Step 3: Create hal BGL + PipelineLayout
        let bgl = unsafe {
            hal_ctx
                .device()
                .create_bind_group_layout(&hal::BindGroupLayoutDescriptor {
                    label: None,
                    flags: hal::BindGroupLayoutFlags::empty(),
                    entries: bind_group_entries,
                })
                .unwrap_or_else(|e| panic!("{label}: BGL creation error: {e:?}"))
        };

        let pipeline_layout = unsafe {
            hal_ctx
                .device()
                .create_pipeline_layout(&hal::PipelineLayoutDescriptor {
                    label: None,
                    flags: hal::PipelineLayoutFlags::empty(),
                    bind_group_layouts: &[&bgl],
                    immediate_size: 0,
                })
                .unwrap_or_else(|e| panic!("{label}: PipelineLayout creation error: {e:?}"))
        };

        // Step 4: Compile WGSL → MSL via naga
        // Must use the pipeline layout's per_stage_map for correct Metal argument indices.
        // The per_stage_map is built by create_pipeline_layout and contains the
        // binding → Metal buffer/texture/sampler index mapping.
        //
        // Access pipeline_layout.per_stage_map.cs for the compute stage resources.
        // This is a private field on hal::metal::PipelineLayout, so we need to use
        // hal's own create_compute_pipeline which calls load_shader internally.

        // Step 5-6: Create the pipeline through hal (which handles naga→MSL→MTL internally)
        let naga_shader = hal::NagaShader {
            module: std::borrow::Cow::Owned(module),
            info,
            debug_source: None,
        };
        let shader_module = hal::ShaderModuleDescriptor {
            label: Some(label),
            runtime_checks: wgpu::wgt::ShaderRuntimeChecks::unchecked(),
        };
        let hal_shader_module = unsafe {
            hal_ctx
                .device()
                .create_shader_module(
                    &shader_module,
                    hal::ShaderInput::Naga(naga_shader),
                )
                .unwrap_or_else(|e| panic!("{label}: ShaderModule creation error: {e}"))
        };

        let pipeline = unsafe {
            hal_ctx
                .device()
                .create_compute_pipeline(&hal::ComputePipelineDescriptor {
                    label: Some(label),
                    layout: &pipeline_layout,
                    stage: hal::ProgrammableStage {
                        module: &hal_shader_module,
                        entry_point,
                        constants: &Default::default(),
                        zero_initialize_workgroup_memory: true,
                    },
                    cache: None,
                })
                .unwrap_or_else(|e| panic!("{label}: ComputePipeline creation error: {e}"))
        };

        unsafe {
            hal_ctx.device().destroy_shader_module(hal_shader_module);
        }

        HalComputePipeline {
            pipeline,
            pipeline_layout,
            bind_group_layout: bgl,
        }
    }
}

#[cfg(all(target_os = "macos", feature = "hal-encoding"))]
pub use inner::*;
