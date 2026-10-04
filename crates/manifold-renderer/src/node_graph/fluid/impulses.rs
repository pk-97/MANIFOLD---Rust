//! Resolved, bounded impulse delivery through the existing worker handoff.
//!
//! Sending a worker request seals its input ticks. Later arrivals are assigned
//! after that batch; the reply records which assigned ticks actually began.

use manifold_physics::input::{AppliedEvent, EventQueue, EventStamp};
use manifold_physics::{FieldValue, TickStamp, VectorField};

use super::{FluidRuntime, TICK};
use crate::node_graph::physics_events::{ImpulseTarget, ResolvedNodeImpulse};
use manifold_core::Seconds;

pub(super) const IMPULSE_CAPACITY: usize = 256;

pub(super) fn new_queue() -> EventQueue<ResolvedNodeImpulse> {
    EventQueue::new(1, Seconds::ZERO, Seconds(TICK), IMPULSE_CAPACITY)
        .expect("fixed fluid impulse queue configuration is valid")
}

impl FluidRuntime {
    /// Epoch for already resolved inputs. Call `observe` before capturing an
    /// impulse; its timestamp is simulation-relative time, not transport time.
    pub fn impulse_epoch(&self) -> Option<u64> {
        self.settings.map(|_| self.epoch)
    }

    /// Capture an impulse timestamp from the observation that was accepted for
    /// this exact transport value. Playback and cache recording have no live
    /// impulse clock, and observations never extrapolate native time.
    pub fn impulse_stamp(&self, transport: Seconds, sequence: u64) -> Result<EventStamp, String> {
        if !transport.0.is_finite() {
            return Err("Water: impulse transport must be finite".into());
        }
        if self.cache_mode != super::CacheMode::Live {
            return Err("Water: impulses require Live mode until input takes are recorded".into());
        }
        if self.settings.is_none() {
            return Err("Water: no initialized impulse epoch".into());
        }
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let Some((accepted_transport, native_time)) = self.accepted_observation else {
            return Err("Water: no successfully accepted observation at this transport".into());
        };
        if accepted_transport != transport.0 {
            return Err("Water: impulse transport does not match the accepted observation".into());
        }
        Ok(EventStamp {
            epoch: self.epoch,
            time: Seconds(native_time),
            sequence,
        })
    }

    /// Capture a scene-space delta-velocity field, in metres per second.
    /// The returned tick is a scheduling decision, not native completion.
    /// An in-flight request owns all its tick inputs, so arrivals during that
    /// request are late until the next unscheduled batch boundary.
    pub fn enqueue_impulse(
        &mut self,
        stamp: EventStamp,
        field: FieldValue,
    ) -> Result<TickStamp, String> {
        self.enqueue_scene_impulse(
            stamp,
            ResolvedNodeImpulse {
                field,
                target: ImpulseTarget::Fluid,
            },
        )
    }

    /// Admit one resolved scene event to the shared queue. A combined target
    /// applies the captured field once to each solver under one source stamp.
    pub fn enqueue_scene_impulse(
        &mut self,
        stamp: EventStamp,
        impulse: ResolvedNodeImpulse,
    ) -> Result<TickStamp, String> {
        if self.settings.is_none() {
            return Err("Water: observe the domain before capturing an impulse".into());
        }
        if self.cache_mode != super::CacheMode::Live {
            return Err("Water: impulses require Live mode until input takes are recorded".into());
        }
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if let Some(targets) = impulse.target.rigid_targets() {
            self.coupled
                .as_ref()
                .ok_or("Water: rigid impulses require a connected rigid world")?
                .validate_impulse_targets(targets)?;
        }
        // Pause and Simulation Speed 0 discard incoming events, so resume never
        // bursts (WATER_SIMULATION_DESIGN.md "Transport pause / water speed
        // zero"). Admission succeeds so the producer rearms; no receipt will
        // ever name the returned tick.
        if self.held.is_held() {
            return Ok(self.impulses.next_tick());
        }
        // Includes the worker-owned batch and undrained delivery receipts.
        // Neither a busy worker nor a slow recorder can grow memory silently.
        if self.impulse_outstanding == IMPULSE_CAPACITY {
            let error = "Water: impulse history is full; restart the simulation or bake the scene";
            self.failure = Some(error.into());
            return Err(error.into());
        }
        let tick = self
            .impulses
            .enqueue(stamp, impulse)
            .map_err(|error| format!("Water impulse: {error}"))?;
        self.impulse_outstanding += 1;
        Ok(tick)
    }

    /// Drain standalone liquid receipts, leaving rigid or combined receipts
    /// for `drain_scene_impulses`. Dropping this iterator retains unread events.
    pub fn drain_applied_impulses(
        &mut self,
    ) -> impl Iterator<Item = AppliedEvent<FieldValue>> + '_ {
        let outstanding = &mut self.impulse_outstanding;
        self.applied_impulses
            .extract_if(.., |event| event.value.target == ImpulseTarget::Fluid)
            .map(move |event| {
                *outstanding -= 1;
                AppliedEvent {
                    source: event.source,
                    applied: event.applied,
                    lateness: event.lateness,
                    value: event.value.field,
                }
            })
    }

    /// These receipts identify ticks begun by the native worker, including a
    /// tick that subsequently failed. They never assert native completion.
    pub fn drain_scene_impulses(
        &mut self,
    ) -> impl Iterator<Item = AppliedEvent<ResolvedNodeImpulse>> + '_ {
        self.impulse_outstanding -= self.applied_impulses.len();
        self.applied_impulses.drain(..)
    }

    pub(super) fn prepare_impulse_batch(
        &mut self,
        start_tick: u64,
        count: usize,
        schedule: Option<&manifold_physics::clock::ClockFrame>,
    ) -> Result<Vec<AppliedEvent<ResolvedNodeImpulse>>, String> {
        let mut events = self
            .spare_impulses
            .take()
            .expect("one recycled impulse batch per worker request");
        events.clear();
        for index in 0..count {
            let tick = start_tick + index as u64;
            let stamp = TickStamp { epoch: self.epoch, tick };
            let result = if let Some(frame) = schedule {
                let Some(interval) = tick.checked_sub(frame.first_sequence)
                    .and_then(|ordinal| frame.interval(ordinal)) else {
                    self.spare_impulses = Some(events);
                    return Err(format!("Water impulse: tick {tick} has no accepted interval"));
                };
                self.impulses.begin_interval(stamp, interval, |event| events.push(event))
            } else {
                self.impulses.begin_tick(stamp, |event| events.push(event))
            };
            if let Err(error) = result {
                self.spare_impulses = Some(events);
                let error = format!("Water impulse: {error}");
                self.failure = Some(error.clone());
                return Err(error);
            }
        }
        Ok(events)
    }

    pub(super) fn accept_impulse_batch(
        &mut self,
        epoch: u64,
        started_tick: u64,
        mut events: Vec<AppliedEvent<ResolvedNodeImpulse>>,
    ) {
        if epoch == self.epoch {
            // A failed/cancelled batch can have an unstarted suffix. Retain it
            // in the spare buffer until reset; no receipt claims it was run.
            let started = events.partition_point(|event| event.applied.tick < started_tick);
            self.applied_impulses.extend(events.drain(..started));
        } else {
            events.clear();
        }
        self.spare_impulses = Some(events);
    }
}

pub(super) struct ImpulseSum<'a> {
    pub events: &'a [AppliedEvent<ResolvedNodeImpulse>],
    pub origin: [f32; 3],
}

impl ImpulseSum<'_> {
    pub fn is_empty(&self) -> bool {
        !self
            .events
            .iter()
            .any(|event| event.value.target.affects_fluid())
    }
}

impl VectorField for ImpulseSum<'_> {
    fn sample(&self, position: [f32; 3]) -> [f32; 3] {
        let scene_position = std::array::from_fn(|axis| position[axis] + self.origin[axis]);
        let mut sum = [0.0; 3];
        for event in self
            .events
            .iter()
            .filter(|event| event.value.target.affects_fluid())
        {
            let value = event.value.field.sample(scene_position);
            for axis in 0..3 {
                sum[axis] += value[axis];
            }
        }
        sum
    }
}

#[cfg(test)]
mod tests;
