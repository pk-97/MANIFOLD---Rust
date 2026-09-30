//! Encode replay, the Metal store (`docs/ENCODE_REPLAY_DESIGN.md`): each
//! ring entry keeps its commands in indirect command buffer chunks, its
//! inline bytes in shared arenas, and a retain on every pipeline and buffer
//! it names. While a span is open, each recordable dispatch is checked
//! against the entry at the current position (`crate::replay`); matched
//! and newly recorded commands wait as one pending stretch, which runs with
//! a single execute the moment anything else needs the encoder.

use std::ops::Range;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::msg_send;
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
    ARENA_BYTES, BYTES_ALIGN, BytesSlot, CHUNK_COMMANDS, DispatchKey, GpuReplayStats, KeyBinding, MAX_ARENAS,
    MAX_CHUNKS, MAX_KEY_BINDINGS, Recording, pick_entry, replay_allowed_by_env,
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

struct StoredCommand {
    /// Retained so a recorded pipeline can't be freed while named here.
    _pipeline: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    retained_start: u32,
    resources_start: u32,
    arena_before: (u32, u32),
    label: Retained<NSString>,
}

#[derive(Default)]
struct ReplayStore {
    chunks: Vec<Retained<ProtocolObject<dyn MTLIndirectCommandBuffer>>>,
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

    /// The only place a store allocates.
    fn reserve(&mut self, device: &GpuDevice, stats: &mut GpuReplayStats) {
        let chunks = (self.commands.len() + self.short_commands).div_ceil(CHUNK_COMMANDS).clamp(1, MAX_CHUNKS);
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
            .clamp(1, MAX_ARENAS);
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

    fn truncate(&mut self, len: usize) {
        let Some(first) = self.commands.get(len) else {
            return;
        };
        self.retained.truncate(first.retained_start as usize);
        self.resources.truncate(first.resources_start as usize);
        self.arena_cursor = first.arena_before;
        self.commands.truncate(len);
    }

    /// Write `key` as the next command. `buffers` holds the buffer of every
    /// buffer binding and `slots` the arena slot of every inline binding,
    /// both in binding order.
    fn record(
        &mut self,
        pipeline: &GpuComputePipeline,
        key: &DispatchKey,
        buffers: &[Option<&GpuBuffer>],
        slots: &[BytesSlot],
        arena_before: (u32, u32),
        label: &str,
    ) {
        let index = self.commands.len();
        let retained_start = self.retained.len() as u32;
        let resources_start = self.resources.len() as u32;
        let command = unsafe { self.chunks[index / CHUNK_COMMANDS].indirectComputeCommandAtIndex(index % CHUNK_COMMANDS) };
        command.reset();
        command.setComputePipelineState(&pipeline.state);
        let mut buffers = buffers.iter();
        let mut slots = slots.iter();
        for binding in key.bindings() {
            match *binding {
                KeyBinding::Buffer { slot, offset, .. } => {
                    let buffer = buffers.next().copied().flatten().expect("one buffer per buffer binding");
                    unsafe { command.setKernelBuffer_offset_atIndex(&buffer.raw, offset as usize, slot as usize) };
                    self.resources.push(resource(&buffer.raw));
                    self.retained.push(buffer.raw.clone());
                }
                KeyBinding::Bytes { slot, data } => {
                    let store = *slots.next().expect("one arena slot per inline binding");
                    self.write_bytes(store, data);
                    let arena = &self.arenas[store.arena as usize].raw;
                    unsafe { command.setKernelBuffer_offset_atIndex(arena, store.offset as usize, slot as usize) };
                    self.resources.push(resource(arena));
                }
            }
        }
        command.concurrentDispatchThreadgroups_threadsPerThreadgroup(mtl_size(key.groups), mtl_size(pipeline.workgroup_size));
        command.setBarrier();
        self.commands.push(StoredCommand {
            _pipeline: pipeline.state.clone(),
            retained_start,
            resources_start,
            arena_before,
            label: NSString::from_str(&format!("replay: {label}")),
        });
    }

    /// Run commands `range` on `enc`, in order and each after the last,
    /// with everything encoded before them finished first. Returns the
    /// number of execute calls.
    fn execute(
        &mut self,
        enc: &ProtocolObject<dyn MTLComputeCommandEncoder>,
        range: Range<usize>,
        cmd_buf: &Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    ) -> u64 {
        let first = &self.commands[range.start];
        let resources_start = first.resources_start as usize;
        let resources_end = self.commands.get(range.end).map_or(self.resources.len(), |c| c.resources_start as usize);
        let mut executes = 0;
        unsafe {
            enc.pushDebugGroup(&first.label);
            enc.insertDebugSignpost(&first.label);
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
                let chunk = start / CHUNK_COMMANDS;
                let end = range.end.min((chunk + 1) * CHUNK_COMMANDS);
                let icb = &self.chunks[chunk];
                let () = msg_send![enc, useResource: &**icb, usage: MTLResourceUsage::Read];
                enc.executeCommandsInBuffer_withRange(icb, NSRange::new(start % CHUNK_COMMANDS, end - start));
                executes += 1;
                start = end;
            }
            enc.memoryBarrierWithScope(MTLBarrierScope::Buffers);
            enc.popDebugGroup();
        }
        self.last_cmd_buf = Some(cmd_buf.clone());
        executes
    }
}

fn new_chunk(device: &GpuDevice) -> Option<Retained<ProtocolObject<dyn MTLIndirectCommandBuffer>>> {
    let desc = MTLIndirectCommandBufferDescriptor::new();
    desc.setCommandTypes(MTLIndirectCommandType::ConcurrentDispatch);
    desc.setInheritPipelineState(false);
    desc.setInheritBuffers(false);
    desc.setMaxKernelBufferBindCount(ICB_BUFFER_SLOTS);
    unsafe {
        device.raw_device().newIndirectCommandBufferWithDescriptor_maxCommandCount_options(
            &desc,
            CHUNK_COMMANDS,
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
}

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
        self.replay = Some(ReplaySpan { cache, entry, cursor: 0, pending_start: 0, mode: SpanMode::Validate });
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
        if self.replay.as_ref().is_none_or(|span| span.cursor == span.pending_start) {
            return;
        }
        let mut span = self.replay.take().expect("checked above");
        self.flush_span(&mut span);
        self.replay = Some(span);
    }

    fn flush_span(&mut self, span: &mut ReplaySpan) {
        if span.cursor == span.pending_start {
            return;
        }
        let entry = span.entry.expect("a pending stretch belongs to an entry");
        let enc = self.ensure_compute_raw();
        let store = &mut span.cache.entries[entry].store;
        span.cache.stats.executes += store.execute(&enc, span.pending_start..span.cursor, &self.cmd_buf);
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
        let taken = self.replay_dispatch_in(&mut span, pipeline, bindings, groups, label);
        self.replay = Some(span);
        taken
    }

    fn replay_dispatch_in(
        &mut self,
        span: &mut ReplaySpan,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        groups: Option<[u32; 3]>,
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
                    span.cursor += 1;
                    span.cache.stats.replayed += 1;
                    return true;
                }
                // A miss: run what matched, cut the recording here, record on.
                self.flush_span(span);
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
        let fits = span.cursor < store.capacity() && {
            let (fits, count) = store.alloc_bytes(&key, &mut slots);
            fits && {
                recording.push(&key, &slots[..count]);
                store.record(pipeline, &key, &buffers[..buffer_count], &slots[..count], arena_before, label);
                true
            }
        };
        if fits {
            span.cursor += 1;
            span.cache.stats.recorded += 1;
            return true;
        }
        store.short_commands += 1;
        store.short_bytes += key.arena_bytes();
        span.mode = SpanMode::Full;
        span.cache.stats.direct += 1;
        self.flush_span(span);
        false
    }
}
