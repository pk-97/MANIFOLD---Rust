//! The rigid owner of a coupled matter domain (`docs/GPU_MPM_SOLVER_DESIGN.md`
//! section 5, D12). The scene's Box3D world runs one tick behind the liquid:
//! fluid tick k runs on the GPU from the bodies' state at its start, and once
//! the frame that ran it has retired, [`RigidOwner::settle`] turns the tick's
//! reaction words into one impulse per body and steps Box3D over the same
//! tick. The content thread never waits: while the reaction is in flight the
//! pair holds.

use std::sync::Arc;

use manifold_physics::stepping::{StepCoupling, SubstepExchange, Uncoupled};
use manifold_physics::{BodyHandle, BodyImpulse, PhysicsWorld, Seconds, TickStamp};

use super::{MAX_SUBSTEPS, MatterBody, REACTION_WORDS, WATER_DENSITY, substeps_per_tick};
use crate::node_graph::fluid::{CoupledRigidFrame, CoupledRigidLayout, TICK};
use crate::node_graph::fluid_role::PreparedFluidGeometry;
use crate::node_graph::physics::{RigidImpulseTargets, RigidSceneInputs, RigidSimulation};

/// One coupled Box3D body, in row order.
struct Coupled {
    handle: BodyHandle,
    /// Scene body slot; None for a copy, which takes the prototype's controls.
    slot: Option<usize>,
    /// The largest face of the body's local hull box, the D4 `A_b`.
    face_area: f32,
    friction: f32,
}

/// The fluid tick whose reaction words the GPU is still writing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReactionSlot {
    pub tick: u64,
    /// Frame-clock stamp of the frame that ran the tick (0 = already retired).
    pub stamp: u64,
    /// The tick's momentum unit U: a word is value·2^24/U.
    pub unit: f32,
    pub cell_size: f32,
    /// Rows before the first coupled body's (the Collider roles').
    pub offset: usize,
}

/// A coupled domain's rigid world, stepped only by settled fluid ticks.
pub struct RigidOwner {
    rigid: RigidSimulation,
    layout: CoupledRigidLayout,
    initial: RigidSceneInputs,
    colliders: RigidImpulseTargets,
    epoch: u64,
    completed: u64,
    bodies: Vec<Coupled>,
    geometries: Vec<Arc<PreparedFluidGeometry>>,
    frame: CoupledRigidFrame,
    rows: Vec<MatterBody>,
    impulses: Vec<BodyImpulse>,
    pending: Option<ReactionSlot>,
}

impl RigidOwner {
    /// Prepare the rigid world of `inputs` at tick 0 of `epoch`. Hull
    /// geometry, and so the distance lattices, carries over from `reuse` when
    /// its topology is unchanged.
    pub fn new(
        inputs: &RigidSceneInputs,
        colliders: RigidImpulseTargets,
        epoch: u64,
        reuse: Option<&RigidOwner>,
    ) -> Result<Self, String> {
        let mut rigid = RigidSimulation::with_worker_epoch(epoch)?;
        rigid.advance_worker(inputs, Seconds::ZERO, 0, &mut Uncoupled)?;
        let layout = CoupledRigidLayout::new(&rigid, inputs);
        let world = rigid.native_world().ok_or("Matter coupling: the rigid world was not prepared")?;
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

    /// One row per coupled body at the start of the next fluid tick, in the
    /// geometries' order; a row's shape index counts from the first coupled
    /// body.
    pub fn rows(&self) -> &[MatterBody] {
        &self.rows
    }

    /// Each coupled body's hull about its centre of mass, unscaled.
    pub fn geometries(&self) -> &[Arc<PreparedFluidGeometry>] {
        &self.geometries
    }

    /// Box3D ticks settled in this epoch.
    pub fn completed(&self) -> u64 {
        self.completed
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The fluid tick now running on the GPU; the next frame settles it.
    pub fn set_pending(&mut self, slot: ReactionSlot) {
        self.pending = Some(slot);
    }

    pub fn pending(&self) -> Option<ReactionSlot> {
        self.pending
    }

    pub fn rigid(&self) -> &RigidSimulation {
        &self.rigid
    }

    pub fn rigid_mut(&mut self) -> &mut RigidSimulation {
        &mut self.rigid
    }

    /// Settle the pending fluid tick if its frame has retired: apply its
    /// reaction and step Box3D over it. Returns the ticks the liquid may run
    /// this frame: 0 while the reaction is still in flight, else 1.
    /// `words` is read only once `complete` says the frame retired.
    pub fn settle<'w>(
        &mut self,
        inputs: &RigidSceneInputs,
        complete: impl FnOnce(u64) -> bool,
        words: impl FnOnce() -> Option<&'w [i32]>,
    ) -> Result<u32, String> {
        let Some(slot) = self.pending else { return Ok(1) };
        if !complete(slot.stamp) {
            return Ok(0);
        }
        self.pending = None;
        if slot.tick != self.completed {
            return Err(format!(
                "Matter coupling: the reaction of fluid tick {} does not follow rigid tick {}",
                slot.tick, self.completed
            ));
        }
        let words = words().ok_or("Matter coupling: the reaction words are not readable")?;
        self.decode(slot, words)?;
        self.step(inputs)?;
        Ok(1)
    }

    /// The reaction words of `slot` as one centre-of-mass impulse per body.
    fn decode(&mut self, slot: ReactionSlot, words: &[i32]) -> Result<(), String> {
        let stride = REACTION_WORDS as usize;
        if words.len() < (slot.offset + self.bodies.len()) * stride {
            return Err("Matter coupling: the reaction array is smaller than the bodies".into());
        }
        let scale = f64::from(slot.unit) / 16_777_216.0;
        self.impulses.clear();
        for (index, (body, row)) in self.bodies.iter().zip(&self.rows).enumerate() {
            if !takes_reaction(row) {
                continue;
            }
            let base = (slot.offset + index) * stride;
            let mass = 1.0 / f64::from(row.position_inv_mass[3]);
            let at = |word: usize, factor: f64| (f64::from(words[base + word]) * scale * factor) as f32;
            let arm = f64::from(slot.cell_size);
            self.impulses.push(BodyImpulse {
                body: body.handle,
                linear: [at(0, mass), at(1, mass), at(2, mass)],
                angular: [at(6, mass * arm), at(7, mass * arm), at(8, mass * arm)],
            });
        }
        Ok(())
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
        let mut coupling = MatterCoupling {
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
            return Err("Matter coupling: the rigid owner did not step the settled tick".into());
        }
        self.completed += 1;
        Ok(())
    }

    /// The D4 body term: the shortest `0.5·(dx/c)·√(m_b/(ρ0·A_b·dx))` over the
    /// dynamic coupled bodies, with `c` the wave speed. Errors, naming the
    /// body, when it needs more than [`MAX_SUBSTEPS`] substeps with `dt_f`.
    pub fn body_limit(&self, cell_size: f32, wave: f32, v_est: f32) -> Result<Option<f32>, String> {
        let mut limit: Option<(f32, usize)> = None;
        for (index, (body, row)) in self.bodies.iter().zip(&self.rows).enumerate() {
            if !takes_reaction(row) {
                continue;
            }
            let dt = body_substep(cell_size, wave, 1.0 / row.position_inv_mass[3], body.face_area);
            if limit.is_none_or(|(known, _)| dt < known) {
                limit = Some((dt, index));
            }
        }
        let Some((dt, index)) = limit else { return Ok(None) };
        if substeps_per_tick(cell_size, wave, v_est, Some(dt), None) > MAX_SUBSTEPS {
            let row = &self.rows[index];
            return Err(format!(
                "Matter coupling: coupled body {index} ({:.3} kg, {:.4} m² face) is too light for this resolution: it needs more than {MAX_SUBSTEPS} substeps. Make it heavier or lower the Stiffness.",
                1.0 / row.position_inv_mass[3],
                self.bodies[index].face_area
            ));
        }
        Ok(Some(dt))
    }
}

/// A dynamic body with a collider shape. A NaN inverse mass is not dynamic.
fn takes_reaction(row: &MatterBody) -> bool {
    row.position_inv_mass[3] > 0.0 && row.accel_shape[3] >= 0.0
}

/// D4's `dt_b` for one body of `mass` kg and largest face `area` m².
pub fn body_substep(cell_size: f32, wave: f32, mass: f32, area: f32) -> f32 {
    let dx = f64::from(cell_size);
    let ratio = f64::from(mass) / (f64::from(WATER_DENSITY) * f64::from(area) * dx);
    (0.5 * dx / f64::from(wave) * ratio.sqrt()) as f32
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
        return Err("Matter coupling: a coupled body has no solid hull".into());
    }
    let size: [f32; 3] = std::array::from_fn(|axis| high[axis] - low[axis]);
    let face = (size[0] * size[1]).max(size[1] * size[2]).max(size[0] * size[2]);
    Ok((Arc::new(PreparedFluidGeometry::new(meshes)), face))
}

/// Each coupled body's state as a row: its centre of mass, 1/m, velocities,
/// world inverse inertia (angular acceleration in the rows' w) and the
/// predicted linear acceleration; shape −1 while disabled.
fn capture_rows(world: &PhysicsWorld, bodies: &[Coupled], rows: &mut Vec<MatterBody>) -> Result<(), String> {
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
        rows.push(MatterBody {
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
struct MatterCoupling<'a> {
    expected: TickStamp,
    layout: &'a CoupledRigidLayout,
    frame: &'a mut CoupledRigidFrame,
    bodies: &'a [Coupled],
    rows: &'a mut Vec<MatterBody>,
    impulses: &'a [BodyImpulse],
    begun: bool,
    applied: bool,
    finished: bool,
}

impl<'a> StepCoupling for MatterCoupling<'a> {
    type Error = String;
    type Frame<'b>
        = &'b mut MatterCoupling<'a>
    where
        Self: 'b;

    fn begin_tick(&mut self, stamp: TickStamp, duration: Seconds) -> Result<Self::Frame<'_>, String> {
        if self.begun {
            return Err("Matter coupling: one settled fluid tick steps Box3D once".into());
        }
        if stamp != self.expected {
            return Err(format!(
                "Matter coupling: rigid tick {:?} does not match fluid tick {:?}",
                stamp, self.expected
            ));
        }
        if (duration.0 - TICK).abs() > 1e-12 {
            return Err("Matter coupling: rigid and liquid ticks differ in length".into());
        }
        self.begun = true;
        Ok(self)
    }
}

impl SubstepExchange for &mut MatterCoupling<'_> {
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
            mass: 32.0,
            bounce: 0.0,
            ..RigidBody::default()
        });
        scene
    }

    /// While the frame that ran fluid tick k is in flight the pair holds: no
    /// tick is allowed, Box3D stays at k and the slot stays pending. Once it
    /// retires, Box3D steps tick k and the next fluid tick may run.
    #[test]
    fn matter_coupled_holds_when_reaction_pending() {
        let scene = scene();
        let colliders = RigidImpulseTargets { bodies: 1, copies: false };
        let mut owner = RigidOwner::new(&scene, colliders, 7, None).expect("owner");
        assert_eq!((owner.completed(), owner.rows().len(), owner.geometries().len()), (0, 1, 1));
        let row = owner.rows()[0];
        assert!((row.position_inv_mass[3] - 1.0 / 32.0).abs() < 1e-6);
        assert!((row.position_inv_mass[1] - 1.0).abs() < 1e-5, "the row sits at the centre of mass");
        assert!((row.accel_shape[1] + 9.81).abs() < 1e-4 && row.accel_shape[3] == 0.0);

        assert_eq!(owner.settle(&scene, |_| panic!("nothing pending"), || None).unwrap(), 1);
        let slot = ReactionSlot { tick: 0, stamp: 42, unit: 128.0, cell_size: 0.0625, offset: 0 };
        owner.set_pending(slot);
        let held = owner
            .settle(&scene, |stamp| {
                assert_eq!(stamp, 42);
                false
            }, || panic!("words read before the frame retired"))
            .unwrap();
        assert_eq!((held, owner.completed(), owner.pending()), (0, 0, Some(slot)));
        assert_eq!(owner.frame().stamp, TickStamp { epoch: 7, tick: 0 });

        let words = [0i32; 16];
        assert_eq!(owner.settle(&scene, |_| true, || Some(&words[..])).unwrap(), 1);
        assert_eq!((owner.completed(), owner.pending()), (1, None));
        assert_eq!(owner.frame().stamp, TickStamp { epoch: 7, tick: 1 });
        let fallen = owner.rows()[0].linear_velocity[1];
        assert!((fallen + 9.81 * TICK as f32).abs() < 1e-3, "free fall over one tick: {fallen}");
    }

    /// A reaction of Δv = +1 m/s (encoded at U = 128) reaches Box3D as a
    /// linear impulse of m·Δv, on top of the tick's gravity.
    #[test]
    fn matter_coupled_reaction_decodes_to_body_impulse() {
        let scene = scene();
        let colliders = RigidImpulseTargets { bodies: 1, copies: false };
        let mut owner = RigidOwner::new(&scene, colliders, 3, None).expect("owner");
        let unit = 128.0f32;
        let mut words = [0i32; 16];
        words[1] = (16_777_216.0 / f64::from(unit)) as i32;
        owner.set_pending(ReactionSlot { tick: 0, stamp: 0, unit, cell_size: 0.0625, offset: 0 });
        owner.settle(&scene, |_| true, || Some(&words[..])).unwrap();
        let v = owner.rows()[0].linear_velocity[1];
        assert!((v - (1.0 - 9.81 * TICK as f32)).abs() < 1e-3, "{v}");
    }
}
