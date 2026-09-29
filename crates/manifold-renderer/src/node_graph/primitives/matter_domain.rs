//! `node.matter_domain` — the scene-facing CPU bridge of a matter domain
//! (`docs/GPU_MPM_SOLVER_DESIGN.md` D17): it speaks `node.fluid_surface`'s
//! scene contract (names, types, meanings) and turns it into the lattice,
//! fill boxes, the fixed-tick clock (D8) and the per-tick substep count and
//! material dials (D3, D4) the matter atoms read as wires. P1 carries the
//! domain, walls, box fill, clock, gravity, speed, reset, seed and the water
//! dials; roles, fields, impulses and coupling join in later phases.
//! Exempt from the codegen mandate as a CPU bridge (ADDING_PRIMITIVES.md
//! exclusion 3).

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::fluid::{TICK, domain_layout};
use crate::node_graph::matter::{
    MAX_SUBSTEPS, MatterClock, MatterLattice, WATER_DENSITY, free_fall_speed, momentum_unit,
    stiffness_fitting_cap, substeps_per_tick, water_lambda, wave_speed,
};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Everything whose change restarts the simulation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatterSetup {
    lattice: MatterLattice,
    closed_faces: u32,
    pool_cells: u32,
    column: [[u32; 2]; 3],
    points_per_cell: u32,
    seed: u32,
}

crate::primitive! {
    name: MatterDomain,
    type_id: "node.matter_domain",
    purpose: "Define a live GPU liquid domain with node.fluid_surface's scene contract: the axis-aligned domain box, resolution, six closed faces, initial fill height and box, gravity, simulation speed, reset and seed, plus the matter dials Points per Cell, Stiffness, Cohesion and Liveliness. Outputs the lattice, fill boxes, this frame's fixed 60 Hz ticks and substeps per tick, the epoch and the display clock for the Live Matter atoms.",
    inputs: {
        domain: Transform optional,
        initial_volume: Transform optional,
        resolution: ScalarF32 optional,
        domain_size: ScalarF32 optional,
        fill_height: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        speed: ScalarF32 optional,
        reset: ScalarF32 optional,
        stiffness: ScalarF32 optional,
        cohesion: ScalarF32 optional,
        liveliness: ScalarF32 optional,
    },
    outputs: {
        lattice_min_x: ScalarF32, lattice_min_y: ScalarF32, lattice_min_z: ScalarF32,
        cell_size: ScalarF32,
        nodes_x: ScalarF32, nodes_y: ScalarF32, nodes_z: ScalarF32,
        closed_faces: ScalarF32,
        gravity_x: ScalarF32, gravity: ScalarF32, gravity_z: ScalarF32,
        pool_cells: ScalarF32,
        column_x0: ScalarF32, column_x1: ScalarF32,
        column_y0: ScalarF32, column_y1: ScalarF32,
        column_z0: ScalarF32, column_z1: ScalarF32,
        points_per_cell: ScalarF32,
        fill_seed: ScalarF32,
        ticks: ScalarF32,
        substeps_per_tick: ScalarF32,
        epoch: ScalarF32,
        simulation_time: ScalarF32,
        display_time: ScalarF32,
        dropped_seconds: ScalarF32,
        lambda: ScalarF32,
        cohesion: ScalarF32,
        liveliness: ScalarF32,
        density: ScalarF32,
        limited_by_substeps: ScalarF32,
        blocks_x: ScalarF32, blocks_y: ScalarF32, blocks_z: ScalarF32,
        sort_center_x: ScalarF32, sort_center_y: ScalarF32, sort_center_z: ScalarF32,
        sort_size_x: ScalarF32, sort_size_y: ScalarF32, sort_size_z: ScalarF32,
        momentum_unit: ScalarF32,
    },
    params: [
        ParamDef { name: Cow::Borrowed("seed"), label: "Seed", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16777215.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("resolution"), label: "Resolution", ty: ParamType::Int, default: ParamValue::Float(64.0), range: Some((8.0, 512.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("grid_budget_mcells"), label: "Grid Budget (M cells)", ty: ParamType::Float, default: ParamValue::Float(8.0), range: Some((0.01, 512.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("domain_size"), label: "Domain Size", ty: ParamType::Float, default: ParamValue::Float(4.0), range: Some((0.5, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_x"), label: "Closed −X", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_x"), label: "Closed +X", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_y"), label: "Closed Bottom", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_y"), label: "Closed Top", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_z"), label: "Closed −Z", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_z"), label: "Closed +Z", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fill_height"), label: "Initial Fill Height", ty: ParamType::Float, default: ParamValue::Float(0.4), range: Some((0.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_x"), label: "Gravity X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity"), label: "Gravity Y", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_z"), label: "Gravity Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("speed"), label: "Simulation Speed", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("reset"), label: "Reset", ty: ParamType::Trigger, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("points_per_cell"), label: "Points per Cell", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 1.0)), enum_values: &["8", "27"] },
        ParamDef { name: Cow::Borrowed("stiffness"), label: "Stiffness", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.5, 3.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cohesion"), label: "Cohesion", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("liveliness"), label: "Liveliness", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "The Live Matter group's source of truth: wire its lattice, fill, clock and dial outputs into node.matter_fill, node.matter_state, the region body atoms and node.matter_frame. The domain box, resolution, faces, fill, Points per Cell and Seed restart the simulation; gravity, Simulation Speed, Stiffness, Cohesion and Liveliness are live. Stiffness sets how springy the water is and costs substeps (Stiffness 0.5 → 21, 1 → 34, 2 → 61 at 64³ in 4 m); a value that would need more than 128 runs at the largest that fits and reports it on limited_by_substeps. Live runs at most three ticks per display frame and reports dropped time; export runs every tick.",
    examples: ["WaterDamBreakMatter", "WaterStillPoolMatter"],
    picker: { label: "Matter Domain", category: Atom },
    summary: "Sets up a live GPU liquid: its box, resolution, walls, starting fill, gravity and how the water behaves.",
    category: Particles3D,
    role: Source,
    aliases: ["matter", "mpm", "live water", "gpu liquid", "liquid domain"],
    boundary_reason: NonGpu,
    extra_fields: {
        clock: MatterClock = MatterClock::default(),
        setup: Option<MatterSetup> = None,
        limited: bool = false,
        published: Option<[f32; OUTPUTS.len()]> = None,
    },
}

fn closed_faces(ctx: &EffectNodeContext<'_, '_>) -> u32 {
    ["closed_neg_x", "closed_pos_x", "closed_neg_y", "closed_pos_y", "closed_neg_z", "closed_pos_z"]
        .iter()
        .enumerate()
        .fold(0, |mask, (bit, name)| {
            let closed = !matches!(ctx.params.get(*name), Some(ParamValue::Bool(false)));
            mask | (u32::from(closed) << bit)
        })
}

/// Every scalar output, in the order [`MatterDomain::compute`] fills them.
const OUTPUTS: [&str; 41] = [
    "lattice_min_x", "lattice_min_y", "lattice_min_z", "cell_size", "nodes_x", "nodes_y",
    "nodes_z", "closed_faces", "gravity_x", "gravity", "gravity_z", "pool_cells", "column_x0",
    "column_x1", "column_y0", "column_y1", "column_z0", "column_z1", "points_per_cell",
    "fill_seed", "ticks", "substeps_per_tick", "epoch", "simulation_time", "display_time",
    "dropped_seconds", "lambda", "cohesion", "liveliness", "density", "limited_by_substeps",
    "blocks_x", "blocks_y", "blocks_z", "sort_center_x", "sort_center_y", "sort_center_z",
    "sort_size_x", "sort_size_y", "sort_size_z", "momentum_unit",
];
const TICKS: usize = 20;

impl Primitive for MatterDomain {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // A physics sample reads authored inputs only; it never advances time.
        if crate::node_graph::physics::authored_sample_only() {
            return;
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

impl MatterDomain {
    fn compute(&mut self, ctx: &EffectNodeContext<'_, '_>) -> Result<[f32; OUTPUTS.len()], String> {
        let resolution = ctx.scalar_or_param("resolution", 64.0).round().max(0.0) as u32;
        let domain_size = ctx.scalar_or_param("domain_size", 4.0);
        let layout = domain_layout(ctx.inputs.transform("domain"), domain_size, resolution)?;
        let lattice = MatterLattice::from_layout(&layout);
        let blocks = lattice.blocks();
        let (sort_centre, sort_size) = lattice.cell_sort_box();
        let budget = ctx.param_f32("grid_budget_mcells", 8.0);
        let nodes = f64::from(lattice.node_count());
        if !budget.is_finite() || budget <= 0.0 || nodes > f64::from(budget) * 1e6 {
            return Err(format!(
                "Matter lattice needs {:.3} million nodes; Grid Budget is {budget:.3} million. Increase Grid Budget or lower Resolution. GPU time grows with node count.",
                nodes / 1e6
            ));
        }
        let dx = f64::from(lattice.cell_size);
        let fill_height = ctx.scalar_or_param("fill_height", 0.4);
        if !fill_height.is_finite() || fill_height < 0.0 || fill_height >= layout.size[1] {
            return Err("Matter: fill height must lie within the domain height".into());
        }
        let pool_cells = ((f64::from(fill_height) / dx).round() as u32).min(lattice.cells[1]);
        let mut column = [[0u32; 2]; 3];
        if let Some(volume) = ctx.inputs.transform("initial_volume") {
            if volume.billboard || volume.rot_euler.iter().any(|v| !v.is_finite() || v.abs() > 1e-6) {
                return Err("Matter: the initial volume must be an axis-aligned box; rotation and billboarding are not supported".into());
            }
            if volume.pos.iter().chain(&volume.scale).any(|v| !v.is_finite())
                || volume.scale.iter().any(|v| *v <= 0.0)
                || (0..3).any(|d| {
                    let half = volume.scale[d] * 0.5;
                    volume.pos[d] - half < layout.min[d] - 1e-4
                        || volume.pos[d] + half > layout.min[d] + layout.size[d] + 1e-4
                })
            {
                return Err("Matter: the initial volume must be a finite box fully inside the domain".into());
            }
            for (d, range) in column.iter_mut().enumerate() {
                let cell = |x: f32| (((f64::from(x) - f64::from(layout.min[d])) / dx).round().max(0.0) as u32).min(lattice.cells[d]);
                *range = [
                    cell(volume.pos[d] - volume.scale[d] * 0.5),
                    cell(volume.pos[d] + volume.scale[d] * 0.5),
                ];
            }
        }
        let points_per_cell = match ctx.params.get("points_per_cell") {
            Some(ParamValue::Enum(1)) => 27,
            _ => 8,
        };
        let seed = ctx.param_f32("seed", 0.0).round().clamp(0.0, 16_777_215.0) as u32;
        let faces = closed_faces(ctx);
        let setup = MatterSetup {
            lattice,
            closed_faces: faces,
            pool_cells,
            column,
            points_per_cell,
            seed,
        };
        let speed = ctx.scalar_or_param("speed", 1.0);
        let live = ["gravity_x", "gravity", "gravity_z", "stiffness", "cohesion", "liveliness"]
            .map(|name| ctx.scalar_or_param(name, 0.0));
        if !speed.is_finite() || !(0.0..=4.0).contains(&speed) || live.iter().any(|v| !v.is_finite()) {
            return Err("Matter: Simulation Speed must be between 0 and 4, and gravity and the dials must be finite".into());
        }
        let setup_changed = self.setup != Some(setup);
        self.setup = Some(setup);
        let frame = self.clock.advance(
            ctx.time.seconds.0,
            ctx.time.delta.0,
            speed,
            ctx.scalar_or_param("reset", 0.0),
            setup_changed,
            crate::node_graph::physics::offline_simulation(),
        );

        // D4: substeps from the stiffness/CFL rule, from parameters only.
        let longest = f64::from(layout.size.iter().copied().fold(0.0f32, f32::max));
        let unit_wave = wave_speed(water_lambda(longest, 1.0), f64::from(WATER_DENSITY));
        let v_est = free_fall_speed(f64::from(layout.size[1]));
        let stiffness = f64::from(ctx.scalar_or_param("stiffness", 1.0).clamp(0.5, 3.0));
        let fitted = stiffness.min(stiffness_fitting_cap(dx, unit_wave, v_est));
        let limited = fitted < stiffness;
        if limited != self.limited {
            self.limited = limited;
            if limited {
                log::warn!(
                    "[matter] Stiffness {stiffness:.2} needs more than {MAX_SUBSTEPS} substeps at this resolution; running at {fitted:.2}"
                );
            }
        }
        let substeps = substeps_per_tick(lattice.cell_size, (unit_wave * fitted) as f32, v_est as f32, None, None)
            .min(MAX_SUBSTEPS);
        let lambda = water_lambda(longest, fitted);

        Ok([
            lattice.min[0],
            lattice.min[1],
            lattice.min[2],
            lattice.cell_size,
            lattice.nodes[0] as f32,
            lattice.nodes[1] as f32,
            lattice.nodes[2] as f32,
            faces as f32,
            ctx.scalar_or_param("gravity_x", 0.0),
            ctx.scalar_or_param("gravity", -9.81),
            ctx.scalar_or_param("gravity_z", 0.0),
            pool_cells as f32,
            column[0][0] as f32,
            column[0][1] as f32,
            column[1][0] as f32,
            column[1][1] as f32,
            column[2][0] as f32,
            column[2][1] as f32,
            points_per_cell as f32,
            seed as f32,
            frame.ticks as f32,
            substeps as f32,
            frame.epoch as f32,
            frame.simulation_time as f32,
            frame.display_time as f32,
            frame.dropped_seconds as f32,
            lambda as f32,
            ctx.scalar_or_param("cohesion", 0.0).clamp(0.0, 1.0),
            ctx.scalar_or_param("liveliness", 0.0).clamp(0.0, 1.0),
            WATER_DENSITY,
            if limited { fitted as f32 } else { 0.0 },
            blocks[0] as f32,
            blocks[1] as f32,
            blocks[2] as f32,
            sort_centre[0],
            sort_centre[1],
            sort_centre[2],
            sort_size[0],
            sort_size[1],
            sort_size[2],
            momentum_unit(lattice.cell_size, TICK / f64::from(substeps)),
        ])
    }
}
