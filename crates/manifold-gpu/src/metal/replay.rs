//! Encode replay, the Metal store (`docs/ENCODE_REPLAY_DESIGN.md`): each
//! ring entry keeps its commands in indirect command buffer chunks, its
//! inline bytes in shared arenas, and a retain on every pipeline and buffer
//! it names. While a span is open, each recordable dispatch is checked
//! against the entry at the current position (`crate::replay`); matched
//! and newly recorded commands wait as one pending stretch, which runs with
//! a single execute the moment anything else needs the encoder.
//!
//! A gated segment (`GpuEncoder::begin_gated_segment`) is a run of
//! dispatches whose number the GPU decides: it lives in its own indirect
//! command buffer, sized to the length the segment declared, and is executed
//! from a range the GPU wrote (`{0, commands}` live, `{0, 0}` dead), so a
//! converged solver's remaining rounds cost one empty execute each instead
//! of a CPU-encoded indirect dispatch apiece. Slots the segment didn't fill
//! are reset, which Metal runs as no-ops; a segment is always executed as a
//! whole, so a flush landing inside one cuts the recording there and the
//! rest of that segment runs directly.

use std::ops::Range;
use std::ptr::NonNull;

use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSRange, NSString};
use objc2_metal::{
    MTLBarrierScope, MTLBuffer, MTLCommandBuffer, MTLCommandBufferStatus, MTLCommandEncoder,
    MTLComputeCommandEncoder, MTLComputePipelineState, MTLDevice, MTLIndirectCommandBuffer,
    MTLIndirectCommandBufferDescriptor, MTLIndirectCommandType, MTLIndirectComputeCommand, MTLResource,
    MTLResourceOptions, MTLResourceUsage, MTLSize,
};

use super::device::GpuDevice;
use super::encoder::{EncoderState, GpuEncoder, MAX_BUFFER_SLOTS, RT_STAGE_PREFIX, collect_buffer_sizes};
use super::types::{GpuBuffer, GpuComputePipeline};
use super::SIZES_BUFFER_BINDING;
use crate::replay::{
    ARENA_BYTES, BYTES_ALIGN, BytesSlot, CHUNK_COMMANDS, DispatchKey, GateKey, GpuReplayStats, KeyBinding,
    MAX_KEY_BINDINGS, Recording, pick_entry, replay_allowed_by_env,
};
use crate::types::GpuBinding;

/// Kernel buffer slots an indirect compute command can set (Metal's limit).
const ICB_BUFFER_SLOTS: usize = 31;

/// Recordings for one replay span, owned by the caller between spans.
/// `Default` allocates nothing. Entries and their GPU storage are created in
/// `begin_replay`, never while a span is encoding.
#[derive(Default)]
pub struct GpuReplayCache {
    entries: Vec<ReplayEntry>,
    mru: Option<usize>,
    stats: GpuReplayStats,
}

// Safety: the cache holds Metal objects, which Metal documents as safe to
// use from any thread, and plain data; one thread uses it at a time.
unsafe impl Send for GpuReplayCache {}

impl GpuReplayCache {
    pub fn stats(&self) -> GpuReplayStats {
        self.stats
    }

    /// Pick the entry for a span visit and grow it to what its last visit
    /// wanted. `None`: no idle entry, or its storage couldn't be made.
    fn enter(&mut self, device: &GpuDevice) -> Option<usize> {
        let entries = &self.entries;
        let Some(index) = pick_entry(entries.len(), self.mru, |i| entries[i].store.is_idle()) else {
            self.stats.ring_busy += 1;
            return None;
        };
        if index == self.entries.len() {
            self.entries.push(ReplayEntry::default());
        }
        let store = &mut self.entries[index].store;
        store.reserve(device, &mut self.stats);
        (!store.chunks.is_empty() && !store.arenas.is_empty()).then_some(index)
    }
}

#[derive(Default)]
struct ReplayEntry {
    recording: Recording,
    store: ReplayStore,
}

/// Where a stored command's indirect command sits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    /// Position across the entry's chunks.
    Chunk(usize),
    /// Slot inside one gated segment's command buffer.
    Segment { segment: usize, slot: usize },
}

struct StoredCommand {
    /// Retained so a recorded pipeline can't be freed while named here.
    _pipeline: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    place: Place,
    retained_start: u32,
    resources_start: u32,
    arena_before: (u32, u32),
    label: Retained<NSString>,
}

/// One gated segment's command buffer and the range the GPU executes it by.
struct StoredSegment {
    icb: Retained<ProtocolObject<dyn MTLIndirectCommandBuffer>>,
    commands: usize,
    ranges: Retained<ProtocolObject<dyn MTLBuffer>>,
    offset: u64,
    /// Executes per stretch: 1 for a plain segment, a template's chunks
    /// (`crate::replay::template_chunks`).
    executes: usize,
    /// Copies of the recorded commands the buffer holds back to back: a
    /// chunk of n rounds executes the first n · commands.
    replicas: usize,
    /// Bytes between consecutive executes' range entries.
    stride_bytes: u64,
}

#[derive(Default)]
struct ReplayStore {
    chunks: Vec<Retained<ProtocolObject<dyn MTLIndirectCommandBuffer>>>,
    /// Next free chunk position; chunk commands are placed in order.
    chunk_cursor: usize,
    segments: Vec<StoredSegment>,
    arenas: Vec<GpuBuffer>,
    /// Next free byte: (arena, offset).
    arena_cursor: (u32, u32),
    commands: Vec<StoredCommand>,
    /// Every buffer a command names, retained so its address can't be
    /// reused while this store refers to it.
    retained: Vec<Retained<ProtocolObject<dyn MTLBuffer>>>,
    /// Per command, the resources an execute must declare, contiguous in
    /// command order so any stretch is one slice.
    resources: Vec<NonNull<ProtocolObject<dyn MTLResource>>>,
    /// The command buffer that last executed from this store.
    last_cmd_buf: Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>>,
    /// What the last visit couldn't fit: grows the store at the next entry.
    short_commands: usize,
    short_bytes: usize,
}

impl ReplayStore {
    fn is_idle(&self) -> bool {
        self.last_cmd_buf.as_ref().is_none_or(|buf| {
            matches!(unsafe { buf.status() }, MTLCommandBufferStatus::Completed | MTLCommandBufferStatus::Error)
        })
    }

    fn capacity(&self) -> usize {
        self.chunks.len() * CHUNK_COMMANDS
    }

    /// Where chunks and arenas are allocated: between spans, from what the
    /// last visit fell short of. Segment buffers are the exception, made
    /// when a segment is first recorded (a changed segment structure is a
    /// recording cut, which already allocates nothing while warm).
    fn reserve(&mut self, device: &GpuDevice, stats: &mut GpuReplayStats) {
        let chunks = (self.chunk_cursor + self.short_commands).div_ceil(CHUNK_COMMANDS).max(1);
        while self.chunks.len() < chunks {
            let Some(chunk) = new_chunk(device) else {
                log::warn!("encode replay: indirect command buffer allocation failed; the span encodes directly");
                break;
            };
            self.chunks.push(chunk);
            stats.store_allocations += 1;
        }
        let used = self.arena_cursor.0 as usize * ARENA_BYTES + self.arena_cursor.1 as usize;
        let arenas = ((used + self.short_bytes).div_ceil(ARENA_BYTES) + usize::from(self.short_bytes > 0))
            .max(1);
        while self.arenas.len() < arenas {
            match device.try_create_buffer_shared(ARENA_BYTES as u64) {
                Ok(arena) => {
                    self.arenas.push(arena);
                    stats.store_allocations += 1;
                }
                Err(error) => {
                    log::warn!("encode replay: uniform arena allocation failed ({error}); the span encodes directly");
                    break;
                }
            }
        }
        self.short_commands = 0;
        self.short_bytes = 0;
    }

    /// Arena slots for every inline binding of `key`, in binding order, or
    /// `false` with the cursor untouched when they don't all fit.
    fn alloc_bytes(&mut self, key: &DispatchKey, slots: &mut [BytesSlot; MAX_KEY_BINDINGS]) -> (bool, usize) {
        let before = self.arena_cursor;
        let mut count = 0;
        for binding in key.bindings() {
            let KeyBinding::Bytes { data, .. } = binding else {
                continue;
            };
            let size = data.len().next_multiple_of(BYTES_ALIGN);
            let (mut arena, mut offset) = self.arena_cursor;
            if offset as usize + size > ARENA_BYTES {
                arena += 1;
                offset = 0;
            }
            if size > ARENA_BYTES || arena as usize >= self.arenas.len() {
                self.arena_cursor = before;
                return (false, 0);
            }
            self.arena_cursor = (arena, offset + size as u32);
            slots[count] = BytesSlot { arena, offset };
            count += 1;
        }
        (true, count)
    }

    fn write_bytes(&self, slot: BytesSlot, data: &[u8]) {
        let base = self.arenas[slot.arena as usize].mapped_ptr().expect("replay arenas are shared memory");
        // Safety: the slot was allocated for `data.len()` bytes inside the
        // arena, and the entry is not being read by the GPU (it was idle
        // when the span took it, and this command has not executed yet).
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), base.add(slot.offset as usize), data.len()) };
    }

    /// Drop every command from `len` on. A dropped segment slot is reset so
    /// the segment, always executed whole, runs it as a no-op; a segment
    /// whose first slot is dropped goes with its buffer.
    fn truncate(&mut self, len: usize) {
        let Some(first) = self.commands.get(len) else {
            return;
        };
        self.retained.truncate(first.retained_start as usize);
        self.resources.truncate(first.resources_start as usize);
        self.arena_cursor = first.arena_before;
        let mut chunk_cursor = self.chunk_cursor;
        let mut segments = self.segments.len();
        for command in &self.commands[len..] {
            match command.place {
                Place::Chunk(position) => chunk_cursor = chunk_cursor.min(position),
                Place::Segment { segment, slot } => {
                    let stored = &self.segments[segment];
                    for replica in 0..stored.replicas {
                        unsafe { stored.icb.indirectComputeCommandAtIndex(slot + replica * stored.commands) }.reset();
                    }
                    if slot == 0 {
                        segments = segments.min(segment);
                    }
                }
            }
        }
        self.chunk_cursor = chunk_cursor;
        self.segments.truncate(segments);
        self.commands.truncate(len);
    }

    /// Whether the next command of `key` has a place: a chunk position, or
    /// a slot inside the open segment (`gate.slot` within its declared
    /// length; slot 0 needs a new segment buffer, made here).
    fn place_for(
        &mut self,
        device: &ProtocolObject<dyn MTLDevice>,
        key: &DispatchKey,
        ranges: Option<&Retained<ProtocolObject<dyn MTLBuffer>>>,
        stats: &mut GpuReplayStats,
    ) -> Option<Place> {
        let Some(gate) = key.gate else {
            return (self.chunk_cursor < self.capacity()).then_some(Place::Chunk(self.chunk_cursor));
        };
        let slot = gate.slot as usize;
        if slot >= gate.commands as usize {
            return None;
        }
        if slot == 0 {
            let ranges = ranges.expect("a gated key names its range buffer");
            let replicas = gate.chunk.max(1) as usize;
            let icb = new_icb(device, gate.commands as usize * replicas)?;
            for i in 0..gate.commands as usize * replicas {
                unsafe { icb.indirectComputeCommandAtIndex(i) }.reset();
            }
            stats.store_allocations += 1;
            self.segments.push(StoredSegment {
                icb,
                commands: gate.commands as usize,
                ranges: ranges.clone(),
                offset: gate.offset,
                executes: if gate.copies == 0 { 1 } else { crate::replay::template_chunks(gate.copies, gate.chunk).count() },
                replicas,
                stride_bytes: u64::from(gate.stride) * crate::replay::GATED_RANGE_BYTES,
            });
        }
        let segment = self.segments.len().checked_sub(1)?;
        (self.segments[segment].commands == gate.commands as usize).then_some(Place::Segment { segment, slot })
    }

    /// Write `key` as the next command at `place`. `buffers` holds the
    /// buffer of every buffer binding and `slots` the arena slot of every
    /// inline binding, both in binding order.
    fn record(
        &mut self,
        pipeline: &GpuComputePipeline,
        key: &DispatchKey,
        place: Place,
        buffers: &[Option<&GpuBuffer>],
        slots: &[BytesSlot],
        arena_before: (u32, u32),
        label: &str,
    ) {
        let retained_start = self.retained.len() as u32;
        let resources_start = self.resources.len() as u32;
        // A template's command is written once per replica; the replicas
        // share its bindings, arena slots and declared resources.
        let (icb, first, step, count) = match place {
            Place::Chunk(position) => {
                debug_assert_eq!(position, self.chunk_cursor, "chunk commands are placed in order");
                self.chunk_cursor = position + 1;
                (&self.chunks[position / CHUNK_COMMANDS], position % CHUNK_COMMANDS, 0, 1)
            }
            Place::Segment { segment, slot } => {
                let stored = &self.segments[segment];
                if slot == 0 {
                    self.resources.push(resource(&stored.ranges));
                }
                (&stored.icb, slot, stored.commands, stored.replicas)
            }
        };
        let icb = icb.clone();
        let commands: Vec<_> = (0..count).map(|r| unsafe { icb.indirectComputeCommandAtIndex(first + r * step) }).collect();
        for command in &commands {
            command.reset();
            command.setComputePipelineState(&pipeline.state);
        }
        let mut buffers = buffers.iter();
        let mut slots = slots.iter();
        for binding in key.bindings() {
            match *binding {
                KeyBinding::Buffer { slot, offset, .. } => {
                    let buffer = buffers.next().copied().flatten().expect("one buffer per buffer binding");
                    for command in &commands {
                        unsafe { command.setKernelBuffer_offset_atIndex(&buffer.raw, offset as usize, slot as usize) };
                    }
                    self.resources.push(resource(&buffer.raw));
                    self.retained.push(buffer.raw.clone());
                }
                KeyBinding::Bytes { slot, data } => {
                    let store = *slots.next().expect("one arena slot per inline binding");
                    self.write_bytes(store, data);
                    let arena = &self.arenas[store.arena as usize].raw;
                    for command in &commands {
                        unsafe { command.setKernelBuffer_offset_atIndex(arena, store.offset as usize, slot as usize) };
                    }
                    self.resources.push(resource(arena));
                }
            }
        }
        for command in &commands {
            command.concurrentDispatchThreadgroups_threadsPerThreadgroup(mtl_size(key.groups), mtl_size(pipeline.workgroup_size));
            command.setBarrier();
        }
        self.commands.push(StoredCommand {
            _pipeline: pipeline.state.clone(),
            place,
            retained_start,
            resources_start,
            arena_before,
            label: NSString::from_str(&format!("replay: {label}")),
        });
    }

    /// Run commands `range` on `enc`, in order and each after the last,
    /// with everything encoded before them finished first: chunk runs by
    /// explicit range, each gated segment whole, by the range the GPU
    /// wrote. Returns (execute calls, segment executes).
    fn execute(
        &mut self,
        enc: &ProtocolObject<dyn MTLComputeCommandEncoder>,
        range: Range<usize>,
        cmd_buf: &Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    ) -> (u64, u64) {
        let first = &self.commands[range.start];
        let resources_start = first.resources_start as usize;
        let resources_end = self.commands.get(range.end).map_or(self.resources.len(), |c| c.resources_start as usize);
        let (mut executes, mut segments) = (0, 0);
        unsafe {
            enc.pushDebugGroup(&first.label);
            enc.insertDebugSignpost(&first.label);
            // The command buffers themselves are direct arguments of the
            // execute calls and need no useResource: declaring one costs
            // ~6 us of GPU time per call (measured, dead-segment probe), and
            // the proofs hold without it under the Metal debug layer.
            enc.memoryBarrierWithScope(MTLBarrierScope::Buffers);
            if resources_end > resources_start {
                let resources = NonNull::new_unchecked(self.resources.as_mut_ptr().add(resources_start));
                enc.useResources_count_usage(
                    resources,
                    resources_end - resources_start,
                    MTLResourceUsage::Read | MTLResourceUsage::Write,
                );
            }
            let mut start = range.start;
            while start < range.end {
                if executes > 0 {
                    enc.memoryBarrierWithScope(MTLBarrierScope::Buffers);
                }
                match self.commands[start].place {
                    Place::Chunk(position) => {
                        let chunk = position / CHUNK_COMMANDS;
                        let mut end = start + 1;
                        while end < range.end
                            && self.commands[end].place == Place::Chunk(position + (end - start))
                            && (position + (end - start)) / CHUNK_COMMANDS == chunk
                        {
                            end += 1;
                        }
                        let icb = &self.chunks[chunk];
                        enc.executeCommandsInBuffer_withRange(icb, NSRange::new(position % CHUNK_COMMANDS, end - start));
                        start = end;
                    }
                    Place::Segment { segment, .. } => {
                        let mut end = start + 1;
                        while end < range.end && matches!(self.commands[end].place, Place::Segment { segment: s, .. } if s == segment) {
                            end += 1;
                        }
                        // A template runs once per copy, each by its own range
                        // entry, a buffer barrier between copies as between
                        // any two executes.
                        let stored = &self.segments[segment];
                        for copy in 0..stored.executes {
                            if copy > 0 {
                                enc.memoryBarrierWithScope(MTLBarrierScope::Buffers);
                                executes += 1;
                            }
                            let offset = stored.offset + copy as u64 * stored.stride_bytes;
                            enc.executeCommandsInBuffer_indirectBuffer_indirectBufferOffset(&stored.icb, &stored.ranges, offset as usize);
                        }
                        segments += stored.executes as u64;
                        start = end;
                    }
                }
                executes += 1;
            }
            enc.memoryBarrierWithScope(MTLBarrierScope::Buffers);
            enc.popDebugGroup();
        }
        self.last_cmd_buf = Some(cmd_buf.clone());
        (executes, segments)
    }
}

fn new_chunk(device: &GpuDevice) -> Option<Retained<ProtocolObject<dyn MTLIndirectCommandBuffer>>> {
    new_icb(device.raw_device(), CHUNK_COMMANDS)
}

fn new_icb(device: &ProtocolObject<dyn MTLDevice>, commands: usize) -> Option<Retained<ProtocolObject<dyn MTLIndirectCommandBuffer>>> {
    let desc = MTLIndirectCommandBufferDescriptor::new();
    desc.setCommandTypes(MTLIndirectCommandType::ConcurrentDispatch);
    desc.setInheritPipelineState(false);
    desc.setInheritBuffers(false);
    desc.setMaxKernelBufferBindCount(ICB_BUFFER_SLOTS);
    unsafe {
        device.newIndirectCommandBufferWithDescriptor_maxCommandCount_options(
            &desc,
            commands,
            MTLResourceOptions::StorageModeShared,
        )
    }
}

fn resource(buffer: &ProtocolObject<dyn MTLBuffer>) -> NonNull<ProtocolObject<dyn MTLResource>> {
    NonNull::from(buffer).cast()
}

fn mtl_size(v: [u32; 3]) -> MTLSize {
    MTLSize { width: v[0] as usize, height: v[1] as usize, depth: v[2] as usize }
}

fn identity<T: ?Sized>(object: &T) -> usize {
    object as *const T as *const () as usize
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SpanMode {
    /// Checking dispatches against the recording.
    Validate,
    /// Writing dispatches after a miss or past the recording's end.
    Record,
    /// No room left: the rest of the span encodes directly.
    Full,
}

/// An open span: the cache it owns until `end_replay`, and where it is.
pub(crate) struct ReplaySpan {
    cache: GpuReplayCache,
    entry: Option<usize>,
    cursor: usize,
    /// Commands `pending_start..cursor` are validated or recorded and wait
    /// for one execute.
    pending_start: usize,
    mode: SpanMode,
    /// The device's word-copy kernel while the span replays (D9).
    copy_kernel: Option<std::sync::Arc<GpuComputePipeline>>,
    /// The gated segment being encoded, between `begin_gated_segment` and
    /// the next begin or `end_gated_segments`.
    segment: Option<OpenSegment>,
    /// The device, for a segment buffer recorded mid-span.
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    /// The gated template being walked (`GpuEncoder::repeat_gated_template`).
    template: Option<Template>,
}

struct OpenSegment {
    ranges: Retained<ProtocolObject<dyn MTLBuffer>>,
    offset: u64,
    commands: u32,
    /// Gated dispatches the span took so far: the next one's slot.
    taken: u32,
    /// A flush ran inside this segment: the rest of it encodes directly,
    /// since the segment's buffer already executed as a whole.
    broken: bool,
    /// A template's rounds, range stride in entries and most rounds an
    /// execute; 0 for a plain segment.
    copies: u32,
    stride: u32,
    chunk: u32,
}

/// The word copy a replaying span turns buffer copies into (D9): one thread
/// per 4-byte word; offsets and count in words.
pub(super) const COPY_KERNEL_WGSL: &str = r#"
struct CopyWords { src_offset: u32, dst_offset: u32, words: u32, pad: u32 };
@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
@group(0) @binding(2) var<uniform> copy: CopyWords;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= copy.words) { return; }
    dst[copy.dst_offset + id.x] = src[copy.src_offset + id.x];
}
"#;

const COPY_KERNEL_GROUP: u64 = 256;

impl GpuEncoder {
    /// Open a replay span. Until `end_replay`, recordable compute dispatches
    /// are checked against, or recorded into, one entry of `cache`; anything
    /// else encodes directly. Encodes everything directly while dispatch
    /// profiling or GPU fault diagnostics are on, or with
    /// `MANIFOLD_ENCODE_REPLAY=0`.
    pub fn begin_replay(&mut self, device: &GpuDevice, mut cache: GpuReplayCache) {
        debug_assert!(self.replay.is_none(), "replay spans never nest");
        let enabled = self.profile.is_none() && !super::gpu_fault::diagnostics_enabled() && replay_allowed_by_env();
        let entry = if enabled { cache.enter(device) } else { None };
        let copy_kernel = entry.is_some().then(|| device.replay_copy_kernel().clone());
        self.replay = Some(ReplaySpan {
            cache,
            entry,
            cursor: 0,
            pending_start: 0,
            mode: SpanMode::Validate,
            copy_kernel,
            segment: None,
            device: device.raw_device().retain(),
            template: None,
        });
    }

    /// Open a gated segment inside the span: the next `dispatch_compute_gated`
    /// calls, up to `commands` of them, run from one recording the GPU
    /// executes by `ranges[index]` (`GATED_RANGE_BYTES` per entry), written
    /// earlier in this command buffer as `{0, commands}` to run or `{0, 0}`
    /// to skip. Closes the segment before it. Outside a span, or when the
    /// span encodes directly, gating is the dispatches' own indirect
    /// arguments, as before. Every dispatch of a segment must stay inside
    /// the segment's range buffer decision: the GPU writes both.
    pub fn begin_gated_segment(&mut self, ranges: &GpuBuffer, index: u32, commands: u32) {
        let Some(mut span) = self.replay.take() else {
            return;
        };
        self.close_segment(&mut span);
        if span.entry.is_some() {
            span.segment = Some(OpenSegment {
                ranges: ranges.raw.clone(),
                offset: u64::from(index) * crate::replay::GATED_RANGE_BYTES,
                commands,
                taken: 0,
                broken: false,
                copies: 0,
                stride: 0,
                chunk: 0,
            });
        }
        self.replay = Some(span);
    }

    /// Close the open gated segment; later dispatches are ungated.
    pub fn end_gated_segments(&mut self) {
        let Some(mut span) = self.replay.take() else {
            return;
        };
        self.close_segment(&mut span);
        self.replay = Some(span);
    }

    /// A dispatch the GPU may have switched off: `gate` holds its indirect
    /// threadgroup counts at `gate_offset` (zeros when off), `groups` the
    /// counts it has when on. Inside an open gated segment of a replaying
    /// span it is recorded with `groups` and the segment's range decides;
    /// everywhere else it is today's indirect dispatch.
    pub fn dispatch_compute_gated(
        &mut self,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        groups: [u32; 3],
        gate: &GpuBuffer,
        gate_offset: u64,
        label: &str,
    ) {
        if let Some(mut span) = self.replay.take() {
            let taken = match &span.segment {
                Some(segment) if !segment.broken && segment.taken < segment.commands => {
                    let gate = GateKey {
                        ranges: identity(&*segment.ranges),
                        offset: segment.offset,
                        commands: segment.commands,
                        slot: segment.taken,
                        copies: segment.copies,
                        stride: segment.stride,
                        chunk: segment.chunk,
                    };
                    self.replay_dispatch_in(&mut span, pipeline, bindings, Some(groups), Some(gate), label)
                }
                _ => false,
            };
            if taken {
                span.segment.as_mut().expect("taken inside a segment").taken += 1;
            } else {
                span.cache.stats.segments_direct += 1;
            }
            self.replay = Some(span);
            if taken {
                return;
            }
        }
        self.dispatch_compute_indirect(pipeline, bindings, gate, gate_offset, label);
    }

    /// Close the open segment. A recording that continues the segment past
    /// what this visit took is cut there: the segment executes whole, so
    /// the slots it didn't take must be no-ops, which the cut resets.
    fn close_segment(&mut self, span: &mut ReplaySpan) {
        let Some(segment) = span.segment.take() else {
            return;
        };
        let Some(entry) = span.entry else {
            return;
        };
        if segment.taken > 0 && span.mode == SpanMode::Validate {
            let ReplayEntry { recording, store } = &mut span.cache.entries[entry];
            if recording.continues_segment(span.cursor) {
                recording.truncate(span.cursor);
                store.truncate(span.cursor);
            }
        }
    }

    /// Offer a buffer copy to the open span as a word-copy dispatch (D9), so
    /// it joins the recording instead of ending the stretch. True when it
    /// went that way; false leaves it to the blit. Only a replaying span
    /// takes copies, only word-aligned ones, and never one whose source and
    /// destination ranges overlap.
    pub(super) fn replay_copy(&mut self, src: &GpuBuffer, src_offset: u64, dst: &GpuBuffer, dst_offset: u64, size: u64) -> bool {
        let Some(kernel) = self.replay.as_ref().and_then(|span| span.copy_kernel.clone()) else {
            return false;
        };
        let aligned = src_offset.is_multiple_of(4) && dst_offset.is_multiple_of(4) && size.is_multiple_of(4);
        let overlap =
            std::ptr::eq(&*src.raw, &*dst.raw) && src_offset < dst_offset + size && dst_offset < src_offset + size;
        let words = size / 4;
        let (Ok(src_word), Ok(dst_word), Ok(count), Ok(groups)) = (
            u32::try_from(src_offset / 4),
            u32::try_from(dst_offset / 4),
            u32::try_from(words),
            u32::try_from(words.div_ceil(COPY_KERNEL_GROUP)),
        ) else {
            return false;
        };
        if !aligned || overlap || words == 0 {
            return false;
        }
        let params = [src_word, dst_word, count, 0];
        // SAFETY: plain u32 data, read for the length of the call.
        let bytes = unsafe { std::slice::from_raw_parts(params.as_ptr().cast::<u8>(), std::mem::size_of_val(&params)) };
        self.dispatch_compute(
            &kernel,
            &[
                GpuBinding::Buffer { binding: 0, buffer: src, offset: 0 },
                GpuBinding::Buffer { binding: 1, buffer: dst, offset: 0 },
                GpuBinding::Bytes { binding: 2, data: bytes },
            ],
            [groups, 1, 1],
            "replay copy",
        );
        true
    }

    /// Run the pending stretch and hand the cache back.
    pub fn end_replay(&mut self) -> GpuReplayCache {
        self.flush_replay();
        let span = self.replay.take().expect("end_replay without begin_replay");
        let mut cache = span.cache;
        if span.entry.is_some() {
            cache.mru = span.entry;
        }
        cache
    }

    /// Execute the pending stretch, if any.
    pub(super) fn flush_replay(&mut self) {
        // An open template holds provisional state even with nothing pending
        // (a segment buffer made before its bytes failed): flush_span rolls
        // it back.
        if self.replay.as_ref().is_none_or(|span| span.cursor == span.pending_start && span.template.is_none()) {
            return;
        }
        let mut span = self.replay.take().expect("checked above");
        self.flush_span(&mut span);
        self.replay = Some(span);
    }

    fn flush_span(&mut self, span: &mut ReplaySpan) {
        // Nothing of an open template may execute: a flush reaching one
        // (no template body can cause it) rolls the walk back first.
        if span.template.as_ref().is_some_and(|t| t.failure.is_none()) {
            Self::fail_template(span, TemplateFailure::Shape);
        }
        if span.cursor == span.pending_start {
            return;
        }
        let entry = span.entry.expect("a pending stretch belongs to an entry");
        // A flush inside a gated segment runs the segment whole.
        Self::break_segment(span);
        let enc = self.ensure_compute_raw();
        let store = &mut span.cache.entries[entry].store;
        let (executes, segments) = store.execute(&enc, span.pending_start..span.cursor, &self.cmd_buf);
        span.cache.stats.executes += executes;
        span.cache.stats.segments_replayed += segments;
        span.pending_start = span.cursor;
        // What an executed indirect range leaves bound is not specified:
        // the next direct dispatch binds everything again.
        self.compute_cache.clear();
    }

    /// Offer one dispatch to the open span. True when the span took it
    /// (matched or recorded); false means encode it directly.
    pub(super) fn replay_dispatch(
        &mut self,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        groups: Option<[u32; 3]>,
        label: &str,
    ) -> bool {
        let Some(mut span) = self.replay.take() else {
            return false;
        };
        // An ungated dispatch inside a gated segment breaks it: a segment
        // executes whole, so a chunk command between its slots would run
        // the segment twice around the chunk.
        Self::break_segment(&mut span);
        let taken = self.replay_dispatch_in(&mut span, pipeline, bindings, groups, None, label);
        self.replay = Some(span);
        taken
    }

    /// End the open segment's recording here: the rest of it runs directly,
    /// and a recording that continued the segment past here is cut so the
    /// slots it would run are no-ops.
    fn break_segment(span: &mut ReplaySpan) {
        let Some(entry) = span.entry else {
            return;
        };
        let Some(segment) = span.segment.as_mut().filter(|s| s.taken > 0 && !s.broken) else {
            return;
        };
        segment.broken = true;
        let ReplayEntry { recording, store } = &mut span.cache.entries[entry];
        if recording.continues_segment(span.cursor) {
            recording.truncate(span.cursor);
            store.truncate(span.cursor);
        }
    }

    fn replay_dispatch_in(
        &mut self,
        span: &mut ReplaySpan,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        groups: Option<[u32; 3]>,
        gate: Option<GateKey>,
        label: &str,
    ) -> bool {
        let Some(entry) = span.entry else {
            span.cache.stats.direct += 1;
            return false;
        };
        let recordable = pipeline.supports_replay
            && !label.starts_with(RT_STAGE_PREFIX)
            && !matches!(self.state, EncoderState::Render(_));
        let Some(groups) = groups.filter(|_| recordable) else {
            span.cache.stats.direct += 1;
            return false;
        };

        // The key mirrors the direct path: bindings the pipeline doesn't
        // map are skipped, and the sizes buffer comes last.
        let mut key = DispatchKey::new(identity(&*pipeline.state), groups);
        key.gate = gate;
        let mut buffers: [Option<&GpuBuffer>; MAX_KEY_BINDINGS] = [None; MAX_KEY_BINDINGS];
        let mut buffer_count = 0;
        for binding in bindings {
            let pushed = match binding {
                GpuBinding::Buffer { binding, buffer, offset } => {
                    let Some(slot) = pipeline.slot_map.get(*binding) else {
                        continue;
                    };
                    let pushed = key.push(KeyBinding::Buffer {
                        slot: slot.metal_index,
                        id: identity(&*buffer.raw),
                        offset: *offset,
                    });
                    if pushed {
                        buffers[buffer_count] = Some(*buffer);
                        buffer_count += 1;
                    }
                    pushed
                }
                GpuBinding::Bytes { binding, data } => {
                    let Some(slot) = pipeline.slot_map.get(*binding) else {
                        continue;
                    };
                    key.push(KeyBinding::Bytes { slot: slot.metal_index, data })
                }
                GpuBinding::Texture { .. } | GpuBinding::Sampler { .. } => false,
            };
            if !pushed {
                span.cache.stats.direct += 1;
                return false;
            }
        }
        let (sizes, sizes_len): ([u32; MAX_BUFFER_SLOTS], usize) = collect_buffer_sizes(&pipeline.slot_map, bindings);
        // Safety: `sizes` is plain u32 data that outlives `key`.
        let sizes_bytes = unsafe { std::slice::from_raw_parts(sizes.as_ptr().cast::<u8>(), sizes_len * 4) };
        if pipeline.needs_sizes_buffer {
            let slot = pipeline.slot_map.get(SIZES_BUFFER_BINDING).expect("sizes buffer slot missing").metal_index;
            if !key.push(KeyBinding::Bytes { slot, data: sizes_bytes }) {
                span.cache.stats.direct += 1;
                return false;
            }
        }

        if span.mode == SpanMode::Full {
            let store = &mut span.cache.entries[entry].store;
            store.short_commands += 1;
            store.short_bytes += key.arena_bytes();
            span.cache.stats.direct += 1;
            return false;
        }
        if span.mode == SpanMode::Validate {
            let ReplayEntry { recording, store } = &mut span.cache.entries[entry];
            if span.cursor < recording.len() {
                if recording.matches(span.cursor, &key) {
                    recording.refresh_bytes(span.cursor, &key, |slot, data| store.write_bytes(slot, data));
                    if let Some(template) = span.template.as_mut() {
                        template.walk_bytes += key.arena_bytes();
                    }
                    span.cursor += 1;
                    span.cache.stats.replayed += 1;
                    return true;
                }
                // A miss: cut the recording here and record on. What
                // matched stays pending; the cut touches nothing before
                // the cursor, and a segment's dropped slots become no-ops.
                let ReplayEntry { recording, store } = &mut span.cache.entries[entry];
                recording.truncate(span.cursor);
                store.truncate(span.cursor);
            }
            span.mode = SpanMode::Record;
        }

        debug_assert!(span.cursor >= span.pending_start, "a recording is never written where this span already ran");
        let ReplayEntry { recording, store } = &mut span.cache.entries[entry];
        debug_assert_eq!(recording.len(), span.cursor, "records append at the cursor");
        let arena_before = store.arena_cursor;
        let mut slots = [BytesSlot { arena: 0, offset: 0 }; MAX_KEY_BINDINGS];
        let ranges = span.segment.as_ref().map(|s| &s.ranges);
        let fits = store.place_for(&span.device, &key, ranges, &mut span.cache.stats).is_some_and(|place| {
            let (fits, count) = store.alloc_bytes(&key, &mut slots);
            fits && {
                recording.push(&key, &slots[..count]);
                store.record(pipeline, &key, place, &buffers[..buffer_count], &slots[..count], arena_before, label);
                true
            }
        });
        if fits {
            if let Some(template) = span.template.as_mut() {
                template.walk_bytes += key.arena_bytes();
            }
            span.cursor += 1;
            span.cache.stats.recorded += 1;
            return true;
        }
        store.short_commands += 1;
        store.short_bytes += key.arena_bytes();
        span.mode = SpanMode::Full;
        span.cache.stats.direct += 1;
        // Inside a template the walk is provisional: the template rolls back
        // and runs directly, which flushes what came before it.
        if span.template.is_none() {
            self.flush_span(span);
        }
        false
    }
}

/// Where a gated template's rounds run: grouped into executes of up to
/// `chunk` rounds (`crate::template_chunks`), execute j by range entry
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

/// What a template body can do: issue gated dispatches, nothing else. A
/// body that could encode a plain dispatch, a copy or a texture pass could
/// flush the provisional walk, so none of those is expressible here.
pub struct GatedRecorder<'e> {
    enc: &'e mut GpuEncoder,
    mode: RecorderMode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecorderMode {
    /// Walking the template once, provisionally.
    Walk,
    /// Running a copy directly.
    Direct,
    /// Checking the body ends Ok before a direct copy encodes anything.
    Dry,
}

impl GatedRecorder<'_> {
    /// One gated dispatch: `gate` holds its indirect group counts at
    /// `gate_offset` (zeros when off), `groups` the counts it has when on.
    pub fn dispatch_gated(
        &mut self,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        groups: [u32; 3],
        gate: &GpuBuffer,
        gate_offset: u64,
        label: &str,
    ) {
        match self.mode {
            RecorderMode::Walk => self.enc.template_dispatch(pipeline, bindings, groups, label),
            RecorderMode::Direct => self.enc.dispatch_compute_gated(pipeline, bindings, groups, gate, gate_offset, label),
            RecorderMode::Dry => {}
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TemplateFailure {
    /// No room in the store: the rest of the walk only measures, so the
    /// next visit's store fits the whole template.
    Capacity,
    /// A count mismatch, an unrecordable dispatch or a forced flush.
    Shape,
}

/// The checkpoint a template rolls back to: everything it walked is
/// provisional until the walk ends whole.
struct Template {
    cursor: usize,
    /// The arena position the template's first command starts at.
    arena: (u32, u32),
    /// `replayed`, `recorded`, `direct` before the walk.
    stats: (u64, u64, u64),
    /// Arena bytes the walk's taken dispatches need.
    walk_bytes: usize,
    failure: Option<TemplateFailure>,
}

impl GpuEncoder {
    /// Run `body` as one gated round the GPU executes `copies` times, copy c
    /// by range entry `at.first + c * at.stride` (written on the GPU as for
    /// `begin_gated_segment`). Inside a replaying span the body is walked
    /// once and validated or recorded as a template of exactly `commands`
    /// gated dispatches, all of it provisional: if it cannot be taken whole
    /// (a count mismatch, an unrecordable dispatch, no room) every provisional
    /// allocation is rolled back, nothing of the walk ever executes, and
    /// `body` runs `copies` times directly, each dispatch an indirect dispatch
    /// on its gate. `Err` from the body encodes nothing on any path and is returned. The body
    /// must issue the same dispatches every time it runs.
    pub fn repeat_gated_template(
        &mut self,
        at: TemplateRanges<'_>,
        commands: u32,
        copies: u32,
        mut body: impl FnMut(&mut GatedRecorder<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        let executes = crate::replay::template_chunks(copies, at.chunk).count() as u32;
        let last = executes.checked_sub(1).and_then(|c| c.checked_mul(at.stride)).and_then(|c| c.checked_add(at.first));
        let end = last.and_then(|l| (u64::from(l) + 1).checked_mul(crate::replay::GATED_RANGE_BYTES));
        if commands == 0 || at.chunk == 0 || end.is_none_or(|end| end > at.ranges.size) {
            return Err(format!(
                "gated template: {commands} commands, {copies} rounds by {} in {executes} executes from entry {} by {} do not fit a {}-byte range buffer",
                at.chunk, at.first, at.stride, at.ranges.size
            ));
        }
        let mut walked = false;
        if let Some(mut span) = self.replay.take() {
            debug_assert!(span.template.is_none(), "templates never nest");
            self.close_segment(&mut span);
            match span.entry {
                None => span.cache.stats.templates_direct += 1,
                Some(entry) => {
                    let store = &span.cache.entries[entry].store;
                    let arena = store.commands.get(span.cursor).map_or(store.arena_cursor, |c| c.arena_before);
                    let stats = span.cache.stats;
                    span.template = Some(Template {
                        cursor: span.cursor,
                        arena,
                        stats: (stats.replayed, stats.recorded, stats.direct),
                        walk_bytes: 0,
                        failure: (span.mode == SpanMode::Full).then_some(TemplateFailure::Capacity),
                    });
                    span.segment = Some(OpenSegment {
                        ranges: at.ranges.raw.clone(),
                        offset: u64::from(at.first) * crate::replay::GATED_RANGE_BYTES,
                        commands,
                        taken: 0,
                        broken: false,
                        copies,
                        stride: at.stride,
                        chunk: at.chunk,
                    });
                    walked = true;
                }
            }
            self.replay = Some(span);
        }
        if walked {
            let result = body(&mut GatedRecorder { enc: self, mode: RecorderMode::Walk });
            let mut span = self.replay.take().expect("the walk keeps its span");
            let taken = span.segment.as_ref().map_or(0, |s| s.taken);
            if result.is_err() || taken != commands {
                Self::fail_template(&mut span, TemplateFailure::Shape);
            }
            let template = span.template.take().expect("the walk keeps its template");
            if template.failure.is_none() {
                self.close_segment(&mut span);
                let stats = &mut span.cache.stats;
                if stats.recorded > template.stats.1 {
                    stats.templates_recorded += 1;
                } else {
                    stats.templates_replayed += 1;
                }
                self.replay = Some(span);
                return Ok(());
            }
            span.segment = None;
            let stats = &mut span.cache.stats;
            (stats.replayed, stats.recorded, stats.direct) = template.stats;
            if result.is_ok() {
                stats.templates_direct += 1;
            }
            self.replay = Some(span);
            result?;
        } else {
            // No walk ran (no span, replay off, no idle entry): a body that
            // fails must fail before its first copy encodes anything.
            body(&mut GatedRecorder { enc: self, mode: RecorderMode::Dry })?;
        }
        for _ in 0..copies {
            body(&mut GatedRecorder { enc: self, mode: RecorderMode::Direct })?;
        }
        Ok(())
    }

    /// One dispatch of a template walk: taken into the open segment, or,
    /// once the template failed, measured (no room) or dropped.
    fn template_dispatch(&mut self, pipeline: &GpuComputePipeline, bindings: &[GpuBinding], groups: [u32; 3], label: &str) {
        let mut span = self.replay.take().expect("a template walks inside its span");
        match span.template.as_ref().and_then(|t| t.failure) {
            Some(TemplateFailure::Shape) => {}
            // The span is full: this only adds the dispatch to the shortage.
            Some(TemplateFailure::Capacity) => {
                self.replay_dispatch_in(&mut span, pipeline, bindings, Some(groups), None, label);
            }
            None => {
                let segment = span.segment.as_ref().expect("a template walks inside its segment");
                if segment.taken >= segment.commands {
                    Self::fail_template(&mut span, TemplateFailure::Shape);
                } else {
                    let gate = GateKey {
                        ranges: identity(&*segment.ranges),
                        offset: segment.offset,
                        commands: segment.commands,
                        slot: segment.taken,
                        copies: segment.copies,
                        stride: segment.stride,
                        chunk: segment.chunk,
                    };
                    if self.replay_dispatch_in(&mut span, pipeline, bindings, Some(groups), Some(gate), label) {
                        span.segment.as_mut().expect("checked above").taken += 1;
                    } else {
                        let why = if span.mode == SpanMode::Full { TemplateFailure::Capacity } else { TemplateFailure::Shape };
                        Self::fail_template(&mut span, why);
                    }
                }
            }
        }
        self.replay = Some(span);
    }

    /// Roll the open template back to its checkpoint: the recording and the
    /// store cut there, every segment buffer no kept command owns dropped
    /// (one made for a slot 0 whose bytes then didn't fit included), the
    /// arena cursor restored, and the span left recording at the checkpoint,
    /// or full when it ran out of room. A capacity failure adds the walk so
    /// far to the store's shortage; the rest of the walk adds the remainder.
    fn fail_template(span: &mut ReplaySpan, why: TemplateFailure) {
        let Some(template) = span.template.as_mut().filter(|t| t.failure.is_none()) else {
            return;
        };
        template.failure = Some(why);
        let entry = span.entry.expect("a template belongs to an entry");
        debug_assert!(span.pending_start <= template.cursor, "nothing of a template ever executes");
        let taken = span.segment.as_ref().map_or(0, |s| s.taken) as usize;
        let ReplayEntry { recording, store } = &mut span.cache.entries[entry];
        if why == TemplateFailure::Capacity {
            store.short_commands += taken;
            store.short_bytes += template.walk_bytes;
        }
        recording.truncate(template.cursor);
        store.truncate(template.cursor);
        let owned = store
            .commands
            .iter()
            .rev()
            .find_map(|c| match c.place {
                Place::Segment { segment, .. } => Some(segment + 1),
                Place::Chunk(_) => None,
            })
            .unwrap_or(0);
        store.segments.truncate(owned);
        store.arena_cursor = template.arena;
        span.cursor = template.cursor;
        span.mode = if why == TemplateFailure::Capacity { SpanMode::Full } else { SpanMode::Record };
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
impl GpuReplayCache {
    /// Gated segment buffers the cache's entries hold.
    pub(super) fn segment_buffers(&self) -> usize {
        self.entries.iter().map(|e| e.store.segments.len()).sum()
    }
}
