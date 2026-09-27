//! The rigid participant travels through the existing fluid worker handoff.
//! Transport mapping, cancellation and publication remain owned by FluidRuntime.

use std::sync::Arc;

use manifold_physics::input::{InputHistory, Timestamped};
use manifold_physics::{Seconds, TickStamp};

use crate::node_graph::physics::{MAX_BODIES, RigidImpulseTargets, RigidSceneInputs};
use crate::node_graph::transform::Transform;

use super::HISTORY_CAPACITY;

mod native;
pub(super) use native::Native;

#[cfg(test)]
mod tests;

/// Resolved rigid inputs for one fluid domain. The same rigid world supplies
/// contacts for all its bodies; `colliders` selects those exchanging momentum
/// with this domain. Density is physical liquid density in kg/m³.
#[derive(Clone, Copy)]
pub struct CoupledRigidInputs<'a> {
    pub scene: &'a RigidSceneInputs,
    pub colliders: RigidImpulseTargets,
    pub density: f64,
}

impl CoupledRigidInputs<'_> {
    pub(super) fn validate(self) -> Result<(), String> {
        if !self.density.is_finite() || self.density <= 0.0 {
            return Err("Fluid coupling: density must be finite and positive".into());
        }
        for index in 0..MAX_BODIES {
            if self.colliders.contains_body(index) && self.scene.bodies[index].is_none() {
                return Err(format!("Fluid coupling: body {index} is not connected"));
            }
        }
        if self.colliders.copies && self.scene.prototype.is_none() {
            return Err("Fluid coupling: copies require a connected prototype".into());
        }
        Ok(())
    }
}

/// An immutable visible rigid result, accepted with the liquid surface at the
/// same completed boundary. Tick zero is the prepared, unstepped scene.
#[derive(Clone, Debug)]
pub struct CoupledRigidFrame {
    pub stamp: TickStamp,
    pub poses: [Transform; MAX_BODIES],
    pub copies: Vec<Transform>,
}

impl Default for CoupledRigidFrame {
    fn default() -> Self {
        Self {
            stamp: TickStamp { epoch: 0, tick: 0 },
            poses: [Transform::default(); MAX_BODIES],
            copies: Vec::new(),
        }
    }
}

pub(super) struct Setup {
    pub initial: RigidSceneInputs,
    pub colliders: RigidImpulseTargets,
    pub density: f64,
}

impl Setup {
    fn matches(&self, inputs: CoupledRigidInputs<'_>) -> bool {
        self.colliders == inputs.colliders
            && self.density == inputs.density
            && self.initial.same_topology(inputs.scene)
            // The paired pose layout retains inactive fragment parent handles.
            && self.initial.bodies.iter().zip(&inputs.scene.bodies).all(|(left, right)| {
                left.as_ref().and_then(|body| body.fragment_parent)
                    == right.as_ref().and_then(|body| body.fragment_parent)
            })
    }
}

#[derive(Clone)]
pub(super) struct Sample {
    pub sequence: u64,
    pub time: Seconds,
    pub inputs: RigidSceneInputs,
}

impl Timestamped for Sample {
    fn time(&self) -> Seconds {
        self.time
    }
}

pub(super) struct Request {
    pub setup: Arc<Setup>,
    pub history: Vec<Sample>,
    pub output: CoupledRigidFrame,
}

pub(super) struct Runtime {
    setup: Arc<Setup>,
    history: InputHistory<Sample>,
    spare_history: Option<Vec<Sample>>,
    spare_output: Option<CoupledRigidFrame>,
    pub accepted: Option<CoupledRigidFrame>,
    sequence: u64,
}

impl Runtime {
    pub fn new(inputs: CoupledRigidInputs<'_>) -> Self {
        Self {
            setup: Arc::new(Setup {
                initial: inputs.scene.clone(),
                colliders: inputs.colliders,
                density: inputs.density,
            }),
            history: InputHistory::with_capacity(HISTORY_CAPACITY)
                .expect("coupled history has at least two entries"),
            spare_history: Some(Vec::with_capacity(HISTORY_CAPACITY)),
            spare_output: Some(CoupledRigidFrame::default()),
            accepted: None,
            sequence: 0,
        }
    }

    pub fn matches(&self, inputs: CoupledRigidInputs<'_>) -> bool {
        self.setup.matches(inputs)
    }

    pub fn latest_matches(&self, inputs: CoupledRigidInputs<'_>) -> bool {
        self.history
            .back()
            .is_some_and(|sample| sample.inputs == *inputs.scene)
    }

    pub fn validate_impulse_targets(&self, targets: RigidImpulseTargets) -> Result<(), String> {
        if targets.is_empty() {
            return Err("Fluid coupling: rigid impulse targets are empty".into());
        }
        for index in 0..MAX_BODIES {
            if targets.contains_body(index)
                && self.setup.initial.bodies[index]
                    .as_ref()
                    .is_none_or(|body| !body.enabled)
            {
                return Err(format!(
                    "Fluid coupling: rigid impulse body {index} is absent"
                ));
            }
        }
        if targets.copies
            && (self
                .setup
                .initial
                .prototype
                .as_ref()
                .is_none_or(|body| !body.enabled)
                || !self.setup.initial.copy_count.is_finite()
                || self.setup.initial.copy_count.round() < 1.0)
        {
            return Err("Fluid coupling: rigid impulse has no active copies".into());
        }
        Ok(())
    }

    pub fn clear(&mut self) {
        self.history.clear();
        self.accepted = None;
        self.sequence = 0;
    }

    /// Setup-only reseeding keeps the existing in-flight request's recycled
    /// buffers owned by that request until its stale reply returns.
    pub fn reseed(&mut self, inputs: CoupledRigidInputs<'_>) {
        self.setup = Arc::new(Setup {
            initial: inputs.scene.clone(),
            colliders: inputs.colliders,
            density: inputs.density,
        });
        self.clear();
    }

    pub fn observe(
        &mut self,
        inputs: CoupledRigidInputs<'_>,
        time: Seconds,
        completed: Seconds,
    ) -> Result<(), String> {
        if self.history.back().is_some_and(|last| last.time == time) && self.latest_matches(inputs)
        {
            return Ok(());
        }
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or("Fluid coupling: input sequence exhausted")?;
        self.history
            .record(
                Sample {
                    sequence,
                    time,
                    inputs: inputs.scene.clone(),
                },
                completed,
            )
            .map_err(|error| format!("Fluid coupling: {error}"))?;
        self.sequence = sequence;
        Ok(())
    }

    pub fn prune(&mut self, from: Seconds) -> Result<(), String> {
        self.history
            .prune_before(from)
            .map_err(|error| format!("Fluid coupling: {error}"))?;
        Ok(())
    }

    pub fn request(&mut self) -> Request {
        let mut history = self
            .spare_history
            .take()
            .expect("one coupled request in flight");
        history.clear();
        history.extend(self.history.iter().cloned());
        Request {
            setup: Arc::clone(&self.setup),
            history,
            output: self
                .spare_output
                .take()
                .expect("one coupled output in flight"),
        }
    }

    pub fn accept(&mut self, request: Request, publish: bool) {
        self.spare_history = Some(request.history);
        self.spare_output = Some(if publish {
            self.accepted.replace(request.output).unwrap_or_default()
        } else {
            request.output
        });
    }

    pub fn recover_missing_request(&mut self) {
        // A malformed reply latches a failure in FluidRuntime. Restore loaned
        // storage so an explicit reset can recover instead of panicking.
        self.spare_history
            .get_or_insert_with(|| Vec::with_capacity(HISTORY_CAPACITY));
        self.spare_output
            .get_or_insert_with(CoupledRigidFrame::default);
    }
}
