#[cfg(feature = "gpu-proofs")]
use manifold_fluids::{
    CoupledFluidFrame, FluidFrame, FluidWorld, FrameStats, MeshRole, RigidFluidCoupling,
};
#[cfg(feature = "gpu-proofs")]
use manifold_physics::input::AppliedEvent;
#[cfg(feature = "gpu-proofs")]
use manifold_physics::stepping::{StepCoupling, StepInterval, SubstepExchange, Uncoupled};
#[cfg(feature = "gpu-proofs")]
use manifold_physics::{FieldInput, Seconds, TickStamp};
use manifold_physics::{BodyHandle, PhysicsWorld};

#[cfg(feature = "gpu-proofs")]
use crate::node_graph::physics::{
    ResolvedRigidImpulse,
};
#[cfg(feature = "gpu-proofs")]
use crate::node_graph::physics_events::ResolvedNodeImpulse;
use crate::node_graph::transform::quat_to_render_scene_euler;
use crate::node_graph::transform::Transform;

#[cfg(feature = "gpu-proofs")]
use super::super::impulses::ImpulseSum;
#[cfg(feature = "gpu-proofs")]
use super::{Request, Setup};
use super::CoupledRigidFrame;
use crate::node_graph::physics::{MAX_BODIES, RigidSceneInputs, RigidSimulation};
#[cfg(feature = "gpu-proofs")]
use crate::node_graph::fluid::{FluidDomainLayout, FluidRuntime, Sample, TICK};

/// How a prepared rigid world's bodies map onto a [`CoupledRigidFrame`]:
/// shared by every liquid that owns a rigid world in-thread.
pub(crate) struct Layout {
    bodies: [Option<BodyHandle>; MAX_BODIES],
    fragment_parents: [Option<usize>; MAX_BODIES],
    authored: [Transform; MAX_BODIES],
    copies: Vec<BodyHandle>,
    copy_scale: [f32; 3],
}

impl Layout {
    /// The layout of `rigid`, prepared from `initial`.
    pub(crate) fn new(rigid: &RigidSimulation, initial: &RigidSceneInputs) -> Self {
        let (bodies, copies) = rigid.native_handles();
        Self {
            bodies: *bodies,
            fragment_parents: std::array::from_fn(|index| {
                initial.bodies[index]
                    .as_ref()
                    .and_then(|body| body.fragment_parent)
            }),
            authored: std::array::from_fn(|index| {
                initial.bodies[index]
                    .as_ref()
                    .map_or(Transform::default(), |body| body.transform)
            }),
            copies: copies.iter().copied().flatten().collect(),
            copy_scale: initial
                .prototype
                .as_ref()
                .map_or([1.0; 3], |body| body.transform.scale),
        }
    }

    /// Size `output`'s copy storage for this layout, before a tick captures into it.
    pub(crate) fn prepare_output(&self, output: &mut CoupledRigidFrame) {
        output
            .copies
            .resize(self.copies.len(), Transform::default());
    }

    pub(crate) fn capture(
        &self,
        world: &PhysicsWorld,
        output: &mut CoupledRigidFrame,
    ) -> Result<(), String> {
        for (index, destination) in output.poses.iter_mut().enumerate() {
            let Some(mut handle) = self.bodies[index] else {
                *destination = self.authored[index];
                continue;
            };
            // Prepared, inactive fragment geometry follows its intact parent,
            // exactly as in the ordinary rigid output path.
            if let Some(parent) = self.fragment_parents[index]
                && !world
                    .dynamics(handle)
                    .map_err(|error| error.to_string())?
                    .enabled
            {
                handle = self.bodies[parent].ok_or("Fluid coupling: fragment parent is absent")?;
            }
            let pose = world.pose(handle).map_err(|error| error.to_string())?;
            *destination = Transform {
                pos: pose.position,
                rot_euler: quat_to_render_scene_euler(pose.rotation),
                ..self.authored[index]
            };
        }
        if output.copies.len() != self.copies.len() {
            return Err("Fluid coupling: output copy storage was not prepared".into());
        }
        for (&handle, destination) in self.copies.iter().zip(&mut output.copies) {
            let pose = world.pose(handle).map_err(|error| error.to_string())?;
            *destination = Transform {
                pos: pose.position,
                rot_euler: quat_to_render_scene_euler(pose.rotation),
                scale: self.copy_scale,
                billboard: false,
            };
        }
        Ok(())
    }
}

/// Both native worlds live on the existing fluid worker. The rigid owner's
/// histories, contacts, events and substep loop are reused without another
/// transport-to-simulation clock.
#[cfg(feature = "gpu-proofs")]
pub(crate) struct Native {
    rigid: RigidSimulation,
    coupling: Option<RigidFluidCoupling>,
    layout: Layout,
    observed_time: Seconds,
    observed_sequence: u64,
    completed: TickStamp,
    rigid_events: Vec<AppliedEvent<ResolvedRigidImpulse>>,
}

#[cfg(feature = "gpu-proofs")]
impl Native {
    pub fn prepare(
        fluid: &mut FluidWorld,
        setup: &Setup,
        epoch: u64,
        domain: FluidDomainLayout,
    ) -> Result<Self, String> {
        let mut rigid = RigidSimulation::with_worker_epoch(epoch)?;
        rigid.advance_worker(&setup.initial, Seconds::ZERO, 0, &mut Uncoupled)?;
        let world = rigid
            .native_world()
            .ok_or("Fluid coupling: rigid world was not prepared")?;
        let (bodies, copies) = rigid.native_handles();
        let layout = Layout::new(&rigid, &setup.initial);
        let selected = bodies
            .iter()
            .enumerate()
            .filter(|(index, _)| setup.colliders.contains_body(*index))
            .filter_map(|(_, &handle)| handle)
            .chain(
                copies
                    .iter()
                    .copied()
                    .flatten()
                    .filter(|_| setup.colliders.copies),
            );
        let mut bindings = Vec::new();
        for body in selected {
            // These are the actual installed Box3D hulls, including compound
            // part boundaries and authored scale, not a second hull cook.
            let meshes = world.hull_meshes(body).map_err(|error| error.to_string())?;
            let mut pose = world.pose(body).map_err(|error| error.to_string())?;
            pose.position = domain.to_native(pose.position);
            let enabled = world
                .dynamics(body)
                .map_err(|error| error.to_string())?
                .enabled;
            for mesh in meshes {
                let collider = fluid
                    .add_mesh(&mesh, MeshRole::Collider, pose)
                    .map_err(|error| error.to_string())?;
                fluid
                    .set_mesh_enabled(collider, enabled)
                    .map_err(|error| error.to_string())?;
                bindings.push((collider, body));
            }
        }
        let coupling = if bindings.is_empty() {
            None
        } else {
            Some(
                RigidFluidCoupling::prepare(
                    fluid,
                    world,
                    &bindings,
                    domain.native_origin(),
                    setup.density,
                )
                .map_err(|error| error.to_string())?,
            )
        };
        Ok(Self {
            rigid,
            coupling,
            layout,
            observed_time: Seconds::ZERO,
            observed_sequence: 0,
            completed: TickStamp { epoch, tick: 0 },
            rigid_events: Vec::with_capacity(crate::node_graph::fluid::impulses::IMPULSE_CAPACITY),
        })
    }

    pub fn capture_initial(&self, output: &mut CoupledRigidFrame) -> Result<(), String> {
        self.layout
            .capture(self.rigid.native_world().expect("prepared world"), output)?;
        output.stamp = self.completed;
        Ok(())
    }

    pub fn prepare_output(&self, output: &mut CoupledRigidFrame) {
        // The two recycled buffers acquire capacity on first use or topology
        // changes, before any native tick starts.
        self.layout.prepare_output(output);
    }

    pub fn step(
        &mut self,
        fluid: &mut FluidWorld,
        request: &mut Request,
        stamp: TickStamp,
        interval: Option<StepInterval>,
        fields: &[FieldInput<'_>],
        impulses: &[AppliedEvent<ResolvedNodeImpulse>],
        live_history: &[Sample],
        domain: FluidDomainLayout,
    ) -> Result<FrameStats, String> {
        if stamp != self.completed {
            return Err("Fluid coupling: rigid and liquid tick boundaries differ".into());
        }
        let end = interval.map_or((stamp.tick + 1) as f64 * TICK, |interval| interval.end.0);
        // Keep one right-hand bracket beyond this tick for interpolation, but
        // do not fill the native owner's shorter history with a whole backlog.
        let bracket = request
            .history
            .partition_point(|sample| sample.time.0 <= end);
        let length =
            (bracket + usize::from(bracket < request.history.len())).min(request.history.len());
        let inputs = &request.history[..length];
        for sample in inputs {
            if sample.sequence <= self.observed_sequence {
                continue;
            }
            self.rigid
                .advance_worker(&sample.inputs, sample.time, 0, &mut Uncoupled)?;
            self.observed_time = sample.time;
            self.observed_sequence = sample.sequence;
        }
        let latest = inputs
            .last()
            .ok_or("Fluid coupling: request has no rigid inputs")?;
        if self.observed_time.0 + 1e-9 < end {
            return Err(
                "Fluid coupling: rigid input history does not reach the requested tick".into(),
            );
        }
        self.rigid_events.clear();
        for event in impulses {
            if let Some(targets) = event.value.target.rigid_targets() {
                if self.rigid_events.len() == self.rigid_events.capacity() {
                    return Err(
                        "Fluid coupling: assigned impulse batch exceeds prepared capacity".into(),
                    );
                }
                self.rigid_events.push(AppliedEvent {
                    source: event.source,
                    applied: event.applied,
                    lateness: event.lateness,
                    value: ResolvedRigidImpulse {
                        field: event.value.field.clone(),
                        targets,
                    },
                });
            }
        }
        let mut plain_stats = None;
        {
            let initial_fields = if interval.is_some() {
                fields.get(..1).unwrap_or(&[])
            } else {
                fields
            };
            let mut participant = Participant {
                fluid,
                coupling: self.coupling.as_mut(),
                layout: &self.layout,
                output: &mut request.output,
                expected: stamp,
                fields: initial_fields,
                live_interval: interval,
                live_impulses: impulses,
                live_origin: domain.native_origin(),
                live_history,
                live_domain: domain,
                plain_stats: &mut plain_stats,
            };
            let result = if let Some(interval) = interval {
                self.rigid.advance_worker_interval(
                    &latest.inputs,
                    interval,
                    &self.rigid_events,
                    &mut participant,
                )
            } else {
                self.rigid.advance_worker_tick(
                    &latest.inputs,
                    self.observed_time,
                    &self.rigid_events,
                    &mut participant,
                )
            };
            // The outer request owns delivery receipts for both participants.
            // Drain the native owner's retained receipt scratch after every
            // attempt, preserving the original assignment without re-enqueueing.
            let receipts = self.rigid.drain_applied_impulses();
            let matching = receipts.len() == self.rigid_events.len()
                && receipts
                    .zip(&self.rigid_events)
                    .all(|(actual, expected)| &actual == expected);
            result?;
            if !matching {
                return Err(
                    "Fluid coupling: native rigid impulse receipts differ from the assigned batch"
                        .into(),
                );
            }
        }
        let completed = TickStamp {
            epoch: stamp.epoch,
            tick: stamp.tick + 1,
        };
        if request.output.stamp != completed {
            return Err("Fluid coupling: rigid owner did not complete the requested tick".into());
        }
        self.completed = completed;
        self.coupling
            .as_ref()
            .and_then(RigidFluidCoupling::last_stats)
            .or(plain_stats)
            .ok_or_else(|| "Fluid coupling: completed tick has no liquid statistics".into())
    }
}

#[cfg(feature = "gpu-proofs")]
struct Participant<'a> {
    fluid: &'a mut FluidWorld,
    coupling: Option<&'a mut RigidFluidCoupling>,
    layout: &'a Layout,
    output: &'a mut CoupledRigidFrame,
    expected: TickStamp,
    fields: &'a [FieldInput<'a>],
    plain_stats: &'a mut Option<FrameStats>,
    live_interval: Option<StepInterval>,
    live_impulses: &'a [AppliedEvent<ResolvedNodeImpulse>],
    live_origin: [f32; 3],
    live_history: &'a [Sample],
    live_domain: FluidDomainLayout,
}

#[cfg(feature = "gpu-proofs")]
enum LiquidFrame<'a> {
    Coupled(CoupledFluidFrame<'a, 'a>),
    Plain(FluidFrame<'a>),
}

#[cfg(feature = "gpu-proofs")]
impl LiquidFrame<'_> {
    fn set_fields(
        &mut self,
        duration: Seconds,
        fields: &[FieldInput<'_>],
    ) -> Result<(), manifold_fluids::FluidError> {
        match self {
            LiquidFrame::Coupled(frame) => frame.set_fields(duration, fields),
            LiquidFrame::Plain(frame) => frame.set_fields(duration, fields),
        }
    }
}

#[cfg(feature = "gpu-proofs")]
struct Exchange<'a> {
    liquid: LiquidFrame<'a>,
    layout: &'a Layout,
    output: &'a mut CoupledRigidFrame,
    stamp: TickStamp,
    plain_stats: &'a mut Option<FrameStats>,
    live_interval: Option<StepInterval>,
    live_impulses: &'a [AppliedEvent<ResolvedNodeImpulse>],
    live_origin: [f32; 3],
    live_history: &'a [Sample],
    live_domain: FluidDomainLayout,
    live_current: Seconds,
    live_active_events: usize,
    live_first_active_event: usize,
    live_fields_prepared: bool,
}

#[cfg(feature = "gpu-proofs")]
impl StepCoupling for Participant<'_> {
    type Error = String;
    type Frame<'a>
        = Exchange<'a>
    where
        Self: 'a;

    fn begin_tick(
        &mut self,
        stamp: TickStamp,
        duration: Seconds,
    ) -> Result<Self::Frame<'_>, String> {
        if stamp != self.expected || (self.live_interval.is_none() && duration != Seconds(TICK)) {
            return Err("Fluid coupling: rigid owner requested a different epoch or tick".into());
        }
        let liquid = if let Some(coupling) = self.coupling.as_deref_mut() {
            LiquidFrame::Coupled(if self.live_interval.is_some() {
                coupling
                    .begin_live_frame(self.fluid, duration, self.fields)
                    .map_err(|error| error.to_string())?
            } else {
                coupling
                    .begin_frame(self.fluid, duration, self.fields)
                    .map_err(|error| error.to_string())?
            })
        } else {
            LiquidFrame::Plain(if self.live_interval.is_some() {
                self.fluid
                    .begin_live_frame_with_fields(duration, self.fields)
                    .map_err(|error| error.to_string())?
            } else {
                self.fluid
                    .begin_frame_with_fields(duration, self.fields)
                    .map_err(|error| error.to_string())?
            })
        };
        Ok(Exchange {
            liquid,
            layout: self.layout,
            output: self.output,
            stamp,
            plain_stats: self.plain_stats,
            live_interval: self.live_interval,
            live_impulses: self.live_impulses,
            live_origin: self.live_origin,
            live_history: self.live_history,
            live_domain: self.live_domain,
            live_current: self
                .live_interval
                .map_or(Seconds::ZERO, |interval| interval.start),
            live_active_events: 0,
            live_first_active_event: 0,
            live_fields_prepared: false,
        })
    }
}

#[cfg(feature = "gpu-proofs")]
impl Exchange<'_> {
    fn set_live_fields(&mut self, duration: Seconds, include_impulses: bool) -> Result<(), String> {
        let Some(interval) = self.live_interval else {
            return Ok(());
        };
        while self.live_active_events < self.live_impulses.len()
            && self.live_impulses[self.live_active_events].source.time.0
                <= self.live_current.0 + 1e-12
        {
            self.live_active_events += 1;
        }
        let segment_end = self
            .live_impulses
            .get(self.live_active_events)
            .map_or(interval.end.0, |event| {
                event.source.time.0.min(interval.end.0)
            });
        let segment_duration = Seconds((segment_end - self.live_current.0).max(0.0));
        let field =
            FluidRuntime::field_at_time(self.live_history, self.live_current, self.live_domain);
        let impulse = ImpulseSum {
            events: if include_impulses {
                &self.live_impulses[self.live_first_active_event..self.live_active_events]
            } else {
                &[]
            },
            origin: self.live_origin,
        };
        let fields = [
            FieldInput {
                field: &field,
                acceleration: 1.0,
                delta_velocity: 0.0,
            },
            FieldInput {
                field: &impulse,
                acceleration: 0.0,
                delta_velocity: 1.0,
            },
        ];
        let field_duration = if include_impulses || self.live_active_events == self.live_impulses.len() {
            duration
        } else {
            Seconds(segment_duration.0.min(duration.0))
        };
        self.liquid
            .set_fields(field_duration, &fields)
            .map_err(|error| error.to_string())?;
        self.live_fields_prepared = true;
        Ok(())
    }
}

#[cfg(feature = "gpu-proofs")]
impl SubstepExchange for Exchange<'_> {
    type Error = String;

    fn next_substep(&mut self, rigid: &PhysicsWorld, maximum: Seconds) -> Result<Seconds, String> {
        let maximum = if let Some(interval) = self.live_interval {
            if !self.live_fields_prepared {
                self.set_live_fields(maximum, false)?;
            }
            self.live_impulses.get(self.live_active_events).map_or(maximum, |event| {
                Seconds(maximum.0.min((event.source.time.0.min(interval.end.0) - self.live_current.0).max(0.0)))
            })
        } else {
            maximum
        };
        if maximum.0 <= 0.0 {
            return Err("Fluid coupling: live event boundary did not advance".into());
        }
        match &mut self.liquid {
            LiquidFrame::Coupled(frame) => frame
                .next_substep(rigid, maximum)
                .map_err(|error| error.to_string()),
            LiquidFrame::Plain(frame) => frame
                .next_substep()
                .map_err(|error| error.to_string())?
                .map(|offer| Seconds(offer.0.min(maximum.0)))
                .ok_or_else(|| "Fluid coupling: liquid finished before the rigid tick".into()),
        }
    }

    fn exchange(&mut self, rigid: &mut PhysicsWorld, duration: Seconds) -> Result<(), String> {
        if self.live_interval.is_some() {
            // Delta-velocity fields are normalized by the accepted duration;
            // install them immediately before this one native substep so an
            // offered step clipped by the owner cannot over-apply an event.
            self.set_live_fields(duration, true)?;
        }
        match &mut self.liquid {
            LiquidFrame::Coupled(frame) => frame.exchange(rigid, duration),
            LiquidFrame::Plain(frame) => frame.advance(duration),
        }
        .map_err(|error| error.to_string())
        .map(|()| {
            if let Some(interval) = self.live_interval {
                self.live_current = Seconds(self.live_current.0 + duration.0);
                self.live_first_active_event = self.live_active_events;
                let segment_end = interval.end.0;
                let next_event = self
                    .live_impulses
                    .get(self.live_active_events)
                    .map_or(segment_end, |event| event.source.time.0.min(segment_end));
                if self.live_current.0 + 1e-12 >= next_event {
                    while self.live_first_active_event < self.live_active_events
                        && self.live_impulses[self.live_first_active_event]
                            .source
                            .time
                            .0
                            <= self.live_current.0 + 1e-12
                    {
                        self.live_first_active_event += 1;
                    }
                    self.live_fields_prepared = false;
                }
            }
        })
    }

    fn finish(self, rigid: &PhysicsWorld) -> Result<(), String> {
        self.layout.capture(rigid, self.output)?;
        match self.liquid {
            LiquidFrame::Coupled(frame) => {
                frame.finish(rigid).map_err(|error| error.to_string())?
            }
            LiquidFrame::Plain(frame) => {
                *self.plain_stats = Some(frame.finish().map_err(|error| error.to_string())?)
            }
        }
        self.output.stamp = TickStamp {
            epoch: self.stamp.epoch,
            tick: self.stamp.tick + 1,
        };
        Ok(())
    }
}
