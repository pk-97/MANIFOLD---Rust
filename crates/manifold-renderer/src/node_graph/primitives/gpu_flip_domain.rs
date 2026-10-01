//! `node.gpu_flip_domain` — the scene-facing CPU bridge of a GPU FLIP liquid
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` P7a, `docs/GPU_FLIP_PRESSURE_SOLVE.md`):
//! it speaks `node.fluid_surface`'s scene contract (names, types, meanings)
//! and turns it into the fixed-tick clock, the fill's sites, gravity and the
//! padded lattice the solid distance and the particle frame read. The
//! solver's own lattice is still baked into the preset, so any other lattice
//! is refused by name. Exempt from the codegen mandate as a CPU bridge
//! (ADDING_PRIMITIVES.md exclusion 3).

use std::borrow::Cow;

use manifold_gpu::GpuBuffer;

use super::coarse_inverse::multigrid_refusal;
use super::liquid_fill::{SITES_PER_CELL, filled_sites, site_range};
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid::{FluidDomainLayout, domain_layout};
use crate::node_graph::fluid_role::{FluidRoleKind, MAX_FLUID_ROLES};
use crate::node_graph::liquid::bodies::{LiquidBody, LiquidShape};
use crate::node_graph::liquid::clock::LiquidClock;
use crate::node_graph::liquid::lattice::LiquidLattice;
use crate::node_graph::liquid::{EXACT_F32_COUNT, ROLE_PORTS};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::physics::offline_simulation;
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
/// the pressure solve cannot transform, a fill that does not fit the domain,
/// a fill past the count a wire carries exactly, and a lattice other than the
/// one the preset's solver is built for.
pub(crate) fn gpu_flip_geometry(
    read: impl Fn(&str, f32) -> f32,
    params: &ParamValues,
    domain: Option<Transform>,
    initial_volume: Option<Transform>,
) -> Result<GpuFlipGeometry, String> {
    let resolution = read("resolution", 64.0).round().max(0.0) as u32;
    let layout = domain_layout(domain, read("domain_size", 4.0), resolution)?;
    if let Some(reason) = multigrid_refusal(layout.cells) {
        return Err(format!("GPU FLIP: {reason}. Change Resolution."));
    }
    let (pool_sites, box_sites) = fill_sites(&layout, read("fill_height", 0.4), initial_volume)?;
    let particles = filled_sites(layout.cells, pool_sites, box_sites);
    if particles > u64::from(EXACT_F32_COUNT) {
        return Err(format!(
            "GPU FLIP: the fill places {particles} particles, more than the {EXACT_F32_COUNT} a particle count carries exactly. Lower Resolution or Initial Fill Height."
        ));
    }
    let float = |name: &str, default: f32| match params.get(name) {
        Some(ParamValue::Float(v)) => *v,
        _ => default,
    };
    let built_resolution = float("built_resolution", 64.0).round().max(8.0) as u32;
    let built_size = float("built_domain_size", 4.0);
    let built = domain_layout(None, built_size, built_resolution)?;
    if layout.cells != built.cells || layout.min != built.min || layout.cell_size != built.cell_size {
        return Err(format!(
            "GPU FLIP: this solver is built for Resolution {built_resolution} and Domain Size {built_size} m with no domain box, and its lattice cannot change yet. Set Resolution to {built_resolution} and Domain Size to {built_size}."
        ));
    }
    let setup = GpuFlipSetup { lattice: LiquidLattice::from_layout(&layout), pool_sites, box_sites };
    Ok(GpuFlipGeometry { layout, setup, particles })
}

/// The refusal of any wired role until sources, drains and solids reach
/// GPU FLIP (solids are owed: GPU_FLIP_PRESSURE_SOLVE.md section 8 (owed)).
fn refuse_roles(ctx: &EffectNodeContext<'_, '_>) -> Result<(), String> {
    for port in ROLE_PORTS {
        if ctx.inputs.slot(port).is_none() {
            continue;
        }
        return Err(match ctx.inputs.fluid_role(port).map(|role| role.kind) {
            Some(FluidRoleKind::Collider) => {
                "GPU FLIP: Collider roles are not supported yet; the water does not meet solids until bodies join its pressure solve".into()
            }
            _ => "GPU FLIP: Fill, Inflow and Outflow roles are not supported; unwire them from the domain".into(),
        });
    }
    Ok(())
}

/// Every scalar output, in the order [`GpuFlipDomain::compute`] fills them.
const OUTPUTS: [&str; 26] = [
    "lattice_min_x", "lattice_min_y", "lattice_min_z", "cell_size", "nodes_x", "nodes_y", "nodes_z",
    "closed_faces", "pool_sites", "box_x0", "box_x1", "box_y0", "box_y1", "box_z0", "box_z1",
    "particle_mass", "gravity_x", "gravity", "gravity_z", "ticks", "epoch", "simulation_time",
    "display_time", "dropped_seconds", "body_count", "body_rows",
];
const TICKS: usize = 19;

/// Empty body, shape and atlas storage: GPU FLIP carries no bodies yet, and
/// node.liquid_solid_distance reads them with a body count of 0.
pub struct EmptyBodies {
    bodies: GpuBuffer,
    shapes: GpuBuffer,
    atlas: GpuBuffer,
}

crate::primitive! {
    name: GpuFlipDomain,
    type_id: "node.gpu_flip_domain",
    purpose: "Define a GPU FLIP liquid domain with node.fluid_surface's scene contract: the axis-aligned domain box, resolution, initial fill height and box, gravity, simulation speed and reset. Outputs this frame's fixed 60 Hz ticks, the epoch and the display clock, the fill's half-cell sites and particle mass, gravity, and the padded lattice with its closed walls for the solid distance and the particle frame. The tank is closed on every side. The solver's lattice is baked into its preset: any other Resolution, Domain Size or domain box is refused by name, as are every role and pairing with a physics world.",
    inputs: {
        domain: Transform optional,
        initial_volume: Transform optional,
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
        bodies: Array(LiquidBody), shapes: Array(LiquidShape), atlas: Array(u32),
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
        ParamDef { name: Cow::Borrowed("built_resolution"), label: "Built Resolution", ty: ParamType::Int, default: ParamValue::Float(64.0), range: Some((8.0, 512.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("built_domain_size"), label: "Built Domain Size", ty: ParamType::Float, default: ParamValue::Float(4.0), range: Some((0.5, 20.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "The GPU FLIP group's source of truth: ticks and epoch into node.liquid_state (the tick region's clock owner), the fill sites into node.liquid_fill, gravity into every step's node.face_gravity, and the padded lattice with bodies, shapes and atlas into node.liquid_solid_distance and node.liquid_frame; simulation_time, display_time and epoch into node.liquid_frame; particle_mass into node.liquid_stats. The domain box, Resolution and fill restart the liquid; gravity and Simulation Speed are live. Live runs at most three ticks per display frame and reports dropped time; export runs every tick. Built Resolution and Built Domain Size name the lattice the preset's atoms are built for.",
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
        empty: Option<EmptyBodies> = None,
        coupled: bool = false,
    },
}

impl Primitive for GpuFlipDomain {
    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "bodies" | "shapes" | "atlas")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        let empty = self.empty.as_ref()?;
        match port {
            "bodies" => Some(&empty.bodies),
            "shapes" => Some(&empty.shapes),
            "atlas" => Some(&empty.atlas),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        matches!(port_name, "bodies" | "shapes" | "atlas").then_some(1)
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
        if self.empty.is_none() {
            let device = ctx.gpu_encoder().device;
            let zeroed = |bytes: usize| {
                let buffer = device.create_buffer_shared(bytes as u64);
                buffer.zero_fill();
                buffer
            };
            self.empty = Some(EmptyBodies {
                bodies: zeroed(std::mem::size_of::<LiquidBody>()),
                shapes: zeroed(std::mem::size_of::<LiquidShape>()),
                atlas: zeroed(4),
            });
        }
        // Every output is published every frame, so no consumer reads a slot
        // this node left unwritten. On an error the liquid holds: the last
        // good outputs repeat with zero ticks.
        let values = match self.compute(ctx) {
            Ok(values) => {
                self.published = Some(values);
                values
            }
            Err(error) => {
                ctx.error(error);
                let mut held = self.published.unwrap_or([0.0; OUTPUTS.len()]);
                held[TICKS] = 0.0;
                held
            }
        };
        for (name, value) in OUTPUTS.iter().zip(values) {
            ctx.outputs.set_scalar(name, ParamValue::Float(value));
        }
    }
}

impl GpuFlipDomain {
    fn compute(&mut self, ctx: &EffectNodeContext<'_, '_>) -> Result<[f32; OUTPUTS.len()], String> {
        if self.coupled {
            return Err("GPU FLIP: the water does not couple with a physics world yet; take the physics world out of this scene".into());
        }
        refuse_roles(ctx)?;
        let geometry = gpu_flip_geometry(
            |name, default| ctx.scalar_or_param(name, default),
            ctx.params,
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
        let restart = self.setup != Some(geometry.setup);
        self.setup = Some(geometry.setup);
        let frame = self.clock.advance(
            ctx.time.seconds.0,
            ctx.time.delta.0,
            speed,
            ctx.scalar_or_param("reset", 0.0),
            restart,
            offline_simulation(),
        );
        let per_frame = [
            ("gravity_x", gravity[0]),
            ("gravity", gravity[1]),
            ("gravity_z", gravity[2]),
            ("ticks", frame.ticks as f32),
            ("epoch", frame.epoch as f32),
            ("simulation_time", frame.simulation_time as f32),
            ("display_time", frame.display_time as f32),
            ("dropped_seconds", frame.dropped_seconds as f32),
            ("body_count", 0.0),
            ("body_rows", 0.0),
        ];
        let mut values = [0.0; OUTPUTS.len()];
        let mut written = 0u32;
        for (name, value) in geometry.outputs().into_iter().chain(per_frame) {
            let slot = OUTPUTS.iter().position(|output| *output == name).expect("every output has a slot");
            values[slot] = value;
            written |= 1 << slot;
        }
        debug_assert_eq!(written, (1 << OUTPUTS.len()) - 1, "every output written once");
        Ok(values)
    }
}

const _: () = assert!(ROLE_PORTS.len() == MAX_FLUID_ROLES);

#[cfg(test)]
mod tests {
    use super::*;

    fn params(built_resolution: f32) -> ParamValues {
        let mut params = ParamValues::default();
        params.insert("built_resolution".into(), ParamValue::Float(built_resolution));
        params.insert("built_domain_size".into(), ParamValue::Float(4.0));
        params
    }

    fn geometry(resolution: f32, fill: f32, volume: Option<Transform>) -> Result<GpuFlipGeometry, String> {
        let read = |name: &str, default: f32| match name {
            "resolution" => resolution,
            "fill_height" => fill,
            _ => default,
        };
        gpu_flip_geometry(read, &params(resolution.round()), None, volume)
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
        assert!(refused(geometry(63.0, 0.16, None)).contains("Resolution"));
        assert!(refused(geometry(64.0, 4.0, None)).contains("Initial Fill Height"));
        let turned = Transform { pos: [0.0, 1.0, 0.0], scale: [1.0; 3], rot_euler: [0.0, 0.3, 0.0], ..Transform::default() };
        assert!(refused(geometry(64.0, 0.16, Some(turned))).contains("initial volume"));
        // 256³ with a 2.5 m pool is 8 · 256² · 160 particles, past 2^24.
        let over = refused(geometry(256.0, 2.5, None));
        assert!(over.contains("Resolution") && over.contains("Initial Fill Height"), "{over}");
        // Any lattice but the built one.
        let read = |name: &str, default: f32| if name == "resolution" { 32.0 } else { default };
        let other = refused(gpu_flip_geometry(read, &params(64.0), None, None));
        assert!(other.contains("Resolution") && other.contains("Domain Size"), "{other}");
    }
}
