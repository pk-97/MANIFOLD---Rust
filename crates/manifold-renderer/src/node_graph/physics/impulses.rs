use manifold_core::Seconds;
use manifold_physics::{
    FieldInput, FieldValue, TickStamp, VectorField,
    input::{AppliedEvent, EventQueue, EventStamp},
};

use super::{FIXED_TICK, IMPULSE_CAPACITY, MAX_BODIES, RigidSimulation, TARGET_SLOTS};

/// A fixed set of ordinary body slots and the reset-latched copy group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RigidImpulseTargets {
    pub bodies: u64,
    pub copies: bool,
}

impl RigidImpulseTargets {
    pub const fn is_empty(self) -> bool {
        self.bodies == 0 && !self.copies
    }

    pub const fn contains_body(self, index: usize) -> bool {
        index < MAX_BODIES && (self.bodies & (1u64 << index)) != 0
    }
}

/// A resolved scene-space delta velocity retained by value until its fixed
/// tick begins. The field is sampled at each recipient's current center of
/// mass and is consumed with delta_velocity=1, acceleration=0.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedRigidImpulse {
    pub field: FieldValue,
    pub targets: RigidImpulseTargets,
}

/// A borrowed sum of all impulse fields targeting one recipient class during
/// one fixed tick. It keeps the hot path allocation-free while allowing one
/// `apply_fields_by_target` batch to use a distinct sum for each slot.
pub(crate) struct ImpulseSumField<'a> {
    events: &'a [AppliedEvent<ResolvedRigidImpulse>],
    body: Option<usize>,
    copies: bool,
}

impl<'a> ImpulseSumField<'a> {
    pub(crate) fn for_body(events: &'a [AppliedEvent<ResolvedRigidImpulse>], body: usize) -> Self {
        Self {
            events,
            body: Some(body),
            copies: false,
        }
    }

    pub(crate) fn for_copies(events: &'a [AppliedEvent<ResolvedRigidImpulse>]) -> Self {
        Self {
            events,
            body: None,
            copies: true,
        }
    }

    pub(crate) fn has_events(&self) -> bool {
        self.events.iter().any(|event| {
            self.body
                .is_some_and(|body| event.value.targets.contains_body(body))
                || (self.copies && event.value.targets.copies)
        })
    }
}

impl VectorField for ImpulseSumField<'_> {
    fn sample(&self, position: [f32; 3]) -> [f32; 3] {
        let mut sum = [0.0; 3];
        for event in self.events.iter().filter(|event| {
            self.body
                .is_some_and(|body| event.value.targets.contains_body(body))
                || (self.copies && event.value.targets.copies)
        }) {
            let sample = event.value.field.sample(position);
            for component in 0..3 {
                sum[component] += sample[component];
            }
        }
        sum
    }
}

impl RigidSimulation {
    /// The epoch assigned to resolved impulses once the native world exists.
    pub fn impulse_epoch(&self) -> Option<u64> {
        self.impulse_epoch
    }

    /// Capture an impulse timestamp from the observation that was accepted for
    /// this exact transport value. Native time is authored time, so paused
    /// observations retain their current clock without extrapolation.
    pub fn impulse_stamp(&self, transport: Seconds, sequence: u64) -> Result<EventStamp, String> {
        if !transport.0.is_finite() {
            return Err("Physics: impulse transport must be finite".into());
        }
        let Some(epoch) = self.impulse_epoch else {
            return Err("Physics: no initialized impulse epoch".into());
        };
        if let Some(error) = &self.impulse_failure {
            return Err(error.clone());
        }
        if self.impulse_overflow_latched {
            return Err(
                "Physics: impulse history is full; restart the simulation or bake the scene".into(),
            );
        }
        let Some((accepted_transport, native_time)) = self.accepted_observation else {
            return Err("Physics: no successfully accepted observation at this transport".into());
        };
        if accepted_transport != transport.0 {
            return Err(
                "Physics: impulse transport does not match the accepted observation".into(),
            );
        }
        Ok(EventStamp {
            epoch,
            time: Seconds(native_time),
            sequence,
        })
    }

    /// Queue a scene-space delta velocity for one fixed tick. Validation is
    /// complete before EventQueue sees the stamp, so rejected inputs do not
    /// consume the producer sequence.
    pub fn enqueue_impulse(
        &mut self,
        stamp: EventStamp,
        payload: ResolvedRigidImpulse,
    ) -> Result<TickStamp, String> {
        if self.world.is_none() || self.impulse_epoch.is_none() {
            return Err("Physics: cannot enqueue an impulse before world initialization".into());
        }
        if let Some(error) = self.impulse_failure.as_ref() {
            return Err(error.clone());
        }
        if self.impulse_overflow_latched {
            return Err("Physics: impulse queue capacity overflow is latched until rebuild".into());
        }
        self.validate_impulse_targets(payload.targets)?;
        let outstanding = self
            .impulse_queue
            .as_ref()
            .expect("initialized world has an impulse queue")
            .len()
            + self.impulse_receipts.len()
            + self.impulse_tick_events.len();
        if outstanding >= IMPULSE_CAPACITY {
            self.impulse_overflow_latched = true;
            return Err("Physics: impulse queue and receipt capacity is full".into());
        }
        match self
            .impulse_queue
            .as_mut()
            .expect("initialized world has an impulse queue")
            .enqueue(stamp, payload)
        {
            Ok(planned) => Ok(planned),
            Err(error) => {
                if error == manifold_physics::input::EventError::CapacityOverflow {
                    self.impulse_overflow_latched = true;
                }
                Err(format!("Physics: failed to enqueue impulse: {error}"))
            }
        }
    }

    /// Drain receipts assigned at native tick start. The backing storage is
    /// retained for reuse by the next delivery batch.
    pub fn drain_applied_impulses(
        &mut self,
    ) -> std::vec::Drain<'_, AppliedEvent<ResolvedRigidImpulse>> {
        self.impulse_receipts.drain(..)
    }

    fn validate_impulse_targets(&self, targets: RigidImpulseTargets) -> Result<(), String> {
        if targets.is_empty() {
            return Err("Physics: impulse targets must select a body or copies".into());
        }
        for index in 0..MAX_BODIES {
            if targets.contains_body(index) && self.handles[index].is_none() {
                return Err(format!(
                    "Physics: impulse target body slot {index} is absent"
                ));
            }
        }
        if targets.copies && self.active_copy_count == 0 {
            return Err("Physics: impulse target copies has no active copies".into());
        }
        Ok(())
    }

    pub(super) fn reset_impulse_runtime(&mut self, epoch: u64) -> Result<(), String> {
        if let Some(queue) = self.impulse_queue.as_mut() {
            queue
                .reset(epoch, Seconds::ZERO)
                .map_err(|error| format!("Physics: failed to reset impulse queue: {error}"))?;
        } else {
            self.impulse_queue = Some(
                EventQueue::new(epoch, Seconds::ZERO, FIXED_TICK, IMPULSE_CAPACITY)
                    .map_err(|error| format!("Physics: failed to create impulse queue: {error}"))?,
            );
        }
        self.impulse_receipts.clear();
        self.impulse_tick_events.clear();
        self.impulse_epoch = Some(epoch);
        self.impulse_failure = None;
        self.impulse_overflow_latched = false;
        Ok(())
    }

    pub(super) fn begin_impulse_tick(&mut self) -> Result<TickStamp, String> {
        self.impulse_tick_events.clear();
        let tick = self
            .impulse_queue
            .as_ref()
            .expect("initialized world has an impulse queue")
            .next_tick();
        let events = &mut self.impulse_tick_events;
        self.impulse_queue
            .as_mut()
            .expect("initialized world has an impulse queue")
            .begin_tick(tick, |event| events.push(event))
            .map_err(|error| format!("Physics: failed to begin impulse tick: {error}"))?;
        Ok(tick)
    }

    pub(super) fn apply_impulse_tick(&mut self) -> Result<(), String> {
        if self.impulse_tick_events.is_empty() {
            return Ok(());
        }
        let events = &self.impulse_tick_events;
        let sum_fields: [_; TARGET_SLOTS] = std::array::from_fn(|index| {
            if index == MAX_BODIES {
                ImpulseSumField::for_copies(events)
            } else {
                ImpulseSumField::for_body(events, index)
            }
        });
        let inputs: [FieldInput<'_>; TARGET_SLOTS] = std::array::from_fn(|index| FieldInput {
            field: &sum_fields[index],
            acceleration: 0.0,
            delta_velocity: 1.0,
        });
        let handles = &self.handles;
        let copy_handles = &self.copy_handles[..self.active_copy_count];
        let result = {
            let world = self.world.as_mut().expect("world constructed above");
            let recipients = handles
                .iter()
                .enumerate()
                .filter_map(|(index, handle)| {
                    handle
                        .filter(|_| sum_fields[index].has_events())
                        .map(|handle| (handle, std::slice::from_ref(&inputs[index])))
                })
                .chain(
                    copy_handles
                        .iter()
                        .flatten()
                        .filter(|_| sum_fields[MAX_BODIES].has_events())
                        .map(|&handle| (handle, std::slice::from_ref(&inputs[MAX_BODIES]))),
                );
            world
                .apply_fields_by_target(recipients, FIXED_TICK)
                .map_err(|error| error.to_string())
        };
        if let Err(error) = result {
            self.impulse_failure = Some(error.clone());
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn finish_impulse_tick(&mut self) {
        self.impulse_receipts.append(&mut self.impulse_tick_events);
    }
}

#[cfg(test)]
mod tests;
