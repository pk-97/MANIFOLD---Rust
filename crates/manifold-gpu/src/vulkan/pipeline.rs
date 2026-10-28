//! Compute and render pipelines from SPIR-V: one descriptor set layout per
//! pipeline from its `SlotMap`, written through an update template (D2),
//! and the three cache layers Metal has (D11): in memory per wrapper, a
//! `VkPipelineCache` blob on disk, and a SPIR-V disk cache that skips naga
//! and `spirv-opt`. A render pipeline builds one `VkPipeline` per topology
//! and fill mode, the first at creation and any other on first use;
//! viewport, scissor, culling, front face and depth state are dynamic, as
//! they are encoder state on Metal.

use std::collections::hash_map::DefaultHasher;
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ash::vk;

use super::core::{Core, vk_format};
use super::{Slot, SlotKind, SlotMap};
use crate::{
    GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuCompareFunction, GpuPrimitiveType,
    GpuTriangleFillMode, GpuVertexFormat, GpuVertexLayout, RenderPipelineDesc,
};

/// Bytes per descriptor record in update-template data: a
/// `VkDescriptorBufferInfo` and a `VkDescriptorImageInfo` are both 24.
pub(crate) const RECORD: usize = 24;
const _: () = assert!(std::mem::size_of::<vk::DescriptorBufferInfo>() == RECORD);
const _: () = assert!(std::mem::size_of::<vk::DescriptorImageInfo>() == RECORD);

pub(crate) fn descriptor_type(kind: SlotKind) -> vk::DescriptorType {
    match kind {
        SlotKind::UniformBuffer => vk::DescriptorType::UNIFORM_BUFFER,
        SlotKind::StorageBuffer => vk::DescriptorType::STORAGE_BUFFER,
        SlotKind::SampledImage => vk::DescriptorType::SAMPLED_IMAGE,
        SlotKind::StorageImage => vk::DescriptorType::STORAGE_IMAGE,
        SlotKind::Sampler => vk::DescriptorType::SAMPLER,
    }
}

/// A pipeline's layout, set layout and update template.
pub(crate) struct Layouts {
    pub(crate) core: Arc<Core>,
    pub(crate) layout: vk::PipelineLayout,
    pub(crate) set_layout: vk::DescriptorSetLayout,
    pub(crate) template: vk::DescriptorUpdateTemplate,
    /// The slots in template order; record `i` describes `slots[i]`.
    pub(crate) slots: Vec<(u32, Slot)>,
}

impl Drop for Layouts {
    fn drop(&mut self) {
        let device = &self.core.device;
        unsafe {
            device.destroy_descriptor_update_template(self.template, None);
            device.destroy_pipeline_layout(self.layout, None);
            device.destroy_descriptor_set_layout(self.set_layout, None);
        }
    }
}

/// A compute `VkPipeline` with its layouts.
pub(crate) struct PipelineCore {
    pub(crate) raw: vk::Pipeline,
    pub(crate) layouts: Layouts,
}

impl PipelineCore {
    /// The device's workgroup invocation limit.
    pub(crate) fn max_invocations(&self) -> u32 {
        self.layouts.core.limits.max_compute_work_group_invocations
    }
}

impl Drop for PipelineCore {
    fn drop(&mut self) {
        unsafe { self.layouts.core.device.destroy_pipeline(self.raw, None) };
    }
}

/// The shader translation identity the caches key on.
pub(crate) fn compute_hash(wgsl_source: &str, entry_point: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    "vk-compute-v1".hash(&mut hasher);
    wgsl_source.hash(&mut hasher);
    entry_point.hash(&mut hasher);
    hasher.finish()
}

/// Layout, set layout and template for `slot_map`'s bindings, seen by
/// `stages`.
fn build_layouts(
    core: &Arc<Core>,
    slot_map: &SlotMap,
    stages: vk::ShaderStageFlags,
    label: &str,
) -> Layouts {
    let slots: Vec<(u32, Slot)> = slot_map
        .bindings()
        .map(|b| (b, *slot_map.get(b).expect("bindings() yields mapped slots")))
        .collect();
    let bindings: Vec<vk::DescriptorSetLayoutBinding> = slots
        .iter()
        .map(|(_, slot)| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(slot.vulkan_binding)
                .descriptor_type(descriptor_type(slot.kind))
                .descriptor_count(1)
                .stage_flags(stages)
        })
        .collect();
    let set_layout = unsafe {
        core.device.create_descriptor_set_layout(
            &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
            None,
        )
    }
    .unwrap_or_else(|e| panic!("{label}: descriptor set layout: {e}"));
    let set_layouts = [set_layout];
    let layout = unsafe {
        core.device.create_pipeline_layout(
            &vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts),
            None,
        )
    }
    .unwrap_or_else(|e| panic!("{label}: pipeline layout: {e}"));
    let entries: Vec<vk::DescriptorUpdateTemplateEntry> = slots
        .iter()
        .enumerate()
        .map(|(i, (_, slot))| vk::DescriptorUpdateTemplateEntry {
            dst_binding: slot.vulkan_binding,
            dst_array_element: 0,
            descriptor_count: 1,
            descriptor_type: descriptor_type(slot.kind),
            offset: i * RECORD,
            stride: RECORD,
        })
        .collect();
    let bind_point = if stages == vk::ShaderStageFlags::COMPUTE {
        vk::PipelineBindPoint::COMPUTE
    } else {
        vk::PipelineBindPoint::GRAPHICS
    };
    // A template with no entries is invalid; such pipelines bind no set.
    let template = if entries.is_empty() {
        vk::DescriptorUpdateTemplate::null()
    } else {
        unsafe {
            core.device.create_descriptor_update_template(
                &vk::DescriptorUpdateTemplateCreateInfo::default()
                    .descriptor_update_entries(&entries)
                    .template_type(vk::DescriptorUpdateTemplateType::DESCRIPTOR_SET)
                    .descriptor_set_layout(set_layout)
                    .pipeline_bind_point(bind_point)
                    .pipeline_layout(layout)
                    .set(0),
                None,
            )
        }
        .unwrap_or_else(|e| panic!("{label}: descriptor update template: {e}"))
    };
    Layouts {
        core: core.clone(),
        layout,
        set_layout,
        template,
        slots,
    }
}

/// Build the compute `VkPipeline` for compiled SPIR-V.
pub(crate) fn create_compute(
    core: &Arc<Core>,
    cache: vk::PipelineCache,
    slot_map: &SlotMap,
    spirv: &[u32],
    entry_point: &str,
    label: &str,
) -> Arc<PipelineCore> {
    let module = unsafe {
        core.device
            .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(spirv), None)
    }
    .unwrap_or_else(|e| panic!("{label}: shader module: {e}"));
    let layouts = build_layouts(core, slot_map, vk::ShaderStageFlags::COMPUTE, label);
    let entry = std::ffi::CString::new(entry_point).expect("entry point has no NUL");
    let stage = vk::PipelineShaderStageCreateInfo::default()
        .stage(vk::ShaderStageFlags::COMPUTE)
        .module(module)
        .name(&entry);
    let info = [vk::ComputePipelineCreateInfo::default()
        .stage(stage)
        .layout(layouts.layout)];
    let pipelines = unsafe { core.device.create_compute_pipelines(cache, &info, None) };
    unsafe { core.device.destroy_shader_module(module, None) };
    let raw = match pipelines {
        Ok(p) => p[0],
        Err((_, e)) => panic!("{label}: compute pipeline: {e}"),
    };
    core.name(raw, label);
    Arc::new(PipelineCore { raw, layouts })
}

/// What a `VkPipeline` bakes in that a Metal draw chooses per draw.
pub(crate) type Variant = (GpuPrimitiveType, GpuTriangleFillMode);

const DYNAMIC: [vk::DynamicState; 9] = [
    vk::DynamicState::VIEWPORT,
    vk::DynamicState::SCISSOR,
    vk::DynamicState::CULL_MODE,
    vk::DynamicState::FRONT_FACE,
    vk::DynamicState::DEPTH_TEST_ENABLE,
    vk::DynamicState::DEPTH_WRITE_ENABLE,
    vk::DynamicState::DEPTH_COMPARE_OP,
    vk::DynamicState::DEPTH_BIAS,
    vk::DynamicState::DEPTH_BIAS_ENABLE,
];

pub(crate) struct RenderCore {
    pub(crate) layouts: Layouts,
    module: vk::ShaderModule,
    vs_entry: CString,
    fs_entry: CString,
    /// The colour attachment, then the auxiliary ones.
    color_formats: Vec<vk::Format>,
    blend: Option<GpuBlendState>,
    depth_format: vk::Format,
    samples: vk::SampleCountFlags,
    alpha_to_coverage: bool,
    vertex_layout: Option<GpuVertexLayout>,
    label: String,
    variants: Mutex<Vec<(Variant, vk::Pipeline)>>,
}

impl Drop for RenderCore {
    fn drop(&mut self) {
        let device = &self.layouts.core.device;
        let variants = self.variants.get_mut().expect("render variants poisoned");
        unsafe {
            for &(_, raw) in variants.iter() {
                device.destroy_pipeline(raw, None);
            }
            device.destroy_shader_module(self.module, None);
        }
    }
}

fn blend_factor(factor: GpuBlendFactor) -> vk::BlendFactor {
    match factor {
        GpuBlendFactor::Zero => vk::BlendFactor::ZERO,
        GpuBlendFactor::One => vk::BlendFactor::ONE,
        GpuBlendFactor::SrcAlpha => vk::BlendFactor::SRC_ALPHA,
        GpuBlendFactor::OneMinusSrcAlpha => vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        GpuBlendFactor::DstAlpha => vk::BlendFactor::DST_ALPHA,
        GpuBlendFactor::OneMinusDstAlpha => vk::BlendFactor::ONE_MINUS_DST_ALPHA,
        GpuBlendFactor::SrcColor => vk::BlendFactor::SRC_COLOR,
        GpuBlendFactor::OneMinusSrcColor => vk::BlendFactor::ONE_MINUS_SRC_COLOR,
        GpuBlendFactor::DstColor => vk::BlendFactor::DST_COLOR,
        GpuBlendFactor::OneMinusDstColor => vk::BlendFactor::ONE_MINUS_DST_COLOR,
    }
}

fn blend_op(op: GpuBlendOp) -> vk::BlendOp {
    match op {
        GpuBlendOp::Add => vk::BlendOp::ADD,
        GpuBlendOp::Subtract => vk::BlendOp::SUBTRACT,
        GpuBlendOp::ReverseSubtract => vk::BlendOp::REVERSE_SUBTRACT,
        GpuBlendOp::Min => vk::BlendOp::MIN,
        GpuBlendOp::Max => vk::BlendOp::MAX,
    }
}

fn vertex_format(format: GpuVertexFormat) -> vk::Format {
    match format {
        GpuVertexFormat::Float32 => vk::Format::R32_SFLOAT,
        GpuVertexFormat::Float32x2 => vk::Format::R32G32_SFLOAT,
        GpuVertexFormat::Float32x3 => vk::Format::R32G32B32_SFLOAT,
        GpuVertexFormat::Float32x4 => vk::Format::R32G32B32A32_SFLOAT,
        GpuVertexFormat::Uint32 => vk::Format::R32_UINT,
        GpuVertexFormat::Uint8x4 => vk::Format::R8G8B8A8_UINT,
    }
}

pub(crate) fn compare_op(compare: GpuCompareFunction) -> vk::CompareOp {
    match compare {
        GpuCompareFunction::Never => vk::CompareOp::NEVER,
        GpuCompareFunction::Less => vk::CompareOp::LESS,
        GpuCompareFunction::Equal => vk::CompareOp::EQUAL,
        GpuCompareFunction::LessEqual => vk::CompareOp::LESS_OR_EQUAL,
        GpuCompareFunction::Greater => vk::CompareOp::GREATER,
        GpuCompareFunction::NotEqual => vk::CompareOp::NOT_EQUAL,
        GpuCompareFunction::GreaterEqual => vk::CompareOp::GREATER_OR_EQUAL,
        GpuCompareFunction::Always => vk::CompareOp::ALWAYS,
    }
}

impl RenderCore {
    /// The `VkPipeline` for `variant`, built on first use.
    pub(crate) fn pipeline(&self, variant: Variant) -> vk::Pipeline {
        let mut variants = self.variants.lock().expect("render variants poisoned");
        if let Some(&(_, raw)) = variants.iter().find(|(v, _)| *v == variant) {
            return raw;
        }
        let raw = self.build(variant, vk::PipelineCache::null());
        variants.push((variant, raw));
        raw
    }

    fn build(&self, (primitive, fill): Variant, cache: vk::PipelineCache) -> vk::Pipeline {
        let core = &self.layouts.core;
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(self.module)
                .name(&self.vs_entry),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(self.module)
                .name(&self.fs_entry),
        ];
        let buffers: Vec<vk::VertexInputBindingDescription> = self
            .vertex_layout
            .iter()
            .map(|layout| vk::VertexInputBindingDescription {
                binding: 0,
                stride: layout.stride,
                input_rate: vk::VertexInputRate::VERTEX,
            })
            .collect();
        let attributes: Vec<vk::VertexInputAttributeDescription> = self
            .vertex_layout
            .iter()
            .flat_map(|layout| &layout.attributes)
            .map(|a| vk::VertexInputAttributeDescription {
                location: a.shader_location,
                binding: 0,
                format: vertex_format(a.format),
                offset: a.offset,
            })
            .collect();
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&buffers)
            .vertex_attribute_descriptions(&attributes);
        let input_assembly =
            vk::PipelineInputAssemblyStateCreateInfo::default().topology(match primitive {
                GpuPrimitiveType::Triangle => vk::PrimitiveTopology::TRIANGLE_LIST,
                GpuPrimitiveType::Line => vk::PrimitiveTopology::LINE_LIST,
                GpuPrimitiveType::Point => vk::PrimitiveTopology::POINT_LIST,
            });
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(match fill {
                GpuTriangleFillMode::Fill => vk::PolygonMode::FILL,
                GpuTriangleFillMode::Lines => vk::PolygonMode::LINE,
            })
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(self.samples)
            .alpha_to_coverage_enable(self.alpha_to_coverage);
        let depth = vk::PipelineDepthStencilStateCreateInfo::default();
        // Auxiliary targets write raw values: only the colour target blends.
        let attachments: Vec<vk::PipelineColorBlendAttachmentState> = (0..self.color_formats.len())
            .map(|i| {
                let state = vk::PipelineColorBlendAttachmentState::default()
                    .color_write_mask(vk::ColorComponentFlags::RGBA);
                match self.blend.filter(|_| i == 0) {
                    Some(b) => state
                        .blend_enable(true)
                        .src_color_blend_factor(blend_factor(b.src_factor))
                        .dst_color_blend_factor(blend_factor(b.dst_factor))
                        .color_blend_op(blend_op(b.operation))
                        .src_alpha_blend_factor(blend_factor(b.src_alpha_factor))
                        .dst_alpha_blend_factor(blend_factor(b.dst_alpha_factor))
                        .alpha_blend_op(blend_op(b.alpha_operation)),
                    None => state,
                }
            })
            .collect();
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&DYNAMIC);
        let mut rendering = vk::PipelineRenderingCreateInfo::default()
            .color_attachment_formats(&self.color_formats)
            .depth_attachment_format(self.depth_format);
        let info = [vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .depth_stencil_state(&depth)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(self.layouts.layout)
            .push_next(&mut rendering)];
        let raw = match unsafe { core.device.create_graphics_pipelines(cache, &info, None) } {
            Ok(p) => p[0],
            Err((_, e)) => panic!("{}: render pipeline: {e}", self.label),
        };
        core.name(raw, &self.label);
        raw
    }
}

/// Build the render pipeline for compiled SPIR-V, with the variant `desc`
/// implies (points when it has a point size, else filled triangles).
pub(crate) fn create_render(
    core: &Arc<Core>,
    cache: vk::PipelineCache,
    slot_map: &SlotMap,
    spirv: &[u32],
    desc: &RenderPipelineDesc,
) -> Arc<RenderCore> {
    let label = desc.label;
    let module = unsafe {
        core.device
            .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(spirv), None)
    }
    .unwrap_or_else(|e| panic!("{label}: shader module: {e}"));
    let layouts = build_layouts(
        core,
        slot_map,
        vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
        label,
    );
    let mut render = RenderCore {
        layouts,
        module,
        vs_entry: CString::new(desc.vs_entry).expect("entry point has no NUL"),
        fs_entry: CString::new(desc.fs_entry).expect("entry point has no NUL"),
        color_formats: desc
            .color_format
            .iter()
            .chain(desc.aux_color_formats)
            .map(|&f| vk_format(f))
            .collect(),
        blend: desc.blend,
        depth_format: desc.depth_format.map_or(vk::Format::UNDEFINED, vk_format),
        samples: vk::SampleCountFlags::from_raw(desc.sample_count.max(1)),
        alpha_to_coverage: desc.alpha_to_coverage,
        vertex_layout: desc.vertex_layout.cloned(),
        label: label.to_string(),
        variants: Mutex::new(Vec::new()),
    };
    let first = if desc.point_size_location.is_some() {
        GpuPrimitiveType::Point
    } else {
        GpuPrimitiveType::Triangle
    };
    let raw = render.build((first, GpuTriangleFillMode::Fill), cache);
    render
        .variants
        .get_mut()
        .expect("render variants poisoned")
        .push(((first, GpuTriangleFillMode::Fill), raw));
    Arc::new(render)
}

/// The `VkPipelineCache` blob on disk beside where Metal keeps its binary
/// archive.
pub(crate) struct DiskPipelineCache {
    pub(crate) raw: vk::PipelineCache,
    path: PathBuf,
}

impl DiskPipelineCache {
    pub(crate) fn load(core: &Core, archive_path: &Path) -> Self {
        let path = archive_path.with_extension("vkpipelinecache");
        let data = std::fs::read(&path).unwrap_or_default();
        let create = |data: &[u8]| unsafe {
            core.device.create_pipeline_cache(
                &vk::PipelineCacheCreateInfo::default().initial_data(data),
                None,
            )
        };
        // The driver rejects a blob from another driver or version.
        let raw = create(&data)
            .or_else(|_| create(&[]))
            .unwrap_or_else(|e| panic!("Vulkan: pipeline cache: {e}"));
        Self { raw, path }
    }

    pub(crate) fn save(&self, core: &Core) {
        let data = match unsafe { core.device.get_pipeline_cache_data(self.raw) } {
            Ok(data) => data,
            Err(e) => {
                log::warn!("[Vulkan] pipeline cache data: {e}");
                return;
            }
        };
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&self.path, data) {
            log::warn!(
                "[Vulkan] pipeline cache save to {}: {e}",
                self.path.display()
            );
        }
    }
}

/// One compiled compute shader on disk: the slot map, workgroup size and
/// SPIR-V words.
pub(crate) struct SpirvCache {
    dir: PathBuf,
    hits: std::sync::atomic::AtomicU64,
    misses: std::sync::atomic::AtomicU64,
}

const SPIRV_CACHE_MAGIC: u32 = 0x4d53_5631; // "MSV1"

fn kind_code(kind: SlotKind) -> u32 {
    match kind {
        SlotKind::UniformBuffer => 0,
        SlotKind::StorageBuffer => 1,
        SlotKind::SampledImage => 2,
        SlotKind::StorageImage => 3,
        SlotKind::Sampler => 4,
    }
}

fn kind_from(code: u32) -> Option<SlotKind> {
    Some(match code {
        0 => SlotKind::UniformBuffer,
        1 => SlotKind::StorageBuffer,
        2 => SlotKind::SampledImage,
        3 => SlotKind::StorageImage,
        4 => SlotKind::Sampler,
        _ => return None,
    })
}

impl SpirvCache {
    pub(crate) fn new(dir: &Path) -> Self {
        Self {
            dir: dir.join("spirv"),
            hits: Default::default(),
            misses: Default::default(),
        }
    }

    fn path(&self, hash: u64) -> PathBuf {
        self.dir.join(format!("{hash:016x}.spv"))
    }

    pub(crate) fn get(&self, hash: u64) -> Option<(SlotMap, Vec<u32>, [u32; 3])> {
        let found = std::fs::read(self.path(hash))
            .ok()
            .and_then(|bytes| Self::decode(&bytes));
        let counter = if found.is_some() {
            &self.hits
        } else {
            &self.misses
        };
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        found
    }

    fn decode(bytes: &[u8]) -> Option<(SlotMap, Vec<u32>, [u32; 3])> {
        if !bytes.len().is_multiple_of(4) {
            return None;
        }
        let words: Vec<u32> = bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let mut it = words.iter().copied();
        if it.next()? != SPIRV_CACHE_MAGIC {
            return None;
        }
        let workgroup = [it.next()?, it.next()?, it.next()?];
        let slot_count = it.next()?;
        let mut slot_map = SlotMap::new();
        for _ in 0..slot_count {
            let binding = it.next()?;
            let kind = kind_from(it.next()?)?;
            let writable = it.next()? != 0;
            slot_map.insert(
                binding,
                Slot {
                    kind,
                    vulkan_binding: binding,
                    writable,
                },
            );
        }
        let spirv: Vec<u32> = it.collect();
        (!spirv.is_empty()).then_some((slot_map, spirv, workgroup))
    }

    pub(crate) fn put(&self, hash: u64, slot_map: &SlotMap, spirv: &[u32], workgroup: [u32; 3]) {
        let mut words = vec![SPIRV_CACHE_MAGIC, workgroup[0], workgroup[1], workgroup[2]];
        let slots: Vec<u32> = slot_map.bindings().collect();
        words.push(slots.len() as u32);
        for binding in slots {
            let slot = slot_map
                .get(binding)
                .expect("bindings() yields mapped slots");
            words.extend([binding, kind_code(slot.kind), u32::from(slot.writable)]);
        }
        words.extend_from_slice(spirv);
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        if std::fs::create_dir_all(&self.dir).is_ok() {
            // Write then rename, so a reader never sees half a file.
            let path = self.path(hash);
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }

    pub(crate) fn log_stats(&self) {
        log::info!(
            "[Vulkan] SPIR-V cache: {} hits, {} misses",
            self.hits.load(std::sync::atomic::Ordering::Relaxed),
            self.misses.load(std::sync::atomic::Ordering::Relaxed)
        );
    }
}
