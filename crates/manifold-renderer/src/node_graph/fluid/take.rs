//! Resolved worker inputs for replay and higher-quality rebakes. This journal
//! records the existing handoff, not a second event or control evaluation path.
//! Geometry is stored once in the setup; rigid history reuses that geometry.
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use manifold_physics::input::AppliedEvent;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    CacheMode, FluidControls, FluidSettings, HISTORY_CAPACITY, Request, Sample, TICK, coupled,
    roles,
};
use crate::node_graph::physics_events::ResolvedNodeImpulse;

mod timing;
pub(super) use timing::{Capture, TimingHandoff};
pub use timing::{TakeRange, TakeTime};

const VERSION: u32 = 3;
const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;
const HEADER: &str = "take-header.zst";
const PROGRESS: &str = "take-progress.zst";
type Hash = [u8; 32];

/// A completed replay boundary. Surface, particles and rigid poses always
/// belong to this same tick. Consume receipts only when `advance` returns true;
/// they describe the most recently completed batch.
pub struct FluidTakeFrame<'a> {
    pub tick: u64,
    pub surface: &'a [crate::generators::mesh_common::MeshVertex],
    pub whitewater: &'a super::WhitewaterFrame,
    pub obstacle: super::Transform,
    pub stats: manifold_fluids::FrameStats,
    pub rigid: Option<&'a super::CoupledRigidFrame>,
    pub impulses: &'a [AppliedEvent<ResolvedNodeImpulse>],
}

/// Sequential replay through the existing native worker implementation.
/// Opening and advancing perform file I/O and native work: an offline job must
/// own this on its worker, never on the UI or audio thread. Each advance runs
/// at most the existing four-tick worker batch, irrespective of recording FPS.
pub struct FluidTakeReplay {
    reader: Reader,
    native: super::native::NativeSimulation,
    pending: Option<Request>,
    accepted: Option<super::Reply>,
    spare: Option<super::Reply>,
    failure: Option<String>,
}

impl FluidTakeReplay {
    pub fn open(directory: impl AsRef<Path>) -> Result<Self, String> {
        Ok(Self {
            reader: Reader::open(Arc::new(directory.as_ref().to_owned()))?,
            native: Default::default(),
            pending: None,
            accepted: None,
            spare: None,
            failure: None,
        })
    }

    /// Last committed input boundary, not a claim that a requested bake range
    /// finished. A failed recording can still have a replayable prefix.
    pub fn recorded_tick(&self) -> u64 {
        self.reader.completed_tick()
    }

    pub fn recording_failure(&self) -> Option<&str> {
        self.reader.progress.failed.as_deref()
    }

    /// Project range backed by completed native ticks, including recorded
    /// holds. Seconds-only legacy callers have no project timing provenance.
    pub fn project_range(&self) -> Option<TakeRange> {
        self.reader.clock_range
    }

    /// Resolve a requested project boundary on the owning offline worker.
    /// These bounded-memory scans perform I/O; do not call on a render thread.
    pub fn simulation_time_at(
        &self,
        transport: manifold_core::Seconds,
    ) -> Result<manifold_core::Seconds, String> {
        self.reader.resolve_time(transport.0, false)
    }

    pub fn simulation_time_at_beat(
        &self,
        beat: manifold_core::Beats,
    ) -> Result<manifold_core::Seconds, String> {
        self.reader.resolve_time(beat.0, true)
    }

    pub fn frame(&self) -> Option<FluidTakeFrame<'_>> {
        self.accepted.as_ref().map(|reply| FluidTakeFrame {
            tick: reply.tick,
            surface: &reply.vertices,
            whitewater: &reply.whitewater,
            obstacle: reply.obstacle,
            stats: reply.stats,
            rigid: reply.coupled.as_ref().map(|rigid| &rigid.output),
            impulses: &reply.impulses,
        })
    }

    /// Returns false after the committed prefix. Errors latch; subsequent calls
    /// cannot accidentally skip a failed native batch and publish later ticks.
    pub fn advance(&mut self) -> Result<bool, String> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let result = self.advance_inner();
        if let Err(error) = &result {
            self.failure = Some(error.clone());
        }
        result
    }

    fn advance_inner(&mut self) -> Result<bool, String> {
        const EPOCH: u64 = 1;
        if self.pending.is_none() {
            self.pending = self.reader.next_request(EPOCH)?;
        }
        let Some(pending) = self.pending.as_mut() else {
            return Ok(false);
        };
        let count = pending.count.min(super::BATCH);
        let end = pending.start_tick + count as u64;
        let mut history = self
            .spare
            .as_mut()
            .map(|reply| std::mem::take(&mut reply.history))
            .unwrap_or_default();
        history.clear();
        history.extend(pending.history.iter().cloned());
        let mut role_history = self
            .spare
            .as_mut()
            .map(|reply| std::mem::take(&mut reply.role_history))
            .unwrap_or_default();
        role_history.clear();
        role_history.extend_from_slice(&pending.role_history);
        let mut impulses = self
            .spare
            .as_mut()
            .map(|reply| std::mem::take(&mut reply.impulses))
            .unwrap_or_default();
        impulses.clear();
        impulses.extend(
            pending
                .impulses
                .iter()
                .filter(|event| {
                    event.applied.tick >= pending.start_tick && event.applied.tick < end
                })
                .map(|event| AppliedEvent {
                    source: event.source,
                    applied: event.applied,
                    lateness: event.lateness,
                    value: event.value.clone(),
                }),
        );
        let recycled_coupled = self.spare.as_mut().and_then(|reply| reply.coupled.take());
        let coupled = pending.coupled.as_ref().map(|rigid| {
            let (mut history, output) = recycled_coupled
                .map(|old| (old.history, old.output))
                .unwrap_or_default();
            history.clear();
            history.extend(rigid.history.iter().cloned());
            coupled::Request {
                setup: Arc::clone(&rigid.setup),
                history,
                output,
            }
        });
        let request = Request {
            epoch: EPOCH,
            settings: pending.settings,
            initial: pending.initial,
            start_tick: pending.start_tick,
            count,
            history,
            role_setup: Arc::clone(&pending.role_setup),
            role_history,
            impulses,
            recycle: self
                .spare
                .as_mut()
                .map(|reply| std::mem::take(&mut reply.vertices))
                .unwrap_or_default(),
            recycle_whitewater: self
                .spare
                .as_mut()
                .map(|reply| std::mem::take(&mut reply.whitewater))
                .unwrap_or_default(),
            cache_mode: CacheMode::Live,
            cache_path: Arc::clone(&pending.cache_path),
            coupled,
            timing: TimingHandoff::default(),
        };
        let reply = self.native.process(request, &AtomicU64::new(EPOCH));
        if let Some(error) = &reply.error {
            return Err(error.clone());
        }
        if reply.tick != end
            || reply.coupled.as_ref().is_some_and(|rigid| {
                rigid.output.stamp
                    != manifold_physics::TickStamp {
                        epoch: EPOCH,
                        tick: end,
                    }
            })
        {
            return Err("Physics take: native replay returned an incomplete boundary".into());
        }
        self.spare = self.accepted.replace(reply);
        pending.start_tick = end;
        pending.count -= count;
        if pending.count == 0 {
            self.pending = None;
        }
        Ok(true)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Header {
    version: u32,
    upstream_revision: String,
    numerics_revision: u32,
    solver_identity: Hash,
    fixed_tick: f64,
    epoch: u64,
    settings: FluidSettings,
    initial: FluidControls,
    role_setup: Arc<roles::Setup>,
    coupled_setup: Option<Arc<coupled::Setup>>,
    clock_origin: Option<TakeTime>,
}

/// Only a committed prefix is playable. No progress record claims a complete
/// user-selected bake range; that requires the owning job to finish its range.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Progress {
    header_hash: Hash,
    completed_tick: u64,
    last_batch_hash: Hash,
    failed: Option<String>,
    records: u64,
    clock_end: Option<TakeTime>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Batch {
    header_hash: Hash,
    previous_hash: Hash,
    start_tick: u64,
    completed_count: usize,
    started_tick: u64,
    history: Vec<Sample>,
    role_history: Vec<roles::Controls>,
    coupled_history: Option<Vec<coupled::Sample>>,
    impulses: Vec<AppliedEvent<ResolvedNodeImpulse>>,
    clock: Vec<TakeTime>,
}

pub(super) struct Writer {
    directory: Arc<PathBuf>,
    progress: Progress,
}

impl Writer {
    pub fn create(directory: Arc<PathBuf>, request: &Request) -> Result<Self, String> {
        if request.start_tick != 0 || request.epoch == 0 {
            return Err(
                "Physics take: recording must start at an initialized epoch boundary".into(),
            );
        }
        let header = Header {
            version: VERSION,
            upstream_revision: manifold_fluids::UPSTREAM_REVISION.into(),
            numerics_revision: manifold_fluids::NUMERICS_REVISION,
            solver_identity: super::identity::solver_identity(),
            fixed_tick: TICK,
            epoch: request.epoch,
            settings: request.settings,
            initial: request.initial,
            role_setup: Arc::clone(&request.role_setup),
            coupled_setup: request
                .coupled
                .as_ref()
                .map(|rigid| Arc::clone(&rigid.setup)),
            clock_origin: request.timing.points.first().copied(),
        };
        if let Some(origin) = header.clock_origin {
            timing::validate_point(origin)?;
            if origin.simulation.0 != 0.0 {
                return Err("Physics take: project timing must begin at simulation zero".into());
            }
        }
        let header_hash = write_new(&directory.join(HEADER), &header)?;
        let progress = Progress {
            header_hash,
            completed_tick: 0,
            last_batch_hash: header_hash,
            failed: None,
            records: 0,
            clock_end: header.clock_origin,
        };
        publish_progress(&directory, &progress)?;
        Ok(Self {
            directory,
            progress,
        })
    }

    /// Called after native processing and before publishing its reply. Failed
    /// attempts retain an honest completed prefix, including the input record
    /// for any native tick that started but did not complete.
    pub fn append(
        &mut self,
        request: &Request,
        completed: usize,
        started_tick: u64,
        failure: Option<&str>,
    ) -> Result<(), String> {
        if request.count == 0 && request.timing.points.is_empty() {
            if let Some(failure) = failure {
                self.progress.failed = Some(failure.to_owned());
                publish_progress(&self.directory, &self.progress)?;
            }
            return Ok(());
        }
        if self.progress.failed.is_some()
            || request.start_tick != self.progress.completed_tick
            || completed > request.count
            || request.start_tick.checked_add(completed as u64).is_none()
        {
            return Err("Physics take: nonconsecutive or invalid worker prefix".into());
        }
        let records = self
            .progress
            .records
            .checked_add(1)
            .ok_or("Physics take: input record count overflows")?;
        let clock_end = validate_clock(&request.timing.points, self.progress.clock_end)?;
        let mut coupled_history = request.coupled.as_ref().map(|rigid| rigid.history.clone());
        if let Some(history) = &mut coupled_history {
            for sample in history {
                for body in sample
                    .inputs
                    .bodies
                    .iter_mut()
                    .flatten()
                    .chain(sample.inputs.prototype.iter_mut())
                {
                    body.collider = None;
                }
            }
        }
        let batch = Batch {
            header_hash: self.progress.header_hash,
            previous_hash: self.progress.last_batch_hash,
            start_tick: request.start_tick,
            completed_count: completed,
            started_tick,
            history: request.history.clone(),
            role_history: request.role_history.clone(),
            coupled_history,
            impulses: request
                .impulses
                .iter()
                .map(|event| AppliedEvent {
                    source: event.source,
                    applied: event.applied,
                    lateness: event.lateness,
                    value: event.value.clone(),
                })
                .collect(),
            clock: request.timing.points.clone(),
        };
        let hash = write_new(&batch_path(&self.directory, self.progress.records), &batch)?;
        let next = Progress {
            header_hash: self.progress.header_hash,
            completed_tick: request.start_tick + completed as u64,
            last_batch_hash: hash,
            failed: failure.map(str::to_owned),
            records,
            clock_end,
        };
        publish_progress(&self.directory, &next)?;
        self.progress = next;
        Ok(())
    }
}

pub(super) struct Reader {
    directory: Arc<PathBuf>,
    header: Header,
    progress: Progress,
    next_tick: u64,
    previous_hash: Hash,
    next_record: u64,
    clock_range: Option<TakeRange>,
}

impl Reader {
    pub fn open(directory: Arc<PathBuf>) -> Result<Self, String> {
        let (header, header_hash): (Header, Hash) = read_record(&directory.join(HEADER))?;
        let (progress, _): (Progress, Hash) = read_record(&directory.join(PROGRESS))?;
        if header.version != VERSION
            || header.epoch == 0
            || header.upstream_revision != manifold_fluids::UPSTREAM_REVISION
            || header.numerics_revision != manifold_fluids::NUMERICS_REVISION
            || header.solver_identity != super::identity::solver_identity()
            || header.fixed_tick.to_bits() != TICK.to_bits()
            || progress.header_hash != header_hash
            || progress.completed_tick > (1_u64 << 53) - 1
        {
            return Err("Physics take: incompatible solver, schema or setup identity".into());
        }
        header.settings.validate()?;
        if let Some(origin) = header.clock_origin {
            timing::validate_point(origin)?;
            if origin.simulation.0 != 0.0 {
                return Err("Physics take: invalid project clock origin".into());
            }
        }
        header.initial.validate()?;
        header.role_setup.validate_recording()?;
        if let Some(rigid) = &header.coupled_setup {
            rigid.initial.validate_recording()?;
            super::CoupledRigidInputs {
                scene: &rigid.initial,
                colliders: rigid.colliders,
                density: rigid.density,
            }
            .validate()?;
        }
        // The writer only creates immutable batch files. Verify the complete
        // committed chain before any native work, so a replaced early batch
        // cannot publish output before a later link reveals the mismatch.
        let mut tick = 0;
        let mut previous_hash = header_hash;
        let mut clock_end = header.clock_origin;
        let mut clock_range = header
            .clock_origin
            .map(|start| TakeRange { start, end: start });
        let mut completed_time = progress.completed_tick as f64 * TICK;
        if let Some(end) = progress.clock_end {
            timing::validate_point(end)?;
            // The scheduler publishes the last complete fixed tick. Preserve
            // a held fractional remainder when that tick is already complete;
            // requiring an exact tick boundary would erase the entire hold.
            if super::simulation_tick(end.simulation.0) <= progress.completed_tick {
                completed_time = end.simulation.0;
            }
        }
        for record in 0..progress.records {
            let (batch, hash): (Batch, Hash) = read_record(&batch_path(&directory, record))?;
            let end = batch
                .start_tick
                .checked_add(batch.completed_count as u64)
                .ok_or("Physics take: tick range overflows")?;
            if batch.start_tick != tick
                || end > progress.completed_tick
                || batch.started_tick < end
                || batch.header_hash != header_hash
                || batch.previous_hash != previous_hash
            {
                return Err("Physics take: invalid committed input chain".into());
            }
            let next_clock_end = validate_clock(&batch.clock, clock_end)?;
            for &point in &batch.clock {
                if let Some(range) = &mut clock_range {
                    if point.simulation.0 <= completed_time {
                        range.end = point;
                    } else if let Some(previous) = clock_end
                        && previous.simulation.0 < completed_time
                    {
                        range.end = interpolate_time(
                            previous,
                            point,
                            (completed_time - previous.simulation.0)
                                / (point.simulation.0 - previous.simulation.0),
                        );
                    }
                }
                clock_end = Some(point);
            }
            clock_end = next_clock_end;
            tick = end;
            previous_hash = hash;
        }
        if tick != progress.completed_tick
            || previous_hash != progress.last_batch_hash
            || clock_end != progress.clock_end
        {
            return Err("Physics take: committed input hash mismatch".into());
        }
        Ok(Self {
            directory,
            header,
            progress,
            next_tick: 0,
            previous_hash: header_hash,
            next_record: 0,
            clock_range,
        })
    }

    pub fn completed_tick(&self) -> u64 {
        self.progress.completed_tick
    }

    fn resolve_time(&self, value: f64, by_beat: bool) -> Result<manifold_core::Seconds, String> {
        let range = self
            .clock_range
            .ok_or("Physics take: no project timing was recorded")?;
        let coordinate = |point: TakeTime| {
            if by_beat {
                point.beat.0
            } else {
                point.transport.0
            }
        };
        if !value.is_finite() || value < coordinate(range.start) || value > coordinate(range.end) {
            return Err(
                "Physics take: requested time is outside the completed project range".into(),
            );
        }
        let mut previous = range.start;
        let mut resolved = (value == coordinate(previous)).then_some(previous.simulation);
        let mut previous_hash = self.progress.header_hash;
        for record in 0..self.progress.records {
            let (batch, hash): (Batch, Hash) = read_record(&batch_path(&self.directory, record))?;
            if batch.header_hash != self.progress.header_hash
                || batch.previous_hash != previous_hash
            {
                return Err("Physics take: project timing hash chain changed".into());
            }
            validate_clock(&batch.clock, Some(previous))?;
            for point in batch.clock {
                if resolved.is_none() && coordinate(point) >= value {
                    let width = coordinate(point) - coordinate(previous);
                    resolved = Some(if width == 0.0 {
                        point.simulation
                    } else {
                        interpolate_time(previous, point, (value - coordinate(previous)) / width)
                            .simulation
                    });
                }
                previous = point;
            }
            previous_hash = hash;
        }
        if previous_hash != self.progress.last_batch_hash
            || Some(previous) != self.progress.clock_end
        {
            return Err("Physics take: committed project timing changed".into());
        }
        resolved
            .ok_or_else(|| "Physics take: recorded project time has no simulation boundary".into())
    }

    /// Replay uses the recorded assigned ticks. Only epoch identity is remapped
    /// to the new owner; no event is re-admitted through a different clock.
    pub fn next_request(&mut self, epoch: u64) -> Result<Option<Request>, String> {
        if epoch == 0 {
            return Err("Physics take: replay epoch must be nonzero".into());
        }
        loop {
            if self.next_record == self.progress.records {
                return Ok(None);
            }
            let (mut batch, hash): (Batch, Hash) =
                read_record(&batch_path(&self.directory, self.next_record))?;
            let end = batch
                .start_tick
                .checked_add(batch.completed_count as u64)
                .ok_or("Physics take: tick range overflows")?;
            if batch.header_hash != self.progress.header_hash
                || batch.previous_hash != self.previous_hash
                || batch.start_tick != self.next_tick
                || end > self.progress.completed_tick
                || batch.started_tick < end
                || end > (1_u64 << 53) - 1
                || batch.history.is_empty()
                || batch.history.len() > HISTORY_CAPACITY
                || batch.role_history.len() != batch.history.len() * self.header.role_setup.len()
                || batch.impulses.len() > super::impulses::IMPULSE_CAPACITY
                || (self.next_record + 1 == self.progress.records
                    && hash != self.progress.last_batch_hash)
            {
                return Err("Physics take: corrupt input prefix or hash chain".into());
            }
            let mut previous_time = f64::NEG_INFINITY;
            for sample in &batch.history {
                if !sample.time.is_finite() || sample.time < 0.0 || sample.time < previous_time {
                    return Err("Physics take: invalid continuous input order".into());
                }
                sample.controls.validate()?;
                previous_time = sample.time;
            }
            self.header
                .role_setup
                .validate_history(&batch.role_history)?;
            let coupled = match (&self.header.coupled_setup, batch.coupled_history.take()) {
                (None, None) => None,
                (Some(setup), Some(mut history)) => {
                    if history.is_empty() || history.len() > HISTORY_CAPACITY {
                        return Err("Physics take: invalid rigid input history".into());
                    }
                    let mut previous = None;
                    for sample in &mut history {
                        if !sample.time.0.is_finite()
                            || sample.time.0 < 0.0
                            || sample.sequence == 0
                            || previous.is_some_and(|(sequence, time)| {
                                sample.sequence <= sequence || sample.time.0 < time
                            })
                        {
                            return Err("Physics take: invalid rigid input order".into());
                        }
                        previous = Some((sample.sequence, sample.time.0));
                        for (body, initial) in sample
                            .inputs
                            .bodies
                            .iter_mut()
                            .zip(&setup.initial.bodies)
                            .chain(std::iter::once((
                                &mut sample.inputs.prototype,
                                &setup.initial.prototype,
                            )))
                        {
                            match (body, initial) {
                                (Some(body), Some(initial)) if body.collider.is_none() => {
                                    body.collider = initial.collider.clone()
                                }
                                (None, None) => {}
                                _ => {
                                    return Err(
                                        "Physics take: rigid geometry layout changed".into()
                                    );
                                }
                            }
                        }
                        if !setup.initial.same_topology(&sample.inputs) {
                            return Err(
                                "Physics take: rigid topology changed within an epoch".into()
                            );
                        }
                        sample.inputs.validate_recording()?;
                        if sample.inputs.bodies.iter().zip(&setup.initial.bodies).any(
                            |(body, initial)| {
                                body.as_ref().and_then(|b| b.fragment_parent)
                                    != initial.as_ref().and_then(|b| b.fragment_parent)
                            },
                        ) {
                            return Err("Physics take: rigid fragment layout changed".into());
                        }
                    }
                    Some(coupled::Request {
                        setup: Arc::clone(setup),
                        history,
                        output: Default::default(),
                    })
                }
                _ => return Err("Physics take: missing or unexpected rigid inputs".into()),
            };
            batch.impulses.retain(|event| event.applied.tick < end);
            let mut previous_event = None;
            for event in &mut batch.impulses {
                if event.applied.tick < batch.start_tick
                    || !event.source.time.0.is_finite()
                    || !event.lateness.0.is_finite()
                    || event.lateness.0 < 0.0
                    || event.source.epoch != event.applied.epoch
                    || event.source.epoch != self.header.epoch
                    || previous_event
                        .is_some_and(|key| (event.applied.tick, event.source.sequence) <= key)
                {
                    return Err("Physics take: invalid assigned impulse".into());
                }
                if let Some(targets) = event.value.target.rigid_targets() {
                    let setup = self
                        .header
                        .coupled_setup
                        .as_ref()
                        .ok_or("Physics take: rigid impulse has no owner")?;
                    setup.validate_impulse_targets(targets)?;
                }
                previous_event = Some((event.applied.tick, event.source.sequence));
                event.source.epoch = epoch;
                event.applied.epoch = epoch;
            }
            self.next_tick = end;
            self.next_record += 1;
            self.previous_hash = hash;
            if batch.completed_count == 0 {
                continue;
            }
            return Ok(Some(Request {
                epoch,
                settings: self.header.settings,
                initial: self.header.initial,
                start_tick: batch.start_tick,
                count: batch.completed_count,
                history: batch.history,
                impulses: batch.impulses,
                role_setup: Arc::clone(&self.header.role_setup),
                role_history: batch.role_history,
                recycle: Vec::new(),
                recycle_whitewater: Default::default(),
                cache_mode: CacheMode::Live,
                cache_path: Arc::new(PathBuf::new()),
                coupled,
                timing: TimingHandoff::default(),
            }));
        }
    }
}

fn validate_clock(
    points: &[TakeTime],
    previous: Option<TakeTime>,
) -> Result<Option<TakeTime>, String> {
    let Some(mut previous) = previous else {
        return if points.is_empty() {
            Ok(None)
        } else {
            Err("Physics take: timing was added without a project clock origin".into())
        };
    };
    if points.len() > HISTORY_CAPACITY || points.first() != Some(&previous) {
        return Err("Physics take: project clock history is missing its continuity anchor".into());
    }
    for &point in points {
        timing::validate_step(previous, point)?;
        previous = point;
    }
    Ok(Some(previous))
}

fn interpolate_time(a: TakeTime, b: TakeTime, fraction: f64) -> TakeTime {
    if fraction <= 0.0 {
        return a;
    }
    if fraction >= 1.0 {
        return b;
    }
    TakeTime {
        beat: manifold_core::Beats(a.beat.0 + (b.beat.0 - a.beat.0) * fraction),
        transport: manifold_core::Seconds(
            a.transport.0 + (b.transport.0 - a.transport.0) * fraction,
        ),
        simulation: manifold_core::Seconds(
            a.simulation.0 + (b.simulation.0 - a.simulation.0) * fraction,
        ),
    }
}

fn batch_path(directory: &Path, record: u64) -> PathBuf {
    directory.join(format!("take_{record:012}.zst"))
}

struct BoundedBytes(Vec<u8>);
impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_RECORD_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("physics input record exceeds 64 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn write_new<T: Serialize>(path: &Path, record: &T) -> Result<Hash, String> {
    let mut bytes = BoundedBytes(Vec::new());
    serde_json::to_writer(&mut bytes, record)
        .map_err(|e| format!("Physics take: serialize: {e}"))?;
    let hash = Sha256::digest(&bytes.0).into();
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("Physics take: create {}: {e}", path.display()))?;
    let mut encoder = zstd::stream::write::Encoder::new(file, 3).map_err(|e| e.to_string())?;
    encoder.include_checksum(true).map_err(|e| e.to_string())?;
    encoder.write_all(&bytes.0).map_err(|e| e.to_string())?;
    encoder
        .finish()
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(hash)
}

fn publish_progress(directory: &Path, progress: &Progress) -> Result<(), String> {
    let pending = directory.join(".take-progress.pending");
    write_new(&pending, progress)?;
    fs::rename(&pending, directory.join(PROGRESS))
        .map_err(|e| format!("Physics take: publish prefix: {e}"))
}

fn read_record<T: serde::de::DeserializeOwned>(path: &Path) -> Result<(T, Hash), String> {
    let file =
        File::open(path).map_err(|e| format!("Physics take: open {}: {e}", path.display()))?;
    let mut decoder = zstd::stream::read::Decoder::new(file).map_err(|e| e.to_string())?;
    decoder.window_log_max(23).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    decoder
        .take(MAX_RECORD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err("Physics take: input record exceeds 64 MiB".into());
    }
    let hash = Sha256::digest(&bytes).into();
    let record =
        serde_json::from_slice(&bytes).map_err(|e| format!("Physics take: decode: {e}"))?;
    Ok((record, hash))
}

#[cfg(test)]
pub(super) mod tests;
