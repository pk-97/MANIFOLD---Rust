//! Gated round templates (docs/GPU_FLIP_PRESSURE_CAP_DESIGN.md section 3.2
//! (The template on every path)): one round of gated dispatches, recorded
//! once into an indirect command buffer that holds it `chunk` times back to
//! back, and run by chunked executes whose lengths the GPU writes. The store
//! is the caller's, apart from frame replay, so every encoder path (a
//! replaying span, direct, profiled, diagnostic, a busy ring) runs the same
//! executes and the CPU never encodes a round per round.
//!
//! A visit is two calls: `prepare_template` walks the body once into a
//! provisional round, validates it whole and finds or builds a slot,
//! encoding nothing; `execute_template` runs the slot where the rounds go.
//! A failed prepare leaves every slot as it was.

use std::ptr::NonNull;

use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBarrierScope, MTLBuffer, MTLCommandBuffer, MTLCommandBufferStatus, MTLCommandEncoder,
    MTLComputeCommandEncoder, MTLComputePipelineState, MTLDevice, MTLIndirectCommandBuffer,
    MTLIndirectCommandBufferDescriptor, MTLIndirectCommandType, MTLIndirectComputeCommand, MTLResource,
    MTLResourceOptions, MTLResourceUsage, MTLSize,
};

use super::SIZES_BUFFER_BINDING;
use super::device::GpuDevice;
use super::encoder::{EncoderState, GpuEncoder, MAX_BUFFER_SLOTS, collect_buffer_sizes};
use super::types::{GpuBuffer, GpuComputePipeline};
use crate::replay::{BYTES_ALIGN, GATED_RANGE_BYTES, MAX_KEY_BINDINGS, template_chunks};
use crate::types::GpuBinding;

/// Kernel buffer slots an indirect compute command can set (Metal's limit).
const ICB_BUFFER_SLOTS: usize = 31;

/// The most commands one replicated template buffer may hold: `commands`
/// × `chunk`, and each execute's length is a u32 range entry.
pub(crate) const MAX_TEMPLATE_COMMANDS: u32 = 1 << 16;

/// Idle slots kept after a lookup; more are freed.
const IDLE_SLOTS_KEPT: usize = 4;

/// Where a template's rounds run: grouped into executes of up to `chunk`
/// rounds (`crate::template_chunks`), execute j by range entry
/// `first + j * stride` of `ranges` (`GATED_RANGE_BYTES` per entry), which
/// the GPU writes as {0, rounds · commands} while live and {0, 0} once the
/// work is done. Rounds of an execute that run after the GPU decided to
/// stop must write nothing: the caller's kernels guard themselves.
#[derive(Clone, Copy)]
pub struct TemplateRanges<'a> {
    pub ranges: &'a GpuBuffer,
    pub first: u32,
    pub stride: u32,
    pub chunk: u32,
}

/// What a template store did, summed over its life.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuTemplateStats {
    /// Body walks: one per prepare that ran its body.
    pub walks: u64,
    /// Prepares that found a slot with an equal key.
    pub hits: u64,
    /// Slots built (cold, or a changed key).
    pub builds: u64,
    /// Indirect commands written, every replica counted.
    pub command_writes: u64,
    /// Slots appended because no idle slot was free.
    pub grown: u64,
    /// Execute calls.
    pub executes: u64,
    /// Prepares refused (`Err`), nothing touched.
    pub refused: u64,
}

/// What a template body can do: issue gated dispatches into the store's
/// provisional round, nothing else.
pub struct GatedRecorder<'s> {
    round: &'s mut Round,
}

impl GatedRecorder<'_> {
    /// One gated dispatch of the round: `groups` are its threadgroup counts
    /// when its round runs. `_gate` and `_gate_offset` are the dispatch's
    /// per-round indirect arguments, unused here: a template's rounds are
    /// gated by their execute ranges, and its kernels guard rounds past the
    /// stop themselves.
    pub fn dispatch_gated(
        &mut self,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        groups: [u32; 3],
        _gate: &GpuBuffer,
        _gate_offset: u64,
        label: &str,
    ) {
        self.round.push(pipeline, bindings, groups, label);
    }
}

enum RoundBinding {
    Buffer { slot: u8, buffer: Retained<ProtocolObject<dyn MTLBuffer>>, offset: u64 },
    /// Inline bytes at `start..start + len` of the round's byte vector.
    Bytes { slot: u8, start: u32, len: u32 },
}

struct RoundCommand {
    pipeline: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    workgroup: [u32; 3],
    groups: [u32; 3],
    /// This command's bindings: `bindings[first..first + count]` of the round.
    first: u32,
    count: u32,
}

/// One walked round: its commands, their bindings and inline bytes (flat,
/// so a warm walk reuses every vector's capacity), and why it can't be a
/// template (the first refusal), if it can't.
#[derive(Default)]
struct Round {
    commands: Vec<RoundCommand>,
    bindings: Vec<RoundBinding>,
    bytes: Vec<u8>,
    label: String,
    refusal: Option<String>,
}

impl Round {
    fn clear(&mut self) {
        self.commands.clear();
        self.bindings.clear();
        self.bytes.clear();
        self.label.clear();
        self.refusal = None;
    }

    fn bindings_of(&self, command: &RoundCommand) -> &[RoundBinding] {
        &self.bindings[command.first as usize..(command.first + command.count) as usize]
    }

    fn refuse(&mut self, why: String) {
        if self.refusal.is_none() {
            self.refusal = Some(why);
        }
    }

    fn push(&mut self, pipeline: &GpuComputePipeline, bindings: &[GpuBinding], groups: [u32; 3], label: &str) {
        if self.commands.is_empty() {
            self.label.push_str(label);
        }
        if !pipeline.supports_replay {
            self.refuse(format!("{label}: the pipeline has no indirect command buffer support"));
        }
        // Commands inherit no bindings: a referenced binding left out would
        // read whatever the buffer slot last held.
        if let Some(missing) = pipeline.unbound_binding(bindings) {
            self.refuse(format!("{label} leaves binding {missing} unbound"));
        }
        let first = self.bindings.len();
        for binding in bindings {
            match binding {
                GpuBinding::Buffer { binding, buffer, offset } => {
                    if let Some(slot) = pipeline.slot_map.get(*binding) {
                        self.bindings.push(RoundBinding::Buffer { slot: slot.metal_index as u8, buffer: buffer.raw.clone(), offset: *offset });
                    }
                }
                GpuBinding::Bytes { binding, data } => {
                    if let Some(slot) = pipeline.slot_map.get(*binding) {
                        let start = self.bytes.len() as u32;
                        self.bytes.extend_from_slice(data);
                        self.bindings.push(RoundBinding::Bytes { slot: slot.metal_index as u8, start, len: data.len() as u32 });
                    }
                }
                GpuBinding::Texture { .. } | GpuBinding::Sampler { .. } => {
                    self.refuse(format!("{label}: a template binds buffers only"));
                }
            }
        }
        if pipeline.needs_sizes_buffer {
            let (sizes, len): ([u32; MAX_BUFFER_SLOTS], usize) = collect_buffer_sizes(&pipeline.slot_map, bindings);
            let slot = pipeline.slot_map.get(SIZES_BUFFER_BINDING).expect("sizes buffer slot missing").metal_index as u8;
            let start = self.bytes.len() as u32;
            for word in &sizes[..len] {
                self.bytes.extend_from_slice(&word.to_ne_bytes());
            }
            self.bindings.push(RoundBinding::Bytes { slot, start, len: (len * 4) as u32 });
        }
        let count = self.bindings.len() - first;
        let too_many = count > MAX_KEY_BINDINGS
            || self.bindings[first..].iter().any(|b| match b {
                RoundBinding::Buffer { slot, .. } | RoundBinding::Bytes { slot, .. } => usize::from(*slot) >= ICB_BUFFER_SLOTS,
            });
        if too_many {
            self.refuse(format!("{label}: more buffer slots than an indirect command sets"));
        }
        self.commands.push(RoundCommand {
            pipeline: pipeline.state.clone(),
            workgroup: pipeline.workgroup_size,
            groups,
            first: first as u32,
            count: count as u32,
        });
    }

    /// Bit-equal rounds: the same pipelines, groups, buffers at the same
    /// offsets and slots, and the same inline bytes. Identities are objects
    /// both rounds retain, so an equal pointer is the same object.
    fn same_as(&self, other: &Round) -> bool {
        self.commands.len() == other.commands.len()
            && self.commands.iter().zip(&other.commands).all(|(a, b)| {
                std::ptr::eq(&*a.pipeline, &*b.pipeline)
                    && a.groups == b.groups
                    && a.count == b.count
                    && self.bindings_of(a).iter().zip(other.bindings_of(b)).all(|pair| match pair {
                        (RoundBinding::Buffer { slot: s, buffer: x, offset: o }, RoundBinding::Buffer { slot: t, buffer: y, offset: p }) => {
                            s == t && o == p && std::ptr::eq(&**x, &**y)
                        }
                        (RoundBinding::Bytes { slot: s, start: i, len: n }, RoundBinding::Bytes { slot: t, start: j, len: m }) => {
                            s == t && n == m && self.bytes[*i as usize..(*i + *n) as usize] == other.bytes[*j as usize..(*j + *m) as usize]
                        }
                        _ => false,
                    })
            })
    }
}

/// One built template: the round it was built from (retaining every
/// pipeline and buffer it names), its replicated command buffer and inline
/// bytes, and the command buffers that ran it and may still be running.
struct Slot {
    round: Round,
    chunk: u32,
    icb: Retained<ProtocolObject<dyn MTLIndirectCommandBuffer>>,
    arena: Retained<ProtocolObject<dyn MTLBuffer>>,
    /// Every buffer the commands name, and the arena, for `useResources`.
    resources: Vec<NonNull<ProtocolObject<dyn MTLResource>>>,
    label: Retained<NSString>,
    users: Vec<Retained<ProtocolObject<dyn MTLCommandBuffer>>>,
    /// Lookup clock of the last visit: the least recent idle slot is rebuilt.
    used: u64,
}

// Safety: Metal objects are documented as usable from any thread; the
// resource pointers name buffers this slot retains.
unsafe impl Send for Slot {}

impl Slot {
    fn prune(&mut self) {
        self.users.retain(|buf| !matches!(unsafe { buf.status() }, MTLCommandBufferStatus::Completed | MTLCommandBufferStatus::Error));
    }

    fn idle(&self) -> bool {
        self.users.is_empty()
    }
}

/// A prepared visit: the slot to run and its execute ranges.
pub struct TemplateTicket {
    slot: usize,
    ranges: Retained<ProtocolObject<dyn MTLBuffer>>,
    first_bytes: u64,
    stride_bytes: u64,
    executes: usize,
}

/// A caller's gated templates: slots built on demand, each run on any
/// encoder path and rebuilt only when no command buffer still runs it.
pub struct GpuTemplateStore {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    slots: Vec<Slot>,
    round: Round,
    clock: u64,
    stats: GpuTemplateStats,
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub(super) fail_next_alloc: bool,
}

// Safety: as for `Slot`; one thread uses a store at a time.
unsafe impl Send for GpuTemplateStore {}

impl GpuTemplateStore {
    pub fn new(device: &GpuDevice) -> Self {
        Self {
            device: device.raw_device().retain(),
            slots: Vec::new(),
            round: Round::default(),
            clock: 0,
            stats: GpuTemplateStats::default(),
            #[cfg(all(test, feature = "gpu-proofs"))]
            fail_next_alloc: false,
        }
    }

    pub fn stats(&self) -> GpuTemplateStats {
        self.stats
    }

    /// Slots held, idle or in flight.
    pub fn slots(&self) -> usize {
        self.slots.len()
    }

    /// Build the walked round into a slot: an idle slot's storage reused
    /// when it fits, else new storage. On failure nothing published changes:
    /// the slot is pushed or overwritten only once fully built.
    fn build(&mut self, chunk: u32) -> Result<usize, String> {
        let commands = self.round.commands.len();
        let count = commands * chunk as usize;
        let arena_bytes = self.round.bindings.iter().map(|b| match b {
            RoundBinding::Bytes { len, .. } => (*len as usize).next_multiple_of(BYTES_ALIGN),
            RoundBinding::Buffer { .. } => 0,
        }).sum::<usize>().max(BYTES_ALIGN);
        let idle = self.slots.iter().enumerate().filter(|(_, s)| s.idle()).min_by_key(|(_, s)| s.used).map(|(i, _)| i);
        #[cfg(all(test, feature = "gpu-proofs"))]
        if std::mem::take(&mut self.fail_next_alloc) {
            return Err("gated template: storage allocation failed (forced)".into());
        }
        let reuse = idle.filter(|&i| self.slots[i].icb.size() >= count && self.slots[i].arena.length() >= arena_bytes);
        let (icb, arena) = match reuse {
            Some(i) => (self.slots[i].icb.clone(), self.slots[i].arena.clone()),
            None => {
                let icb = new_icb(&self.device, count).ok_or("gated template: indirect command buffer allocation failed")?;
                let arena = self
                    .device
                    .newBufferWithLength_options(arena_bytes, MTLResourceOptions::StorageModeShared)
                    .ok_or("gated template: arena allocation failed")?;
                (icb, arena)
            }
        };
        // A reused slot is reset before any write: a failure past here can't
        // leave it half written and keyed (nothing below fails).
        let base = arena.contents().as_ptr().cast::<u8>();
        let mut resources = Vec::with_capacity(self.round.bindings.len() + 1);
        resources.push(resource(&arena));
        let mut offset = 0usize;
        // Arena offsets per inline binding, in round order.
        let mut placed: Vec<usize> = Vec::with_capacity(self.round.bindings.len());
        for binding in &self.round.bindings {
            match binding {
                RoundBinding::Bytes { start, len, .. } => {
                    let data = &self.round.bytes[*start as usize..(*start + *len) as usize];
                    // Safety: the arena holds `arena_bytes`, laid out here;
                    // a reused arena belongs to an idle slot, so no GPU work
                    // reads it.
                    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), base.add(offset), data.len()) };
                    placed.push(offset);
                    offset += (*len as usize).next_multiple_of(BYTES_ALIGN);
                }
                RoundBinding::Buffer { buffer, .. } => resources.push(resource(buffer)),
            }
        }
        for replica in 0..chunk as usize {
            let mut placed = placed.iter();
            for (i, command) in self.round.commands.iter().enumerate() {
                let ic = unsafe { icb.indirectComputeCommandAtIndex(replica * commands + i) };
                ic.reset();
                ic.setComputePipelineState(&command.pipeline);
                for binding in self.round.bindings_of(command) {
                    match binding {
                        RoundBinding::Buffer { slot, buffer, offset } => unsafe {
                            ic.setKernelBuffer_offset_atIndex(buffer, *offset as usize, usize::from(*slot));
                        },
                        RoundBinding::Bytes { slot, .. } => unsafe {
                            let at = *placed.next().expect("one arena offset per inline binding");
                            ic.setKernelBuffer_offset_atIndex(&arena, at, usize::from(*slot));
                        },
                    }
                }
                ic.concurrentDispatchThreadgroups_threadsPerThreadgroup(mtl_size(command.groups), mtl_size(command.workgroup));
                ic.setBarrier();
            }
        }
        for i in count..icb.size() {
            unsafe { icb.indirectComputeCommandAtIndex(i) }.reset();
        }
        self.stats.builds += 1;
        self.stats.command_writes += count as u64;
        let label = NSString::from_str(&format!("template: {}", self.round.label));
        let mut round = Round::default();
        std::mem::swap(&mut round, &mut self.round);
        let slot = Slot { round, chunk, icb, arena, resources, label, users: Vec::new(), used: self.clock };
        Ok(match idle {
            Some(i) => {
                // The old round's vectors come back as the next walk's scratch.
                let old = std::mem::replace(&mut self.slots[i], slot);
                self.round = old.round;
                self.round.clear();
                i
            }
            None => {
                self.slots.push(slot);
                self.stats.grown += 1;
                self.slots.len() - 1
            }
        })
    }

    /// Free idle slots past the kept count, least recent first, never `keep`.
    fn trim(&mut self, keep: usize) -> usize {
        let mut keep = keep;
        loop {
            let others = |i: &usize| *i != keep && self.slots[*i].idle();
            if (0..self.slots.len()).filter(others).count() < IDLE_SLOTS_KEPT {
                return keep;
            }
            let oldest = (0..self.slots.len()).filter(others).min_by_key(|&i| self.slots[i].used).expect("non-empty");
            self.slots.swap_remove(oldest);
            if keep == self.slots.len() {
                keep = oldest;
            }
        }
    }
}

impl Drop for GpuTemplateStore {
    fn drop(&mut self) {
        for slot in self.slots.drain(..) {
            let pending = slot.users.iter().any(|buf| {
                matches!(unsafe { buf.status() }, MTLCommandBufferStatus::NotEnqueued | MTLCommandBufferStatus::Enqueued)
            });
            if pending {
                // A buffer that may never be committed can't be waited on;
                // freeing what it names could free it under the GPU.
                log::warn!("gated template dropped while an uncommitted command buffer names it; its storage is leaked");
                std::mem::forget(slot);
                continue;
            }
            for buf in &slot.users {
                buf.waitUntilCompleted();
            }
        }
    }
}

impl GpuEncoder {
    /// Walk `body` once as one round of exactly `commands` gated
    /// dispatches the GPU runs `copies` times in chunked executes, and find
    /// or build its slot in `store`. Encodes nothing. `Err` (a body error, a
    /// count mismatch, an unrecordable dispatch, ranges that don't fit, no
    /// storage) leaves every slot as it was.
    pub fn prepare_template(
        &mut self,
        store: &mut GpuTemplateStore,
        at: TemplateRanges<'_>,
        commands: u32,
        copies: u32,
        body: impl FnOnce(&mut GatedRecorder<'_>) -> Result<(), String>,
    ) -> Result<TemplateTicket, String> {
        let result = prepare(store, at, commands, copies, body);
        if result.is_err() {
            store.stats.refused += 1;
            store.round.clear();
        }
        result
    }

    /// Run a prepared template here: the pending frame-replay stretch first,
    /// then the slot's executes on a compute encoder that declares every
    /// buffer the slot names, a buffer barrier around and between executes.
    pub fn execute_template(&mut self, store: &mut GpuTemplateStore, ticket: TemplateTicket) {
        self.end_gated_segments();
        self.flush_replay();
        let slot = &mut store.slots[ticket.slot];
        let per_execute = self.profile.as_ref().is_some_and(|p| p.granularity == super::ProfileGranularity::Dispatch);
        let mut enc: Option<Retained<ProtocolObject<dyn MTLComputeCommandEncoder>>> = None;
        for j in 0..ticket.executes {
            if enc.is_none() || per_execute {
                if let Some(open) = enc.take() {
                    close(&open);
                    self.end_current();
                }
                let opened = if self.profile.is_some() {
                    self.end_current();
                    self.begin_profiled_compute_named("pressure rounds", 0)
                } else {
                    self.ensure_compute_raw()
                };
                unsafe {
                    opened.pushDebugGroup(&slot.label);
                    opened.memoryBarrierWithScope(MTLBarrierScope::Buffers);
                    let resources = NonNull::new_unchecked(slot.resources.as_mut_ptr());
                    opened.useResources_count_usage(resources, slot.resources.len(), MTLResourceUsage::Read | MTLResourceUsage::Write);
                }
                enc = Some(opened);
            } else if let Some(open) = &enc {
                open.memoryBarrierWithScope(MTLBarrierScope::Buffers);
            }
            let open = enc.as_ref().expect("opened above");
            let offset = ticket.first_bytes + j as u64 * ticket.stride_bytes;
            unsafe { open.executeCommandsInBuffer_indirectBuffer_indirectBufferOffset(&slot.icb, &ticket.ranges, offset as usize) };
            store.stats.executes += 1;
        }
        if let Some(open) = enc {
            close(&open);
        }
        // What an executed command buffer leaves bound is not specified: the
        // next direct dispatch binds everything again.
        self.compute_cache.clear();
        if self.profile.is_some() && matches!(self.state, EncoderState::Compute(_)) {
            self.end_current();
        }
        if slot.users.last().is_none_or(|last| !std::ptr::eq(&**last, &*self.cmd_buf)) {
            slot.users.push(self.cmd_buf.clone());
        }
    }
}

/// The trailing barrier and the debug group's end.
fn close(enc: &ProtocolObject<dyn MTLComputeCommandEncoder>) {
    unsafe { enc.memoryBarrierWithScope(MTLBarrierScope::Buffers) };
    enc.popDebugGroup();
}

fn prepare(
    store: &mut GpuTemplateStore,
    at: TemplateRanges<'_>,
    commands: u32,
    copies: u32,
    body: impl FnOnce(&mut GatedRecorder<'_>) -> Result<(), String>,
) -> Result<TemplateTicket, String> {
    let executes = template_chunks(copies, at.chunk).count() as u32;
    let last = executes.checked_sub(1).and_then(|c| c.checked_mul(at.stride)).and_then(|c| c.checked_add(at.first));
    let end = last.and_then(|l| (u64::from(l) + 1).checked_mul(GATED_RANGE_BYTES));
    let replicated = commands.checked_mul(at.chunk).filter(|&n| n <= MAX_TEMPLATE_COMMANDS);
    if commands == 0 || copies == 0 || at.chunk == 0 || replicated.is_none() || end.is_none_or(|end| end > at.ranges.size) {
        return Err(format!(
            "gated template: {commands} commands, {copies} rounds by {} in {executes} executes from entry {} by {} do not fit a {}-byte range buffer",
            at.chunk, at.first, at.stride, at.ranges.size
        ));
    }
    store.round.clear();
    store.stats.walks += 1;
    body(&mut GatedRecorder { round: &mut store.round })?;
    if let Some(why) = store.round.refusal.take() {
        return Err(format!("gated template: {why}"));
    }
    let taken = store.round.commands.len();
    if taken != commands as usize {
        return Err(format!("gated template: the body issued {taken} dispatches, not the {commands} declared"));
    }
    store.clock += 1;
    for slot in &mut store.slots {
        slot.prune();
    }
    let found = store.slots.iter().position(|s| s.chunk == at.chunk && s.round.same_as(&store.round));
    let index = match found {
        Some(i) => {
            store.stats.hits += 1;
            store.round.clear();
            i
        }
        None => store.build(at.chunk)?,
    };
    store.slots[index].used = store.clock;
    let index = store.trim(index);
    Ok(TemplateTicket {
        slot: index,
        ranges: at.ranges.raw.clone(),
        first_bytes: u64::from(at.first) * GATED_RANGE_BYTES,
        stride_bytes: u64::from(at.stride) * GATED_RANGE_BYTES,
        executes: executes as usize,
    })
}

fn new_icb(device: &ProtocolObject<dyn MTLDevice>, commands: usize) -> Option<Retained<ProtocolObject<dyn MTLIndirectCommandBuffer>>> {
    let desc = MTLIndirectCommandBufferDescriptor::new();
    desc.setCommandTypes(MTLIndirectCommandType::ConcurrentDispatch);
    desc.setInheritPipelineState(false);
    desc.setInheritBuffers(false);
    desc.setMaxKernelBufferBindCount(ICB_BUFFER_SLOTS);
    unsafe { device.newIndirectCommandBufferWithDescriptor_maxCommandCount_options(&desc, commands, MTLResourceOptions::StorageModeShared) }
}

fn resource(buffer: &ProtocolObject<dyn MTLBuffer>) -> NonNull<ProtocolObject<dyn MTLResource>> {
    NonNull::from(buffer).cast()
}

fn mtl_size(v: [u32; 3]) -> MTLSize {
    MTLSize { width: v[0] as usize, height: v[1] as usize, depth: v[2] as usize }
}

#[cfg(all(test, feature = "gpu-proofs"))]
impl GpuTemplateStore {
    /// Outstanding command buffers per slot, after pruning completed ones.
    pub(super) fn users(&mut self) -> Vec<usize> {
        self.slots.iter_mut().map(|s| {
            s.prune();
            s.users.len()
        }).collect()
    }
}
