//! Resolved, bounded impulse delivery through the existing worker handoff.
//!
//! Sending a worker request seals its input ticks. Later arrivals are assigned
//! after that batch; the reply records which assigned ticks actually began.

use manifold_physics::input::{AppliedEvent, EventQueue, EventStamp};
use manifold_physics::{FieldValue, TickStamp, VectorField};

use super::{FluidRuntime, TICK};
use manifold_core::Seconds;

pub(super) const IMPULSE_CAPACITY: usize = 256;

pub(super) fn new_queue() -> EventQueue<FieldValue> {
    EventQueue::new(1, Seconds::ZERO, Seconds(TICK), IMPULSE_CAPACITY)
        .expect("fixed fluid impulse queue configuration is valid")
}

impl FluidRuntime {
    /// Epoch for already resolved inputs. Call `observe` before capturing an
    /// impulse; its timestamp is simulation-relative time, not transport time.
    pub fn impulse_epoch(&self) -> Option<u64> {
        self.settings.map(|_| self.epoch)
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
        if self.settings.is_none() {
            return Err("Water: observe the domain before capturing an impulse".into());
        }
        if self.cache_mode != super::CacheMode::Live {
            return Err("Water: impulses require Live mode until input takes are recorded".into());
        }
        if let Some(error) = &self.failure {
            return Err(error.clone());
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
            .enqueue(stamp, field)
            .map_err(|error| format!("Water impulse: {error}"))?;
        self.impulse_outstanding += 1;
        Ok(tick)
    }

    /// These receipts identify ticks begun by the native worker, including a
    /// tick that subsequently failed. They never assert native completion.
    pub fn drain_applied_impulses(
        &mut self,
    ) -> impl Iterator<Item = AppliedEvent<FieldValue>> + '_ {
        self.impulse_outstanding -= self.applied_impulses.len();
        self.applied_impulses.drain(..)
    }

    pub(super) fn prepare_impulse_batch(
        &mut self,
        start_tick: u64,
        count: usize,
    ) -> Result<Vec<AppliedEvent<FieldValue>>, String> {
        let mut events = self
            .spare_impulses
            .take()
            .expect("one recycled impulse batch per worker request");
        events.clear();
        for index in 0..count {
            let result = self.impulses.begin_tick(
                TickStamp {
                    epoch: self.epoch,
                    tick: start_tick + index as u64,
                },
                |event| events.push(event),
            );
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
        mut events: Vec<AppliedEvent<FieldValue>>,
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
    pub events: &'a [AppliedEvent<FieldValue>],
    pub origin: [f32; 3],
}

impl VectorField for ImpulseSum<'_> {
    fn sample(&self, position: [f32; 3]) -> [f32; 3] {
        let scene_position = std::array::from_fn(|axis| position[axis] + self.origin[axis]);
        let mut sum = [0.0; 3];
        for event in self.events {
            let value = event.value.sample(scene_position);
            for axis in 0..3 {
                sum[axis] += value[axis];
            }
        }
        sum
    }
}

#[cfg(test)]
mod tests;
