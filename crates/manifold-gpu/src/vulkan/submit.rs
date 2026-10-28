//! Submission and lifetime (P1 step 12). Each `VkQueue` has one timeline
//! semaphore whose value is assigned under the queue's submit mutex, so
//! signals never go backwards (D8). Each queue has one waiter thread: it
//! waits for a submit's value, runs its completion handlers in order, drops
//! every resource the submit touched and recycles its command pool,
//! descriptor pools and uniform ring (D6, D3).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};

use ash::vk;

use super::core::Core;
use super::pipeline::PipelineCore;
use super::types::{BufferCore, SamplerCore, ViewCore};

/// Ring blocks hold uniform bytes and upload staging; they are this big
/// unless one write needs more (D3).
const RING_BLOCK: u64 = 256 << 10;
/// Ring allocations are aligned like Metal's `setBytes` replay data.
pub(crate) const RING_ALIGN: u64 = 256;
/// Sets per descriptor pool; a full pool chains to the next.
const SETS_PER_POOL: u32 = 512;

/// Something a submit must keep alive until it completes (D8): held, never
/// read.
#[allow(dead_code)]
pub(crate) enum Keep {
    Buffer(Arc<BufferCore>),
    View(Arc<ViewCore>),
    Sampler(Arc<SamplerCore>),
    Pipeline(Arc<PipelineCore>),
    Render(Arc<super::pipeline::RenderCore>),
    Event(Arc<super::types::EventCore>),
    Swapchain(Arc<super::surface::Swapchain>),
}

/// The per-submit resources an encoder records with, recycled once the
/// submit completes (D6).
pub(crate) struct FrameResources {
    pool: vk::CommandPool,
    cmds: Vec<vk::CommandBuffer>,
    used_cmds: usize,
    descriptor_pools: Vec<vk::DescriptorPool>,
    current_pool: usize,
    ring: Vec<super::GpuBuffer>,
    ring_block: usize,
    ring_offset: u64,
    /// Two timestamps: the encoder's start and its timed commit's end.
    pub(crate) queries: vk::QueryPool,
    pub(crate) keep: Vec<Keep>,
    /// Descriptor sets by set layout and bound records (D2).
    pub(crate) sets: HashMap<Box<[u64]>, vk::DescriptorSet>,
}

impl FrameResources {
    fn new(core: &Core) -> Self {
        let pool = unsafe {
            core.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(core.queue_family)
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT),
                None,
            )
        }
        .expect("Vulkan: command pool");
        let queries = unsafe {
            core.device.create_query_pool(
                &vk::QueryPoolCreateInfo::default()
                    .query_type(vk::QueryType::TIMESTAMP)
                    .query_count(2),
                None,
            )
        }
        .expect("Vulkan: query pool");
        Self {
            pool,
            cmds: Vec::new(),
            used_cmds: 0,
            descriptor_pools: Vec::new(),
            current_pool: 0,
            ring: Vec::new(),
            ring_block: 0,
            ring_offset: 0,
            queries,
            keep: Vec::new(),
            sets: HashMap::new(),
        }
    }

    /// A recycled bundle, or a new one.
    pub(crate) fn take(core: &Core) -> Self {
        core.frames
            .lock()
            .expect("frame resources poisoned")
            .pop()
            .unwrap_or_else(|| Self::new(core))
    }

    /// The next command buffer, begun.
    pub(crate) fn begin_cmd(&mut self, core: &Core) -> vk::CommandBuffer {
        if self.used_cmds == self.cmds.len() {
            let cmd = unsafe {
                core.device.allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(self.pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
            }
            .expect("Vulkan: command buffer")[0];
            self.cmds.push(cmd);
        }
        let cmd = self.cmds[self.used_cmds];
        self.used_cmds += 1;
        unsafe {
            core.device.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
        .expect("Vulkan: begin command buffer");
        cmd
    }

    fn new_descriptor_pool(core: &Core) -> vk::DescriptorPool {
        let sizes = [
            (vk::DescriptorType::UNIFORM_BUFFER, 2),
            (vk::DescriptorType::STORAGE_BUFFER, 6),
            (vk::DescriptorType::SAMPLED_IMAGE, 3),
            (vk::DescriptorType::STORAGE_IMAGE, 2),
            (vk::DescriptorType::SAMPLER, 2),
        ]
        .map(|(ty, per_set)| vk::DescriptorPoolSize {
            ty,
            descriptor_count: per_set * SETS_PER_POOL,
        });
        unsafe {
            core.device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .max_sets(SETS_PER_POOL)
                    .pool_sizes(&sizes),
                None,
            )
        }
        .expect("Vulkan: descriptor pool")
    }

    /// One descriptor set for `layout`, chaining pools as they fill.
    pub(crate) fn allocate_set(
        &mut self,
        core: &Core,
        layout: vk::DescriptorSetLayout,
    ) -> vk::DescriptorSet {
        let layouts = [layout];
        loop {
            if self.current_pool == self.descriptor_pools.len() {
                self.descriptor_pools.push(Self::new_descriptor_pool(core));
            }
            let pool = self.descriptor_pools[self.current_pool];
            let result = unsafe {
                core.device.allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(pool)
                        .set_layouts(&layouts),
                )
            };
            match result {
                Ok(sets) => return sets[0],
                Err(vk::Result::ERROR_OUT_OF_POOL_MEMORY | vk::Result::ERROR_FRAGMENTED_POOL) => {
                    self.current_pool += 1;
                }
                Err(e) => panic!("Vulkan: descriptor set: {e}"),
            }
        }
    }

    /// `len` bytes of host-visible ring memory: the buffer and the offset.
    pub(crate) fn ring_alloc(&mut self, core: &Arc<Core>, len: u64) -> (super::GpuBuffer, u64) {
        loop {
            if let Some(block) = self.ring.get(self.ring_block) {
                let offset = self.ring_offset.next_multiple_of(RING_ALIGN);
                if offset + len <= block.size {
                    self.ring_offset = offset + len;
                    return (block.clone(), offset);
                }
                self.ring_block += 1;
                self.ring_offset = 0;
                continue;
            }
            let size = RING_BLOCK.max(len.next_multiple_of(RING_ALIGN));
            log::debug!("[Vulkan] ring grew a {size}-byte block");
            let block = super::device::create_buffer_on(
                core,
                size,
                gpu_allocator::MemoryLocation::CpuToGpu,
            )
            .unwrap_or_else(|e| panic!("Vulkan: uniform ring {e}"));
            self.ring.push(block);
        }
    }

    /// Back to the pool unsubmitted: nothing on the GPU references it.
    pub(crate) fn recycle(mut self, core: &Core) {
        self.reset(&core.device);
        core.frames
            .lock()
            .expect("frame resources poisoned")
            .push(self);
    }

    /// Ready for reuse: pools reset, ring rewound, kept resources dropped.
    fn reset(&mut self, device: &ash::Device) {
        unsafe {
            device
                .reset_command_pool(self.pool, vk::CommandPoolResetFlags::empty())
                .expect("Vulkan: reset command pool");
            for &pool in &self.descriptor_pools {
                device
                    .reset_descriptor_pool(pool, vk::DescriptorPoolResetFlags::empty())
                    .expect("Vulkan: reset descriptor pool");
            }
        }
        self.used_cmds = 0;
        self.current_pool = 0;
        self.ring_block = 0;
        self.ring_offset = 0;
        self.keep.clear();
        self.sets.clear();
    }
}

/// One command buffer in a submit, with the semaphore values it waits for
/// before it starts and signals once it completes.
pub(crate) struct Batch {
    pub(crate) cmd: vk::CommandBuffer,
    pub(crate) waits: Vec<(vk::Semaphore, u64)>,
    pub(crate) signals: Vec<(vk::Semaphore, u64)>,
}

/// What the waiter does once a queue's timeline reaches `value`.
struct Retirement {
    value: u64,
    handlers: Vec<Box<dyn FnOnce() + Send>>,
    resources: Option<FrameResources>,
}

/// The highest value a queue's waiter has finished with.
struct Progress {
    value: Mutex<u64>,
    done: Condvar,
}

pub(crate) struct QueueState {
    pub(crate) raw: vk::Queue,
    /// The last timeline value a submit on this queue signals.
    last_value: u64,
}

/// One `VkQueue`, its timeline and its waiter thread. Wrappers that outnumber
/// the family's queues share one; the mutex serialises submission (D5).
pub(crate) struct Queue {
    pub(crate) state: Mutex<QueueState>,
    pub(crate) timeline: vk::Semaphore,
    retire: Mutex<Sender<Retirement>>,
    progress: Arc<Progress>,
}

/// Set once any call reports `VK_ERROR_DEVICE_LOST` (D19).
static DEVICE_LOST: AtomicBool = AtomicBool::new(false);

/// Whether the device has been lost; every later submit fails too.
pub(crate) fn is_device_lost() -> bool {
    DEVICE_LOST.load(Ordering::Acquire)
}

/// Record a lost device once, with what noticed it.
pub(crate) fn device_lost(context: &str) {
    if !DEVICE_LOST.swap(true, Ordering::AcqRel) {
        log::error!("[Vulkan] device lost ({context})");
        crate::gpu_fault::record_fault(crate::gpu_fault::FaultKind::DeviceLost);
    }
}

fn semaphore_infos(list: &[(vk::Semaphore, u64)]) -> Vec<vk::SemaphoreSubmitInfo<'static>> {
    list.iter()
        .map(|&(semaphore, value)| {
            vk::SemaphoreSubmitInfo::default()
                .semaphore(semaphore)
                .value(value)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        })
        .collect()
}

fn wait_value(
    device: &ash::Device,
    timeline: vk::Semaphore,
    value: u64,
    timeout_ns: u64,
) -> vk::Result {
    let semaphores = [timeline];
    let values = [value];
    let info = vk::SemaphoreWaitInfo::default()
        .semaphores(&semaphores)
        .values(&values);
    match unsafe { device.wait_semaphores(&info, timeout_ns) } {
        Ok(()) => vk::Result::SUCCESS,
        Err(e) => e,
    }
}

/// Block until `timeline` reaches `value`; `false` on device loss.
pub(crate) fn wait_timeline(device: &ash::Device, timeline: vk::Semaphore, value: u64) -> bool {
    loop {
        match wait_value(device, timeline, value, 1_000_000_000) {
            vk::Result::SUCCESS => return true,
            vk::Result::TIMEOUT => continue,
            vk::Result::ERROR_DEVICE_LOST => {
                device_lost("timeline wait");
                return false;
            }
            e => panic!("Vulkan: wait for timeline: {e}"),
        }
    }
}

fn run_waiter(
    device: ash::Device,
    timeline: vk::Semaphore,
    retirements: Receiver<Retirement>,
    progress: Arc<Progress>,
    frames: Arc<Mutex<Vec<FrameResources>>>,
) {
    for retirement in retirements {
        wait_timeline(&device, timeline, retirement.value);
        for handler in retirement.handlers {
            handler();
        }
        if let Some(mut resources) = retirement.resources {
            resources.reset(&device);
            frames
                .lock()
                .expect("frame resources poisoned")
                .push(resources);
        }
        let mut value = progress.value.lock().expect("queue progress poisoned");
        *value = (*value).max(retirement.value);
        progress.done.notify_all();
    }
}

impl Queue {
    pub(crate) fn new(
        device: &ash::Device,
        raw: vk::Queue,
        frames: Arc<Mutex<Vec<FrameResources>>>,
        index: u32,
    ) -> Self {
        let mut timeline_info = vk::SemaphoreTypeCreateInfo::default()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0);
        let timeline = unsafe {
            device.create_semaphore(
                &vk::SemaphoreCreateInfo::default().push_next(&mut timeline_info),
                None,
            )
        }
        .expect("Vulkan: queue timeline");
        let (sender, receiver) = channel();
        let progress = Arc::new(Progress {
            value: Mutex::new(0),
            done: Condvar::new(),
        });
        let waiter_progress = progress.clone();
        let waiter_device = device.clone();
        std::thread::Builder::new()
            .name(format!("vk-queue-{index}-waiter"))
            .spawn(move || run_waiter(waiter_device, timeline, receiver, waiter_progress, frames))
            .expect("spawn queue waiter");
        Self {
            state: Mutex::new(QueueState { raw, last_value: 0 }),
            timeline,
            retire: Mutex::new(sender),
            progress,
        }
    }

    /// Hand a value's handlers and resources to the waiter. Callers hold the
    /// state lock, so the waiter sees values in submission order and a
    /// finished value means every handler up to it ran.
    fn retire(&self, retirement: Retirement) {
        // The waiter only stops with the process.
        let _ = self
            .retire
            .lock()
            .expect("queue retire poisoned")
            .send(retirement);
    }

    /// Submit `batches` in order, the last signalling the queue's next
    /// timeline value, and hand `handlers` and `resources` to the waiter for
    /// that value. A successful submit runs `after` on the queue while it is
    /// still held (presents). A failed submit still retires them, after the
    /// queue's earlier work, and reports the error.
    pub(crate) fn submit(
        &self,
        device: &ash::Device,
        batches: &[Batch],
        handlers: Vec<Box<dyn FnOnce() + Send>>,
        resources: FrameResources,
        after: impl FnOnce(vk::Queue),
    ) -> Result<u64, String> {
        let mut state = self.state.lock().expect("queue poisoned");
        let value = state.last_value + 1;
        let cmds: Vec<[vk::CommandBufferSubmitInfo; 1]> = batches
            .iter()
            .map(|b| [vk::CommandBufferSubmitInfo::default().command_buffer(b.cmd)])
            .collect();
        let waits: Vec<_> = batches.iter().map(|b| semaphore_infos(&b.waits)).collect();
        let mut signals: Vec<_> = batches
            .iter()
            .map(|b| semaphore_infos(&b.signals))
            .collect();
        signals
            .last_mut()
            .expect("a submit has a batch")
            .extend(semaphore_infos(&[(self.timeline, value)]));
        let submits: Vec<vk::SubmitInfo2> = (0..batches.len())
            .map(|i| {
                vk::SubmitInfo2::default()
                    .wait_semaphore_infos(&waits[i])
                    .command_buffer_infos(&cmds[i])
                    .signal_semaphore_infos(&signals[i])
            })
            .collect();
        let result = unsafe { device.queue_submit2(state.raw, &submits, vk::Fence::null()) };
        let (retire_at, outcome) = match result {
            Ok(()) => {
                state.last_value = value;
                after(state.raw);
                (value, Ok(value))
            }
            Err(e) => {
                if e == vk::Result::ERROR_DEVICE_LOST {
                    device_lost("queue submit");
                } else {
                    crate::gpu_fault::record_fault(crate::gpu_fault::FaultKind::Other);
                }
                (
                    state.last_value,
                    Err(format!("[Vulkan] queue submit failed: {e}")),
                )
            }
        };
        self.retire(Retirement {
            value: retire_at,
            handlers,
            resources: Some(resources),
        });
        outcome
    }

    /// Run `handler` once everything submitted on this queue so far is done.
    pub(crate) fn after_submitted(&self, handler: Box<dyn FnOnce() + Send>) {
        let state = self.state.lock().expect("queue poisoned");
        self.retire(Retirement {
            value: state.last_value,
            handlers: vec![handler],
            resources: None,
        });
    }

    /// Block until the waiter has finished with `value`: its handlers ran
    /// and its resources dropped.
    pub(crate) fn wait_retired(&self, value: u64) {
        let mut done = self.progress.value.lock().expect("queue progress poisoned");
        while *done < value {
            done = self
                .progress
                .done
                .wait(done)
                .expect("queue progress poisoned");
        }
    }
}
