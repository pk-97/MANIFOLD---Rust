//! Compute pipelines from SPIR-V (P1 step 10): one descriptor set layout per
//! pipeline from its `SlotMap`, written through an update template (D2),
//! and the three cache layers Metal has (D11): in memory per wrapper, a
//! `VkPipelineCache` blob on disk, and a SPIR-V disk cache that skips naga
//! and `spirv-opt`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ash::vk;

use super::core::Core;
use super::{Slot, SlotKind, SlotMap};

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

/// A `VkPipeline` with its layout, set layout and update template.
pub(crate) struct PipelineCore {
    core: Arc<Core>,
    pub(crate) raw: vk::Pipeline,
    pub(crate) layout: vk::PipelineLayout,
    pub(crate) set_layout: vk::DescriptorSetLayout,
    pub(crate) template: vk::DescriptorUpdateTemplate,
    /// The slots in template order; record `i` describes `slots[i]`.
    pub(crate) slots: Vec<(u32, Slot)>,
}

impl PipelineCore {
    /// The device's workgroup invocation limit.
    pub(crate) fn max_invocations(&self) -> u32 {
        self.core.limits.max_compute_work_group_invocations
    }
}

impl Drop for PipelineCore {
    fn drop(&mut self) {
        let device = &self.core.device;
        unsafe {
            device.destroy_pipeline(self.raw, None);
            device.destroy_descriptor_update_template(self.template, None);
            device.destroy_pipeline_layout(self.layout, None);
            device.destroy_descriptor_set_layout(self.set_layout, None);
        }
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
    core: &Core,
    slot_map: &SlotMap,
    stages: vk::ShaderStageFlags,
    label: &str,
) -> (
    vk::DescriptorSetLayout,
    vk::PipelineLayout,
    vk::DescriptorUpdateTemplate,
    Vec<(u32, Slot)>,
) {
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
    (set_layout, layout, template, slots)
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
    let (set_layout, layout, template, slots) =
        build_layouts(core, slot_map, vk::ShaderStageFlags::COMPUTE, label);
    let entry = std::ffi::CString::new(entry_point).expect("entry point has no NUL");
    let stage = vk::PipelineShaderStageCreateInfo::default()
        .stage(vk::ShaderStageFlags::COMPUTE)
        .module(module)
        .name(&entry);
    let info = [vk::ComputePipelineCreateInfo::default()
        .stage(stage)
        .layout(layout)];
    let pipelines = unsafe { core.device.create_compute_pipelines(cache, &info, None) };
    unsafe { core.device.destroy_shader_module(module, None) };
    let raw = match pipelines {
        Ok(p) => p[0],
        Err((_, e)) => panic!("{label}: compute pipeline: {e}"),
    };
    core.name(raw, label);
    Arc::new(PipelineCore {
        core: core.clone(),
        raw,
        layout,
        set_layout,
        template,
        slots,
    })
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
