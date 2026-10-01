//! `node.gpu_flip_domain` — the scene-facing CPU bridge of a GPU FLIP liquid
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` P7a, `docs/GPU_FLIP_PRESSURE_SOLVE.md`):
//! it speaks `node.fluid_surface`'s scene contract (names, types, meanings)
//! and turns it into the fixed-tick clock, the fill's sites, gravity, the
//! Collider roles as body rows, shapes and a distance atlas, and the padded
//! lattice the solid distance and the particle frame read. Scene
//! forces and impulses reach the water through the shared field lattices of
//! `liquid::fields` (seam P8), sampled over the face grid's box. Every
//! lattice atom reads the lattice from its wires, so Resolution applies on
//! change. Exempt from the codegen mandate as a CPU bridge
//! (ADDING_PRIMITIVES.md exclusion 3).

use std::borrow::Cow;

use manifold_gpu::GpuBuffer;
use manifold_physics::FieldValue;

use super::gpu_flip_pressure::lattice_refusal;
use super::liquid_fill::{SITES_PER_CELL, filled_sites, site_range};
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid::{FluidDomainLayout, TICK, domain_layout};
use crate::node_graph::fluid_role::{FluidRole, MAX_FLUID_ROLES};
use crate::node_graph::liquid::bodies::{BodiesStatus, LiquidBodies, LiquidBody, LiquidShape};
use crate::node_graph::liquid::body_buffers::LiquidBodyBuffers;
use crate::node_graph::liquid::clock::LiquidClock;
use crate::node_graph::liquid::fields::{self, FieldLattice, LiquidFields, LiquidImpulses};
use crate::node_graph::liquid::lattice::LiquidLattice;
use crate::node_graph::liquid::{EXACT_F32_COUNT, ROLE_PORTS};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::physics::offline_simulation;
use crate::node_graph::physics_events::ResolvedNodeImpulse;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::transform::Transform;

/// Water, kg/m³: each fill particle weighs an eighth of a cell of it.
const REST_DENSITY: f64 = 1000.0;

/// Every wall of the tank is closed.
const CLOSED_FACES: u32 = 63;

/// Everything whose change restarts the liquid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GpuFlipSetup {
    pub(crate) lattice: LiquidLattice,
    pub(crate) pool_sites: u32,
    pub(crate) box_sites: [[u32; 2]; 3],
}

/// The domain's setup and the layout it came from, computed from params and
/// wires alone: the node and the extent checker both call [`gpu_flip_geometry`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GpuFlipGeometry {
    pub(crate) layout: FluidDomainLayout,
    pub(crate) setup: GpuFlipSetup,
    pub(crate) particles: u64,
}

impl GpuFlipGeometry {
    /// The scalar outputs fixed by the setup, by name.
    pub(crate) fn outputs(&self) -> [(&'static str, f32); 16] {
        let GpuFlipSetup { lattice, pool_sites, box_sites } = self.setup;
        let h = self.layout.cell_size;
        [
            ("lattice_min_x", lattice.min()[0]),
            ("lattice_min_y", lattice.min()[1]),
            ("lattice_min_z", lattice.min()[2]),
            ("cell_size", lattice.cell_size()),
            ("nodes_x", lattice.nodes()[0] as f32),
            ("nodes_y", lattice.nodes()[1] as f32),
            ("nodes_z", lattice.nodes()[2] as f32),
            ("closed_faces", CLOSED_FACES as f32),
            ("pool_sites", pool_sites as f32),
            ("box_x0", box_sites[0][0] as f32),
            ("box_x1", box_sites[0][1] as f32),
            ("box_y0", box_sites[1][0] as f32),
            ("box_y1", box_sites[1][1] as f32),
            ("box_z0", box_sites[2][0] as f32),
            ("box_z1", box_sites[2][1] as f32),
            ("particle_mass", (REST_DENSITY * h * h * h / f64::from(SITES_PER_CELL)) as f32),
        ]
    }
}

/// The fill in the layout's half-cell sites: the pool's height and the
/// initial volume's site range per axis (empty without one), refused by name
/// when either does not fit the domain.
fn fill_sites(
    layout: &FluidDomainLayout,
    fill_height: f32,
    initial_volume: Option<Transform>,
) -> Result<(u32, [[u32; 2]; 3]), String> {
    if !fill_height.is_finite() || fill_height < 0.0 || fill_height >= layout.size[1] {
        return Err("GPU FLIP: Initial Fill Height must lie within the domain height".into());
    }
    let min = layout.min.map(f64::from);
    let h = layout.cell_size;
    let pool = site_range(min[1], min[1] + f64::from(fill_height), min[1], h, layout.cells[1])[1];
    let mut sites = [[0u32; 2]; 3];
    if let Some(volume) = initial_volume {
        if volume.billboard || volume.rot_euler.iter().any(|v| !v.is_finite() || v.abs() > 1e-6) {
            return Err("GPU FLIP: the initial volume must be an axis-aligned box; rotation and billboarding are not supported".into());
        }
        if volume.pos.iter().chain(&volume.scale).any(|v| !v.is_finite())
            || volume.scale.iter().any(|v| *v <= 0.0)
            || (0..3).any(|d| {
                let half = volume.scale[d] * 0.5;
                volume.pos[d] - half < layout.min[d] - 1e-4 || volume.pos[d] + half > layout.min[d] + layout.size[d] + 1e-4
            })
        {
            return Err("GPU FLIP: the initial volume must be a finite box fully inside the domain".into());
        }
        for (d, range) in sites.iter_mut().enumerate() {
            let half = f64::from(volume.scale[d]) * 0.5;
            let centre = f64::from(volume.pos[d]);
            *range = site_range(centre - half, centre + half, min[d], h, layout.cells[d]);
        }
    }
    Ok((pool, sites))
}

/// The domain box, lattice and fill from the domain's params and wires
/// (`read` is `scalar_or_param`). Refused by name, in this order: a lattice
/// the pressure solve cannot take, a fill that does not fit the domain, and
/// a fill past the count a wire carries exactly.
pub(crate) fn gpu_flip_geometry(
    read: impl Fn(&str, f32) -> f32,
    domain: Option<Transform>,
    initial_volume: Option<Transform>,
) -> Result<GpuFlipGeometry, String> {
    let resolution = read("resolution", 64.0).round().max(0.0) as u32;
    let layout = domain_layout(domain, read("domain_size", 4.0), resolution)?;
    if let Some(reason) = lattice_refusal(layout.cells) {
        return Err(format!("GPU FLIP: {reason}. Lower Resolution."));
    }
    let (pool_sites, box_sites) = fill_sites(&layout, read("fill_height", 0.4), initial_volume)?;
    let particles = filled_sites(layout.cells, pool_sites, box_sites);
    if particles > u64::from(EXACT_F32_COUNT) {
        return Err(format!(
            "GPU FLIP: the fill places {particles} particles, more than the {EXACT_F32_COUNT} a particle count carries exactly. Lower Resolution or Initial Fill Height."
        ));
    }
    let setup = GpuFlipSetup { lattice: LiquidLattice::from_layout(&layout), pool_sites, box_sites };
    Ok(GpuFlipGeometry { layout, setup, particles })
}

impl GpuFlipGeometry {
    /// The field lattice over the face grid's box: from its minimum corner
    /// to its far wall faces.
    pub(crate) fn field_lattice(&self) -> FieldLattice {
        FieldLattice::covering(self.layout.min, self.layout.cell_size as f32, self.layout.cells.map(|n| n + 1))
    }
}

/// Every scalar output, in the order [`GpuFlipDomain::compute`] fills them.
const OUTPUTS: [&str; 33] = [
    "lattice_min_x", "lattice_min_y", "lattice_min_z", "cell_size", "nodes_x", "nodes_y", "nodes_z",
    "closed_faces", "pool_sites", "box_x0", "box_x1", "box_y0", "box_y1", "box_z0", "box_z1",
    "particle_mass", "gravity_x", "gravity", "gravity_z", "ticks", "epoch", "simulation_time",
    "display_time", "dropped_seconds", "body_count", "body_rows", "first_tick", "field_nodes_x",
    "field_nodes_y", "field_nodes_z", "field_spacing", "force_lattices", "impulse_tick",
];
const TICKS: usize = 19;
const IMPULSE_TICK: usize = 32;
const _: () = assert!(matches!(OUTPUTS[TICKS].as_bytes(), b"ticks"));
const _: () = assert!(matches!(OUTPUTS[IMPULSE_TICK].as_bytes(), b"impulse_tick"));

crate::primitive! {
    name: GpuFlipDomain,
    type_id: "node.gpu_flip_domain",
    purpose: "Define a GPU FLIP liquid domain with node.fluid_surface's scene contract: the axis-aligned domain box, resolution, initial fill height and box, gravity, the scene's acceleration field and impulses, simulation speed and reset. Outputs this frame's fixed 60 Hz ticks, the epoch and the display clock, the fill's half-cell sites and particle mass, gravity, the scene's forces and impulses on coarse field lattices over the box, and the padded lattice with its closed walls for the solid distance and the particle frame. The tank is closed on every side. A hit fired while the liquid is held (paused, Speed 0) is discarded, never replayed. Up to 64 Collider roles become one body row per collider per tick of this frame (its pose at the tick's start and its motion over the tick), a shape per collider and their distance lattices packed in one half-precision atlas; the water flows around them. Fill, Inflow and Outflow roles and pairing with a physics world are refused by name.",
    inputs: {
        domain: Transform optional,
        initial_volume: Transform optional,
        acceleration_field: VectorField optional,
        resolution: ScalarF32 optional,
        domain_size: ScalarF32 optional,
        fill_height: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        speed: ScalarF32 optional,
        reset: ScalarF32 optional,
        role_0: FluidRole optional,
        role_1: FluidRole optional,
        role_2: FluidRole optional,
        role_3: FluidRole optional,
        role_4: FluidRole optional,
        role_5: FluidRole optional,
        role_6: FluidRole optional,
        role_7: FluidRole optional,
        role_8: FluidRole optional,
        role_9: FluidRole optional,
        role_10: FluidRole optional,
        role_11: FluidRole optional,
        role_12: FluidRole optional,
        role_13: FluidRole optional,
        role_14: FluidRole optional,
        role_15: FluidRole optional,
        role_16: FluidRole optional,
        role_17: FluidRole optional,
        role_18: FluidRole optional,
        role_19: FluidRole optional,
        role_20: FluidRole optional,
        role_21: FluidRole optional,
        role_22: FluidRole optional,
        role_23: FluidRole optional,
        role_24: FluidRole optional,
        role_25: FluidRole optional,
        role_26: FluidRole optional,
        role_27: FluidRole optional,
        role_28: FluidRole optional,
        role_29: FluidRole optional,
        role_30: FluidRole optional,
        role_31: FluidRole optional,
        role_32: FluidRole optional,
        role_33: FluidRole optional,
        role_34: FluidRole optional,
        role_35: FluidRole optional,
        role_36: FluidRole optional,
        role_37: FluidRole optional,
        role_38: FluidRole optional,
        role_39: FluidRole optional,
        role_40: FluidRole optional,
        role_41: FluidRole optional,
        role_42: FluidRole optional,
        role_43: FluidRole optional,
        role_44: FluidRole optional,
        role_45: FluidRole optional,
        role_46: FluidRole optional,
        role_47: FluidRole optional,
        role_48: FluidRole optional,
        role_49: FluidRole optional,
        role_50: FluidRole optional,
        role_51: FluidRole optional,
        role_52: FluidRole optional,
        role_53: FluidRole optional,
        role_54: FluidRole optional,
        role_55: FluidRole optional,
        role_56: FluidRole optional,
        role_57: FluidRole optional,
        role_58: FluidRole optional,
        role_59: FluidRole optional,
        role_60: FluidRole optional,
        role_61: FluidRole optional,
        role_62: FluidRole optional,
        role_63: FluidRole optional,
    },
    outputs: {
        lattice_min_x: ScalarF32, lattice_min_y: ScalarF32, lattice_min_z: ScalarF32,
        cell_size: ScalarF32,
        nodes_x: ScalarF32, nodes_y: ScalarF32, nodes_z: ScalarF32,
        closed_faces: ScalarF32,
        pool_sites: ScalarF32,
        box_x0: ScalarF32, box_x1: ScalarF32,
        box_y0: ScalarF32, box_y1: ScalarF32,
        box_z0: ScalarF32, box_z1: ScalarF32,
        particle_mass: ScalarF32,
        gravity_x: ScalarF32, gravity: ScalarF32, gravity_z: ScalarF32,
        ticks: ScalarF32,
        epoch: ScalarF32,
        simulation_time: ScalarF32,
        display_time: ScalarF32,
        dropped_seconds: ScalarF32,
        body_count: ScalarF32, body_rows: ScalarF32,
        first_tick: ScalarF32,
        field_nodes_x: ScalarF32, field_nodes_y: ScalarF32, field_nodes_z: ScalarF32,
        field_spacing: ScalarF32,
        force_lattices: ScalarF32,
        impulse_tick: ScalarF32,
        bodies: Array(LiquidBody), shapes: Array(LiquidShape), atlas: Array(u32),
        forces: Array(f32), impulses: Array(f32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("resolution"), label: "Resolution", ty: ParamType::Int, default: ParamValue::Float(64.0), range: Some((8.0, 512.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("domain_size"), label: "Domain Size", ty: ParamType::Float, default: ParamValue::Float(4.0), range: Some((0.5, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fill_height"), label: "Initial Fill Height", ty: ParamType::Float, default: ParamValue::Float(0.4), range: Some((0.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_x"), label: "Gravity X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity"), label: "Gravity Y", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_z"), label: "Gravity Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("speed"), label: "Simulation Speed", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("reset"), label: "Reset", ty: ParamType::Trigger, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "The GPU FLIP group's source of truth: ticks and epoch into node.liquid_state (the tick region's clock owner), the fill sites and the padded lattice into node.liquid_fill, gravity, forces, impulses, the field scalars (field_nodes_x/y/z, field_spacing, force_lattices, first_tick, impulse_tick), bodies, shapes, atlas and the padded lattice into every node.gpu_flip_step, and the padded lattice with bodies, shapes and atlas into node.liquid_solid_distance, node.liquid_frame and node.face_sample_component; simulation_time, display_time and epoch into node.liquid_frame; particle_mass into node.liquid_stats. The domain box, Resolution and fill restart the liquid; gravity and Simulation Speed are live. Live runs at most three ticks per display frame and reports dropped time; export runs every tick.",
    examples: [],
    picker: { label: "GPU FLIP Domain", category: Atom },
    summary: "Sets up a GPU FLIP liquid: its box, resolution, starting fill, gravity and speed.",
    category: Particles3D,
    role: Source,
    aliases: ["gpu flip", "gpu water", "liquid domain"],
    boundary_reason: NonGpu,
    extra_fields: {
        clock: LiquidClock = LiquidClock::default(),
        setup: Option<GpuFlipSetup> = None,
        published: Option<[f32; OUTPUTS.len()]> = None,
        bodies: LiquidBodies = LiquidBodies::default(),
        body_buffers: LiquidBodyBuffers = LiquidBodyBuffers::default(),
        role_pending: bool = false,
        body_rows: f32 = 0.0,
        rows_fresh: bool = false,
        coupled: bool = false,
        impulses: LiquidImpulses = LiquidImpulses::default(),
        fields: LiquidFields = LiquidFields::default(),
        acceleration: Option<FieldValue> = None,
        holding: bool = false,
    },
}

impl Primitive for GpuFlipDomain {
    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "bodies" | "shapes" | "atlas" | "forces" | "impulses")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "forces" => return self.fields.forces_buffer(),
            "impulses" => return self.fields.impulses_buffer(),
            _ => {}
        }
        self.body_buffers.output(port)
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        matches!(port_name, "bodies" | "shapes" | "atlas" | "forces" | "impulses").then_some(1)
    }

    fn warmup_pending(&self) -> bool {
        self.role_pending
    }

    fn set_coupled_physics(&mut self, enabled: bool) {
        self.coupled = enabled;
    }

    fn clear_state(&mut self) {
        self.clock.restart();
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // A physics sample reads authored inputs only; it never advances time.
        if crate::node_graph::physics::authored_sample_only() {
            return;
        }
        let mut roles: [Option<FluidRole>; MAX_FLUID_ROLES] = std::array::from_fn(|_| None);
        self.role_pending = false;
        for (slot, port) in ROLE_PORTS.iter().enumerate() {
            if let Some(input) = ctx.inputs.slot(port) {
                roles[slot] = ctx.inputs.fluid_role(port);
                self.role_pending |= !ctx.inputs.slot_content_ready(input) || roles[slot].is_none();
            }
        }
        self.acceleration = ctx.inputs.vector_field("acceleration_field");
        if let Some(slot) = ctx.inputs.slot("acceleration_field") {
            self.role_pending |= !ctx.inputs.slot_content_ready(slot) || self.acceleration.is_none();
        }
        // Every output is published every frame, so no consumer reads a slot
        // this node left unwritten. While a role or the field is still being
        // prepared, or on an error, the liquid holds: the last good outputs
        // repeat with zero ticks and no impulse.
        let computed = if self.role_pending { Ok(None) } else { self.compute(ctx, &roles) };
        let held = || {
            let mut held = self.published.unwrap_or([0.0; OUTPUTS.len()]);
            held[TICKS] = 0.0;
            held[IMPULSE_TICK] = -1.0;
            held
        };
        let (values, fresh) = match computed {
            Ok(Some(values)) => (values, true),
            Ok(None) => {
                self.role_pending = true;
                (held(), false)
            }
            Err(error) => {
                ctx.error(error);
                (held(), false)
            }
        };
        self.holding = !fresh;
        if fresh {
            self.published = Some(values);
        }
        for (name, value) in OUTPUTS.iter().zip(values) {
            ctx.outputs.set_scalar(name, ParamValue::Float(value));
        }
        if let Some(gpu) = ctx.gpu.as_deref_mut() {
            self.body_buffers.upload(gpu, &self.bodies, fresh && self.rows_fresh, "node.gpu_flip_domain.bodies");
        }
        let uploaded = match ctx.gpu.as_deref_mut().map(|gpu| self.fields.upload(gpu, offline_simulation())) {
            Some(Ok(())) => true,
            Some(Err(error)) => {
                ctx.error(error);
                false
            }
            None => false,
        };
        // A hit is "applied" only once its tick's fields are on the GPU; a
        // frame that held or failed discards its hits with a receipt.
        if fresh && uploaded {
            self.impulses.commit_frame();
        } else {
            self.impulses.abandon_frame();
        }
    }

    fn physics_impulse_epoch(&self) -> Option<u64> {
        self.impulses.epoch()
    }

    /// A hit fired this frame is stamped at the liquid's simulated time, so
    /// it lands on the first tick of the next frame.
    fn physics_impulse_stamp(
        &self,
        transport: manifold_core::Seconds,
        sequence: u64,
    ) -> Result<manifold_physics::input::EventStamp, String> {
        if self.holding {
            return Err("GPU FLIP impulses: cannot capture an impulse while the liquid is pending or failed".into());
        }
        self.impulses.stamp(transport.0, sequence)
    }

    /// GPU FLIP carries no bodies yet, so a hit aimed at a rigid body is
    /// refused; the solids work plugs its rigid owner in here.
    fn enqueue_physics_impulse(
        &mut self,
        stamp: manifold_physics::input::EventStamp,
        impulse: ResolvedNodeImpulse,
    ) -> Result<manifold_physics::TickStamp, String> {
        self.impulses.enqueue(stamp, impulse, None)
    }

    fn drain_physics_impulses(
        &mut self,
        consume: &mut dyn FnMut(manifold_physics::input::AppliedEvent<ResolvedNodeImpulse>),
    ) {
        self.impulses.drain_applied(consume, None);
    }

    fn drain_discarded_impulses(&mut self, consume: &mut dyn FnMut(manifold_physics::input::EventStamp)) {
        self.impulses.drain_discarded(consume);
    }
}

impl GpuFlipDomain {
    /// This frame's outputs, or None while a collider's distance lattice is
    /// still building (the liquid holds and the clock does not advance).
    fn compute(
        &mut self,
        ctx: &EffectNodeContext<'_, '_>,
        roles: &[Option<FluidRole>],
    ) -> Result<Option<[f32; OUTPUTS.len()]>, String> {
        if self.coupled {
            return Err("GPU FLIP: the water does not couple with a physics world yet; take the physics world out of this scene".into());
        }
        let geometry = gpu_flip_geometry(
            |name, default| ctx.scalar_or_param(name, default),
            ctx.inputs.transform("domain"),
            ctx.inputs.transform("initial_volume"),
        )?;
        let speed = ctx.scalar_or_param("speed", 1.0);
        let gravity = [
            ctx.scalar_or_param("gravity_x", 0.0),
            ctx.scalar_or_param("gravity", -9.81),
            ctx.scalar_or_param("gravity_z", 0.0),
        ];
        if !speed.is_finite() || !(0.0..=4.0).contains(&speed) || gravity.iter().any(|g| !g.is_finite()) {
            return Err("GPU FLIP: Simulation Speed must be between 0 and 4, and gravity must be finite".into());
        }
        let offline = offline_simulation();
        if self.bodies.prepare(roles, &[], geometry.setup.lattice.cell_size(), offline)? == BodiesStatus::Pending {
            return Ok(None);
        }
        let restart = self.setup != Some(geometry.setup);
        self.setup = Some(geometry.setup);
        let frame = self.clock.advance(
            ctx.time.seconds.0,
            ctx.time.delta.0,
            speed,
            ctx.scalar_or_param("reset", 0.0),
            restart,
            offline,
        );
        self.impulses.observe_frame(ctx.time.seconds.0, &frame)?;
        let consumed = frame.simulation_time - f64::from(frame.ticks) * TICK;
        self.bodies.observe(roles, frame.epoch, frame.target_time, consumed)?;
        // A restart runs no tick yet still publishes the first tick's rows: the
        // fill seeds around the colliders' starting poses. Otherwise a frame
        // without ticks keeps the last rows as the bodies' poses.
        let row_ticks = if frame.restarted { frame.ticks.max(1) } else { frame.ticks };
        self.rows_fresh = row_ticks > 0;
        if row_ticks > 0 {
            self.body_rows = self.bodies.rows(fields::first_tick(&frame), row_ticks, &[]).len() as f32;
        }
        let field = self.fields.prepare(geometry.field_lattice(), self.acceleration.as_ref(), &frame, &self.impulses)?;
        let per_frame = [
            ("gravity_x", gravity[0]),
            ("gravity", gravity[1]),
            ("gravity_z", gravity[2]),
            ("ticks", frame.ticks as f32),
            ("epoch", frame.epoch as f32),
            ("simulation_time", frame.simulation_time as f32),
            ("display_time", frame.display_time as f32),
            ("dropped_seconds", frame.dropped_seconds as f32),
            ("body_count", self.bodies.count() as f32),
            ("body_rows", self.body_rows),
            ("first_tick", fields::first_tick(&frame) as f32),
        ];
        let mut values = [0.0; OUTPUTS.len()];
        let mut written = 0u64;
        for (name, value) in geometry.outputs().into_iter().chain(per_frame).chain(field.outputs()) {
            let slot = OUTPUTS.iter().position(|output| *output == name).expect("every output has a slot");
            values[slot] = value;
            written |= 1 << slot;
        }
        debug_assert_eq!(written, (1 << OUTPUTS.len()) - 1, "every output written once");
        Ok(Some(values))
    }
}

const _: () = assert!(ROLE_PORTS.len() == MAX_FLUID_ROLES);

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(resolution: f32, fill: f32, volume: Option<Transform>) -> Result<GpuFlipGeometry, String> {
        let read = |name: &str, default: f32| match name {
            "resolution" => resolution,
            "fill_height" => fill,
            _ => default,
        };
        gpu_flip_geometry(read, None, volume)
    }

    /// Any side the slider reaches is a lattice: the solver halves sides
    /// rounding up, so none needs to divide by a power of two.
    #[test]
    fn gpu_flip_domain_takes_any_resolution() {
        for resolution in [8.0, 24.0, 32.0, 63.0, 72.0, 100.0, 128.0] {
            let at = geometry(resolution, 0.16, None).unwrap_or_else(|reason| panic!("{resolution}: {reason}"));
            let n = resolution as u32;
            assert_eq!(at.setup.lattice.cells(), [n; 3]);
            assert_eq!(at.setup.lattice.nodes(), [n + 7; 3]);
        }
    }

    /// The Dam Break's column, as the preset wires it: the engine's boxes on
    /// the engine's half-cell site rule.
    #[test]
    fn gpu_flip_domain_fills_the_dam_break_sites() {
        let column = Transform { pos: [-1.25, 1.12, 0.0], scale: [1.18, 1.92, 3.5], ..Transform::default() };
        let at64 = geometry(64.0, 0.16, Some(column)).expect("64 fills");
        assert_eq!((at64.setup.pool_sites, at64.setup.box_sites), (5, [[5, 43], [5, 67], [8, 120]]));
        assert_eq!(at64.particles, 128 * 5 * 128 + 38 * 62 * 112);
        assert_eq!(at64.setup.lattice.nodes(), [71; 3]);
        let mass = at64.outputs().iter().find(|(name, _)| *name == "particle_mass").expect("mass").1;
        assert!((mass - 0.030_517_578).abs() < 1e-9, "{mass}");
    }

    #[test]
    fn gpu_flip_domain_refuses_by_name() {
        let refused = |result: Result<GpuFlipGeometry, String>| result.expect_err("refused");
        assert!(refused(geometry(64.0, 4.0, None)).contains("Initial Fill Height"));
        let turned = Transform { pos: [0.0, 1.0, 0.0], scale: [1.0; 3], rot_euler: [0.0, 0.3, 0.0], ..Transform::default() };
        assert!(refused(geometry(64.0, 0.16, Some(turned))).contains("initial volume"));
        // 256³ with a 2.5 m pool is 8 · 256² · 160 particles, past 2^24.
        let over = refused(geometry(256.0, 2.5, None));
        assert!(over.contains("Resolution") && over.contains("Initial Fill Height"), "{over}");
    }
}
