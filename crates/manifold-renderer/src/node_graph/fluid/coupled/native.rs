use manifold_fluids::{
    CoupledFluidFrame, FluidFrame, FluidWorld, FrameStats, MeshRole, RigidFluidCoupling,
};
use manifold_physics::stepping::{StepCoupling, SubstepExchange, Uncoupled};
use manifold_physics::{BodyHandle, FieldInput, PhysicsWorld, Seconds, TickStamp};

use crate::node_graph::physics::{MAX_BODIES, RigidSimulation};
use crate::node_graph::primitives::quat_to_render_scene_euler;
use crate::node_graph::transform::Transform;

use super::{CoupledRigidFrame, Request, Setup};
use crate::node_graph::fluid::{FluidDomainLayout, TICK};

struct Layout {
    bodies: [Option<BodyHandle>; MAX_BODIES],
    fragment_parents: [Option<usize>; MAX_BODIES],
    authored: [Transform; MAX_BODIES],
    copies: Vec<BodyHandle>,
    copy_scale: [f32; 3],
}

impl Layout {
    fn capture(&self, world: &PhysicsWorld, output: &mut CoupledRigidFrame) -> Result<(), String> {
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
pub(crate) struct Native {
    rigid: RigidSimulation,
    coupling: Option<RigidFluidCoupling>,
    layout: Layout,
    observed_time: Seconds,
    observed_sequence: u64,
    completed: TickStamp,
}

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
        let layout = Layout {
            bodies: *bodies,
            fragment_parents: std::array::from_fn(|index| {
                setup.initial.bodies[index]
                    .as_ref()
                    .and_then(|body| body.fragment_parent)
            }),
            authored: std::array::from_fn(|index| {
                setup.initial.bodies[index]
                    .as_ref()
                    .map_or(Transform::default(), |body| body.transform)
            }),
            copies: copies.iter().copied().flatten().collect(),
            copy_scale: setup
                .initial
                .prototype
                .as_ref()
                .map_or([1.0; 3], |body| body.transform.scale),
        };
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
                RigidFluidCoupling::prepare(fluid, world, &bindings, domain.min, setup.density)
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
        output
            .copies
            .resize(self.layout.copies.len(), Transform::default());
    }

    pub fn step(
        &mut self,
        fluid: &mut FluidWorld,
        request: &mut Request,
        stamp: TickStamp,
        fields: &[FieldInput<'_>],
    ) -> Result<FrameStats, String> {
        if stamp != self.completed {
            return Err("Fluid coupling: rigid and liquid tick boundaries differ".into());
        }
        let end = (stamp.tick + 1) as f64 * TICK;
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
        let mut plain_stats = None;
        {
            let mut participant = Participant {
                fluid,
                coupling: self.coupling.as_mut(),
                layout: &self.layout,
                output: &mut request.output,
                expected: stamp,
                fields,
                plain_stats: &mut plain_stats,
            };
            self.rigid
                .advance_worker(&latest.inputs, self.observed_time, 1, &mut participant)?;
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

struct Participant<'a, 'field> {
    fluid: &'a mut FluidWorld,
    coupling: Option<&'a mut RigidFluidCoupling>,
    layout: &'a Layout,
    output: &'a mut CoupledRigidFrame,
    expected: TickStamp,
    fields: &'a [FieldInput<'field>],
    plain_stats: &'a mut Option<FrameStats>,
}

enum LiquidFrame<'a> {
    Coupled(CoupledFluidFrame<'a, 'a>),
    Plain(FluidFrame<'a>),
}

struct Exchange<'a> {
    liquid: LiquidFrame<'a>,
    layout: &'a Layout,
    output: &'a mut CoupledRigidFrame,
    stamp: TickStamp,
    plain_stats: &'a mut Option<FrameStats>,
}

impl StepCoupling for Participant<'_, '_> {
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
        if stamp != self.expected || duration != Seconds(TICK) {
            return Err("Fluid coupling: rigid owner requested a different epoch or tick".into());
        }
        let liquid = if let Some(coupling) = self.coupling.as_deref_mut() {
            LiquidFrame::Coupled(
                coupling
                    .begin_frame(self.fluid, duration, self.fields)
                    .map_err(|error| error.to_string())?,
            )
        } else {
            LiquidFrame::Plain(
                self.fluid
                    .begin_frame_with_fields(duration, self.fields)
                    .map_err(|error| error.to_string())?,
            )
        };
        Ok(Exchange {
            liquid,
            layout: self.layout,
            output: self.output,
            stamp,
            plain_stats: self.plain_stats,
        })
    }
}

impl SubstepExchange for Exchange<'_> {
    type Error = String;

    fn next_substep(&mut self, rigid: &PhysicsWorld, maximum: Seconds) -> Result<Seconds, String> {
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
        match &mut self.liquid {
            LiquidFrame::Coupled(frame) => frame.exchange(rigid, duration),
            LiquidFrame::Plain(frame) => frame.advance(duration),
        }
        .map_err(|error| error.to_string())
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
