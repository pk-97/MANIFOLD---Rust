//! Retained particle-frame publications for `node.liquid_frame`
//! (`docs/GPU_FLIP_DISPLAY_HISTORY_DESIGN.md` section 3): every frame-end
//! endpoint is published into a slot of its own, several may be pending at
//! once, and the display selects the retired pair bracketing the requested
//! time. No slot is written except by its own publication and none reads
//! another. A slot is reused only when it is unpinned, outside the retained
//! history, and both the frame that wrote it and the last frame that read it
//! have completed on the GPU.

use manifold_gpu::{GpuBuffer, GpuDevice};

use crate::node_graph::fluid::display_blend;

/// Slots a history may hold, each admitted against device memory.
pub const H_MAX: usize = 16;

/// Per-slot field arrays beside the particles, by index.
pub const FIELD_SOLID: usize = 0;
pub const FIELD_INTERIOR: usize = 1;
/// Face axes x, y, z at `FIELD_FACES + axis`.
pub const FIELD_FACES: usize = 2;
/// Whitewater classes foam, bubble, spray, dust at `FIELD_WHITEWATER + k`.
pub const FIELD_WHITEWATER: usize = 5;
pub const FIELDS: usize = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotState {
    Free,
    Pending,
    Retired,
    Rejected,
    Obsolete,
}

/// What makes publications comparable: a change starts a new generation.
/// Field sizes are 0 for an unwired field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub epoch: u32,
    pub lattice: [u32; 7],
    pub fields: [u64; FIELDS],
}

#[derive(Clone, Copy, Debug)]
struct SlotMeta {
    state: SlotState,
    generation: u64,
    run: u64,
    capacity_tag: u64,
    seq: u64,
    t: f64,
    count: u32,
    identity: u32,
    writer_stamp: u64,
    reader_stamp: u64,
}

impl SlotMeta {
    const FREE: Self = Self {
        state: SlotState::Free,
        generation: 0,
        run: 0,
        capacity_tag: 0,
        seq: 0,
        t: 0.0,
        count: 0,
        identity: 0,
        writer_stamp: 0,
        reader_stamp: 0,
    };
}

/// The shown pair with everything the outputs carry. A held presentation is
/// re-emitted as it was; its blend is never recomputed against a new request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Presentation {
    pub a: usize,
    pub b: usize,
    pub t_a: f64,
    pub t_b: f64,
    pub blend: f32,
    pub span: f32,
    pub count_a: u32,
    pub count_b: u32,
    pub identity_a: u32,
    pub identity_b: u32,
    generation: u64,
    run: u64,
}

impl Presentation {
    /// The time the picture samples: t_A + blend × (t_B − t_A).
    pub fn presented_time(&self) -> f64 {
        self.t_a + f64::from(self.blend) * (self.t_b - self.t_a)
    }
}

/// How a layout update changed the history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutChange {
    None,
    /// New generation; the pin holds until the new one's first retirement.
    Generation,
    /// New lattice; nothing old is comparable, outputs wait for a retirement.
    Lattice,
}

/// The slot lifecycle, generations, runs and exact selection, free of GPU
/// storage so the contract is testable on the CPU.
#[derive(Debug, Default)]
pub struct HistoryCore {
    slots: Vec<SlotMeta>,
    layout: Option<Layout>,
    generation: u64,
    newest_submitted: Option<(u64, f64)>,
    next_seq: u64,
    runs: u64,
    last_retired: Option<(u64, u32, u64)>,
    run_capacity: u32,
    capacity_tag: u64,
    pinned: Option<Presentation>,
    skipped: u64,
}

impl HistoryCore {
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn state(&self, slot: usize) -> SlotState {
        self.slots[slot].state
    }

    pub fn pinned(&self) -> Option<&Presentation> {
        self.pinned.as_ref()
    }

    /// Endpoints skipped by exhaustion or refusal, over the node's lifetime.
    pub fn publications_skipped(&self) -> u64 {
        self.skipped
    }

    /// Slots holding anything: every state but Free.
    pub fn in_use(&self) -> usize {
        self.slots.iter().filter(|s| s.state != SlotState::Free).count()
    }

    fn is_pinned(&self, slot: usize) -> bool {
        self.pinned.is_some_and(|p| p.a == slot || p.b == slot)
    }

    /// Adopt this frame's layout. A different layout is always a new
    /// generation, never a return to an old one: a publication that went
    /// obsolete stays obsolete when its layout recurs.
    pub fn set_layout(&mut self, layout: Layout) -> LayoutChange {
        if self.layout == Some(layout) {
            return LayoutChange::None;
        }
        let lattice_changed = self.layout.is_some_and(|old| old.lattice != layout.lattice);
        self.layout = Some(layout);
        self.generation += 1;
        for slot in &mut self.slots {
            if slot.state == SlotState::Retired {
                slot.state = SlotState::Obsolete;
            }
        }
        if lattice_changed {
            self.pinned = None;
            LayoutChange::Lattice
        } else {
            LayoutChange::Generation
        }
    }

    /// Whether `time` is an endpoint not yet submitted in this generation.
    pub fn wants_publication(&self, time: f64) -> bool {
        self.newest_submitted.is_none_or(|(generation, t)| generation != self.generation || time > t)
    }

    /// The particle capacity a publication of `count` records needs; growth
    /// starts a run.
    pub fn capacity_for(&mut self, count: u32) -> u32 {
        if count > self.run_capacity {
            self.run_capacity = count;
            self.capacity_tag += 1;
        }
        self.run_capacity.max(1)
    }

    /// A Free slot, preferring `fits`, or None.
    pub fn free_slot(&self, fits: impl Fn(usize) -> bool) -> Option<usize> {
        let free = || (0..self.slots.len()).filter(|&i| self.slots[i].state == SlotState::Free);
        free().find(|&i| fits(i)).or_else(|| free().next())
    }

    pub fn can_grow(&self) -> bool {
        self.slots.len() < H_MAX
    }

    /// Add a Free slot (its storage is allocated by the caller).
    pub fn push_free(&mut self) -> usize {
        self.slots.push(SlotMeta::FREE);
        self.slots.len() - 1
    }

    /// `slot` now holds the encoded publication of the endpoint at `time`.
    pub fn begin(&mut self, slot: usize, time: f64, writer_stamp: u64) {
        let meta = &mut self.slots[slot];
        debug_assert_eq!(meta.state, SlotState::Free);
        *meta = SlotMeta {
            state: SlotState::Pending,
            generation: self.generation,
            run: 0,
            capacity_tag: self.capacity_tag,
            seq: self.next_seq,
            t: time,
            count: 0,
            identity: 0,
            writer_stamp,
            reader_stamp: meta.reader_stamp,
        };
        self.next_seq += 1;
        self.newest_submitted = Some((self.generation, time));
    }

    /// The endpoint at `time` gets no slot. It is counted once: held frames
    /// do not retry it, a newer endpoint is attempted normally.
    pub fn skip(&mut self, time: f64) {
        self.newest_submitted = Some((self.generation, time));
        self.skipped += 1;
    }

    /// Encoding into `slot` failed after work may have reached it; it is
    /// never selectable and frees once `writer_stamp` completes.
    pub fn fail(&mut self, slot: usize, writer_stamp: u64) {
        let meta = &mut self.slots[slot];
        meta.state = SlotState::Rejected;
        meta.writer_stamp = meta.writer_stamp.max(writer_stamp);
    }

    /// The offline commit-wait completed `slot`'s publication.
    pub fn release_writer(&mut self, slot: usize) {
        self.slots[slot].writer_stamp = 0;
    }

    /// Retire completed publications in publication order. `metadata` reads
    /// a completed slot's words (count, identity, accepted, 0). Runs are
    /// assigned here, from retired metadata only.
    pub fn retire(&mut self, complete: impl Fn(u64) -> bool, mut metadata: impl FnMut(usize) -> Option<[u32; 4]>) {
        loop {
            let next = (0..self.slots.len())
                .filter(|&i| self.slots[i].state == SlotState::Pending)
                .min_by_key(|&i| self.slots[i].seq);
            let Some(i) = next else { return };
            if !complete(self.slots[i].writer_stamp) {
                return;
            }
            if self.slots[i].generation != self.generation {
                self.slots[i].state = SlotState::Obsolete;
                continue;
            }
            match metadata(i).filter(|words| words[2] != 0) {
                None => self.slots[i].state = SlotState::Rejected,
                Some(words) => {
                    let key = (self.generation, words[1], self.slots[i].capacity_tag);
                    if self.last_retired != Some(key) {
                        self.runs += 1;
                        self.last_retired = Some(key);
                    }
                    let meta = &mut self.slots[i];
                    meta.state = SlotState::Retired;
                    meta.count = words[0];
                    meta.identity = words[1];
                    meta.run = self.runs;
                }
            }
        }
    }

    /// Select exactly at `c` from the retired endpoints of this generation
    /// and pin the result. B is the earliest with t > c, or the newest; A is
    /// B's predecessor in B's run, else B. Runs only move forward. With
    /// nothing retired in this generation the previous pin holds unchanged.
    pub fn select(&mut self, c: f64) -> Option<Presentation> {
        let min_run = self.pinned.filter(|p| p.generation == self.generation).map_or(0, |p| p.run);
        let mut candidates: Vec<usize> = (0..self.slots.len())
            .filter(|&i| {
                let s = &self.slots[i];
                s.state == SlotState::Retired && s.generation == self.generation && s.run >= min_run
            })
            .collect();
        candidates.sort_by_key(|&i| self.slots[i].seq);
        if candidates.is_empty() {
            return self.pinned;
        }
        let position = candidates.iter().position(|&i| self.slots[i].t > c).unwrap_or(candidates.len() - 1);
        let b = candidates[position];
        let a = position
            .checked_sub(1)
            .map(|p| candidates[p])
            .filter(|&a| self.slots[a].run == self.slots[b].run)
            .unwrap_or(b);
        let (sa, sb) = (self.slots[a], self.slots[b]);
        let (blend, span) = display_blend(c, sa.t, sb.t);
        let presentation = Presentation {
            a,
            b,
            t_a: sa.t,
            t_b: sb.t,
            blend,
            span,
            count_a: sa.count,
            count_b: sb.count,
            identity_a: sa.identity,
            identity_b: sb.identity,
            generation: self.generation,
            run: sb.run,
        };
        self.pinned = Some(presentation);
        for slot in &mut self.slots {
            if slot.state == SlotState::Retired && slot.generation == self.generation && slot.seq < sa.seq {
                slot.state = SlotState::Obsolete;
            }
        }
        self.pinned
    }

    /// This frame reads the pinned slots.
    pub fn stamp_readers(&mut self, stamp: u64) {
        if let Some(p) = self.pinned {
            for slot in [p.a, p.b] {
                let meta = &mut self.slots[slot];
                meta.reader_stamp = meta.reader_stamp.max(stamp);
            }
        }
    }

    /// Free every Rejected or Obsolete slot that is unpinned and whose
    /// writer and reader frames both completed. Run after this frame's pin
    /// is chosen and stamped.
    pub fn reclaim(&mut self, complete: impl Fn(u64) -> bool) {
        for i in 0..self.slots.len() {
            let s = self.slots[i];
            if matches!(s.state, SlotState::Rejected | SlotState::Obsolete)
                && !self.is_pinned(i)
                && complete(s.writer_stamp)
                && complete(s.reader_stamp)
            {
                self.slots[i] = SlotMeta { reader_stamp: 0, ..SlotMeta::FREE };
            }
        }
    }

    /// Check the Free invariant: no Free slot is pinned or has an
    /// incomplete stamp.
    pub fn free_slots_are_safe(&self, complete: impl Fn(u64) -> bool) -> bool {
        (0..self.slots.len()).all(|i| {
            let s = &self.slots[i];
            s.state != SlotState::Free || (!self.is_pinned(i) && complete(s.writer_stamp) && complete(s.reader_stamp))
        })
    }
}

/// One slot's storage, allocated whole before its first publication.
#[derive(Default)]
pub struct SlotBuffers {
    pub particles: Option<GpuBuffer>,
    pub metadata: Option<GpuBuffer>,
    pub fields: [Option<GpuBuffer>; FIELDS],
}

impl SlotBuffers {
    fn fits(&self, particle_bytes: u64, fields: &[u64; FIELDS]) -> bool {
        self.particles.as_ref().is_some_and(|b| b.size >= particle_bytes)
            && self.metadata.is_some()
            && self.fields.iter().zip(fields).all(|(buffer, &bytes)| match buffer {
                None => bytes == 0,
                Some(buffer) => buffer.size == bytes,
            })
    }
}

/// [`HistoryCore`] with each slot's GPU storage.
#[derive(Default)]
pub struct FrameHistory {
    pub core: HistoryCore,
    buffers: Vec<SlotBuffers>,
}

impl FrameHistory {
    pub fn slot(&self, index: usize) -> &SlotBuffers {
        &self.buffers[index]
    }

    /// A Free slot holding `particle_bytes` of particles and exactly the
    /// layout's fields, allocated or reallocated whole under one memory
    /// admission. `Ok(None)` when the budget is exhausted; `Err` names a
    /// refused allocation. Either way the caller skips the endpoint.
    pub fn acquire(&mut self, device: &GpuDevice, particle_bytes: u64, fields: &[u64; FIELDS]) -> Result<Option<usize>, String> {
        let slot = match self.core.free_slot(|i| self.buffers[i].fits(particle_bytes, fields)) {
            Some(slot) => slot,
            None if self.core.can_grow() => {
                self.buffers.push(SlotBuffers::default());
                self.core.push_free()
            }
            None => return Ok(None),
        };
        if self.buffers[slot].fits(particle_bytes, fields) {
            return Ok(Some(slot));
        }
        let total = particle_bytes + 16 + fields.iter().sum::<u64>();
        crate::node_graph::scene_modifier_expand::admit_candidate_bytes(device.modifier_memory_snapshot(), total)
            .map_err(|error| format!("a history slot needs {total} bytes the device cannot give: {error}"))?;
        // The replaced storage drops fence-retired; this slot's stamps are
        // complete, so nothing in flight still reads it.
        let mut fresh = SlotBuffers {
            particles: Some(device.try_create_buffer_shared(particle_bytes)?),
            metadata: Some(device.try_create_buffer_shared(16)?),
            fields: Default::default(),
        };
        for (field, &bytes) in fresh.fields.iter_mut().zip(fields) {
            if bytes > 0 {
                *field = Some(device.try_create_buffer_shared(bytes)?);
            }
        }
        self.buffers[slot] = fresh;
        Ok(Some(slot))
    }

    /// The four metadata words of a completed slot.
    pub fn metadata(&self, slot: usize) -> Option<[u32; 4]> {
        let ptr = self.buffers[slot].metadata.as_ref()?.mapped_ptr()?;
        // SAFETY: the caller only asks for slots whose writer completed; the
        // metadata buffer is shared and 16 bytes.
        let words = unsafe { std::slice::from_raw_parts(ptr.cast::<u32>(), 4) };
        Some([words[0], words[1], words[2], words[3]])
    }

    /// Retire completed publications, reading their metadata.
    pub fn retire(&mut self, complete: impl Fn(u64) -> bool) {
        let buffers = &self.buffers;
        self.core.retire(complete, |slot| {
            let ptr = buffers[slot].metadata.as_ref()?.mapped_ptr()?;
            // SAFETY: as in `metadata`.
            let words = unsafe { std::slice::from_raw_parts(ptr.cast::<u32>(), 4) };
            Some([words[0], words[1], words[2], words[3]])
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_physics::clock::SimulationClock;

    const LATTICE: [u32; 7] = [0, 0, 0, 1, 8, 8, 8];

    fn layout(epoch: u32) -> Layout {
        Layout { epoch, lattice: LATTICE, fields: [0; FIELDS] }
    }

    /// The history driven by frames, each with a frame stamp; a stamp
    /// completes `delay` frames after its frame.
    struct Model {
        core: HistoryCore,
        frame: u64,
        delay: u64,
        rejected: Vec<f64>,
        identity: u32,
        max_in_use: usize,
        shown: Vec<f64>,
    }

    impl Model {
        fn new(delay: u64) -> Self {
            Self { core: HistoryCore::default(), frame: 0, delay, rejected: Vec::new(), identity: 1, max_in_use: 0, shown: Vec::new() }
        }

        fn complete(frame: u64, delay: u64) -> impl Fn(u64) -> bool {
            move |stamp| stamp == 0 || stamp + delay <= frame
        }

        /// One live frame: retire, select at `c`, stamp, reclaim, publish.
        fn live_frame(&mut self, layout: Layout, time: f64, ready: bool, c: f64) -> Option<Presentation> {
            self.frame += 1;
            let stamp = self.frame;
            let complete = Self::complete(self.frame, self.delay);
            self.core.set_layout(layout);
            let rejected = self.rejected.clone();
            let identity = self.identity;
            let times: Vec<f64> = (0..self.core.len()).map(|i| self.core.slots[i].t).collect();
            self.core.retire(&complete, |i| Some([10, identity, u32::from(!rejected.contains(&times[i])), 0]));
            let shown = self.core.select(c);
            self.core.stamp_readers(stamp);
            self.core.reclaim(&complete);
            assert!(self.core.free_slots_are_safe(&complete));
            if ready && self.core.wants_publication(time) {
                match self.core.free_slot(|_| true).or_else(|| self.core.can_grow().then(|| self.core.push_free())) {
                    Some(slot) => self.core.begin(slot, time, stamp),
                    None => self.core.skip(time),
                }
            }
            self.max_in_use = self.max_in_use.max(self.core.in_use());
            if let Some(p) = shown {
                self.shown.push(p.t_b);
            }
            shown
        }

        /// Run the real clock at `fps` render / `hz` simulation for `frames`.
        fn run(&mut self, clock: &mut SimulationClock, fps: f64, hz: f64, speed: f32, frames: usize, start: usize) -> Vec<(manifold_physics::clock::ClockFrame, Option<Presentation>)> {
            (start..start + frames)
                .map(|f| {
                    let frame = clock.advance(f as f64 / fps, 1.0 / hz, speed, 0.0, false, false);
                    let shown = self.live_frame(layout(frame.epoch), frame.simulation_time, true, frame.display_time);
                    (frame, shown)
                })
                .collect()
        }
    }

    fn publish_retire(core: &mut HistoryCore, t: f64) -> usize {
        let slot = core.free_slot(|_| true).unwrap_or_else(|| core.push_free());
        core.begin(slot, t, 0);
        core.retire(|_| true, |_| Some([5, 1, 1, 0]));
        slot
    }

    #[test]
    fn frame_history_selector_matches_frame_ring_within_latest_pair() {
        for (c, expected) in [(1.0, (1.0, 2.0, 0.0)), (2.5, (1.0, 2.0, 1.0)), (0.5, (0.0, 1.0, 0.5))] {
            let mut core = HistoryCore::default();
            core.set_layout(layout(1));
            for t in [0.0, 1.0, 2.0] {
                publish_retire(&mut core, t);
            }
            let p = core.select(c).expect("retired endpoints");
            assert_eq!((p.t_a, p.t_b, f64::from(p.blend)), expected, "c = {c}");
            assert_eq!(p.presented_time(), expected.0 + expected.2 * (expected.1 - expected.0));
        }
    }

    #[test]
    fn frame_history_two_tick_frames_publish_frame_end_only() {
        // 27 fps against 30 Hz: some frames run two ticks.
        let mut model = Model::new(3);
        let mut clock = SimulationClock::default();
        let frames = model.run(&mut clock, 27.0, 30.0, 1.0, 270, 0);
        let two_tick = frames.iter().filter(|(f, _)| f.ticks == 2).count();
        assert!(two_tick > 0, "the cadence has two-tick frames");
        let mut published: Vec<f64> = model.core.slots.iter().filter(|s| s.state != SlotState::Free).map(|s| s.t).collect();
        published.sort_by(f64::total_cmp);
        let ends: Vec<f64> = frames.iter().filter(|(f, _)| f.ticks > 0).map(|(f, _)| f.simulation_time).collect();
        for t in published {
            assert!(ends.contains(&t), "only frame-end endpoints publish: {t}");
        }
        for (f, _) in frames.iter().filter(|(f, _)| f.ticks == 2) {
            let intermediate = f.simulation_time - 1.0 / 30.0;
            assert!(!model.shown.iter().any(|&t| (t - intermediate).abs() < 1e-9), "the intermediate tick is never shown");
        }
        assert_eq!(model.core.publications_skipped(), 0);
    }

    #[test]
    fn frame_history_generation_change_republishes_unchanged_time() {
        let mut core = HistoryCore::default();
        core.set_layout(layout(1));
        publish_retire(&mut core, 1.0);
        assert!(!core.wants_publication(1.0));
        let mut wider = layout(1);
        wider.fields[FIELD_INTERIOR] = 64;
        assert_eq!(core.set_layout(wider), LayoutChange::Generation);
        assert!(core.wants_publication(1.0), "a field change republishes at unchanged time");
        let mut moved = wider;
        moved.lattice[4] = 9;
        assert_eq!(core.set_layout(moved), LayoutChange::Lattice);
        assert!(core.pinned().is_none(), "a lattice change leaves nothing comparable");
        assert!(core.wants_publication(1.0));
    }

    #[test]
    fn frame_history_obsolete_publication_stays_obsolete_when_layout_recurs() {
        let mut core = HistoryCore::default();
        let a = layout(1);
        let mut b = a;
        b.fields[FIELD_SOLID] = 4;
        core.set_layout(a);
        let slot = core.push_free();
        core.begin(slot, 1.0, 7);
        core.set_layout(b);
        core.set_layout(a);
        core.retire(|_| true, |_| Some([5, 1, 1, 0]));
        assert_eq!(core.state(slot), SlotState::Obsolete);
        assert!(core.select(1.0).is_none());
    }

    #[test]
    fn frame_history_pinned_pair_survives_reset_until_first_retirement() {
        let mut core = HistoryCore::default();
        core.set_layout(layout(1));
        for t in [1.0, 2.0] {
            publish_retire(&mut core, t);
        }
        let held = core.select(1.5).expect("pair");
        core.set_layout(layout(2));
        assert_eq!(core.select(0.0), Some(held), "the old water shows unchanged after reset");
        core.reclaim(|_| true);
        assert_ne!(core.state(held.a), SlotState::Free, "a pinned slot is never freed");
        assert_ne!(core.state(held.b), SlotState::Free);
        let slot = core.free_slot(|_| true).unwrap_or_else(|| core.push_free());
        core.begin(slot, 0.0, 0);
        assert_eq!(core.select(0.0), Some(held), "pending is not shown");
        core.retire(|_| true, |_| Some([3, 2, 1, 0]));
        let cut = core.select(0.0).expect("new epoch");
        assert_eq!((cut.a, cut.b, cut.t_b), (slot, slot, 0.0));
        core.reclaim(|_| true);
        assert_eq!(core.state(held.a), SlotState::Free);
    }

    #[test]
    fn frame_history_run_boundary_cuts_forward() {
        let mut core = HistoryCore::default();
        core.set_layout(layout(1));
        for (t, identity) in [(1.0, 1), (2.0, 1), (3.0, 2), (4.0, 2)] {
            let slot = core.free_slot(|_| true).unwrap_or_else(|| core.push_free());
            core.begin(slot, t, 0);
            core.retire(|_| true, |_| Some([5, identity, 1, 0]));
        }
        let p = core.select(2.5).expect("pair");
        assert_eq!((p.t_a, p.t_b), (3.0, 3.0), "a pair never straddles a run");
        let p = core.select(1.5).expect("pair");
        assert_eq!((p.t_a, p.t_b), (3.0, 3.0), "the run cut never moves back");
        let p = core.select(3.5).expect("pair");
        assert_eq!((p.t_a, p.t_b), (3.0, 4.0));
        // Capacity growth is a run boundary too.
        let mut core = HistoryCore::default();
        core.set_layout(layout(1));
        core.capacity_for(10);
        publish_retire(&mut core, 1.0);
        core.capacity_for(20);
        publish_retire(&mut core, 2.0);
        let p = core.select(1.5).expect("pair");
        assert_eq!((p.t_a, p.t_b), (2.0, 2.0));
    }

    #[test]
    fn frame_history_every_free_transition_checks_both_stamps() {
        // A small deterministic generator: no external crate, seeded.
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        for _ in 0..50 {
            let mut core = HistoryCore::default();
            let mut epoch = 1;
            let mut fields = [0; FIELDS];
            let mut t = 0.0;
            for frame in 1..400u64 {
                let delay = 1 + next(4);
                let complete = move |stamp: u64| stamp == 0 || stamp + delay <= frame;
                match next(20) {
                    0 => epoch += 1,
                    1 => fields[FIELD_INTERIOR] = 4 * next(3),
                    2 => fields[FIELD_FACES] = 4 * next(2),
                    _ => {}
                }
                core.set_layout(Layout { epoch, lattice: LATTICE, fields });
                core.retire(complete, |_| Some([5, 1, u32::from(next(5) != 0), 0]));
                core.select(t - 0.05);
                core.stamp_readers(frame);
                core.reclaim(complete);
                assert!(core.free_slots_are_safe(complete), "no Free slot is pinned, retained or has an incomplete stamp");
                t += 1.0 / 30.0;
                if core.wants_publication(t) {
                    match core.free_slot(|_| true).or_else(|| core.can_grow().then(|| core.push_free())) {
                        Some(slot) if next(10) == 0 => {
                            // Encode failure or cancel after work reached it.
                            core.begin(slot, t, frame);
                            core.fail(slot, frame);
                        }
                        Some(slot) => core.begin(slot, t, frame),
                        None => core.skip(t),
                    }
                }
                for i in 0..core.len() {
                    let s = core.slots[i];
                    if s.state == SlotState::Retired {
                        assert_eq!(s.generation, core.generation, "an obsolete Pending never retires");
                    }
                }
            }
        }
    }

    #[test]
    fn frame_history_consecutive_rejections_hold_and_free() {
        let mut model = Model::new(2);
        let mut shown = None;
        for frame in 1..=30u32 {
            let t = f64::from(frame) / 30.0;
            if (10..=14).contains(&frame) {
                model.rejected.push(t);
            }
            if let Some(p) = model.live_frame(layout(1), t, true, t - 0.1) {
                shown = Some(p);
                assert!(!model.rejected.iter().any(|&r| r == p.t_a || r == p.t_b), "a rejected tick is never shown");
            }
        }
        assert!(shown.is_some());
        for _ in 0..5 {
            model.live_frame(layout(1), 1.0, false, 1.0);
        }
        let rejected = (0..model.core.len()).filter(|&i| model.core.state(i) == SlotState::Rejected).count();
        assert_eq!(rejected, 0, "rejected slots free once their stamps complete");
    }

    #[test]
    fn frame_history_allocation_refusal_and_budget_exhaustion_count_skips() {
        let mut core = HistoryCore::default();
        core.set_layout(layout(1));
        // Nothing ever completes: every slot stays Pending.
        for frame in 1..=H_MAX as u64 + 3 {
            let t = frame as f64;
            core.retire(|stamp| stamp == 0, |_| None);
            assert!(core.wants_publication(t));
            match core.free_slot(|_| true).or_else(|| core.can_grow().then(|| core.push_free())) {
                Some(slot) => core.begin(slot, t, frame),
                None => core.skip(t),
            }
            // A held frame does not retry the skipped endpoint.
            assert!(!core.wants_publication(t));
        }
        assert_eq!(core.len(), H_MAX);
        assert_eq!(core.publications_skipped(), 3, "each skipped endpoint counts once");
        // A refused allocation is the same skip.
        core.skip(100.0);
        assert!(!core.wants_publication(100.0));
        assert_eq!(core.publications_skipped(), 4);
        core.set_layout(layout(2));
        assert_eq!(core.publications_skipped(), 4, "the counter spans epochs");
    }

    #[test]
    fn frame_history_pause_holds_speed_scales_backwards_requested_holds() {
        let mut model = Model::new(3);
        let mut clock = SimulationClock::default();
        let ran = model.run(&mut clock, 60.0, 30.0, 1.0, 120, 0);
        let before = ran.last().and_then(|(_, p)| *p).expect("shown");
        // Speed 0: the simulation holds; the presentation converges and holds.
        let held = model.run(&mut clock, 60.0, 30.0, 0.0, 60, 120);
        let settled = held.last().and_then(|(_, p)| *p).expect("shown");
        assert!(settled.presented_time() >= before.presented_time());
        let again = model.run(&mut clock, 60.0, 30.0, 0.0, 10, 180);
        for (_, p) in &again {
            assert_eq!(*p, Some(settled), "paused frames show the same picture");
        }
        // Half Speed: presented time advances at half the transport rate.
        let half = model.run(&mut clock, 60.0, 30.0, 0.5, 240, 190);
        let (first, last) = (half[120].1.expect("shown"), half[239].1.expect("shown"));
        let rate = (last.presented_time() - first.presented_time()) / (119.0 / 60.0);
        assert!((rate - 0.5).abs() < 0.05, "half speed presents at half rate: {rate}");
        // A requested time behind every retained endpoint holds at the oldest.
        let p = model.core.select(-1.0).expect("shown");
        assert_eq!(p.a, p.b);
    }

    #[test]
    fn frame_history_coupled_exact_one_two_tick_rejection_reset() {
        // Coupled water presents exactly at the completed time: r = t_B.
        for ticks_per_frame in [1u32, 2] {
            let mut model = Model::new(2);
            let mut t = 0.0;
            for frame in 1..=60u32 {
                t += f64::from(ticks_per_frame) / 60.0;
                if frame == 20 {
                    model.rejected.push(t);
                }
                let epoch = if frame >= 40 { 2 } else { 1 };
                if frame == 40 {
                    t = 0.0;
                }
                // The completed time is at or past every retired endpoint.
                if let Some(p) = model.live_frame(layout(epoch), t, true, t) {
                    assert!(p.blend == 1.0, "coupled presents its newest endpoint whole");
                    assert!(!model.rejected.contains(&p.t_b));
                }
            }
            let last = model.core.pinned().expect("shown");
            assert!(last.t_b < 21.0 / 60.0 * f64::from(ticks_per_frame), "the reset epoch is shown");
        }
    }

    #[test]
    fn frame_history_slot_use_over_2000_frames() {
        // Section 3.8: pending plus retained from A, plus the pinned pair and
        // obsolete slots awaiting readers.
        for (fps, hz, delay, bound) in [(60.0, 30.0, 3, 4 + 2 + 6), (30.0, 30.0, 3, 6 + 2 + 6), (27.0, 30.0, 3, 6 + 2 + 6), (30.0, 30.0, 4, 6 + 2 + 6)] {
            let mut model = Model::new(delay);
            let mut clock = SimulationClock::default();
            model.run(&mut clock, fps, hz, 1.0, 2000, 0);
            assert!(model.max_in_use <= bound, "{fps}/{hz} R{delay}: {} slots in use", model.max_in_use);
            assert!(model.core.len() <= H_MAX);
            assert_eq!(model.core.publications_skipped(), 0);
        }
    }
}
