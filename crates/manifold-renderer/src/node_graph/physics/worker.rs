use manifold_core::Seconds;
use manifold_physics::input::AppliedEvent;
use manifold_physics::stepping::StepCoupling;
use manifold_physics::{BodyHandle, FieldValue, PhysicsWorld};

use super::{
    AdvancementPolicy, FIXED_TICK, IMPULSE_CAPACITY, MAX_BODIES, ResolvedRigidImpulse, RigidBody,
    RigidSimulation, TARGET_SLOTS, same_collider,
};

/// The retained scene inputs consumed by the shared FLIP worker.
///
/// Copy population controls remain authored inputs here. Native rigid copy
/// count and layout are still latched by [`RigidSimulation`] at the existing
/// reset points.
#[derive(Clone, Debug, PartialEq)]
pub struct RigidSceneInputs {
    pub bodies: [Option<RigidBody>; MAX_BODIES],
    pub prototype: Option<RigidBody>,
    pub copy_count: f32,
    pub copy_spacing: f32,
    pub copy_columns: f32,
    pub layout: f32,
    pub gravity: [f32; 3],
    pub acceleration_field: Option<FieldValue>,
    pub targeted_fields: [Option<FieldValue>; TARGET_SLOTS],
}

impl Default for RigidSceneInputs {
    fn default() -> Self {
        Self {
            bodies: std::array::from_fn(|_| None),
            prototype: None,
            copy_count: 0.0,
            copy_spacing: 1.25,
            copy_columns: 16.0,
            layout: 0.0,
            gravity: [0.0; 3],
            acceleration_field: None,
            targeted_fields: std::array::from_fn(|_| None),
        }
    }
}

impl RigidSceneInputs {
    /// Compare the parts that require native geometry to be rebuilt.
    ///
    /// Count, spacing, columns, and layout intentionally do not participate:
    /// those controls retain the existing reset-latched copy semantics.
    pub(crate) fn same_topology(&self, other: &Self) -> bool {
        self.bodies
            .iter()
            .zip(other.bodies.iter())
            .all(|(left, right)| same_topology_body(left.as_ref(), right.as_ref()))
            && same_topology_body(self.prototype.as_ref(), other.prototype.as_ref())
    }
}

fn same_topology_body(left: Option<&RigidBody>, right: Option<&RigidBody>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.shape == right.shape
                && left.transform.scale == right.transform.scale
                && left.enabled == right.enabled
                && same_collider(left, right)
        }
        _ => false,
    }
}

impl RigidSimulation {
    /// Create a rigid owner whose first native event clock uses `epoch` exactly.
    pub(crate) fn with_worker_epoch(epoch: u64) -> Result<Self, String> {
        if epoch == 0 {
            return Err("Physics worker epoch must be non-zero".into());
        }
        Ok(Self {
            worker_epoch: Some(epoch),
            ..Self::default()
        })
    }

    /// Observe retained inputs and drain at most `max_ticks` fixed native ticks.
    ///
    /// The existing rigid advancement path owns initialization, histories,
    /// events, coupling, and publication. This method only selects its bounded
    /// worker policy and supplies the retained scene inputs.
    pub(crate) fn advance_worker<C: StepCoupling>(
        &mut self,
        inputs: &RigidSceneInputs,
        now: Seconds,
        max_ticks: usize,
        coupling: &mut C,
    ) -> Result<(), String> {
        if self.world.is_some()
            && (!self.native_topology_matches(inputs)
                || self.last_time.is_some_and(|last| now.0 < last.0))
        {
            return Err("Physics worker cannot rebuild native topology within an epoch".into());
        }

        let previous_policy = self.advancement_policy;
        self.advancement_policy = AdvancementPolicy::Worker { max_ticks };
        let result = self.advance_with_coupling(
            inputs.bodies.clone(),
            inputs.prototype.clone(),
            inputs.copy_count,
            inputs.copy_spacing,
            inputs.copy_columns,
            inputs.layout,
            inputs.gravity,
            now,
            1.0,
            0.0,
            inputs.acceleration_field.clone(),
            &inputs.targeted_fields,
            coupling,
        );
        self.advancement_policy = previous_policy;
        result
    }

    /// Consume one batch of events already assigned by the shared worker
    /// queue. The native queue advances its empty cursor, while these events
    /// retain their original source and applied metadata.
    pub(crate) fn advance_worker_tick<C: StepCoupling>(
        &mut self,
        inputs: &RigidSceneInputs,
        now: Seconds,
        events: &[AppliedEvent<ResolvedRigidImpulse>],
        coupling: &mut C,
    ) -> Result<(), String> {
        self.validate_assigned_impulses(events)?;
        if self.world.is_some()
            && (!self.native_topology_matches(inputs)
                || self.last_time.is_some_and(|last| now.0 < last.0))
        {
            return Err("Physics worker cannot rebuild native topology within an epoch".into());
        }
        self.ensure_worker_tick_is_owed(now)?;

        let previous_policy = self.advancement_policy;
        self.advancement_policy = AdvancementPolicy::Worker { max_ticks: 1 };
        let result = self.advance_with_coupling_inner(
            inputs.bodies.clone(),
            inputs.prototype.clone(),
            inputs.copy_count,
            inputs.copy_spacing,
            inputs.copy_columns,
            inputs.layout,
            inputs.gravity,
            now,
            1.0,
            0.0,
            inputs.acceleration_field.clone(),
            &inputs.targeted_fields,
            Some(events),
            coupling,
        );
        self.advancement_policy = previous_policy;
        result
    }

    pub(crate) fn native_world(&self) -> Option<&PhysicsWorld> {
        self.world.as_ref()
    }

    pub(crate) fn native_handles(
        &self,
    ) -> (&[Option<BodyHandle>; MAX_BODIES], &[Option<BodyHandle>]) {
        (&self.handles, &self.copy_handles[..self.active_copy_count])
    }

    fn native_topology_matches(&self, inputs: &RigidSceneInputs) -> bool {
        self.descriptions
            .iter()
            .zip(inputs.bodies.iter())
            .all(|(left, right)| same_topology_body(left.as_ref(), right.as_ref()))
            && same_topology_body(self.copy_description.as_ref(), inputs.prototype.as_ref())
    }

    fn validate_assigned_impulses(
        &self,
        events: &[AppliedEvent<ResolvedRigidImpulse>],
    ) -> Result<(), String> {
        if events.len() > IMPULSE_CAPACITY {
            return Err("Physics worker assigned impulse batch exceeds capacity".into());
        }
        let Some(epoch) = self.impulse_epoch else {
            return Err("Physics worker assigned impulses require an initialized epoch".into());
        };
        let Some(queue) = self.impulse_queue.as_ref() else {
            return Err("Physics worker assigned impulses require an initialized queue".into());
        };
        if !queue.is_empty()
            || !self.impulse_receipts.is_empty()
            || !self.impulse_tick_events.is_empty()
        {
            return Err("Physics worker cannot mix assigned and locally queued impulses".into());
        }
        let next_tick = queue.next_tick();
        for event in events {
            if event.source.epoch != epoch || event.applied.epoch != epoch {
                return Err("Physics worker assigned impulse epoch does not match owner".into());
            }
            if event.applied.tick != next_tick.tick {
                return Err(
                    "Physics worker assigned impulse tick is not the next native tick".into(),
                );
            }
            if !event.source.time.0.is_finite()
                || !event.lateness.0.is_finite()
                || event.lateness.0 < 0.0
            {
                return Err("Physics worker assigned impulse timing is invalid".into());
            }
            self.validate_impulse_targets(event.value.targets)?;
        }
        Ok(())
    }

    fn ensure_worker_tick_is_owed(&self, now: Seconds) -> Result<(), String> {
        if !now.0.is_finite() {
            return Err("Physics worker tick clock must be finite".into());
        }
        let Some(last_time) = self.last_time else {
            return Err("Physics worker tick requires an initialized clock".into());
        };
        let elapsed = now.0 - last_time.0;
        if elapsed < 0.0 {
            return Err("Physics worker cannot seek within an epoch".into());
        }
        let due = ((self.accumulator + elapsed + 1e-9) / FIXED_TICK.0).floor();
        if !due.is_finite() || due < 1.0 {
            return Err("Physics worker has no owed native tick".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::physics::{
        PhysicsAuthoredSampleScope, PhysicsStepScope, ResolvedRigidImpulse, RigidImpulseTargets,
    };
    use manifold_physics::TickStamp;
    use manifold_physics::input::{AppliedEvent, EventStamp};
    use manifold_physics::stepping::Uncoupled;

    fn scene() -> RigidSceneInputs {
        let mut scene = RigidSceneInputs::default();
        scene.bodies[0] = Some(RigidBody::default());
        scene
    }

    fn assigned_event(
        epoch: u64,
        tick: u64,
        sequence: u64,
        targets: RigidImpulseTargets,
    ) -> AppliedEvent<ResolvedRigidImpulse> {
        AppliedEvent {
            source: EventStamp {
                epoch,
                time: Seconds(tick as f64 * super::super::FIXED_TICK.0),
                sequence,
            },
            applied: TickStamp { epoch, tick },
            lateness: Seconds::ZERO,
            value: ResolvedRigidImpulse {
                field: FieldValue::uniform([2.0, 0.0, 0.0]).unwrap(),
                targets,
            },
        }
    }

    #[test]
    fn worker_epoch_is_exact_and_rejects_zero() {
        assert!(RigidSimulation::with_worker_epoch(0).is_err());
        let mut simulation = RigidSimulation::with_worker_epoch(16_777_217).unwrap();
        let mut coupling = Uncoupled;
        simulation
            .advance_worker(&scene(), Seconds::ZERO, 0, &mut coupling)
            .unwrap();
        assert_eq!(simulation.impulse_epoch(), Some(16_777_217));
    }

    #[test]
    fn worker_observes_without_ticks_then_drains_bounded_debt() {
        let mut simulation = RigidSimulation::with_worker_epoch(7).unwrap();
        let mut coupling = Uncoupled;
        let inputs = scene();
        simulation
            .advance_worker(&inputs, Seconds::ZERO, 0, &mut coupling)
            .unwrap();
        simulation
            .advance_worker(
                &inputs,
                Seconds(5.0 * super::super::FIXED_TICK.0),
                0,
                &mut coupling,
            )
            .unwrap();
        assert_eq!(simulation.pending_time.0, 5.0 * super::super::FIXED_TICK.0);
        simulation
            .advance_worker(
                &inputs,
                Seconds(5.0 * super::super::FIXED_TICK.0),
                1,
                &mut coupling,
            )
            .unwrap();
        assert!((simulation.pending_time.0 - 4.0 * super::super::FIXED_TICK.0).abs() < 1e-12);
    }

    #[test]
    fn worker_rejects_topology_change_before_replacing_native_world() {
        let mut simulation = RigidSimulation::with_worker_epoch(1).unwrap();
        let mut coupling = Uncoupled;
        let inputs = scene();
        simulation
            .advance_worker(&inputs, Seconds::ZERO, 0, &mut coupling)
            .unwrap();
        let handle = simulation.native_handles().0[0].unwrap();
        let before_pose = simulation.native_world().unwrap().pose(handle).unwrap();
        let before_epoch = simulation.impulse_epoch();
        let before_last_time = simulation.last_time;
        let before_authored_time = simulation.authored_time;
        let before_physics_time = simulation.physics_time;
        let before_pending_time = simulation.pending_time;
        let before_history_len = simulation.authored_samples.iter().count();
        let before_shape = simulation.descriptions[0].as_ref().unwrap().shape;
        let mut changed = inputs.clone();
        changed.bodies[0].as_mut().unwrap().shape = 2;
        assert!(
            simulation
                .advance_worker(&changed, Seconds::ZERO, 1, &mut coupling)
                .is_err()
        );
        assert_eq!(simulation.native_handles().0[0], Some(handle));
        assert_eq!(
            simulation.native_world().unwrap().pose(handle).unwrap(),
            before_pose
        );
        assert_eq!(simulation.impulse_epoch(), before_epoch);
        assert_eq!(simulation.last_time, before_last_time);
        assert_eq!(simulation.authored_time, before_authored_time);
        assert_eq!(simulation.physics_time, before_physics_time);
        assert_eq!(simulation.pending_time, before_pending_time);
        assert_eq!(
            simulation.authored_samples.iter().count(),
            before_history_len
        );
        assert_eq!(
            simulation.descriptions[0].as_ref().unwrap().shape,
            before_shape
        );
    }

    #[test]
    fn topology_ignores_reset_latched_copy_controls() {
        let left = scene();
        let mut right = left.clone();
        right.copy_count = 20.0;
        right.copy_spacing = 3.0;
        right.copy_columns = 4.0;
        right.layout = 1.0;
        assert!(left.same_topology(&right));
    }

    #[test]
    fn native_handles_expose_only_active_copy_slots() {
        let mut simulation = RigidSimulation::with_worker_epoch(1).unwrap();
        let mut coupling = Uncoupled;
        let mut inputs = scene();
        inputs.prototype = Some(RigidBody::default());
        inputs.copy_count = 3.0;
        simulation
            .advance_worker(&inputs, Seconds::ZERO, 0, &mut coupling)
            .unwrap();
        let (_, copies) = simulation.native_handles();
        assert_eq!(copies.len(), 3);
        assert!(copies.iter().all(Option::is_some));
        assert!(super::super::MAX_COPIES >= copies.len());
    }

    #[test]
    fn worker_bounded_drains_match_unbounded_field_and_single_impulse() {
        let mut inputs = scene();
        inputs.acceleration_field = Some(FieldValue::uniform([1.0, 0.0, 0.0]).unwrap());
        let total_time = 3.0 * super::super::FIXED_TICK.0;

        let mut ordinary = RigidSimulation::default();
        let mut ordinary_coupling = Uncoupled;
        ordinary
            .advance_with_fields(
                inputs.bodies.clone(),
                inputs.prototype.clone(),
                inputs.copy_count,
                inputs.copy_spacing,
                inputs.copy_columns,
                inputs.layout,
                inputs.gravity,
                Seconds::ZERO,
                1.0,
                0.0,
                inputs.acceleration_field.clone(),
            )
            .unwrap();
        let ordinary_epoch = ordinary.impulse_epoch().unwrap();
        ordinary
            .enqueue_impulse(
                ordinary
                    .impulse_stamp(Seconds::ZERO, 1)
                    .expect("ordinary observation accepted"),
                ResolvedRigidImpulse {
                    field: FieldValue::uniform([2.0, 0.0, 0.0]).unwrap(),
                    targets: RigidImpulseTargets {
                        bodies: 1,
                        copies: false,
                    },
                },
            )
            .unwrap();
        ordinary
            .advance_with_coupling(
                inputs.bodies.clone(),
                inputs.prototype.clone(),
                inputs.copy_count,
                inputs.copy_spacing,
                inputs.copy_columns,
                inputs.layout,
                inputs.gravity,
                Seconds(total_time),
                1.0,
                0.0,
                inputs.acceleration_field.clone(),
                &inputs.targeted_fields,
                &mut ordinary_coupling,
            )
            .unwrap();

        let mut worker = RigidSimulation::with_worker_epoch(ordinary_epoch).unwrap();
        let mut worker_coupling = Uncoupled;
        let _preview = PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);
        let _authored = PhysicsAuthoredSampleScope::new();
        worker
            .advance_worker(&inputs, Seconds::ZERO, 0, &mut worker_coupling)
            .unwrap();
        worker
            .enqueue_impulse(
                worker
                    .impulse_stamp(Seconds::ZERO, 1)
                    .expect("worker observation accepted"),
                ResolvedRigidImpulse {
                    field: FieldValue::uniform([2.0, 0.0, 0.0]).unwrap(),
                    targets: RigidImpulseTargets {
                        bodies: 1,
                        copies: false,
                    },
                },
            )
            .unwrap();
        worker
            .advance_worker(&inputs, Seconds(total_time), 0, &mut worker_coupling)
            .unwrap();
        for _ in 0..3 {
            worker
                .advance_worker(&inputs, Seconds(total_time), 1, &mut worker_coupling)
                .unwrap();
        }

        let ordinary_handle = ordinary.native_handles().0[0].unwrap();
        let worker_handle = worker.native_handles().0[0].unwrap();
        let ordinary_pose = ordinary
            .native_world()
            .unwrap()
            .pose(ordinary_handle)
            .unwrap();
        let worker_pose = worker.native_world().unwrap().pose(worker_handle).unwrap();
        for (actual, expected) in worker_pose.position.into_iter().zip(ordinary_pose.position) {
            assert!(
                (actual - expected).abs() < 1e-6,
                "worker={actual}, ordinary={expected}"
            );
        }
        let ordinary_velocity = ordinary
            .native_world()
            .unwrap()
            .linear_velocity(ordinary_handle)
            .unwrap();
        let worker_velocity = worker
            .native_world()
            .unwrap()
            .linear_velocity(worker_handle)
            .unwrap();
        for (actual, expected) in worker_velocity.into_iter().zip(ordinary_velocity) {
            assert!(
                (actual - expected).abs() < 1e-6,
                "worker={actual}, ordinary={expected}"
            );
        }
        assert!(worker.pending_time.0.abs() < 1e-12);
        assert_eq!(ordinary.drain_applied_impulses().count(), 1);
        assert_eq!(worker.drain_applied_impulses().count(), 1);
    }

    #[test]
    fn assigned_worker_tick_applies_one_force_and_preserves_receipt_metadata() {
        let epoch = 41;
        let mut simulation = RigidSimulation::with_worker_epoch(epoch).unwrap();
        let mut coupling = Uncoupled;
        let inputs = scene();
        let _preview = PhysicsStepScope::with_preview_budget(false, std::time::Duration::ZERO);
        let _authored = PhysicsAuthoredSampleScope::new();
        simulation
            .advance_worker(&inputs, Seconds::ZERO, 0, &mut coupling)
            .unwrap();
        let event = assigned_event(
            epoch,
            0,
            2,
            RigidImpulseTargets {
                bodies: 1,
                copies: false,
            },
        );
        let original_source = event.source;
        let original_applied = event.applied;
        let original_lateness = event.lateness;
        let original_value = event.value.clone();
        simulation
            .advance_worker_tick(
                &inputs,
                Seconds(super::super::FIXED_TICK.0),
                std::slice::from_ref(&event),
                &mut coupling,
            )
            .unwrap();
        assert_eq!(event.source, original_source);
        assert_eq!(event.applied, original_applied);
        assert_eq!(event.lateness, original_lateness);
        assert_eq!(event.value, original_value);
        assert!(simulation.pending_time.0.abs() < 1e-12);
        let handle = simulation.native_handles().0[0].unwrap();
        let velocity = simulation
            .native_world()
            .unwrap()
            .linear_velocity(handle)
            .unwrap();
        assert!((velocity[0] - 2.0).abs() < 1e-5, "velocity={velocity:?}");
        assert!(
            (simulation.poses[0].pos[0] - 2.0 * super::super::FIXED_TICK.0 as f32).abs() < 1e-5
        );
        let receipts: Vec<_> = simulation.drain_applied_impulses().collect();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].source, original_source);
        assert_eq!(receipts[0].applied, original_applied);
        assert_eq!(receipts[0].lateness, original_lateness);
    }

    #[test]
    fn assigned_worker_ticks_do_not_re_admit_source_sequences() {
        let epoch = 42;
        let mut simulation = RigidSimulation::with_worker_epoch(epoch).unwrap();
        let mut coupling = Uncoupled;
        let inputs = scene();
        simulation
            .advance_worker(&inputs, Seconds::ZERO, 0, &mut coupling)
            .unwrap();
        let first = assigned_event(
            epoch,
            0,
            2,
            RigidImpulseTargets {
                bodies: 1,
                copies: false,
            },
        );
        simulation
            .advance_worker_tick(
                &inputs,
                Seconds(super::super::FIXED_TICK.0),
                std::slice::from_ref(&first),
                &mut coupling,
            )
            .unwrap();
        let first_receipts: Vec<_> = simulation.drain_applied_impulses().collect();
        assert_eq!(first_receipts.len(), 1);
        let second = assigned_event(
            epoch,
            1,
            1,
            RigidImpulseTargets {
                bodies: 1,
                copies: false,
            },
        );
        simulation
            .advance_worker_tick(
                &inputs,
                Seconds(2.0 * super::super::FIXED_TICK.0),
                std::slice::from_ref(&second),
                &mut coupling,
            )
            .unwrap();
        let second_receipts: Vec<_> = simulation.drain_applied_impulses().collect();
        assert_eq!(second_receipts.len(), 1);
        assert_eq!(first_receipts[0].source.sequence, 2);
        assert_eq!(second_receipts[0].source.sequence, 1);
    }

    #[test]
    fn invalid_assigned_event_leaves_native_tick_cursor_and_pose_unchanged() {
        let epoch = 43;
        let mut simulation = RigidSimulation::with_worker_epoch(epoch).unwrap();
        let mut coupling = Uncoupled;
        let inputs = scene();
        simulation
            .advance_worker(&inputs, Seconds::ZERO, 0, &mut coupling)
            .unwrap();
        let handle = simulation.native_handles().0[0].unwrap();
        let before_pose = simulation.native_world().unwrap().pose(handle).unwrap();
        let before_cursor = simulation.impulse_queue.as_ref().unwrap().next_tick();
        let before_physics_time = simulation.physics_time;
        let invalid_target = assigned_event(
            epoch,
            0,
            1,
            RigidImpulseTargets {
                bodies: 1 << 1,
                copies: false,
            },
        );
        assert!(
            simulation
                .advance_worker_tick(
                    &inputs,
                    Seconds(super::super::FIXED_TICK.0),
                    std::slice::from_ref(&invalid_target),
                    &mut coupling,
                )
                .is_err()
        );
        assert_eq!(
            simulation.impulse_queue.as_ref().unwrap().next_tick(),
            before_cursor
        );
        assert_eq!(simulation.physics_time, before_physics_time);
        assert_eq!(
            simulation.native_world().unwrap().pose(handle).unwrap(),
            before_pose
        );

        let wrong_tick = assigned_event(
            epoch,
            1,
            1,
            RigidImpulseTargets {
                bodies: 1,
                copies: false,
            },
        );
        assert!(
            simulation
                .advance_worker_tick(
                    &inputs,
                    Seconds(super::super::FIXED_TICK.0),
                    std::slice::from_ref(&wrong_tick),
                    &mut coupling,
                )
                .is_err()
        );
        assert_eq!(
            simulation.impulse_queue.as_ref().unwrap().next_tick(),
            before_cursor
        );
        assert_eq!(simulation.physics_time, before_physics_time);
        assert_eq!(
            simulation.native_world().unwrap().pose(handle).unwrap(),
            before_pose
        );
    }
}
