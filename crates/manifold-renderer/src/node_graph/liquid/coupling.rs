//! The rigid owner of a coupled GPU liquid (`docs/LIQUID_SOLVER_SEAM_DESIGN.md`
//! section 3.3, D6). The scene's Box3D world runs one tick behind the liquid:
//! liquid tick k runs on the GPU from the bodies' state at its start, and
//! once the frame that ran it has retired, [`LiquidRigidOwner::settle`] has
//! the solver decode the tick's reaction into one impulse per body and steps
//! Box3D over the same tick. The content thread never waits: while the
//! reaction is in flight the pair holds.

use std::sync::Arc;

use manifold_physics::stepping::{StepCoupling, SubstepExchange, Uncoupled};
use manifold_physics::{BodyHandle, BodyImpulse, PhysicsWorld, Seconds, TickStamp};

use super::bodies::LiquidBody;
use crate::node_graph::fluid::{CoupledRigidFrame, CoupledRigidLayout, TICK};
use crate::node_graph::fluid_role::PreparedFluidGeometry;
use crate::node_graph::physics::{RigidImpulseTargets, RigidSceneInputs, RigidSimulation};

/// One coupled Box3D body, in row order.
struct Coupled {
    handle: BodyHandle,
    /// Scene body slot; None for a copy, which takes the prototype's controls.
    slot: Option<usize>,
    /// The largest face of the body's local hull box.
    face_area: f32,
    friction: f32,
}

/// The liquid tick whose reaction the GPU is still writing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PendingTick {
    pub tick: u64,
    /// Frame-clock stamp of the frame that ran the tick (0 = already retired).
    pub stamp: u64,
}

/// A dynamic body with a collider shape: it takes the liquid's reaction. A
/// NaN inverse mass is not dynamic.
pub fn takes_reaction(row: &LiquidBody) -> bool {
    row.position_inv_mass[3] > 0.0 && row.accel_shape[3] >= 0.0
}

/// Floats one body holds in a float reaction: linear impulse (N·s), then
/// angular impulse about the centre of mass (N·m·s), each padded to four.
pub const REACTION_FLOATS: usize = 8;

/// A pending tick's float reaction as one impulse per row; rows that take no
/// reaction are left alone. `offset` counts the rows before the first
/// coupled body's (the Collider roles').
pub fn decode_reaction(
    offset: usize,
    rows: &[LiquidBody],
    reaction: Option<&[f32]>,
    impulses: &mut [BodyImpulse],
) -> Result<(), String> {
    let reaction = reaction.ok_or("Liquid coupling: the reaction is not readable")?;
    if reaction.len() < (offset + rows.len()) * REACTION_FLOATS {
        return Err("Liquid coupling: the reaction array is smaller than the bodies".into());
    }
    for (index, (row, impulse)) in rows.iter().zip(impulses.iter_mut()).enumerate() {
        if takes_reaction(row) {
            let base = (offset + index) * REACTION_FLOATS;
            impulse.linear = std::array::from_fn(|axis| reaction[base + axis]);
            impulse.angular = std::array::from_fn(|axis| reaction[base + 4 + axis]);
        }
    }
    Ok(())
}

/// A coupled domain's rigid world, stepped only by settled liquid ticks.
pub struct LiquidRigidOwner {
    rigid: RigidSimulation,
    layout: CoupledRigidLayout,
    initial: RigidSceneInputs,
    colliders: RigidImpulseTargets,
    epoch: u64,
    completed: u64,
    bodies: Vec<Coupled>,
    geometries: Vec<Arc<PreparedFluidGeometry>>,
    frame: CoupledRigidFrame,
    rows: Vec<LiquidBody>,
    impulses: Vec<BodyImpulse>,
    pending: Option<PendingTick>,
}

impl LiquidRigidOwner {
    /// Prepare the rigid world of `inputs` at tick 0 of `epoch`. Hull
    /// geometry, and so the distance lattices, carries over from `reuse` when
    /// its topology is unchanged.
    pub fn new(
        inputs: &RigidSceneInputs,
        colliders: RigidImpulseTargets,
        epoch: u64,
        reuse: Option<&LiquidRigidOwner>,
    ) -> Result<Self, String> {
        let mut rigid = RigidSimulation::with_worker_epoch(epoch)?;
        rigid.advance_worker(inputs, Seconds::ZERO, 0, &mut Uncoupled)?;
        let layout = CoupledRigidLayout::new(&rigid, inputs);
        let world = rigid.native_world().ok_or("Liquid coupling: the rigid world was not prepared")?;
        let (handles, copies) = rigid.native_handles();
        let selected: Vec<(BodyHandle, Option<usize>)> = handles
            .iter()
            .enumerate()
            .filter(|(index, _)| colliders.contains_body(*index))
            .filter_map(|(index, handle)| handle.map(|handle| (handle, Some(index))))
            .chain(copies.iter().flatten().filter(|_| colliders.copies).map(|&handle| (handle, None)))
            .collect();
        let reuse = reuse.filter(|owner| owner.matches(inputs, colliders));
        let known = |slot: Option<usize>| {
            reuse.and_then(|owner| {
                owner
                    .bodies
                    .iter()
                    .zip(&owner.geometries)
                    .find(|(body, _)| body.slot == slot)
                    .map(|(body, geometry)| (Arc::clone(geometry), body.face_area))
            })
        };
        let mut bodies = Vec::with_capacity(selected.len());
        let mut geometries = Vec::with_capacity(selected.len());
        let mut prototype: Option<(Arc<PreparedFluidGeometry>, f32)> = None;
        for (handle, slot) in selected {
            let (geometry, face_area) = match (known(slot), slot, &prototype) {
                (Some(known), _, _) => known,
                (None, None, Some((geometry, area))) => (Arc::clone(geometry), *area),
                _ => {
                    let built = hull_geometry(world, handle)?;
                    if slot.is_none() {
                        prototype = Some((Arc::clone(&built.0), built.1));
                    }
                    built
                }
            };
            bodies.push(Coupled { handle, slot, face_area, friction: 0.0 });
            geometries.push(geometry);
        }
        let mut frame = CoupledRigidFrame::default();
        layout.prepare_output(&mut frame);
        layout.capture(world, &mut frame)?;
        frame.stamp = TickStamp { epoch, tick: 0 };
        let mut owner = Self {
            rigid,
            layout,
            initial: inputs.clone(),
            colliders,
            epoch,
            completed: 0,
            bodies,
            geometries,
            frame,
            rows: Vec::new(),
            impulses: Vec::new(),
            pending: None,
        };
        owner.update_friction(inputs);
        let world = owner.rigid.native_world().expect("prepared above");
        capture_rows(world, &owner.bodies, &mut owner.rows)?;
        Ok(owner)
    }

    /// Whether `inputs` and `colliders` keep this owner's native topology.
    pub fn matches(&self, inputs: &RigidSceneInputs, colliders: RigidImpulseTargets) -> bool {
        self.colliders == colliders
            && self.initial.same_topology(inputs)
            && self.initial.bodies.iter().zip(&inputs.bodies).all(|(left, right)| {
                left.as_ref().and_then(|body| body.fragment_parent)
                    == right.as_ref().and_then(|body| body.fragment_parent)
            })
    }

    /// The accepted rigid frame: Box3D at the end of the last settled tick.
    pub fn frame(&self) -> &CoupledRigidFrame {
        &self.frame
    }

    /// One row per coupled body at the start of the next liquid tick, in the
    /// geometries' order; a row's shape index counts from the first coupled
    /// body.
    pub fn rows(&self) -> &[LiquidBody] {
        &self.rows
    }

    /// Each coupled body's hull about its centre of mass, unscaled.
    pub fn geometries(&self) -> &[Arc<PreparedFluidGeometry>] {
        &self.geometries
    }

    /// Each coupled body's largest local hull-box face in m², in row order.
    pub fn face_areas(&self) -> impl Iterator<Item = f32> + '_ {
        self.bodies.iter().map(|body| body.face_area)
    }

    /// Box3D ticks settled in this epoch.
    pub fn completed(&self) -> u64 {
        self.completed
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The liquid tick now running on the GPU; the next frame settles it.
    pub fn set_pending(&mut self, pending: PendingTick) {
        self.pending = Some(pending);
    }

    pub fn pending(&self) -> Option<PendingTick> {
        self.pending
    }

    pub fn rigid(&self) -> &RigidSimulation {
        &self.rigid
    }

    pub fn rigid_mut(&mut self) -> &mut RigidSimulation {
        &mut self.rigid
    }

    /// Settle the pending tick if the GPU finished it: decode its reaction,
    /// step Box3D once, and return how many liquid ticks may run this frame
    /// (0 while the reaction is still in flight, else 1). `decode` runs only
    /// once `complete` says the frame retired; it fills one impulse per row,
    /// each already naming its body, and only rows that take a reaction reach
    /// Box3D.
    pub fn settle(
        &mut self,
        inputs: &RigidSceneInputs,
        complete: impl FnOnce(u64) -> bool,
        decode: impl FnOnce(PendingTick, &[LiquidBody], &mut [BodyImpulse]) -> Result<(), String>,
    ) -> Result<u32, String> {
        let Some(pending) = self.pending else { return Ok(1) };
        if !complete(pending.stamp) {
            return Ok(0);
        }
        self.pending = None;
        if pending.tick != self.completed {
            return Err(format!(
                "Liquid coupling: the reaction of liquid tick {} does not follow rigid tick {}",
                pending.tick, self.completed
            ));
        }
        self.impulses.clear();
        self.impulses.extend(self.bodies.iter().map(|body| BodyImpulse {
            body: body.handle,
            linear: [0.0; 3],
            angular: [0.0; 3],
        }));
        decode(pending, &self.rows, &mut self.impulses)?;
        let mut rows = self.rows.iter();
        self.impulses.retain(|_| rows.next().is_some_and(takes_reaction));
        self.step(inputs)?;
        Ok(1)
    }

    fn update_friction(&mut self, inputs: &RigidSceneInputs) {
        for body in &mut self.bodies {
            let source = match body.slot {
                Some(slot) => inputs.bodies[slot].as_ref(),
                None => inputs.prototype.as_ref(),
            };
            body.friction = source.map_or(0.0, |body| body.friction.clamp(0.0, 1.0));
        }
    }

    /// Step Box3D over tick `completed` with the decoded impulses.
    fn step(&mut self, inputs: &RigidSceneInputs) -> Result<(), String> {
        self.update_friction(inputs);
        let expected = TickStamp { epoch: self.epoch, tick: self.completed };
        let mut coupling = LiquidCoupling {
            expected,
            layout: &self.layout,
            frame: &mut self.frame,
            bodies: &self.bodies,
            rows: &mut self.rows,
            impulses: &self.impulses,
            begun: false,
            applied: false,
            finished: false,
        };
        let now = Seconds((self.completed + 1) as f64 * TICK);
        self.rigid.advance_worker(inputs, now, 1, &mut coupling)?;
        if !coupling.finished {
            return Err("Liquid coupling: the rigid owner did not step the settled tick".into());
        }
        self.completed += 1;
        Ok(())
    }
}

/// A body's installed hulls about its centre of mass, and its largest local
/// box face.
fn hull_geometry(world: &PhysicsWorld, handle: BodyHandle) -> Result<(Arc<PreparedFluidGeometry>, f32), String> {
    let mut meshes = world.hull_meshes(handle).map_err(|error| error.to_string())?;
    let centre = world.local_center_of_mass(handle).map_err(|error| error.to_string())?;
    let (mut low, mut high) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for vertex in meshes.iter_mut().flat_map(|mesh| mesh.vertices.iter_mut()) {
        for axis in 0..3 {
            vertex[axis] -= centre[axis];
            low[axis] = low[axis].min(vertex[axis]);
            high[axis] = high[axis].max(vertex[axis]);
        }
    }
    if meshes.is_empty() || !(0..3).all(|axis| high[axis] > low[axis]) {
        return Err("Liquid coupling: a coupled body has no solid hull".into());
    }
    let size: [f32; 3] = std::array::from_fn(|axis| high[axis] - low[axis]);
    let face = (size[0] * size[1]).max(size[1] * size[2]).max(size[0] * size[2]);
    Ok((Arc::new(PreparedFluidGeometry::new(meshes)), face))
}

/// Each coupled body's state as a row: its centre of mass, 1/m, velocities,
/// world inverse inertia (angular acceleration in the rows' w) and the
/// predicted linear acceleration; shape −1 while disabled.
fn capture_rows(world: &PhysicsWorld, bodies: &[Coupled], rows: &mut Vec<LiquidBody>) -> Result<(), String> {
    rows.clear();
    for (index, body) in bodies.iter().enumerate() {
        let d = world.dynamics(body.handle).map_err(|error| error.to_string())?;
        let pose = world.pose(body.handle).map_err(|error| error.to_string())?;
        let (c, v, w, i, a, alpha) = (
            d.center_of_mass,
            d.linear_velocity,
            d.angular_velocity,
            d.inverse_inertia,
            d.external_linear_acceleration,
            d.external_angular_acceleration,
        );
        rows.push(LiquidBody {
            position_inv_mass: [c[0], c[1], c[2], d.inverse_mass],
            rotation: pose.rotation,
            linear_velocity: [v[0], v[1], v[2], body.friction],
            angular_velocity: [w[0], w[1], w[2], 0.0],
            inv_inertia_x: [i[0][0], i[0][1], i[0][2], alpha[0]],
            inv_inertia_y: [i[1][0], i[1][1], i[1][2], alpha[1]],
            inv_inertia_z: [i[2][0], i[2][1], i[2][2], alpha[2]],
            accel_shape: [a[0], a[1], a[2], if d.enabled { index as f32 } else { -1.0 }],
        });
    }
    Ok(())
}

/// The single exchange of one settled tick: the liquid's reaction goes in as
/// one impulse before Box3D's first contact substep; the end state comes out.
pub struct LiquidCoupling<'a> {
    expected: TickStamp,
    layout: &'a CoupledRigidLayout,
    frame: &'a mut CoupledRigidFrame,
    bodies: &'a [Coupled],
    rows: &'a mut Vec<LiquidBody>,
    impulses: &'a [BodyImpulse],
    begun: bool,
    applied: bool,
    finished: bool,
}

impl<'a> StepCoupling for LiquidCoupling<'a> {
    type Error = String;
    type Frame<'b>
        = &'b mut LiquidCoupling<'a>
    where
        Self: 'b;

    fn begin_tick(&mut self, stamp: TickStamp, duration: Seconds) -> Result<Self::Frame<'_>, String> {
        if self.begun {
            return Err("Liquid coupling: one settled liquid tick steps Box3D once".into());
        }
        if stamp != self.expected {
            return Err(format!(
                "Liquid coupling: rigid tick {:?} does not match liquid tick {:?}",
                stamp, self.expected
            ));
        }
        if (duration.0 - TICK).abs() > 1e-12 {
            return Err("Liquid coupling: rigid and liquid ticks differ in length".into());
        }
        self.begun = true;
        Ok(self)
    }
}

impl SubstepExchange for &mut LiquidCoupling<'_> {
    type Error = String;

    fn next_substep(&mut self, _: &PhysicsWorld, maximum: Seconds) -> Result<Seconds, String> {
        Ok(maximum)
    }

    fn exchange(&mut self, rigid: &mut PhysicsWorld, _: Seconds) -> Result<(), String> {
        if !self.applied {
            self.applied = true;
            rigid.apply_impulses(self.impulses).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn finish(self, rigid: &PhysicsWorld) -> Result<(), String> {
        self.layout.capture(rigid, self.frame)?;
        self.frame.stamp = TickStamp { epoch: self.expected.epoch, tick: self.expected.tick + 1 };
        capture_rows(rigid, self.bodies, self.rows)?;
        self.finished = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::physics::RigidBody;
    use crate::node_graph::transform::Transform;

    fn scene() -> RigidSceneInputs {
        let mut scene = RigidSceneInputs { gravity: [0.0, -9.81, 0.0], ..RigidSceneInputs::default() };
        scene.bodies[0] = Some(RigidBody {
            transform: Transform { pos: [0.0, 1.0, 0.0], scale: [0.4; 3], ..Transform::default() },
            // 32 kg: the cube's edge is its scale times CUBE_EDGE_PER_SCALE.
            density: 32.0 / (0.4 * crate::node_graph::liquid::conformance::CUBE_EDGE_PER_SCALE).powi(3),
            bounce: 0.0,
            ..RigidBody::default()
        });
        scene
    }

    fn no_reaction(_: PendingTick, _: &[LiquidBody], _: &mut [BodyImpulse]) -> Result<(), String> {
        Ok(())
    }

    /// While the frame that ran liquid tick k is in flight the pair holds: no
    /// tick is allowed, Box3D stays at k and the tick stays pending. Once it
    /// retires, Box3D steps tick k and the next liquid tick may run.
    #[test]
    fn liquid_coupled_holds_when_reaction_pending() {
        let scene = scene();
        let colliders = RigidImpulseTargets { bodies: 1, copies: false };
        let mut owner = LiquidRigidOwner::new(&scene, colliders, 7, None).expect("owner");
        assert_eq!((owner.completed(), owner.rows().len(), owner.geometries().len()), (0, 1, 1));
        let row = owner.rows()[0];
        assert!((row.position_inv_mass[3] - 1.0 / 32.0).abs() < 1e-6);
        assert!((row.position_inv_mass[1] - 1.0).abs() < 1e-5, "the row sits at the centre of mass");
        assert!((row.accel_shape[1] + 9.81).abs() < 1e-4 && row.accel_shape[3] == 0.0);

        assert_eq!(owner.settle(&scene, |_| panic!("nothing pending"), no_reaction).unwrap(), 1);
        let pending = PendingTick { tick: 0, stamp: 42 };
        owner.set_pending(pending);
        let held = owner
            .settle(&scene, |stamp| {
                assert_eq!(stamp, 42);
                false
            }, |_, _, _| panic!("reaction read before the frame retired"))
            .unwrap();
        assert_eq!((held, owner.completed(), owner.pending()), (0, 0, Some(pending)));
        assert_eq!(owner.frame().stamp, TickStamp { epoch: 7, tick: 0 });

        assert_eq!(owner.settle(&scene, |_| true, no_reaction).unwrap(), 1);
        assert_eq!((owner.completed(), owner.pending()), (1, None));
        assert_eq!(owner.frame().stamp, TickStamp { epoch: 7, tick: 1 });
        let fallen = owner.rows()[0].linear_velocity[1];
        assert!((fallen + 9.81 * TICK as f32).abs() < 1e-3, "free fall over one tick: {fallen}");
    }

    /// A decoded impulse of m·Δv (Δv = +1 m/s) reaches Box3D on top of the
    /// tick's gravity; the decoder sees one impulse per row, naming its body.
    #[test]
    fn liquid_coupled_reaction_reaches_the_body() {
        let scene = scene();
        let colliders = RigidImpulseTargets { bodies: 1, copies: false };
        let mut owner = LiquidRigidOwner::new(&scene, colliders, 3, None).expect("owner");
        owner.set_pending(PendingTick { tick: 0, stamp: 0 });
        owner
            .settle(&scene, |_| true, |pending, rows, impulses| {
                assert_eq!((pending.tick, rows.len(), impulses.len()), (0, 1, 1));
                impulses[0].linear[1] = 1.0 / rows[0].position_inv_mass[3];
                Ok(())
            })
            .unwrap();
        let v = owner.rows()[0].linear_velocity[1];
        assert!((v - (1.0 - 9.81 * TICK as f32)).abs() < 1e-3, "{v}");
    }

    /// A float reaction of m·(1 m/s) up, read past one Collider role's row,
    /// reaches Box3D on top of the tick's gravity; a reaction too short for
    /// the rows is refused by name.
    #[test]
    fn liquid_coupled_float_reaction_decodes_to_body_impulse() {
        let scene = scene();
        let colliders = RigidImpulseTargets { bodies: 1, copies: false };
        let mut owner = LiquidRigidOwner::new(&scene, colliders, 3, None).expect("owner");
        let mut reaction = [0.0f32; 2 * REACTION_FLOATS];
        reaction[REACTION_FLOATS + 1] = 32.0;
        reaction[REACTION_FLOATS + 5] = 1.0e-9;
        owner.set_pending(PendingTick { tick: 0, stamp: 0 });
        owner
            .settle(&scene, |_| true, |_, rows, impulses| {
                decode_reaction(1, rows, Some(&reaction[..]), impulses)?;
                assert_eq!((impulses[0].linear, impulses[0].angular), ([0.0, 32.0, 0.0], [0.0, 1.0e-9, 0.0]));
                Ok(())
            })
            .unwrap();
        let v = owner.rows()[0].linear_velocity[1];
        assert!((v - (1.0 - 9.81 * TICK as f32)).abs() < 1e-3, "{v}");
        owner.set_pending(PendingTick { tick: 1, stamp: 0 });
        let error = owner
            .settle(&scene, |_| true, |_, rows, impulses| decode_reaction(2, rows, Some(&reaction[..]), impulses))
            .unwrap_err();
        assert!(error.contains("smaller than the bodies"), "{error}");
    }
}
