use manifold_core::Seconds;
use manifold_physics::{
    input::{AppliedEvent, EventQueue, EventStamp},
    FieldInput, FieldValue, TickStamp, VectorField,
};

use manifold_core::scene_impulse::RigidImpulseTargets;

use super::{RigidSimulation, FIXED_TICK, IMPULSE_CAPACITY, MAX_TARGETS};

/// A resolved scene-space delta velocity retained by value until its fixed
/// tick begins. The field is sampled at each recipient's current center of
/// mass and is consumed with delta_velocity=1, acceleration=0.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedRigidImpulse {
    pub field: FieldValue,
    pub targets: RigidImpulseTargets,
}

/// One hit recipient: a body slot or a copy group.
#[derive(Clone, Copy)]
pub(crate) enum ImpulseRecipient {
    Body(usize),
    Group(usize),
}

impl ImpulseRecipient {
    fn selected_by(self, targets: RigidImpulseTargets) -> bool {
        match self {
            Self::Body(body) => targets.contains_body(body),
            Self::Group(group) => targets.contains_group(group),
        }
    }
}

/// A borrowed sum of all impulse fields targeting one recipient during one
/// fixed tick. It keeps the hot path allocation-free while allowing one
/// `apply_fields_by_target` batch to use a distinct sum for each recipient.
pub(crate) struct ImpulseSumField<'a> {
    events: &'a [AppliedEvent<ResolvedRigidImpulse>],
    recipient: ImpulseRecipient,
}

impl<'a> ImpulseSumField<'a> {
    pub(crate) fn new(
        events: &'a [AppliedEvent<ResolvedRigidImpulse>],
        recipient: ImpulseRecipient,
    ) -> Self {
        Self { events, recipient }
    }

    pub(crate) fn has_events(&self) -> bool {
        self.events
            .iter()
            .any(|event| self.recipient.selected_by(event.value.targets))
    }
}

impl VectorField for ImpulseSumField<'_> {
    fn sample(&self, position: [f32; 3]) -> [f32; 3] {
        let mut sum = [0.0; 3];
        for event in self
            .events
            .iter()
            .filter(|event| self.recipient.selected_by(event.value.targets))
        {
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
    /// this exact played time. Native time is authored time, so paused
    /// observations retain their current clock without extrapolation.
    pub fn impulse_stamp(&self, played: manifold_core::PlayedTime, sequence: u64) -> Result<EventStamp, String> {
        self.impulse_stamp_at(played.seconds(), sequence)
    }

    /// [`Self::impulse_stamp`] for a coupled worker, whose accepted
    /// observation is the simulation time its owner accepted.
    pub fn worker_impulse_stamp(&self, accepted: Seconds, sequence: u64) -> Result<EventStamp, String> {
        self.impulse_stamp_at(accepted.0, sequence)
    }

    fn impulse_stamp_at(&self, observed: f64, sequence: u64) -> Result<EventStamp, String> {
        if !observed.is_finite() {
            return Err("Physics: impulse transport must be finite".into());
        }
        let Some(epoch) = self.impulse_epoch else {
            return Err("Physics: no initialized impulse epoch".into());
        };
        if let Some(error) = &self.impulse_failure {
            return Err(error.clone());
        }
        let Some((accepted_transport, native_time)) = self.accepted_observation else {
            return Err("Physics: no successfully accepted observation at this transport".into());
        };
        if accepted_transport != observed {
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
        self.validate_impulse_targets(payload.targets)?;
        // Pause and Simulation Speed 0 discard incoming events, so resume never
        // fires a saved-up pile (WATER_SIMULATION_DESIGN.md "Transport pause /
        // water speed zero"). Admission succeeds so the producer rearms; the
        // discard receipt is the only record, never an applied one.
        if self.held.is_held() {
            if self.impulse_discarded.len() < IMPULSE_CAPACITY {
                self.impulse_discarded.push(stamp);
            } else {
                self.impulse_discarded_beyond += 1;
            }
            return Ok(self
                .impulse_queue
                .as_ref()
                .expect("initialized world has an impulse queue")
                .next_tick());
        }
        let outstanding = self
            .impulse_queue
            .as_ref()
            .expect("initialized world has an impulse queue")
            .len()
            + self.impulse_receipts.len()
            + self.impulse_tick_events.len();
        // A full history refuses this hit only: receipts drain every frame,
        // so the next hit is tried again. The check keeps the queue below its
        // own capacity, so its overflow latch is never reached.
        if outstanding >= IMPULSE_CAPACITY {
            return Err(format!(
                "Physics: {IMPULSE_CAPACITY} hits are already in flight; this hit is refused"
            ));
        }
        self.impulse_queue
            .as_mut()
            .expect("initialized world has an impulse queue")
            .enqueue(stamp, payload)
            .map_err(|error| format!("Physics: failed to enqueue impulse: {error}"))
    }

    /// Drain receipts assigned at native tick start. The backing storage is
    /// retained for reuse by the next delivery batch.
    pub fn drain_applied_impulses(
        &mut self,
    ) -> std::vec::Drain<'_, AppliedEvent<ResolvedRigidImpulse>> {
        self.impulse_receipts.drain(..)
    }

    /// Drain the stamps of hits a held clock discarded; they never apply.
    pub fn drain_discarded_impulses(&mut self) -> std::vec::Drain<'_, EventStamp> {
        self.impulse_discarded.drain(..)
    }

    /// Take the count of held discards past the receipt bound; each is one
    /// discarded hit with no stamp.
    pub fn take_discarded_beyond(&mut self) -> u64 {
        std::mem::take(&mut self.impulse_discarded_beyond)
    }

    /// Whether a hit fired now can be admitted: `Ok(false)` until the world
    /// exists (the hit waits), `Err` once a native step failed.
    pub fn impulse_readiness(&self) -> Result<bool, String> {
        match (&self.impulse_failure, self.impulse_epoch) {
            (Some(error), _) => Err(error.clone()),
            (None, None) => Ok(false),
            (None, Some(_)) => Ok(self.world.is_some()),
        }
    }

    pub(super) fn validate_impulse_targets(
        &self,
        targets: RigidImpulseTargets,
    ) -> Result<(), String> {
        if targets.is_empty() {
            return Err("Physics: impulse targets must select a body or a copy group".into());
        }
        for index in targets.bodies() {
            if self.handles.get(index).is_none_or(Option::is_none) {
                return Err(format!(
                    "Physics: impulse target body slot {index} is absent"
                ));
            }
        }
        for group in targets.groups() {
            if self.groups.get(group).is_none_or(|state| state.active == 0) {
                return Err(format!(
                    "Physics: impulse target copy group {group} has no active copies"
                ));
            }
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
        self.impulse_discarded.clear();
        self.impulse_discarded_beyond = 0;
        self.impulse_epoch = Some(epoch);
        self.impulse_failure = None;
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
        let bodies = self.handles.len();
        let sum_fields: [_; MAX_TARGETS] = std::array::from_fn(|index| {
            ImpulseSumField::new(
                events,
                if index < bodies {
                    ImpulseRecipient::Body(index)
                } else {
                    ImpulseRecipient::Group(index - bodies)
                },
            )
        });
        let inputs: [FieldInput<'_>; MAX_TARGETS] = std::array::from_fn(|index| FieldInput {
            field: &sum_fields[index],
            acceleration: 0.0,
            delta_velocity: 1.0,
        });
        let handles = &self.handles;
        let groups = &self.groups;
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
                .chain(groups.iter().enumerate().flat_map(|(group, state)| {
                    let target = bodies + group;
                    let hit = sum_fields[target].has_events();
                    let input = std::slice::from_ref(&inputs[target]);
                    state.handles[..if hit { state.active } else { 0 }]
                        .iter()
                        .flatten()
                        .map(move |&handle| (handle, input))
                }));
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
