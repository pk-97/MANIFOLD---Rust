//! The rigid owner of a coupled GPU liquid (`docs/LIQUID_SOLVER_SEAM_DESIGN.md`
//! section 3.3, D6, D15-D18). The scene's Box3D world runs one tick behind
//! the liquid: liquid tick k runs on the GPU from the bodies' state and
//! support points at its start, moving each body by the coupled motion law
//! and holding it on its supports through each pressure solve. Once its GPU
//! work has retired, [`LiquidRigidOwner::settle_ready`] has the solver
//! decode the tick's reaction into one impulse per body and steps Box3D
//! over the same tick with that reaction as a steady force. The domain waits
//! at tick boundaries before reading reactions, including between ticks of a
//! coupled live frame.
//!
//! The domain's closed faces are walls for the liquid, so they are walls for
//! the bodies: the owner installs a fixed slab just outside each closed face,
//! spanning it; open faces stay open. The slabs live only in the owner's
//! world (not scene nodes, not serialized).

use std::sync::Arc;

use manifold_physics::coupled_motion::{coupled_state_at, coupled_substep, CoupledStart, SupportPoint, MAX_SUPPORT_POINTS};
use manifold_physics::stepping::{StepCoupling, SubstepExchange, Uncoupled};
use manifold_physics::{BodyHandle, BodyImpulse, PhysicsWorld, Seconds, TickStamp};

use super::bodies::{pack_supports, unpack_supports, BodySupports, LiquidBody};
#[cfg(test)]
use manifold_physics::clock::TICK;
use manifold_core::fluid_domain::FluidDomainLayout;
use manifold_water_rigid::coupled_frame::{CoupledRigidFrame, CoupledRigidLayout};
use crate::fluid_role::PreparedFluidGeometry;
#[cfg(test)]
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_core::scene_impulse::RigidImpulseTargets;
use manifold_water_rigid::physics::{RigidBody, RigidSceneInputs, RigidSimulation, Solid};
use manifold_physics::TriangleMesh;
use manifold_node_engine::scene::transform::Transform;

/// Where a coupled body's controls come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// A wired scene body slot.
    Body(usize),
    /// A copy of copy group k, which takes its copies body's controls.
    Group(usize),
}

/// One coupled Box3D body, in row order.
struct Coupled {
    handle: BodyHandle,
    source: Source,
    /// The largest face of the body's local hull box.
    face_area: f32,
    friction: f32,
}

/// The liquid tick whose reaction the GPU is still writing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PendingTick {
    pub tick: u64,
    pub interval: manifold_physics::stepping::StepInterval,
    pub offline: bool,
    /// Frame-clock stamp of the frame that ran the tick (0 = already retired).
    pub stamp: u64,
}

/// How far Box3D's end of a tick is from the law's, worst body (D18).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HandoverError {
    /// m.
    pub position: f32,
    /// m/s.
    pub velocity: f32,
    /// rad.
    pub rotation: f32,
}

/// D18's bounds: past any, the tick is logged.
pub const HANDOVER_BOUND: HandoverError = HandoverError { position: 5e-4, velocity: 5e-3, rotation: 0.1 * std::f32::consts::PI / 180.0 };

/// A dynamic body with a collider shape: it takes the liquid's reaction. A
/// NaN inverse mass is not dynamic.
#[inline]
pub fn takes_reaction(row: &LiquidBody) -> bool {
    row.position_inv_mass[3] > 0.0 && row.accel_shape[3] >= 0.0
}

/// Floats one body holds in a float reaction: linear impulse (N·s), then
/// angular impulse about the centre of mass (N·m·s), each padded to four.
pub const REACTION_FLOATS: usize = 8;

/// A pending tick's float reaction as one impulse per row; rows that take no
/// reaction are left alone. `offset` counts the rows before the first
/// coupled body's.
#[inline]
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

/// A coupled domain's box and which faces are closed (bit order −x, +x, −y,
/// +y, −z, +z, as `closed_faces`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DomainWalls {
    pub min: [f32; 3],
    pub size: [f32; 3],
    pub closed: u32,
}

impl DomainWalls {
    /// The walls of `layout`'s grid box (the liquid's own walls).
    pub fn of(layout: &FluidDomainLayout, closed: u32) -> Self {
        let size = std::array::from_fn(|d| (f64::from(layout.cells[d]) * layout.cell_size) as f32);
        Self { min: layout.min, size, closed }
    }

    /// One Fixed slab per closed face, in face order.
    fn slabs(&self) -> impl Iterator<Item = RigidBody> + '_ {
        let extent = self.size.iter().fold(0.0f32, |a, &b| a.max(b));
        let half_thick = 0.05 * extent;
        (0..6).filter(|face| self.closed & (1 << face) != 0).map(move |face| {
            let (axis, positive) = (face / 2, face % 2 == 1);
            let mut pos = std::array::from_fn(|d| self.min[d] + 0.5 * self.size[d]);
            pos[axis] = if positive { self.min[axis] + self.size[axis] + half_thick } else { self.min[axis] - half_thick };
            // Spans the face plus the neighbouring slabs' thickness, so edges close.
            let half: [f32; 3] =
                std::array::from_fn(|d| if d == axis { half_thick } else { 0.5 * self.size[d] + 2.0 * half_thick });
            RigidBody {
                transform: Transform { pos, scale: half.map(|h| h * 3f32.sqrt()), ..Transform::default() },
                kind: 0,
                bounce: 0.0,
                wall: true,
                ..RigidBody::default()
            }
        })
    }

    /// Write the slabs into free body slots from the last slot down, then
    /// past the authored bodies once none is free, so a full world still
    /// takes them. A world with room keeps the slots it always had: Box3D's
    /// result depends on body order, and old shows replay bit for bit. An
    /// appended slab's targeted-field entry goes before the copy groups',
    /// which keep theirs. Slots are stable for unchanged inputs.
    fn install(&self, inputs: &mut RigidSceneInputs) {
        let authored = inputs.bodies.len();
        let mut next = authored;
        for slab in self.slabs() {
            match (0..next).rev().find(|&slot| inputs.bodies[slot].is_none()) {
                Some(slot) => {
                    next = slot;
                    inputs.bodies[slot] = Some(slab);
                }
                None => {
                    next = 0;
                    inputs.bodies.push(Some(slab));
                }
            }
        }
        if !inputs.targeted_fields.is_empty() {
            for _ in authored..inputs.bodies.len() {
                inputs.targeted_fields.insert(authored, None);
            }
        }
    }
}

/// A coupled domain's rigid world, stepped only by settled liquid ticks.
pub struct LiquidRigidOwner {
    rigid: RigidSimulation,
    layout: CoupledRigidLayout,
    initial: RigidSceneInputs,
    walls: DomainWalls,
    /// The scene's inputs with the walls installed, rebuilt each step.
    walled: RigidSceneInputs,
    colliders: RigidImpulseTargets,
    /// Colliding bodies past the liquid's capacity: Box3D still moves them,
    /// but the water neither pushes them nor flows round them.
    uncoupled: usize,
    epoch: u64,
    completed: u64,
    completed_time: Seconds,
    bodies: Vec<Coupled>,
    geometries: Vec<Arc<PreparedFluidGeometry>>,
    frame: CoupledRigidFrame,
    rows: Vec<LiquidBody>,
    contacts: Vec<BodySupports>,
    impulses: Vec<BodyImpulse>,
    handover: HandoverError,
    pending: Option<PendingTick>,
    capacity: usize,
    /// Bodies joined the world; the coupled rows follow at the next frame
    /// start ([`Self::rebind`]), once the pending reaction has decoded
    /// against the old ones.
    rebind: bool,
}

impl LiquidRigidOwner {
    /// Prepare the rigid world of `inputs` at tick 0 of `epoch`, coupling
    /// at most `capacity` of the colliding bodies in slot order, then each
    /// group's copies; [`Self::uncoupled`] counts the rest. Hull geometry, and
    /// so the distance lattices, carries over from `reuse` when its topology
    /// is unchanged.
    pub fn new(
        inputs: &RigidSceneInputs,
        walls: DomainWalls,
        colliders: &RigidImpulseTargets,
        capacity: usize,
        epoch: u64,
        reuse: Option<&LiquidRigidOwner>,
    ) -> Result<Self, String> {
        let mut walled = inputs.clone();
        walls.install(&mut walled);
        let mut rigid = RigidSimulation::with_worker_epoch(epoch)?;
        rigid.advance_worker(&walled, Seconds::ZERO, 0, &mut Uncoupled)?;
        let layout = CoupledRigidLayout::new(&rigid, &walled);
        let world = rigid.native_world().ok_or("Liquid coupling: the rigid world was not prepared")?;
        let reuse = reuse.filter(|owner| owner.matches(inputs, walls, colliders, capacity));
        let known = |source: Source, _: BodyHandle| {
            reuse.and_then(|owner| {
                owner
                    .bodies
                    .iter()
                    .zip(&owner.geometries)
                    .find(|(body, _)| body.source == source)
                    .map(|(body, geometry)| (Arc::clone(geometry), body.face_area))
            })
        };
        let Bound { bodies, geometries, uncoupled } = bind(&rigid, &walled, colliders, capacity, known)?;
        let mut frame = CoupledRigidFrame::default();
        layout.prepare_output(&mut frame);
        layout.capture(world, &mut frame)?;
        frame.stamp = TickStamp { epoch, tick: 0 };
        let mut owner = Self {
            rigid,
            layout,
            initial: inputs.clone(),
            walls,
            walled,
            colliders: colliders.clone(),
            uncoupled,
            epoch,
            completed: 0,
            completed_time: Seconds::ZERO,
            bodies,
            geometries,
            frame,
            rows: Vec::new(),
            contacts: Vec::new(),
            impulses: Vec::new(),
            handover: HandoverError::default(),
            pending: None,
            capacity,
            rebind: false,
        };
        owner.update_friction(inputs);
        let world = owner.rigid.native_world().expect("prepared above");
        capture_rows(world, &owner.bodies, &mut owner.rows, &mut owner.contacts)?;
        Ok(owner)
    }

    /// Whether `inputs` and `colliders` keep this owner's native topology,
    /// and `capacity` couples the same bodies.
    pub fn matches(
        &self,
        inputs: &RigidSceneInputs,
        walls: DomainWalls,
        colliders: &RigidImpulseTargets,
        capacity: usize,
    ) -> bool {
        self.colliders == *colliders
            && self.bodies.len() == capacity.min(self.bodies.len() + self.uncoupled)
            && self.walls == walls
            && self.initial.same_topology(inputs)
            && self.initial.bodies.iter().zip(&inputs.bodies).all(|(left, right)| {
                left.as_ref().and_then(|body| body.fragment_parent)
                    == right.as_ref().and_then(|body| body.fragment_parent)
            })
    }

    /// Take a changed scene into the running world without a new epoch: a
    /// body or copy group that joined starts where it is authored, and every
    /// body already moving keeps its state. The poses follow at once; the
    /// liquid takes the new bodies at the next frame start
    /// ([`Self::rebind`]). False when the change needs a fresh world (a body
    /// left or changed its shape, the walls moved, a fragment appeared) or
    /// no tick has run yet; the caller then builds a new owner.
    pub fn admit(
        &mut self,
        inputs: &RigidSceneInputs,
        walls: DomainWalls,
        colliders: &RigidImpulseTargets,
        capacity: usize,
    ) -> Result<bool, String> {
        // Before the first tick a body that arrives (a wired mesh landing a
        // frame late) belongs to the epoch's start: the fill must seed
        // around it, and a restart loses nothing yet.
        if self.completed == 0 && self.pending.is_none() {
            return Ok(false);
        }
        if walls != self.walls || capacity != self.capacity {
            return Ok(false);
        }
        let mut walled = inputs.clone();
        walls.install(&mut walled);
        if !self.rigid.admit_worker_edits(&walled)? {
            return Ok(false);
        }
        self.layout = CoupledRigidLayout::new(&self.rigid, &walled);
        let world = self.rigid.native_world().ok_or("Liquid coupling: admitting bodies lost the rigid world")?;
        let stamp = self.frame.stamp;
        self.layout.prepare_output(&mut self.frame);
        self.layout.capture(world, &mut self.frame)?;
        self.frame.stamp = stamp;
        self.initial = inputs.clone();
        self.walled = walled;
        self.colliders = colliders.clone();
        self.rebind = true;
        Ok(true)
    }

    /// Whether the coupled bodies lag the world: a body joined, or a copy
    /// group laid out a different count at a tick start.
    pub fn wants_rebind(&self) -> bool {
        let selected = self.rigid.native_handles().iter().enumerate()
            .filter(|(index, handle)| handle.is_some() && self.colliders.contains_body(*index))
            .count()
            + self.colliders.groups().map(|group| self.rigid.native_copy_handles(group).iter().flatten().count()).sum::<usize>();
        self.rebind || selected != self.bodies.len() + self.uncoupled
    }

    /// Couple the world's bodies as they stand now, keeping each body's
    /// hull, and so its distance lattice, by its Box3D handle. Only between
    /// liquid ticks; the caller prepares the liquid's bodies again from
    /// [`Self::geometries`].
    pub fn rebind(&mut self) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("Liquid coupling: settle the pending reaction before rebinding bodies".into());
        }
        let known = |_: Source, handle: BodyHandle| {
            self.bodies
                .iter()
                .zip(&self.geometries)
                .find(|(body, _)| body.handle == handle)
                .map(|(body, geometry)| (Arc::clone(geometry), body.face_area))
        };
        let Bound { bodies, geometries, uncoupled } = bind(&self.rigid, &self.walled, &self.colliders, self.capacity, known)?;
        self.bodies = bodies;
        self.geometries = geometries;
        self.uncoupled = uncoupled;
        self.layout = CoupledRigidLayout::new(&self.rigid, &self.walled);
        let world = self.rigid.native_world().ok_or("Liquid coupling: rebinding bodies lost the rigid world")?;
        let stamp = self.frame.stamp;
        self.layout.prepare_output(&mut self.frame);
        self.layout.capture(world, &mut self.frame)?;
        self.frame.stamp = stamp;
        set_friction(&mut self.bodies, &self.walled);
        capture_rows(world, &self.bodies, &mut self.rows, &mut self.contacts)?;
        self.rebind = false;
        Ok(())
    }

    /// Colliding bodies past the capacity it was built with.
    pub fn uncoupled(&self) -> usize {
        self.uncoupled
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

    /// Each coupled body's support points at the start of the next
    /// liquid tick, in row order.
    pub fn contacts(&self) -> &[BodySupports] {
        &self.contacts
    }

    /// The last settled tick's handover error (D18).
    pub fn handover(&self) -> HandoverError {
        self.handover
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

    pub fn completed_time(&self) -> Seconds { self.completed_time }

    /// The step the rigid worker runs under; the domain sets it each frame.
    pub fn set_step(&mut self, step: manifold_water_rigid::physics::SimStep) {
        self.rigid.set_step(step);
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The liquid tick now running on the GPU; the next tick boundary settles it.
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

    /// Reanchor authored poses across discarded transport without stepping
    /// Box3D. Dynamic bodies keep their integrated state; animated bodies move
    /// to the new start instead of sweeping through the discarded gap.
    pub fn reanchor_start(&mut self, inputs: &RigidSceneInputs) -> Result<(), String> {
        if self.pending.is_some() {
            return Err("Liquid coupling: settle the pending reaction before reanchoring".into());
        }
        self.walled.clone_from(inputs);
        self.walls.install(&mut self.walled);
        self.rigid.advance_worker(&self.walled, self.completed_time, 0, &mut Uncoupled)?;
        self.update_friction(inputs);
        let world = self.rigid.native_world().ok_or("Liquid coupling: reanchor lost the rigid world")?;
        self.layout.capture(world, &mut self.frame)?;
        capture_rows(world, &self.bodies, &mut self.rows, &mut self.contacts)
    }

    /// Finish a required tick boundary after the caller waits for the GPU.
    /// Decode one reaction and step Box3D toward the sampled tick-end scene.
    /// Missing samples or reactions fail rather than reducing the tick rate.
    pub fn settle_ready(
        &mut self,
        end: Option<&RigidSceneInputs>,
        complete: impl FnOnce(u64) -> bool,
        decode: impl FnOnce(PendingTick, &[LiquidBody], &mut [BodyImpulse]) -> Result<(), String>,
    ) -> Result<(), String> {
        let Some(pending) = self.pending else { return Ok(()) };
        let (Some(inputs), true) = (end, complete(pending.stamp)) else {
            return Err(format!(
                "Liquid coupling: tick {} (frame stamp {}) cannot start its successor: {}",
                pending.tick,
                pending.stamp,
                if end.is_some() { "reaction did not retire after the GPU wait" } else { "tick end was never sampled" }
            ));
        };
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
        self.step(inputs, pending)
    }

    fn update_friction(&mut self, inputs: &RigidSceneInputs) {
        set_friction(&mut self.bodies, inputs);
    }

    /// Step Box3D over tick `completed` with the decoded impulses.
    fn step(&mut self, inputs: &RigidSceneInputs, pending: PendingTick) -> Result<(), String> {
        self.update_friction(inputs);
        let expected = TickStamp { epoch: self.epoch, tick: self.completed };
        let mut coupling = LiquidCoupling {
            expected,
            duration: pending.interval.duration(),
            layout: &self.layout,
            frame: &mut self.frame,
            bodies: &self.bodies,
            rows: &mut self.rows,
            contacts: &mut self.contacts,
            impulses: &self.impulses,
            handover: &mut self.handover,
            begun: false,
            finished: false,
        };
        self.walled.clone_from(inputs);
        self.walls.install(&mut self.walled);
        self.rigid.advance_worker_interval(&self.walled, pending.interval, &[], &mut coupling)?;
        if !coupling.finished {
            return Err("Liquid coupling: the rigid owner did not step the settled tick".into());
        }
        self.completed += 1;
        self.completed_time = pending.interval.end;
        Ok(())
    }
}

fn set_friction(bodies: &mut [Coupled], inputs: &RigidSceneInputs) {
    for body in bodies {
        let source = match body.source {
            Source::Body(slot) => inputs.bodies[slot].as_ref(),
            Source::Group(group) => inputs.groups[group].as_ref(),
        };
        body.friction = source.map_or(0.0, |body| body.friction.clamp(0.0, 1.0));
    }
}

/// Each colliding body and copy the liquid takes, in slot order then each
/// group's copies, up to `capacity`, with its hull: `known` hands back a
/// hull already built, else it is built from the world. Returns the bodies,
/// their hulls, and how many colliding bodies are past the capacity.
fn bind(
    rigid: &RigidSimulation,
    walled: &RigidSceneInputs,
    colliders: &RigidImpulseTargets,
    capacity: usize,
    known: impl Fn(Source, BodyHandle) -> Option<(Arc<PreparedFluidGeometry>, f32)>,
) -> Result<Bound, String> {
    let world = rigid.native_world().ok_or("Liquid coupling: the rigid world was not prepared")?;
    let mut selected: Vec<(BodyHandle, Source)> = rigid
        .native_handles()
        .iter()
        .enumerate()
        .filter(|(index, _)| colliders.contains_body(*index))
        .filter_map(|(index, handle)| handle.map(|handle| (handle, Source::Body(index))))
        .chain(colliders.groups().flat_map(|group| {
            rigid.native_copy_handles(group).iter().flatten().map(move |&handle| (handle, Source::Group(group)))
        }))
        .collect();
    let uncoupled = selected.len().saturating_sub(capacity);
    selected.truncate(capacity);
    let mut bodies = Vec::with_capacity(selected.len());
    let mut geometries = Vec::with_capacity(selected.len());
    // A group's copies share one hull, built for its first copy.
    let mut group_hull: Option<(usize, Arc<PreparedFluidGeometry>, f32)> = None;
    for (handle, source) in selected {
        let cached = match source {
            Source::Group(group) => group_hull
                .as_ref()
                .filter(|(cached, _, _)| *cached == group)
                .map(|(_, geometry, area)| (Arc::clone(geometry), *area)),
            Source::Body(_) => None,
        };
        let (geometry, face_area) = match known(source, handle).or(cached) {
            Some(known) => known,
            None => {
                let body = match source {
                    Source::Body(index) => walled.bodies.get(index).and_then(Option::as_ref),
                    Source::Group(_) => None,
                };
                let built = hull_geometry(world, handle, body)?;
                if let Source::Group(group) = source {
                    group_hull = Some((group, Arc::clone(&built.0), built.1));
                }
                built
            }
        };
        bodies.push(Coupled { handle, source, face_area, friction: 0.0 });
        geometries.push(geometry);
    }
    Ok(Bound { bodies, geometries, uncoupled })
}

/// The coupled bodies, their hulls, and the colliding bodies past capacity.
struct Bound {
    bodies: Vec<Coupled>,
    geometries: Vec<Arc<PreparedFluidGeometry>>,
    uncoupled: usize,
}

/// A body's installed solid about its centre of mass, and its largest local
/// box face: its hulls, or for a static collider its exact mesh at its scale.
fn hull_geometry(world: &PhysicsWorld, handle: BodyHandle, body: Option<&RigidBody>) -> Result<(Arc<PreparedFluidGeometry>, f32), String> {
    let exact = body.and_then(|body| match body.collider.as_deref() {
        Some(Solid::Mesh(mesh)) => Some(TriangleMesh {
            vertices: mesh.vertices.iter().map(|v| std::array::from_fn(|axis| v[axis] * body.transform.scale[axis])).collect(),
            triangles: mesh.triangles.clone(),
        }),
        _ => None,
    });
    let mut meshes = match exact {
        Some(mesh) => vec![mesh],
        None => world.hull_meshes(handle).map_err(|error| error.to_string())?,
    };
    let centre = world.local_center_of_mass(handle).map_err(|error| error.to_string())?;
    let (mut low, mut high) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for vertex in meshes.iter_mut().flat_map(|mesh| mesh.vertices.iter_mut()) {
        for axis in 0..3 {
            vertex[axis] -= centre[axis];
            low[axis] = low[axis].min(vertex[axis]);
            high[axis] = high[axis].max(vertex[axis]);
        }
    }
    // A flat static mesh (a ground plane) is a shell in the water; it still
    // needs two sides.
    if meshes.is_empty() || (0..3).filter(|&axis| high[axis] > low[axis]).count() < 2 {
        return Err("Liquid coupling: a coupled body has no solid hull".into());
    }
    let size: [f32; 3] = std::array::from_fn(|axis| high[axis] - low[axis]);
    let face = (size[0] * size[1]).max(size[1] * size[2]).max(size[0] * size[2]);
    Ok((Arc::new(PreparedFluidGeometry::new(meshes)), face))
}

/// Each coupled body's state as a row: its centre of mass, 1/m, velocities,
/// world inverse inertia (angular acceleration in the rows' w) and the
/// predicted linear acceleration; shape −1 while disabled. With it, the
/// body's support points (D16).
fn capture_rows(
    world: &PhysicsWorld,
    bodies: &[Coupled],
    rows: &mut Vec<LiquidBody>,
    contacts: &mut Vec<BodySupports>,
) -> Result<(), String> {
    rows.clear();
    contacts.clear();
    for (index, body) in bodies.iter().enumerate() {
        let mut points = [SupportPoint::default(); MAX_SUPPORT_POINTS];
        let count = world.support_points(body.handle, &mut points).map_err(|error| error.to_string())?;
        if !count.complete() {
            log::warn!(
                "Liquid coupling: body {index} touches {} support points ({} contacts unread); the liquid sees {}",
                count.found,
                count.unread,
                count.kept
            );
        }
        contacts.push(pack_supports(&points[..count.kept]));
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

/// The exchange of one settled tick: the liquid's reaction goes in as a
/// steady force over every Box3D step of the tick (D17); the end state comes
/// out, checked against the law (D18).
pub struct LiquidCoupling<'a> {
    expected: TickStamp,
    duration: Seconds,
    layout: &'a CoupledRigidLayout,
    frame: &'a mut CoupledRigidFrame,
    bodies: &'a [Coupled],
    rows: &'a mut Vec<LiquidBody>,
    contacts: &'a mut Vec<BodySupports>,
    impulses: &'a [BodyImpulse],
    handover: &'a mut HandoverError,
    begun: bool,
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
        if (duration.0 - self.duration.0).abs() > 1e-12 {
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
        rigid.queue_reaction_over_step(self.impulses, self.duration).map_err(|error| error.to_string())
    }

    fn finish(self, rigid: &PhysicsWorld) -> Result<(), String> {
        *self.handover = handover_error(rigid, self.bodies, self.rows, self.contacts, self.impulses, self.duration)?;
        if self.handover.position > HANDOVER_BOUND.position
            || self.handover.velocity > HANDOVER_BOUND.velocity
            || self.handover.rotation > HANDOVER_BOUND.rotation
        {
            log::warn!("Liquid coupling: tick {} ended {:?} from the coupled motion law", self.expected.tick, self.handover);
        }
        self.layout.capture(rigid, self.frame)?;
        self.frame.stamp = TickStamp { epoch: self.expected.epoch, tick: self.expected.tick + 1 };
        capture_rows(rigid, self.bodies, self.rows, self.contacts)?;
        self.finished = true;
        Ok(())
    }
}

/// D18: Box3D's end of the tick against the law at its end, from each dynamic
/// body's tick-start row and contacts under the reaction it was handed;
/// the worst body per measure.
fn handover_error(
    rigid: &PhysicsWorld,
    bodies: &[Coupled],
    rows: &[LiquidBody],
    contacts: &[BodySupports],
    impulses: &[BodyImpulse],
    duration: Seconds,
) -> Result<HandoverError, String> {
    let mut worst = HandoverError::default();
    let (t, h) = (duration.0 as f32, coupled_substep(duration));
    for ((body, row), contacts) in bodies.iter().zip(rows).zip(contacts) {
        if !takes_reaction(row) {
            continue;
        }
        let (push, turn) = impulses
            .iter()
            .find(|impulse| impulse.body == body.handle)
            .map_or(([0.0; 3], [0.0; 3]), |impulse| (impulse.linear, impulse.angular));
        let mut points = [SupportPoint::default(); MAX_SUPPORT_POINTS];
        let count = unpack_supports(contacts, &mut points);
        let law = coupled_state_at(&coupled_start(row), &points[..count], push, turn, t, h);
        let end = rigid.dynamics(body.handle).map_err(|error| error.to_string())?;
        let rotation = rigid.pose(body.handle).map_err(|error| error.to_string())?.rotation;
        let gap = |a: [f32; 3], b: [f32; 3]| (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f32>().sqrt();
        worst.position = worst.position.max(gap(law.position, end.center_of_mass));
        worst.velocity = worst.velocity.max(gap(law.linear_velocity, end.linear_velocity));
        worst.rotation = worst.rotation.max(quaternion_angle(law.rotation, rotation));
    }
    Ok(worst)
}

manifold_core::testkit_visible! {
/// A coupled row as the law's start state.
pub(crate) fn coupled_start(row: &LiquidBody) -> CoupledStart {
    let xyz = |v: [f32; 4]| [v[0], v[1], v[2]];
    CoupledStart {
        position: xyz(row.position_inv_mass),
        rotation: row.rotation,
        linear_velocity: xyz(row.linear_velocity),
        angular_velocity: xyz(row.angular_velocity),
        inverse_mass: row.position_inv_mass[3],
        inverse_inertia: [xyz(row.inv_inertia_x), xyz(row.inv_inertia_y), xyz(row.inv_inertia_z)],
        linear_acceleration: xyz(row.accel_shape),
        angular_acceleration: [row.inv_inertia_x[3], row.inv_inertia_y[3], row.inv_inertia_z[3]],
    }
}
}

/// The angle between two unit quaternions, radians.
fn quaternion_angle(a: [f32; 4], b: [f32; 4]) -> f32 {
    // conj(a) ⊗ b
    let r = [
        a[3] * b[0] - a[0] * b[3] - a[1] * b[2] + a[2] * b[1],
        a[3] * b[1] + a[0] * b[2] - a[1] * b[3] - a[2] * b[0],
        a[3] * b[2] - a[0] * b[1] + a[1] * b[0] - a[2] * b[3],
        a[3] * b[3] + a[0] * b[0] + a[1] * b[1] + a[2] * b[2],
    ];
    2.0 * (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt().atan2(r[3].abs())
}

/// The solids one Water's rigid owner shares with every other Water in its
/// scene (SCENE_DOCUMENT_DESIGN.md section 7.8 (Physics and collision
/// shapes)): each Collider's and Body's hull and its row at the first tick
/// of this frame, held to its motion (no inverse mass or inertia). The other
/// water flows round them; only the owner's water pushes the bodies back,
/// since Box3D steps inside one liquid's ticks.
#[derive(Default)]
pub struct SharedSolids {
    geometries: Vec<Arc<PreparedFluidGeometry>>,
    rows: Vec<LiquidBody>,
    contacts: Vec<BodySupports>,
    uncoupled: usize,
    /// Copied from the owner this frame, before this water runs.
    fresh: bool,
    /// Scratch for a later tick's rows.
    advanced: Vec<LiquidBody>,
}

impl SharedSolids {
    /// Take the owner water's solids for this frame, or none.
    pub fn copy_from(&mut self, owner: Option<&LiquidRigidOwner>) {
        self.geometries.clear();
        self.rows.clear();
        self.contacts.clear();
        self.uncoupled = 0;
        self.fresh = true;
        let Some(owner) = owner else { return };
        self.geometries.extend(owner.geometries().iter().cloned());
        self.rows.extend(owner.rows().iter().map(|row| {
            let mut held = *row;
            held.position_inv_mass[3] = 0.0;
            for inertia in [&mut held.inv_inertia_x, &mut held.inv_inertia_y, &mut held.inv_inertia_z] {
                inertia[..3].fill(0.0);
            }
            held
        }));
        self.contacts.resize(self.rows.len(), [[0.0; 4]; super::bodies::SUPPORT_VEC4S]);
        self.uncoupled = owner.uncoupled();
    }

    /// The hulls this frame, or none when the owner did not run first.
    pub fn geometries(&self) -> &[Arc<PreparedFluidGeometry>] {
        if self.fresh { &self.geometries } else { &[] }
    }

    pub fn rows(&self) -> &[LiquidBody] {
        if self.fresh { &self.rows } else { &[] }
    }

    pub fn contacts(&self) -> &[BodySupports] {
        if self.fresh { &self.contacts } else { &[] }
    }

    /// Bodies past the owner's capacity, which no water takes.
    pub fn uncoupled(&self) -> usize {
        self.uncoupled
    }

    /// The rows `seconds` after the first tick's start, each body carried
    /// along its motion: a later tick of this frame starts there.
    pub fn rows_after(&mut self, seconds: f32) -> &[LiquidBody] {
        self.advanced.clear();
        self.advanced.extend(self.rows.iter().map(|row| {
            let (position, rotation) = super::bodies::body_pose_at(row, seconds);
            let mut moved = *row;
            moved.position_inv_mass[..3].copy_from_slice(&position);
            moved.rotation = rotation;
            moved
        }));
        &self.advanced
    }

    /// The frame's water has used them; the next frame takes them anew.
    pub fn used(&mut self) {
        self.fresh = false;
    }
}

/// A liquid domain that owns a rigid world or takes another's solids.
pub trait SharesSolids {
    /// The rigid owner, when this water couples its scene's world.
    fn rigid_owner(&self) -> Option<&LiquidRigidOwner>;
    /// Where this water takes another water's solids.
    fn shared_solids_mut(&mut self) -> &mut SharedSolids;
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_physics::PlayedTime;

    const OPEN: DomainWalls = DomainWalls { min: [0.0; 3], size: [0.0; 3], closed: 0 };

    const CLOSED: DomainWalls = DomainWalls { min: [-1.2, 0.0, -1.2], size: [2.4; 3], closed: 0b11_1111 };

    /// A full world of `count` small cubes in a grid inside [`CLOSED`], and
    /// every one of them colliding with the liquid.
    fn full_world(count: usize) -> (RigidSceneInputs, RigidImpulseTargets) {
        let mut world = RigidSceneInputs { gravity: [0.0, -9.81, 0.0], ..RigidSceneInputs::with_counts(count, 1) };
        let template = scene().bodies[0].clone().unwrap();
        let mut colliders = RigidImpulseTargets::default();
        for (index, slot) in world.bodies.iter_mut().enumerate() {
            let at = |i: usize| -0.9 + 0.45 * (i % 5) as f32;
            *slot = Some(RigidBody {
                transform: Transform { pos: [at(index), 0.3 + 0.4 * (index / 25) as f32, at(index / 5)], scale: [0.15; 3], ..Transform::default() },
                ..template.clone()
            });
            colliders = colliders.union(RigidImpulseTargets::body(index).unwrap());
        }
        (world, colliders)
    }

    /// A world with free slots keeps its walls in them, from the last slot
    /// down, as every show saved so far ran. A full world takes them past
    /// its own bodies, and the copy groups keep their targeted fields.
    #[test]
    fn walls_take_free_slots_then_go_past_a_full_world() {
        let mut roomy = scene();
        CLOSED.install(&mut roomy);
        assert_eq!(roomy.bodies.len(), 64);
        assert!(roomy.bodies[58..].iter().all(|body| body.as_ref().is_some_and(|body| body.wall)));
        assert!(roomy.bodies[1..58].iter().all(Option::is_none));

        let (mut full, _) = full_world(100);
        full.targeted_fields[100] = Some(manifold_physics::FieldValue::uniform([1.0, 0.0, 0.0]).unwrap());
        CLOSED.install(&mut full);
        assert_eq!(full.bodies.len(), 106);
        assert!(full.bodies[..100].iter().all(|body| body.as_ref().is_some_and(|body| !body.wall)));
        assert!(full.bodies[100..].iter().all(|body| body.as_ref().is_some_and(|body| body.wall)));
        assert_eq!(full.targeted_fields.len(), 107);
        assert!(full.targeted_fields[..106].iter().all(Option::is_none));
        assert!(full.targeted_fields[106].is_some(), "the copy group keeps its field");
    }

    /// A full 100-body world couples to the liquid: every body has a row,
    /// and the walls past its own bodies hold every one inside the domain.
    #[test]
    fn a_full_hundred_body_world_couples_every_body_inside_its_walls() {
        let (scene, colliders) = full_world(100);
        let mut owner = LiquidRigidOwner::new(&scene, CLOSED, &colliders, MAX_LIQUID_BODIES, 1, None).expect("owner");
        assert_eq!((owner.rows().len(), owner.uncoupled()), (100, 0));
        for tick in 0..(2.0 / TICK) as u64 {
            owner.set_pending(PendingTick { tick, stamp: 0, interval: manifold_physics::stepping::StepInterval::new(Seconds(tick as f64 * TICK), Seconds((tick + 1) as f64 * TICK)), offline: true });
            owner.settle_ready(Some(&scene), |_| true, no_reaction).unwrap();
        }
        for (index, row) in owner.rows().iter().enumerate() {
            let p = row.position_inv_mass;
            assert!(p[1] > 0.0 && p[1] < 2.4, "body {index} left through the floor or roof: {p:?}");
            assert!([p[0], p[2]].iter().all(|v| v.abs() < 1.2), "body {index} left through a side: {p:?}");
        }
    }

    /// Past its capacity the liquid couples the first bodies in slot order
    /// and counts the rest; only a capacity that changes which bodies couple
    /// needs a new owner.
    #[test]
    fn bodies_past_the_capacity_are_counted_not_coupled() {
        let (scene, colliders) = full_world(100);
        let owner = LiquidRigidOwner::new(&scene, CLOSED, &colliders, 40, 1, None).expect("owner");
        assert_eq!((owner.rows().len(), owner.uncoupled()), (40, 60));
        assert!(owner.matches(&scene, CLOSED, &colliders, 40));
        assert!(!owner.matches(&scene, CLOSED, &colliders, 41));
        let all = LiquidRigidOwner::new(&scene, CLOSED, &colliders, MAX_LIQUID_BODIES, 1, None).expect("owner");
        assert!(all.matches(&scene, CLOSED, &colliders, 100));
        assert!(all.matches(&scene, CLOSED, &colliders, MAX_LIQUID_BODIES - 1));
        assert!(!all.matches(&scene, CLOSED, &colliders, 99));

        // A world one body past the liquid's rows, walls and all.
        let (largest, colliders) = full_world(MAX_LIQUID_BODIES + 1);
        let mut owner = LiquidRigidOwner::new(&largest, CLOSED, &colliders, MAX_LIQUID_BODIES, 1, None).expect("owner");
        assert_eq!((owner.rows().len(), owner.uncoupled()), (MAX_LIQUID_BODIES, 1));
        owner.set_pending(PendingTick { tick: 0, stamp: 0, interval: manifold_physics::stepping::StepInterval::new(Seconds::ZERO, Seconds(TICK)), offline: true });
        owner.settle_ready(Some(&largest), |_| true, no_reaction).unwrap();
    }

    /// A body dropped in an empty coupled domain comes to rest on the floor
    /// slab under the domain's closed bottom, not below it.
    #[test]
    fn liquid_coupled_body_rests_on_the_domain_floor() {
        let scene = scene();
        let walls = DomainWalls { min: [-1.2, 0.0, -1.2], size: [2.4; 3], closed: 0b11_1111 };
        let colliders = RigidImpulseTargets::body(0).unwrap();
        let mut owner = LiquidRigidOwner::new(&scene, walls, &colliders, MAX_LIQUID_BODIES, 1, None).expect("owner");
        for tick in 0..(3.0 / TICK) as u64 {
            owner.set_pending(PendingTick { tick, stamp: 0, interval: manifold_physics::stepping::StepInterval::new(Seconds(tick as f64 * TICK), Seconds((tick + 1) as f64 * TICK)), offline: true });
            owner.settle_ready(Some(&scene), |_| true, no_reaction).unwrap();
        }
        let row = owner.rows()[0];
        // The scale-0.4 cube hull (circumradius 0.4) rests on y = 0 with its
        // centre one half-edge up.
        let half_edge = 0.4 / 3f32.sqrt();
        assert!((row.position_inv_mass[1] - half_edge).abs() < 0.02, "rest height {}", row.position_inv_mass[1]);
        assert!(row.linear_velocity[1].abs() < 0.05, "still falling: {}", row.linear_velocity[1]);
    }

    /// How far body 0 of `scene` moves along x in 2 s under gravity tilted by
    /// a sideways part s·g, with the downward part scaled by `1 + load` to
    /// press it harder. Tilted gravity pushes every substep; a per-tick
    /// impulse would creep the body a little each tick below the Coulomb
    /// limit and hide it.
    fn slide_distance(scene: &RigidSceneInputs, s: f32, load: f32) -> f32 {
        let mut scene = scene.clone();
        scene.gravity = [s * 9.81, -(1.0 + load) * 9.81, 0.0];
        let walls = DomainWalls { min: [-1.2, 0.0, -1.2], size: [2.4; 3], closed: 0b11_1111 };
        let colliders = RigidImpulseTargets::body(0).unwrap();
        let mut owner = LiquidRigidOwner::new(&scene, walls, &colliders, MAX_LIQUID_BODIES, 1, None).expect("owner");
        let mut start = 0.0;
        for tick in 0..(2.5 / TICK) as u64 {
            if tick == (0.5 / TICK) as u64 {
                start = owner.rows()[0].position_inv_mass[0];
            }
            owner.set_pending(PendingTick { tick, stamp: 0, interval: manifold_physics::stepping::StepInterval::new(Seconds(tick as f64 * TICK), Seconds((tick + 1) as f64 * TICK)), offline: true });
            owner.settle_ready(Some(&scene), |_| true, no_reaction).unwrap();
        }
        owner.rows()[0].position_inv_mass[0] - start
    }

    /// The smallest sideways push s, swept from 0 past every expected
    /// threshold in steps of 0.01, at which body 0 slides more than 5 cm.
    fn slide_threshold(scene: &RigidSceneInputs, load: f32) -> Option<f32> {
        (0..=150).map(|step| step as f32 * 0.01).find(|&s| slide_distance(scene, s, load) > 0.05)
    }

    /// The tank floor takes the body's own friction: a 32 kg cube of μ =
    /// 0.25 starts sliding at a push of μ·m·g, not √(0.5·μ)·m·g ≈ 0.35 (the
    /// geometric mean with the slab's default 0.5). Pressing it down with an
    /// extra g of weight doubles the limit: the normal load sets the cone.
    #[test]
    fn domain_floor_slide_threshold_is_the_body_friction() {
        let mut scene = scene();
        let body = scene.bodies[0].as_mut().unwrap();
        body.transform.pos[1] = 0.25;
        body.friction = 0.25;
        let free = slide_threshold(&scene, 0.0).expect("never slid");
        assert!((free - 0.25).abs() < 0.05, "slide threshold {free}, expected μ = 0.25");
        let loaded = slide_threshold(&scene, 1.0).expect("never slid under load");
        assert!((loaded - 0.5).abs() < 0.1, "loaded slide threshold {loaded}, expected 2μ = 0.5");
    }

    /// Two authored bodies keep Box3D's geometric mean: a μ = 0.1 cube on a
    /// fixed authored platform of μ = 0.9 slides at √0.09 = 0.3, not 0.1.
    #[test]
    fn authored_pair_keeps_the_geometric_mean() {
        let mut scene = scene();
        let body = scene.bodies[0].as_mut().unwrap();
        body.friction = 0.1;
        body.transform.pos[1] = 0.6;
        scene.bodies[1] = Some(RigidBody {
            transform: Transform { pos: [0.0, 0.2, 0.0], scale: [1.0, 0.1, 1.0], ..Transform::default() },
            kind: 0,
            friction: 0.9,
            bounce: 0.0,
            ..RigidBody::default()
        });
        let first = slide_threshold(&scene, 0.0).expect("never slid");
        assert!((first - 0.3).abs() < 0.05, "authored-pair slide threshold {first}, expected √(0.1·0.9) = 0.3");
    }

    /// The wall tag is runtime-only: a saved body carries no field for it,
    /// old data loads untagged, and a round trip is unchanged.
    #[test]
    fn wall_tag_is_never_serialized() {
        let body = RigidBody { wall: true, ..RigidBody::default() };
        let json = serde_json::to_value(&body).unwrap();
        assert!(json.get("wall").is_none(), "wall leaked into saved data: {json}");
        let plain = RigidBody::default();
        let saved = serde_json::to_string(&plain).unwrap();
        let loaded: RigidBody = serde_json::from_str(&saved).unwrap();
        assert_eq!(loaded, plain);
        assert!(!loaded.wall);
    }

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

    fn no_reaction(_: PendingTick, _: &[LiquidBody], _: &mut [BodyImpulse]) -> Result<(), String> {
        Ok(())
    }

    #[test]
    fn liquid_coupled_reanchor_moves_only_authored_poses_without_an_extra_step() {
        let mut inputs = scene();
        inputs.bodies[1] = inputs.bodies[0].clone();
        inputs.bodies[1].as_mut().unwrap().transform.pos[0] = 10.0;
        inputs.bodies[0].as_mut().unwrap().kind = 2;
        let colliders = RigidImpulseTargets::body(0).unwrap().union(RigidImpulseTargets::body(1).unwrap());
        let mut owner = LiquidRigidOwner::new(&inputs, OPEN, &colliders, MAX_LIQUID_BODIES, 1, None).unwrap();
        owner.set_step(manifold_water_rigid::physics::SimStep::live(Seconds(TICK)));
        // More than one live frame's budget has already completed before
        // an overloaded frame reanchors the authored scene.
        for tick in 0..6 {
            owner.set_pending(PendingTick { tick, stamp: 0,
                interval: manifold_physics::stepping::StepInterval::new(
                    Seconds(tick as f64 * TICK), Seconds((tick + 1) as f64 * TICK)),
                offline: false });
            inputs.bodies[0].as_mut().unwrap().transform.pos[0] = (tick + 1) as f32 * 0.1;
            owner.settle_ready(Some(&inputs), |_| true, no_reaction).unwrap();
        }
        let before = owner.rows()[1];
        let stamp = owner.frame().stamp;
        inputs.bodies[0].as_mut().unwrap().transform.pos[0] = 3.0;
        owner.reanchor_start(&inputs).unwrap();
        assert_eq!(owner.completed(), 6);
        assert_eq!(owner.completed_time(), Seconds(6.0 * TICK));
        assert_eq!(owner.frame().stamp, stamp);
        assert!((owner.rows()[0].position_inv_mass[0] - 3.0).abs() < 1e-4);
        assert_eq!(owner.rows()[1].position_inv_mass, before.position_inv_mass);
        assert_eq!(owner.rows()[1].linear_velocity, before.linear_velocity);
        assert_eq!(owner.rows()[1].angular_velocity, before.angular_velocity);
        owner.set_pending(PendingTick { tick: 6, stamp: 0,
            interval: manifold_physics::stepping::StepInterval::new(
                Seconds(6.0 * TICK), Seconds(7.0 * TICK)), offline: false });
        owner.settle_ready(Some(&inputs), |_| true, no_reaction).unwrap();
        assert_eq!(owner.completed(), 7);
    }

    /// A decoded impulse of m·Δv (Δv = +1 m/s) reaches Box3D on top of the
    /// tick's gravity; the decoder sees one impulse per row, naming its body.
    #[test]
    fn liquid_coupled_reaction_reaches_the_body() {
        let scene = scene();
        let colliders = RigidImpulseTargets::body(0).unwrap();
        let mut owner = LiquidRigidOwner::new(&scene, OPEN, &colliders, MAX_LIQUID_BODIES, 3, None).expect("owner");
        owner.set_pending(PendingTick { tick: 0, stamp: 0, interval: manifold_physics::stepping::StepInterval::new(manifold_core::Seconds((0) as f64 * TICK), manifold_core::Seconds(TICK)), offline: true });
        owner
            .settle_ready(Some(&scene), |_| true, |pending, rows, impulses| {
                assert_eq!((pending.tick, rows.len(), impulses.len()), (0, 1, 1));
                impulses[0].linear[1] = 1.0 / rows[0].position_inv_mass[3];
                Ok(())
            })
            .unwrap();
        let v = owner.rows()[0].linear_velocity[1];
        assert!((v - (1.0 - 9.81 * TICK as f32)).abs() < 1e-3, "{v}");

        // A body that joins mid-run couples without a new epoch: the first
        // keeps its motion and its hull, and the next tick takes both.
        let mut grown = scene.clone();
        let first = scene.bodies[0].clone().unwrap();
        grown.bodies[1] = Some(RigidBody { transform: Transform { pos: [2.0, 1.0, 0.0], ..first.transform }, ..first });
        let both = colliders.clone().union(RigidImpulseTargets::body(1).unwrap());
        assert!(!owner.matches(&grown, OPEN, &both, MAX_LIQUID_BODIES));
        let hull = Arc::clone(&owner.geometries()[0]);
        assert!(owner.admit(&grown, OPEN, &both, MAX_LIQUID_BODIES).unwrap());
        assert!(owner.matches(&grown, OPEN, &both, MAX_LIQUID_BODIES) && owner.wants_rebind());
        owner.rebind().unwrap();
        assert_eq!((owner.epoch(), owner.completed(), owner.rows().len()), (3, 1, 2));
        assert!(Arc::ptr_eq(&owner.geometries()[0], &hull));
        assert_eq!(owner.rows()[0].linear_velocity[1], v);
        owner.set_pending(PendingTick { tick: 1, stamp: 0, interval: manifold_physics::stepping::StepInterval::new(manifold_core::Seconds(TICK), manifold_core::Seconds(2.0 * TICK)), offline: true });
        owner
            .settle_ready(Some(&grown), |_| true, |_, rows, impulses| {
                assert_eq!((rows.len(), impulses.len()), (2, 2));
                Ok(())
            })
            .unwrap();
        assert!(!owner.wants_rebind());
    }

    /// A float reaction of m·(1 m/s) up, read past one leading row,
    /// reaches Box3D on top of the tick's gravity; a reaction too short for
    /// the rows is refused by name.
    #[test]
    fn liquid_coupled_float_reaction_decodes_to_body_impulse() {
        let scene = scene();
        let colliders = RigidImpulseTargets::body(0).unwrap();
        let mut owner = LiquidRigidOwner::new(&scene, OPEN, &colliders, MAX_LIQUID_BODIES, 3, None).expect("owner");
        let mut reaction = [0.0f32; 2 * REACTION_FLOATS];
        reaction[REACTION_FLOATS + 1] = 32.0;
        reaction[REACTION_FLOATS + 5] = 1.0e-9;
        owner.set_pending(PendingTick { tick: 0, stamp: 0, interval: manifold_physics::stepping::StepInterval::new(manifold_core::Seconds((0) as f64 * TICK), manifold_core::Seconds(TICK)), offline: true });
        owner
            .settle_ready(Some(&scene), |_| true, |_, rows, impulses| {
                decode_reaction(1, rows, Some(&reaction[..]), impulses)?;
                assert_eq!((impulses[0].linear, impulses[0].angular), ([0.0, 32.0, 0.0], [0.0, 1.0e-9, 0.0]));
                Ok(())
            })
            .unwrap();
        let v = owner.rows()[0].linear_velocity[1];
        assert!((v - (1.0 - 9.81 * TICK as f32)).abs() < 1e-3, "{v}");
        owner.set_pending(PendingTick { tick: 1, stamp: 0, interval: manifold_physics::stepping::StepInterval::new(manifold_core::Seconds((1) as f64 * TICK), manifold_core::Seconds(2.0 * TICK)), offline: true });
        let error = owner
            .settle_ready(Some(&scene), |_| true, |_, rows, impulses| decode_reaction(2, rows, Some(&reaction[..]), impulses))
            .unwrap_err();
        assert!(error.contains("smaller than the bodies"), "{error}");
    }

    /// An animated body in a coupled world follows its authored path at the
    /// start of each accepted live interval, with Sim Rate set to each display
    /// rate so this regression tests sampling without overload.
    #[test]
    fn liquid_coupled_animated_rows_match_at_every_frame_rate() {
        use crate::clock::LiquidClock;
        use manifold_physics::tick_samples::TickSamples;

        let scene_at = |t: f64| {
            // Smoothstep ease along an arc that turns as it goes.
            let s = (t / 0.8).clamp(0.0, 1.0);
            let eased = (s * s * (3.0 - 2.0 * s)) as f32;
            let angle = std::f32::consts::PI * eased;
            let mut scene = scene();
            let body = scene.bodies[0].as_mut().unwrap();
            body.kind = 2;
            body.transform.pos = [angle.cos(), 1.0 + 0.5 * angle.sin(), 0.2 * eased];
            body.transform.rot_euler = [0.0, 1.3 * angle, 0.0];
            scene
        };
        let colliders = RigidImpulseTargets::body(0).unwrap();
        let run = |fps: f64| {
            let mut owner = LiquidRigidOwner::new(&scene_at(0.0), OPEN, &colliders, MAX_LIQUID_BODIES, 1, None).expect("owner");
            let (mut clock, mut scenes) = (LiquidClock::default(), TickSamples::default());
            let (mut times, mut last, mut rows) = (Vec::new(), None::<f64>, Vec::new());
            for index in 0..=(fps as u64) {
                let transport = index as f64 / fps;
                if let Some(last) = last {
                    times.clear();
                    {
                        let mut played = Vec::new();
                        scenes.request(&clock, PlayedTime::from_played(last), PlayedTime::from_played(transport), &mut played);
                        times.extend(played.into_iter().map(PlayedTime::seconds));
                    }
                    for &time in times.iter().filter(|&&time| time < transport) {
                        scenes.observe(&clock, PlayedTime::from_played(time), Some(&scene_at(time)));
                    }
                    scenes.observe(&clock, PlayedTime::from_played(transport), Some(&scene_at(transport)));
                }
                last = Some(transport);
                // The previous display frame leaves its accepted interval
                // pending; it is exchanged once its end sample is present.
                owner.settle_ready(scenes.endpoint(owner.completed() + 1), |_| true, no_reaction).unwrap();
                let frame = clock.advance(PlayedTime::from_played(transport), 1.0 / fps, 1.0, 0.0, false, false);
                assert_eq!(frame.dropped_seconds, 0.0, "{fps} fps dropped time");
                if index > 0 {
                    assert_eq!(frame.ticks, 1, "{fps} fps accepted one live interval");
                }
                scenes.settle(&clock, &frame, Some(&scene_at(transport)));
                if frame.ticks == 1 {
                    let interval = frame.interval(0).unwrap();
                    rows.push((interval.start.0, owner.rows()[0]));
                    owner.set_pending(PendingTick { tick: owner.completed(), stamp: index, interval, offline: false });
                }
            }
            rows
        };
        for fps in [20.0, 24.0, 30.0, 60.0] {
            let rows = run(fps);
            assert_eq!(rows.len(), fps as usize, "{fps} fps interval count");
            for (start, row) in rows {
                let expected = scene_at(start).bodies[0].as_ref().unwrap().transform;
                assert!((0..3).all(|i| (row.position_inv_mass[i] - expected.pos[i]).abs() < 1e-4), "{fps} fps at {start}: {:?} vs {:?}", row.position_inv_mass, expected.pos);
            }
        }

        let mut owner = LiquidRigidOwner::new(&scene_at(0.0), OPEN, &colliders, MAX_LIQUID_BODIES, 1, None).expect("owner");
        owner.set_pending(PendingTick { tick: 0, stamp: 0, interval: manifold_physics::stepping::StepInterval::new(manifold_core::Seconds(0.0), manifold_core::Seconds(TICK)), offline: true });
        assert!(owner.settle_ready(None, |_| true, no_reaction).unwrap_err().contains("never sampled"));
        assert_eq!((owner.completed(), owner.pending().map(|p| p.tick)), (0, Some(0)));
    }

    /// Two seconds of a coupled pair driven as the domain drives it: in each
    /// replay sample the pair hands over the scene before the liquid observes
    /// Speed, and the liquid settles each tick once its end is sampled.
    /// Returns each tick's rows as bits, or the pair's error.
    fn coupled_rows_under_speed(fps: u32, offline: bool, speed: impl Fn(f64) -> f32) -> Result<Vec<(u64, [u32; 3])>, String> {
        use crate::clock::LiquidClock;
        use manifold_physics::tick_samples::TickSamples;

        let scene_at = |t: f64| {
            let mut scene = scene();
            let body = scene.bodies[0].as_mut().unwrap();
            body.kind = 2;
            let angle = (1.7 * t) as f32;
            body.transform.pos = [angle.cos(), 1.0 + 0.5 * angle.sin(), 0.1 * angle];
            scene
        };
        let colliders = RigidImpulseTargets::body(0).unwrap();
        let mut owner = LiquidRigidOwner::new(&scene_at(0.0), OPEN, &colliders, MAX_LIQUID_BODIES, 1, None)?;
        let (mut clock, mut scenes) = (LiquidClock::default(), TickSamples::default());
        let frame = clock.advance(PlayedTime::from_played(0.0), TICK, speed(0.0), 0.0, false, offline);
        scenes.settle(&clock, &frame, Some(&scene_at(0.0)));
        let (mut times, mut rows, mut previous) = (Vec::new(), Vec::new(), 0.0);
        let settle = |owner: &mut LiquidRigidOwner, scenes: &TickSamples<RigidSceneInputs>| -> Result<(), String> {
            owner.settle_ready(scenes.endpoint(owner.completed() + 1), |_| true, no_reaction)?;
            if let Some(start) = scenes.reanchored_start(owner.completed()) {
                owner.reanchor_start(start)?;
            }
            Ok(())
        };
        for index in 1..=2 * fps {
            let now = f64::from(index) / f64::from(fps);
            times.clear();
            {
                let mut played = Vec::new();
                scenes.request(&clock, PlayedTime::from_played(previous), PlayedTime::from_played(now), &mut played);
                times.extend(played.into_iter().map(PlayedTime::seconds));
            }
            let mut grid = (previous * 240.0).floor() + 1.0;
            while grid / 240.0 < now {
                times.push(grid / 240.0);
                grid += 1.0;
            }
            times.retain(|&time| time < now);
            times.sort_by(f64::total_cmp);
            times.dedup();
            times.push(now);
            for &time in &times {
                scenes.stage(Some(&scene_at(time)));
                clock.observe_speed(PlayedTime::from_played(time), speed(time));
                scenes.observe_staged(&clock, PlayedTime::from_played(time));
            }
            settle(&mut owner, &scenes)?;
            let frame = clock.advance(PlayedTime::from_played(now), TICK, speed(now), 0.0, false, offline);
            scenes.settle(&clock, &frame, Some(&scene_at(now)));
            for ordinal in 0..u64::from(frame.ticks) {
                if ordinal > 0 {
                    settle(&mut owner, &scenes)?;
                }
                let interval = frame.interval(ordinal).unwrap();
                rows.push((owner.completed(), [0, 1, 2].map(|axis| owner.rows()[0].position_inv_mass[axis].to_bits())));
                owner.set_pending(PendingTick { tick: owner.completed(), stamp: u64::from(index), interval, offline });
            }
            scenes.prune_before(owner.completed());
            previous = now;
        }
        Ok(rows)
    }

    /// Speed crossing 0 between frames in a coupled liquid and rigid pair:
    /// each tick's scene is recorded under the tick named once Speed at its
    /// start is known, each tick that stops on a Speed 0 boundary keeps the
    /// scene there as its end, and the pair never restarts. The rows match
    /// bit for bit at 30, 60, 120 and 144 fps, live and exported.
    #[test]
    fn coupled_pair_speed_crossing_zero_matches_at_30_60_120_144_fps_live_and_export() {
        let speed = |t: f64| (2.0 * (std::f64::consts::TAU * 1.3 * t).cos()).max(0.0) as f32;
        let reference = coupled_rows_under_speed(60, true, speed).unwrap();
        assert!(reference.len() > 40 && reference.len() < 100, "Speed 0 stretches run no tick: {} ran", reference.len());
        for fps in [30, 60, 120, 144] {
            for offline in [false, true] {
                let rows = coupled_rows_under_speed(fps, offline, speed).unwrap_or_else(|error| panic!("{fps} fps, offline {offline}: {error}"));
                assert!(rows == reference, "{fps} fps, offline {offline}: rows differ");
            }
        }
    }

    /// Three ticks in one frame consume three different reactions, and each
    /// successor sees the preceding rigid step rather than frame-start rows.
    #[test]
    fn liquid_coupled_three_tick_reactions_are_ordered_and_required() {
        let mut scene = scene();
        scene.gravity = [0.0; 3];
        let colliders = RigidImpulseTargets::body(0).unwrap();
        let mut owner = LiquidRigidOwner::new(&scene, OPEN, &colliders, MAX_LIQUID_BODIES, 1, None).unwrap();
        let mut velocity = 0.0;
        for tick in 0..3 {
            owner.set_pending(PendingTick { tick, stamp: 42, interval: manifold_physics::stepping::StepInterval::new(Seconds(tick as f64 * TICK), Seconds((tick + 1) as f64 * TICK)), offline: true });
            owner.settle_ready(Some(&scene), |stamp| stamp == 42, |pending, rows, impulses| {
                assert_eq!(pending.tick, tick);
                assert!((rows[0].linear_velocity[0] - velocity).abs() < 1e-5);
                let delta = (tick + 1) as f32;
                impulses[0].linear[0] = delta / rows[0].position_inv_mass[3];
                velocity += delta;
                Ok(())
            }).unwrap();
            assert_eq!(owner.completed(), tick + 1);
            assert_eq!(owner.frame().stamp.tick, tick + 1);
            assert!((owner.rows()[0].linear_velocity[0] - velocity).abs() < 1e-5);
        }
        owner.set_pending(PendingTick { tick: 3, stamp: 43, interval: manifold_physics::stepping::StepInterval::new(manifold_core::Seconds((3) as f64 * TICK), manifold_core::Seconds(((3) + 1) as f64 * TICK)), offline: true });
        let missing = owner.settle_ready(None, |_| true, no_reaction).unwrap_err();
        assert!(missing.contains("tick 3") && missing.contains("never sampled"), "{missing}");
        let late = owner.settle_ready(Some(&scene), |_| false, |_, _, _| panic!("unretired read")).unwrap_err();
        assert!(late.contains("stamp 43") && late.contains("did not retire"), "{late}");
        assert_eq!(owner.completed(), 3);
    }
}
