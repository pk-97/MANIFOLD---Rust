//! `node.matter_domain` — the scene-facing CPU bridge of a matter domain
//!It speaks the shared liquid
//! scene contract (names, types, meanings) and turns it into the grid box,
//! fill boxes, the fixed-tick clock (D8) and the per-tick substep count and
//! material dials (D3, D4) the matter atoms read as wires. P1 carries the
//! domain, walls, box fill, clock, gravity, speed, reset, seed and the water
//! dials; P2a coupled bodies, shapes and a distance atlas (D11);
//! P2b the scene's Box3D bodies as two-way coupled bodies (section 5): the
//! domain owns the rigid world in-thread and steps it one settled fluid tick
//! at a time. Scene forces and impulses reach the liquid through the shared
//! field lattices of `liquid::fields` (seam P8).
//! Exempt from the codegen mandate as a CPU bridge (ADDING_PRIMITIVES.md
//! exclusion 3).

use manifold_gpu::BufferUse;
use std::borrow::Cow;

use manifold_gpu::{FrameClock, GpuBuffer};
use manifold_nodes_physics::force_lattice::felt_field;
use manifold_physics::FieldValue;
use manifold_physics::force_lattice::ForceLattice;

use crate::matter::coupling::{ReactionScale, decode, live_body_limit, taken_body_limit};
use crate::matter::{
    ACOUSTIC_CFL, MAX_MATTER_SLOTS, MAX_SUBSTEPS, MatterMaterial, RADIUS_SCALE, REACTION_WORDS,
    SAND_DENSITY, SAND_FRICTION_ANGLE, SNOW_HARDENING_MAX, field_min, free_fall_speed,
    matter_nodes, matter_pool_ceiling, matter_pool_tiles, momentum_unit, stiffness_room,
    substeps_for_interval, substeps_per_tick, viscous_limit,
};
use manifold_core::fluid_domain::MAX_LIQUID_BODIES;
use manifold_core::fluid_domain::{
    CELL_SIZE_METRES, DEFAULT_CELL_SIZE, FluidDomainLayout, domain_layout, unbounded_seed,
};
use manifold_core::liquid_setup::{
    ColumnOutcome, DEFAULT_FILL_HEIGHT, FillFit, SetupFit, fit_column, fit_fill_height,
};
use manifold_core::physics_gizmo::{PhysicsGizmo, PhysicsGizmoFamily};
use manifold_core::scene_impulse::RigidImpulseTargets;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::exec::substeps::{SubstepClockOutput, SubstepInterval};
use manifold_node_engine::mesh::Vec4Vertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::ports::EXACT_F32_COUNT;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::transform::Transform;
use manifold_nodes_physics::tick_sampled::{TickSampled, TickSampledRegistration};
use manifold_physics::clock::TICK;
use manifold_physics::tick_samples::TickSamples;
use manifold_water_liquid::bodies::{BodiesStatus, LiquidBodies, LiquidBody, LiquidShape};
use manifold_water_liquid::body_buffers::LiquidBodyBuffers;
use manifold_water_liquid::bounds::{
    NO_KILL_HEIGHT, SourceFloor, bounds_enabled, bounds_outputs, box_about, closed_faces,
    face_masks, kill_height, lowest_source, on_world_cells, union_box, wall_velocity_outputs,
    within_key_span,
};
use manifold_water_liquid::clock::LiquidClock;
use manifold_water_liquid::coupling::{
    DomainWalls, LiquidRigidOwner, ObservedPush, PendingTick, SharedSolids, SharesSolids,
    takes_reaction,
};
use manifold_water_liquid::fluid_role::{FluidRole, FluidRoleKind, InitialFill, take_initial_fills};
use manifold_water_liquid::gizmos::SetupReport;
use manifold_water_rigid::coupled_frame::{CoupledRigidInputs, RigidFrame, RigidPresentation};
use manifold_water_rigid::node::{PhysicsNode, PhysicsNodeRegistration};
use manifold_water_rigid::physics::{RigidSceneInputs, RigidSceneObservation};
use manifold_water_rigid::physics_events::ResolvedNodeImpulse;
use {
    manifold_water_liquid::fields, manifold_water_liquid::fields::LiquidFields,
    manifold_water_liquid::fields::LiquidImpulses,
};

/// Everything whose change restarts the simulation. The Bounds box's
/// position, size and faces are not in it: the grid lives on world tiles, so
/// moving or resizing the box keeps the water.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatterSetup {
    pub(crate) cell_size: f64,
    pub(crate) bounds: bool,
    pub(crate) fill_height: f32,
    /// Each Matter item's Initial Fill box as placed (min then max), by
    /// slot.
    pub(crate) fills: [Option<[[f32; 3]; 2]>; MAX_MATTER_SLOTS],
    pub(crate) points_per_cell: u32,
    pub(crate) seed: u32,
}

/// The domain's setup and the layout it came from, computed from params and
/// wires alone: the node and the extent checker both call
/// [`matter_geometry`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MatterGeometry {
    /// The grid box of this frame, moved onto whole world cells.
    pub(crate) layout: FluidDomainLayout,
    /// The Water item's box exactly as placed (min then max): the Bounds box.
    pub(crate) bounds_box: [[f32; 3]; 2],
    pub(crate) setup: MatterSetup,
    /// The pool's height in cells and each slot's Initial Fill cell range
    /// per axis in this frame's grid box (empty without one).
    pub(crate) pool_cells: u32,
    pub(crate) columns: [[[u32; 2]; 3]; MAX_MATTER_SLOTS],
    /// The points the pool and every fill seed.
    pub(crate) points: u32,
    /// The first slot whose fill did not fit the domain, as `fit` reports.
    pub(crate) misfit: Option<usize>,
    /// Tiles of the grid's world tile pool.
    pub(crate) pool_tiles: u32,
    /// What the fill changed to fit the domain, on the values this frame ran.
    pub(crate) fit: SetupFit,
}

impl MatterGeometry {
    /// The scalar outputs fixed by this frame's geometry, by name.
    pub(crate) fn outputs(&self) -> [(&'static str, f32); 32] {
        let MatterGeometry {
            layout,
            pool_cells,
            points,
            pool_tiles,
            setup,
            ..
        } = *self;
        let field = field_min(&layout);
        let MatterSetup {
            points_per_cell,
            seed,
            ..
        } = setup;
        let ranges = self.slot_ranges();
        [
            ("fill_min_x", layout.min[0]),
            ("fill_min_y", layout.min[1]),
            ("fill_min_z", layout.min[2]),
            ("cell_size", self.cell_size()),
            ("fill_cells_x", layout.cells[0] as f32),
            ("fill_cells_y", layout.cells[1] as f32),
            ("fill_cells_z", layout.cells[2] as f32),
            ("field_min_x", field[0]),
            ("field_min_y", field[1]),
            ("field_min_z", field[2]),
            ("pool_tiles", pool_tiles as f32),
            // Bounds holds the material in the box the pool covers; without
            // it the pool grows as far as the budget buys.
            (
                "pool_ceiling",
                if setup.bounds {
                    pool_tiles
                } else {
                    matter_pool_ceiling().max(pool_tiles)
                } as f32,
            ),
            ("pool_cells", pool_cells as f32),
            ("fill_points", points as f32),
            ("points_per_cell", points_per_cell as f32),
            ("fill_seed", seed as f32),
            ("first_0", ranges[0][0] as f32),
            ("first_1", ranges[1][0] as f32),
            ("first_2", ranges[2][0] as f32),
            ("first_3", ranges[3][0] as f32),
            ("first_4", ranges[4][0] as f32),
            ("first_5", ranges[5][0] as f32),
            ("first_6", ranges[6][0] as f32),
            ("first_7", ranges[7][0] as f32),
            ("points_0", ranges[0][1] as f32),
            ("points_1", ranges[1][1] as f32),
            ("points_2", ranges[2][1] as f32),
            ("points_3", ranges[3][1] as f32),
            ("points_4", ranges[4][1] as f32),
            ("points_5", ranges[5][1] as f32),
            ("points_6", ranges[6][1] as f32),
            ("points_7", ranges[7][1] as f32),
        ]
    }

    /// Each slot's first point and count: the points never move in the
    /// array, so an item's are the run its fill seeded, the pool counted as
    /// slot 0's.
    pub(crate) fn slot_ranges(&self) -> [[u32; 2]; MAX_MATTER_SLOTS] {
        let ppc = self.setup.points_per_cell;
        let rows = fill_table(
            self.layout.cells,
            self.pool_cells,
            &self.columns,
            [-1; MAX_MATTER_SLOTS],
            ppc,
        );
        std::array::from_fn(|slot| {
            let count = box_points(&rows[slot], ppc);
            if slot == 0 {
                [0, rows[0][7] + count]
            } else {
                [rows[slot][7], count]
            }
        })
    }

    /// The cell size as every Matter atom reads it.
    pub(crate) fn cell_size(&self) -> f32 {
        self.layout.cell_size as f32
    }

    /// The force lattice over the box and its 3-cell margin.
    pub(crate) fn force_lattice(&self) -> ForceLattice {
        ForceLattice::covering(
            field_min(&self.layout),
            self.cell_size(),
            matter_nodes(self.layout.cells),
        )
    }

    /// The rigid world's walls for the Wall faces `walls`: slabs round the
    /// Bounds box, and none with Bounds off.
    pub(crate) fn domain_walls(&self, walls: u32) -> DomainWalls {
        if self.setup.bounds {
            DomainWalls::new(self.bounds_box[0], self.bounds_box[1], walls)
        } else {
            DomainWalls::default()
        }
    }
}

/// The fill in the layout's cells: the pool's height in cells and each
/// slot's Initial Fill cell range per axis (empty without one), with the
/// first slot whose fill did not fit. A height past the top is capped there
/// and a volume past a wall is clipped to the domain, one wholly outside
/// fills nothing (`manifold_core::liquid_setup`); an unusable height or a
/// volume that is not an axis-aligned finite box is refused by name.
pub(crate) fn fill_region(
    layout: &FluidDomainLayout,
    fill_height: f32,
    fills: &[Option<Transform>; MAX_MATTER_SLOTS],
) -> Result<FilledCells, String> {
    // The f32 cell size, as every matter atom reads it.
    let dx = f64::from(layout.cell_size as f32);
    let filled =
        fit_fill_height(layout, fill_height).map_err(|reason| format!("Matter: {reason}"))?;
    let mut fit = SetupFit {
        fill_capped: matches!(filled, FillFit::Capped(_)),
        column: ColumnOutcome::Fits,
        pool_full: false,
        tile_removed: 0,
    };
    let fill_height = filled.height();
    let pool_cells = ((f64::from(fill_height) / dx).round() as u32).min(layout.cells[1]);
    let mut columns = [[[0u32; 2]; 3]; MAX_MATTER_SLOTS];
    let mut misfit = None;
    for (slot, (volume, column)) in fills.iter().zip(&mut columns).enumerate() {
        let Some(volume) = volume else {
            continue;
        };
        if volume.billboard
            || volume
                .rot_euler
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 1e-6)
        {
            return Err("Matter: the Initial Fill must be an axis-aligned box; rotation and billboarding are not supported".into());
        }
        let (edges, outcome) = fit_column(layout, volume.pos, volume.scale)
            .map_err(|reason| format!("Matter: {reason}"))?;
        if outcome != ColumnOutcome::Fits && misfit.is_none() {
            fit.column = outcome;
            misfit = Some(slot);
        }
        if let Some((lo, hi)) = edges {
            for (d, range) in column.iter_mut().enumerate() {
                // The f32 edges, as every matter atom reads them.
                let cell = |x: f64| {
                    (((f64::from(x as f32) - f64::from(layout.min[d])) / dx)
                        .round()
                        .max(0.0) as u32)
                        .min(layout.cells[d])
                };
                *range = [cell(lo[d]), cell(hi[d])];
            }
        }
    }
    Ok(FilledCells {
        pool_cells,
        columns,
        fit,
        misfit,
    })
}

/// What [`fill_region`] fits in a layout's cells.
pub(crate) struct FilledCells {
    pub(crate) pool_cells: u32,
    pub(crate) columns: [[[u32; 2]; 3]; MAX_MATTER_SLOTS],
    pub(crate) fit: SetupFit,
    pub(crate) misfit: Option<usize>,
}

/// Words of one row of the fill table node.matter_fill reads.
pub(crate) const FILL_ROW_WORDS: usize = 8;

/// The fill table, one row a slot: its box's cells (x0, x1, y0, y1, z0, z1,
/// the box above the pool and inside the grid), the shaped fill's region row
/// (i32 bits, -1 for a box) and the first point the row seeds. The pool's
/// points come first, slot 0's, then each slot's box in slot order.
pub(crate) fn fill_table(
    cells: [u32; 3],
    pool: u32,
    columns: &[[[u32; 2]; 3]; MAX_MATTER_SLOTS],
    regions: [i32; MAX_MATTER_SLOTS],
    points_per_cell: u32,
) -> [[u32; FILL_ROW_WORDS]; MAX_MATTER_SLOTS] {
    let pool = pool.min(cells[1]);
    let mut start = cells[0] * pool * cells[2] * points_per_cell;
    std::array::from_fn(|slot| {
        let clip = |d: usize, lo: u32| {
            let hi = columns[slot][d][1].min(cells[d]);
            [lo.min(hi), hi]
        };
        let [x, z] = [0, 2].map(|d| clip(d, columns[slot][d][0]));
        let y = clip(1, columns[slot][1][0].max(pool));
        let row = [
            x[0],
            x[1],
            y[0],
            y[1],
            z[0],
            z[1],
            regions[slot] as u32,
            start,
        ];
        start += box_points(&row, points_per_cell);
        row
    })
}

/// The points a fill-table row's box seeds.
pub(crate) fn box_points(row: &[u32; FILL_ROW_WORDS], points_per_cell: u32) -> u32 {
    (row[1] - row[0]) * (row[3] - row[2]) * (row[5] - row[4]) * points_per_cell
}

/// Points the pool and every slot's box seed at `points_per_cell`, refused
/// by name past the count the `count` wire carries exactly.
pub(crate) fn fill_points(
    cells: [u32; 3],
    pool: u32,
    columns: &[[[u32; 2]; 3]; MAX_MATTER_SLOTS],
    points_per_cell: u32,
) -> Result<u32, String> {
    let pool = u64::from(pool.min(cells[1]));
    let mut count = u64::from(cells[0]) * pool * u64::from(cells[2]);
    for column in columns {
        let y0 = column[1][0].max(pool as u32);
        let extent = |lo: u32, hi: u32| u64::from(hi.saturating_sub(lo));
        count += extent(column[0][0], column[0][1])
            * extent(y0, column[1][1])
            * extent(column[2][0], column[2][1]);
    }
    let count = count * u64::from(points_per_cell);
    match u32::try_from(count) {
        Ok(count) if count <= EXACT_F32_COUNT => Ok(count),
        _ => Err(format!(
            "Matter fill: {count} points exceeds the exact f32 range of the count wire ({EXACT_F32_COUNT}). Raise Cell Size, or lower Particles per Cell or the fill."
        )),
    }
}

/// The role ports each slot's sources take: the `roles_per_slot` param, or
/// every one of `roles` when it is 0.
pub(crate) fn roles_per_slot(params: &ParamValues, roles: usize) -> usize {
    match params.get("roles_per_slot") {
        Some(ParamValue::Float(value)) if *value >= 1.0 => value.round() as usize,
        _ => roles.max(1),
    }
}

/// Each Matter item's Initial Fill, by slot: the role ports in runs of the
/// `roles_per_slot` param, slot k's run from port k·roles_per_slot (every
/// port slot 0's when it is 0). A slot takes one Initial Fill
/// (Water 0's of [`take_initial_fills`]); a shaped one keeps its port, where its region
/// row is.
pub(crate) fn take_slot_fills(
    roles: &mut [Option<FluidRole>],
    params: &ParamValues,
) -> Result<[Option<InitialFill>; MAX_MATTER_SLOTS], String> {
    let per_slot = roles_per_slot(params, roles.len());
    let mut fills = [None; MAX_MATTER_SLOTS];
    for (slot, run) in roles.chunks_mut(per_slot).enumerate() {
        let Some(fill) = take_initial_fills(run)?[0] else {
            continue;
        };
        let Some(kept) = fills.get_mut(slot) else {
            return Err(format!(
                "one Matter solver holds {MAX_MATTER_SLOTS} Sand items"
            ));
        };
        *kept = Some(InitialFill {
            shape: fill.shape.map(|port| slot * per_slot + port),
            ..fill
        });
    }
    Ok(fills)
}

/// The domain box, pool and fill from the domain's params and
/// wires (`read` is `scalar_or_param`; the Bounds switch and the enums come
/// from `params`), the grid box moved onto whole world cells and the Bounds
/// box as placed. With Bounds off the grid starts from the box round every
/// slot's Fill,
/// else the hidden one ([`unbounded_seed`]) and Fill Height, which fills the
/// Bounds box from its floor, is 0. Refused by name when the box holds more
/// tiles than a World counts or the fill is not a usable shape.
pub(crate) fn matter_geometry(
    read: impl Fn(&str, f32) -> f32,
    params: &ParamValues,
    domain: Option<Transform>,
    fills: &[Option<Transform>; MAX_MATTER_SLOTS],
) -> Result<MatterGeometry, String> {
    let bounds = bounds_enabled(params);
    let domain_size = read("domain_size", 4.0);
    let world = domain.map(Transform::world_box);
    let cell_size = f64::from(read("cell_metres", DEFAULT_CELL_SIZE));
    let seed = match bounds {
        true => world.map(Into::into),
        false => Some(unbounded_seed(
            world.map(Into::into),
            domain_size,
            fills
                .iter()
                .flatten()
                .map(|volume| box_about(volume.pos, volume.scale))
                .reduce(|a, b| {
                    [
                        std::array::from_fn(|i| a[0][i].min(b[0][i])),
                        std::array::from_fn(|i| a[1][i].max(b[1][i])),
                    ]
                }),
            cell_size,
        )),
    };
    let layout = on_world_cells(domain_layout(seed, domain_size, cell_size)?);
    let bounds_box = match world {
        Some(pose) => box_about(pose.pos, pose.scale),
        // No Water item: the cube `domain_layout` places, centred in X/Z with
        // its floor at Y = 0.
        None => box_about([0.0, 0.5 * domain_size, 0.0], [domain_size; 3]),
    };
    let nodes = matter_nodes(layout.cells);
    let fill_height = if bounds {
        read("fill_height", DEFAULT_FILL_HEIGHT)
    } else {
        0.0
    };
    let FilledCells {
        pool_cells,
        columns,
        fit,
        misfit,
    } = fill_region(&layout, fill_height, fills)?;
    let points_per_cell = match params.get("points_per_cell") {
        Some(ParamValue::Enum(1)) => 27,
        _ => 8,
    };
    let seed = match params.get("seed") {
        Some(ParamValue::Float(seed)) => *seed,
        _ => 0.0,
    };
    let setup = MatterSetup {
        cell_size: layout.cell_size,
        bounds,
        fill_height,
        fills: fills.map(|fill| fill.map(|volume| box_about(volume.pos, volume.scale))),
        points_per_cell,
        seed: seed.round().clamp(0.0, 16_777_215.0) as u32,
    };
    let pool_tiles = matter_pool_tiles(nodes)?;
    let points = fill_points(layout.cells, pool_cells, &columns, points_per_cell)?;
    Ok(MatterGeometry {
        layout,
        bounds_box,
        setup,
        pool_cells,
        columns,
        points,
        misfit,
        pool_tiles,
        fit,
    })
}

manifold_node_engine::primitive! {
    name: MatterDomain,
    type_id: "node.matter_domain",
    purpose: "Define a live GPU liquid domain with the shared liquid scene contract: the axis-aligned domain box, cell size, six closed faces, initial fill height and the box an Initial Fill role places, gravity, simulation speed, reset and seed, plus the sand dials Points per Cell, Stiffness, Friction Angle, Cohesion (kPa) and Density; its Sources param sets how many role inputs (role_0, role_1, ...) it takes. Outputs the grid box's first corner and cell counts, the first node of its margin, the fill boxes, this frame's fixed 60 Hz ticks and substeps per tick, the epoch and the display clock for the Live Matter atoms. Paired with a physics world in the same scene, it owns that world and couples its bodies both ways: they join the body rows, the liquid's push on them comes back through the reaction array, and the world steps once per fluid tick after the GPU has finished it.",
    inputs: {
        domain: Transform optional,
        cell_metres: ScalarF32 optional,
        domain_size: ScalarF32 optional,
        fill_height: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        speed: ScalarF32 optional,
        air_drag: ScalarF32 optional,
        air_resistance: ScalarF32 optional,
        reset: ScalarF32 optional,
        // The material chain's folds (node.matter_material): the fastest
        // pressure wave as built, snow's, the fastest damping diffusivity, the
        // heaviest density and the walls' friction.
        wave: ScalarF32 optional,
        snow_wave: ScalarF32 optional,
        diffusivity: ScalarF32 optional,
        density: ScalarF32 optional,
        wall_friction: ScalarF32 optional,
        // The most snow hardening the last read tick held
        // (node.matter_state's `hardened`, a state-capture wire).
        hardened: ScalarF32 optional,
        acceleration_field: VectorField optional,
    },
    outputs: {
        fill_min_x: ScalarF32, fill_min_y: ScalarF32, fill_min_z: ScalarF32,
        cell_size: ScalarF32,
        fill_cells_x: ScalarF32, fill_cells_y: ScalarF32, fill_cells_z: ScalarF32,
        field_min_x: ScalarF32, field_min_y: ScalarF32, field_min_z: ScalarF32,
        closed_faces: ScalarF32,
        gravity_x: ScalarF32, gravity: ScalarF32, gravity_z: ScalarF32,
        pool_cells: ScalarF32,
        // The points the pool and every slot's fill seed, and each slot's
        // first point and count (the pool is slot 0's): a slot's draw
        // publishes its own run.
        fill_points: ScalarF32,
        first_0: ScalarF32, first_1: ScalarF32, first_2: ScalarF32, first_3: ScalarF32,
        first_4: ScalarF32, first_5: ScalarF32, first_6: ScalarF32, first_7: ScalarF32,
        points_0: ScalarF32, points_1: ScalarF32, points_2: ScalarF32, points_3: ScalarF32,
        points_4: ScalarF32, points_5: ScalarF32, points_6: ScalarF32, points_7: ScalarF32,
        points_per_cell: ScalarF32,
        fill_seed: ScalarF32,
        ticks: ScalarF32,
        substeps_per_tick: ScalarF32,
        epoch: ScalarF32,
        simulation_time: ScalarF32,
    interval_duration: ScalarF32,
    target_time: ScalarF32, step_cap_hit: ScalarF32,
        display_time: ScalarF32,
        dropped_seconds: ScalarF32,
        // The distance a substep may carry a wave (the acoustic CFL times the
        // cell) and the speed estimate it is shared with: each point keeps the
        // stiffness the step has room for (`stiffness_room`).
        wave_reach: ScalarF32,
        speed_estimate: ScalarF32,
        wall_friction: ScalarF32,
        limited_by_substeps: ScalarF32,
        pool_tiles: ScalarF32,
        pool_ceiling: ScalarF32,
        open_faces: ScalarF32,
        kill_height: ScalarF32,
        bounds_lo_x: ScalarF32, bounds_lo_y: ScalarF32, bounds_lo_z: ScalarF32,
        bounds_hi_x: ScalarF32, bounds_hi_y: ScalarF32, bounds_hi_z: ScalarF32,
        wall_velocity_lo_x: ScalarF32, wall_velocity_lo_y: ScalarF32, wall_velocity_lo_z: ScalarF32,
        wall_velocity_hi_x: ScalarF32, wall_velocity_hi_y: ScalarF32, wall_velocity_hi_z: ScalarF32,
        momentum_unit: ScalarF32,
        body_count: ScalarF32, body_rows: ScalarF32, first_tick: ScalarF32,
        dynamic_count: ScalarF32,
        static_key: ScalarF32,
        moved_key: ScalarF32,
        moved_lo_x: ScalarF32, moved_lo_y: ScalarF32, moved_lo_z: ScalarF32,
        moved_hi_x: ScalarF32, moved_hi_y: ScalarF32, moved_hi_z: ScalarF32,
        field_nodes_x: ScalarF32, field_nodes_y: ScalarF32, field_nodes_z: ScalarF32,
        field_spacing: ScalarF32,
        force_lattices: ScalarF32,
        impulse_tick: ScalarF32,
        // The fill table (`fill_table`), one row a slot.
        fills: Array(u32),
        bodies: Array(LiquidBody), regions: Array(LiquidBody), shapes: Array(LiquidShape), atlas: Array(u32),
        collider_nodes: Array(Vec4Vertex), collider_triangles: Array(Vec4Vertex),
        reaction: Array(i32),
        forces: Array(f32), impulses: Array(f32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("seed"), label: "Seed", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 16777215.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cell_metres"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(DEFAULT_CELL_SIZE), range: Some(CELL_SIZE_METRES), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("domain_size"), label: "Domain Size", ty: ParamType::Float, default: ParamValue::Float(4.0), range: Some((0.5, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bounds"), label: "Bounds", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_x"), label: "Left Wall", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_x"), label: "Right Wall", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_y"), label: "Floor", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_y"), label: "Ceiling", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_neg_z"), label: "Back Wall", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_pos_z"), label: "Front Wall", ty: ParamType::Bool, default: ParamValue::Bool(true), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("fill_height"), label: "Fill Height", ty: ParamType::Float, default: ParamValue::Float(0.4), range: Some((0.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_x"), label: "Gravity X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity"), label: "Gravity Y", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_z"), label: "Gravity Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("speed"), label: "Speed", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("air_drag"), label: "Air Drag", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 4.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("reset"), label: "Reset", ty: ParamType::Trigger, default: ParamValue::Float(0.0), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("points_per_cell"), label: "Particles per Cell", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 1.0)), enum_values: &["8", "27"] },
        ParamDef { name: Cow::Borrowed("roles"), label: "Sources", ty: ParamType::Int, default: ParamValue::Float(manifold_water_liquid::DEFAULT_ROLE_PORTS as f32), range: None, enum_values: &[] },
        // Every Matter item in a scene shares this solver: slot k's sources
        // are the `roles_per_slot` ports from k·roles_per_slot. 0 is one item,
        // every source its.
        ParamDef { name: Cow::Borrowed("roles_per_slot"), label: "Sources per Item", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, 64.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "The Live Matter group's source of truth: wire its fill, clock and dial outputs into node.matter_fill, node.matter_state, the region body atoms and node.matter_frame. The domain box, cell size, faces, fill, Points per Cell and Seed restart the simulation; gravity, Simulation Speed, Stiffness, Friction Angle, Cohesion and Density are live. Stiffness sets how firm the grains are and costs substeps (Stiffness 0.5 → 14, 1 → 21, 2 → 35 at 64³ in 4 m); the material leaves as mu, lambda, alpha, cohesion (a tensile log strain) and softening for node.grid_to_matter, mu and lambda for node.matter_stats, and wall_friction (tan of the Friction Angle) for node.matter_grid_update and node.matter_body_reaction; a value that would need more than 128 runs at the largest that fits and reports it on limited_by_substeps. Live runs at most two ticks per display frame and reports dropped time; export runs every tick. Bodies, first_tick, body_count and body_rows feed node.matter_move_bodies, and shapes and atlas node.matter_grid_update and node.liquid_solid_distance. Until every body's distance lattice is built the liquid holds. An Initial Fill seeds the water; Inflow and Outflow roles are refused. In a scene with a node.physics_world, the world's bodies selected as colliders couple both ways: wire reaction into node.matter_move_bodies and node.matter_body_reaction (whose reaction_out feeds node.grid_to_matter), and dynamic_count into all three. Live, a coupled domain runs at most two ticks per display frame; live and export wait for the GPU between ticks so Box3D and the liquid exchange once per tick at any frame rate; light bodies raise the substep count, and one too light for 128 substeps is refused by name. Scene forces arrive on acceleration_field and scene impulses through the impulse hooks; both are sampled on a coarse field lattice (a quarter of the cells per axis): wire forces, impulses and the six field scalars into node.matter_grid_update and node.matter_body_reaction. Forces act every substep; an impulse changes the water's velocity once, on the first substep of impulse_tick, the first tick after the frame it was fired. A hit fired while paused or at Simulation Speed 0 is discarded, never replayed on resume; a restart cancels hits not yet applied. Rigid targets of an impulse reach the coupled bodies.",
    examples: ["SandColumn", "SandBoxDrop"],
    picker: { label: "Matter Domain", category: Atom },
    summary: "Sets up a live GPU liquid: its box, cell size, walls, starting fill, gravity and how the water behaves.",
    category: Particles3D,
    role: Source,
    aliases: ["matter", "mpm", "live water", "gpu liquid", "liquid domain"],
    boundary_reason: NonGpu,
    extra_fields: {
        clock: LiquidClock = LiquidClock::default(),
        schedule: Vec<MatterInterval> = Vec::new(),
        setup: Option<MatterSetup> = None,
        // What the setup this frame ran capped or clipped, or why it was
        // refused; it rides the domain snapshot.
        setup_report: SetupReport = SetupReport::default(),
        // The domain box of the accepted setup.
        layout: Option<FluidDomainLayout> = None,
        // The fill's pool height and each slot's Initial Fill range in that
        // box's cells.
        fill_cells: (u32, [[[u32; 2]; 3]; MAX_MATTER_SLOTS]) = (0, [[[0; 2]; 3]; MAX_MATTER_SLOTS]),
        // The fill table as last written and the shared buffer holding it: a
        // new buffer when it changes, so a frame in flight keeps its own.
        fill_rows: [[u32; FILL_ROW_WORDS]; MAX_MATTER_SLOTS] = [[0; FILL_ROW_WORDS]; MAX_MATTER_SLOTS],
        fill_table: Option<([[u32; FILL_ROW_WORDS]; MAX_MATTER_SLOTS], GpuBuffer)> = None,
        limited: bool = false,
        published: Option<[f32; OUTPUTS.len()]> = None,
        // A shaped fill's role is a region row, so the fill reads its shape.
        bodies: LiquidBodies = LiquidBodies::with_regions(),
        // The role inputs, one per source: the `roles` param sizes them.
        role_ports: manifold_water_liquid::RolePorts = manifold_water_liquid::RolePorts::default(),
        // Another water's solids, when this one does not own the world.
        observed: SharedSolids = SharedSolids::default(),
        role_pending: bool = false,
        body_buffers: LiquidBodyBuffers = LiquidBodyBuffers::default(),
        body_rows: f32 = 0.0,
        rows_fresh: bool = false,
        // The static meshes' shapes, trees and rows as last published, the
        // count of their changes node.matter_static_solid rebuilds every
        // held tile on, and the count of moves alone with the box round the
        // last one, which it rebuilds the tiles round.
        static_hash: u64 = 0,
        static_rows: Vec<(usize, LiquidBody)> = Vec::new(),
        static_key: f32 = 0.0,
        moved_key: f32 = 0.0,
        moved_box: [[f32; 3]; 2] = [[0.0; 3]; 2],
        coupled: Coupling = Coupling::default(),
        reaction: Option<GpuBuffer> = None,
        impulses: LiquidImpulses = LiquidImpulses::default(),
        fields: LiquidFields = LiquidFields::default(),
        acceleration: Option<FieldValue> = None,
        // The Bounds box of the last frame with Bounds on: its faces' speed.
        last_bounds: Option<[[f32; 3]; 2]> = None,
        source_floor: SourceFloor = SourceFloor::default(),
    },
}

/// The coupled half of the domain (section 5): the paired physics world's
/// latest observation, and the rigid owner built from it.
#[derive(Default)]
pub struct Coupling {
    mode: bool,
    observation: Option<RigidSceneObservation>,
    /// The paired world's authored scene at each tick's start: Box3D steps a
    /// tick toward the scene at its end, so animated bodies move per tick.
    scenes: TickSamples<RigidSceneInputs>,
    colliders: RigidImpulseTargets,
    error: Option<String>,
    previous_reset: Option<f32>,
    owner: Option<LiquidRigidOwner>,
    /// This frame's domain box and closed faces: the bodies' walls.
    walls: DomainWalls,
    /// The coupled bodies the liquid has body rows for.
    capacity: usize,
    /// This frame's simulation step, handed to the rigid owner.
    step: manifold_water_rigid::physics::SimStep,
    /// How the owner's pending tick's reaction words decode.
    scale: Option<ReactionScale>,
    /// Taking another liquid's solids: how this frame's push on its bodies
    /// decodes, for that owner's next settle.
    push: Option<ReactionScale>,
    /// The owner was built since the clock last restarted: the next frame
    /// restarts the liquid with it.
    owner_fresh: bool,
    epochs: u64,
    /// This frame's compute failed; nothing is published to the pair.
    failed: bool,
    /// Played time of the last frame the pair advanced at.
    transport: Option<manifold_core::PlayedTime>,
    /// This frame runs several coupled ticks, exchanging with Box3D
    /// between them.
    exchange: Option<Exchange>,
    /// A host step failed; the next frame reports it and restarts the pair.
    host_error: Option<String>,
    /// The bodies as drawn: blended between the two newest settled ticks
    /// at the liquid's display time.
    presented: RigidPresentation,
}

/// A frame of several coupled ticks: the region syncs at each later
/// tick's first substep, and the domain settles the tick before it there.
#[derive(Clone, Copy)]
struct Exchange {
    /// The frame's first tick; later ticks keep its fence and mode.
    pending: PendingTick,
    scale: ReactionScale,
}

/// One accepted interval's numerical work and existing clock output scalars.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatterInterval {
    timing: SubstepInterval,
    scalars: [(&'static str, f32); 2],
}

impl MatterInterval {
    fn interval(&self) -> manifold_physics::stepping::StepInterval {
        manifold_physics::stepping::StepInterval::new(self.timing.start, self.timing.end)
    }

    fn contains(&self, iteration: u32) -> bool {
        iteration >= self.timing.first_iteration
            && iteration - self.timing.first_iteration < self.timing.iterations
    }

    fn reaction_scale(&self, previous: ReactionScale) -> ReactionScale {
        ReactionScale {
            unit: self.scalars[1].1,
            ..previous
        }
    }
}

/// Reuse schedule storage; interval CFL counts and quantisation scales do not
/// depend on how export groups transport frames.
fn schedule_intervals(
    schedule: &mut Vec<MatterInterval>,
    frame: &manifold_physics::clock::ClockFrame,
    nominal: u32,
    cell_size: f32,
) -> bool {
    schedule.clear();
    let mut total = 0u32;
    let mut capped = false;
    for ordinal in 0..frame.ticks {
        let interval = frame
            .interval(u64::from(ordinal))
            .expect("accepted Matter interval");
        let requested = substeps_for_interval(interval.duration().0 as f32, nominal);
        let iterations = requested.min(MAX_SUBSTEPS);
        capped |= requested >= MAX_SUBSTEPS;
        schedule.push(MatterInterval {
            timing: SubstepInterval {
                start: interval.start,
                end: interval.end,
                ordinal,
                first_iteration: total,
                iterations,
                total_iterations: 0,
            },
            scalars: [
                ("substeps_per_tick", iterations as f32),
                (
                    "momentum_unit",
                    momentum_unit(cell_size, interval.duration().0 / f64::from(iterations)),
                ),
            ],
        });
        total = total.saturating_add(iterations);
    }
    for record in schedule {
        record.timing.total_iterations = total;
    }
    capped
}

/// The reaction words, once their last GPU writer has retired.
fn reaction_words(buffer: Option<&GpuBuffer>) -> Option<&[i32]> {
    let buffer = buffer?;
    let words = buffer
        .mapped_ptr()
        .expect("matter reaction words must be CPU-visible")
        .cast::<i32>()
        .cast_const();
    // SAFETY: shared, 4-byte aligned storage whose last GPU writer has
    // retired (the caller checked the frame clock or waited on the GPU).
    Some(unsafe { std::slice::from_raw_parts(words, (buffer.size / 4) as usize) })
}

impl Coupling {
    fn reset(&mut self) {
        let (mode, epochs) = (self.mode, self.epochs);
        *self = Self {
            mode,
            epochs,
            ..Self::default()
        };
    }
}

/// Every scalar output, in the order [`MatterDomain::compute`] fills them.
const OUTPUTS: [&str; 82] = [
    "fill_min_x",
    "fill_min_y",
    "fill_min_z",
    "cell_size",
    "fill_cells_x",
    "fill_cells_y",
    "fill_cells_z",
    "field_min_x",
    "field_min_y",
    "field_min_z",
    "gravity_x",
    "gravity",
    "gravity_z",
    "pool_cells",
    "fill_points",
    "points_per_cell",
    "fill_seed",
    "first_0",
    "first_1",
    "first_2",
    "first_3",
    "first_4",
    "first_5",
    "first_6",
    "first_7",
    "points_0",
    "points_1",
    "points_2",
    "points_3",
    "points_4",
    "points_5",
    "points_6",
    "points_7",
    "ticks",
    "substeps_per_tick",
    "epoch",
    "simulation_time",
    "display_time",
    "dropped_seconds",
    "wave_reach",
    "speed_estimate",
    "wall_friction",
    "limited_by_substeps",
    "momentum_unit",
    "body_count",
    "body_rows",
    "first_tick",
    "dynamic_count",
    "field_nodes_x",
    "field_nodes_y",
    "field_nodes_z",
    "field_spacing",
    "force_lattices",
    "impulse_tick",
    "interval_duration",
    "target_time",
    "step_cap_hit",
    "bounds_lo_x",
    "bounds_lo_y",
    "bounds_lo_z",
    "bounds_hi_x",
    "bounds_hi_y",
    "bounds_hi_z",
    "pool_tiles",
    "closed_faces",
    "open_faces",
    "kill_height",
    "wall_velocity_lo_x",
    "wall_velocity_lo_y",
    "wall_velocity_lo_z",
    "wall_velocity_hi_x",
    "wall_velocity_hi_y",
    "wall_velocity_hi_z",
    "static_key",
    "moved_key",
    "moved_lo_x",
    "moved_lo_y",
    "moved_lo_z",
    "moved_hi_x",
    "moved_hi_y",
    "moved_hi_z",
    "pool_ceiling",
];
const TICKS: usize = 33;
const EPOCH: usize = 35;
const IMPULSE_TICK: usize = 53;
const _: () = assert!(matches!(OUTPUTS[TICKS].as_bytes(), b"ticks"));
const _: () = assert!(matches!(OUTPUTS[EPOCH].as_bytes(), b"epoch"));
const _: () = assert!(matches!(OUTPUTS[IMPULSE_TICK].as_bytes(), b"impulse_tick"));

impl Primitive for MatterDomain {
    fn reconfigure(&mut self, params: &manifold_node_engine::exec::effect_node::ParamValues) {
        self.role_ports.configure(
            <Self as manifold_node_engine::primitive::PrimitiveSpec>::INPUTS,
            params,
        );
    }

    fn live_inputs(&self) -> Option<&[manifold_node_engine::ports::NodeInput]> {
        self.role_ports.inputs()
    }

    // The snow packing is last frame's: node.matter_state reads it back from
    // the ticks this domain schedules.
    fn state_capture_input_ports(&self) -> &[&str] {
        &["hardened"]
    }

    /// A liquid follows the Physics World: its gravity and its clock.
    fn world_control_inputs(
        &self,
    ) -> &'static [manifold_core::scene_world_controls::WorldControlInput] {
        &manifold_core::scene_world_controls::WORLD_INPUTS
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(
            port,
            "fills"
                | "bodies"
                | "regions"
                | "shapes"
                | "atlas"
                | "collider_nodes"
                | "collider_triangles"
                | "reaction"
                | "forces"
                | "impulses"
        )
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "reaction" => return self.reaction.as_ref(),
            "fills" => return self.fill_table.as_ref().map(|(_, buffer)| buffer),
            "forces" => return self.fields.forces_buffer(),
            "impulses" => return self.fields.impulses_buffer(),
            _ => {}
        }
        self.body_buffers.output(port)
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "fills" {
            return Some((FILL_ROW_WORDS * MAX_MATTER_SLOTS) as u32);
        }
        // Provided storage: a one-record hint, grown at run time.
        matches!(
            port_name,
            "bodies"
                | "regions"
                | "shapes"
                | "atlas"
                | "collider_nodes"
                | "collider_triangles"
                | "reaction"
                | "forces"
                | "impulses"
        )
        .then_some(1)
    }

    fn warmup_pending(&self) -> bool {
        self.role_pending
    }

    fn substep_clock_interval(&self, iteration: u32) -> Option<SubstepClockOutput<'_>> {
        let record = self
            .schedule
            .iter()
            .find(|record| record.contains(iteration))?;
        Some(SubstepClockOutput {
            duration_port: "interval_duration",
            timing: record.timing,
            scalars: &record.scalars,
        })
    }

    /// Coupled frames sync at each later tick's first substep.
    fn substep_host_sync(&self, iteration: u32) -> bool {
        self.coupled.exchange.is_some()
            && self.schedule.iter().any(|record| {
                record.timing.ordinal > 0 && record.timing.first_iteration == iteration
            })
    }

    /// Section 5 between two ticks of one frame: settle the tick the
    /// GPU just finished, step Box3D over it, and hand the next tick the
    /// bodies' new state and a cleared reaction.
    fn substep_host_step(
        &mut self,
        iteration: u32,
        _gpu: Option<&mut manifold_node_engine::gpu::gpu_encoder::GpuEncoder<'_>>,
    ) -> Result<(), String> {
        let Some(exchange) = self.coupled.exchange else {
            return Ok(());
        };
        let record = *self
            .schedule
            .iter()
            .find(|record| record.contains(iteration))
            .ok_or("Matter coupling: the numerical iteration has no accepted interval")?;
        let result = self.exchange_tick(record, exchange);
        if let Err(error) = &result {
            // The pair restarts next frame with a fresh rigid owner.
            self.coupled.exchange = None;
            self.coupled.owner = None;
            self.coupled.host_error = Some(error.clone());
        }
        result
    }

    // While an input is pending the liquid holds its last good frame.
    fn runs_with_pending_inputs(&self) -> bool {
        true
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.clock
            .set_live_load(manifold_water_rigid::physics::live_load(ctx.sim_step));
        self.clock.set_played_target(ctx.sim_step.played_target);
        self.coupled.step = ctx.sim_step;
        if let Some(owner) = self.coupled.owner.as_mut() {
            owner.set_step(ctx.sim_step);
        }
        let role_pending = self.role_ports.read(&ctx.inputs);
        let mut roles = self.role_ports.take();
        // Each item's Initial Fill seeds its slot; a box fill never reaches
        // the bodies, and a shaped one stays as a region row for its shape.
        // Sand only fills, so an Inflow or Outflow is refused by name.
        let flows = |role: &Option<FluidRole>| {
            role.as_ref()
                .is_some_and(|role| role.kind != FluidRoleKind::InitialFill)
        };
        let fill = if roles.iter().any(flows) {
            for role in roles.iter_mut().filter(|role| flows(role)) {
                *role = None;
            }
            Err("Sand fills only; Inflow and Drain are Water's".to_owned())
        } else {
            take_slot_fills(&mut roles, ctx.params)
        };
        // A physics sample reads the force field and the roles at a tick's
        // start; it never advances time.
        if ctx.sim_step.authored_sample_only {
            self.clock
                .observe_speed(ctx.played(), ctx.scalar_or_param("speed", 1.0));
            match felt_field(ctx, "Matter") {
                Ok(field) => self
                    .fields
                    .observe_sample(&self.clock, ctx.played(), field.as_ref()),
                Err(error) => ctx.error(error),
            }
            self.bodies.observe_sample(
                &self.clock,
                ctx.played(),
                (!role_pending).then_some(&roles[..]),
            );
            self.coupled
                .scenes
                .observe_staged(&self.clock, ctx.played());
            self.role_ports.put_back(roles);
            return;
        }
        self.role_pending = ctx.inputs.any_pending() || role_pending;
        self.acceleration = match felt_field(ctx, "Matter") {
            Ok(field) => field,
            Err(error) => {
                ctx.error(error);
                None
            }
        };
        if ctx.inputs.slot("acceleration_field").is_some() {
            self.role_pending |= self.acceleration.is_none();
        }
        // Every output is published every frame, so no consumer reads a slot
        // this node left unwritten. While a role is still being prepared, or
        // on an error, the liquid holds: the last good outputs repeat with
        // zero ticks. Before the first good frame there is nothing to hold,
        // so the outputs are declared pending.
        self.coupled.exchange = None;
        self.coupled.push = None;
        self.schedule.clear();
        let computed = if self.role_pending {
            Ok(None)
        } else {
            self.compute(ctx, &roles, fill)
        };
        self.role_ports.put_back(roles);
        if let Some(owner) = self.coupled.owner.as_ref() {
            let touching = owner.crowded_support_points();
            if touching > 0 {
                ctx.error(format!(
                    "Matter: a body touches {touching} contact points, more than the water can weigh; where it rests may jitter"
                ));
            }
        }
        let held = || {
            let mut held = self.published.unwrap_or([0.0; OUTPUTS.len()]);
            held[TICKS] = 0.0;
            held[IMPULSE_TICK] = -1.0;
            held
        };
        self.coupled.failed = false;
        let (values, fresh) = match computed {
            Ok(Some(values)) => (values, true),
            Ok(None) => {
                self.role_pending = true;
                (held(), false)
            }
            Err(error) => {
                self.coupled.failed = true;
                self.coupled.push = None;
                ctx.error(error);
                (held(), false)
            }
        };
        if !fresh {
            self.schedule.clear();
            self.coupled.exchange = None;
        }
        if fresh {
            self.published = Some(values);
        }
        if self.published.is_none() {
            ctx.mark_outputs_pending();
        }
        for (name, value) in OUTPUTS.iter().zip(values) {
            ctx.outputs.set_scalar(name, ParamValue::Float(value));
        }
        self.upload_bodies(ctx, fresh && self.rows_fresh, values[TICKS] > 0.0);
        let uploaded = match ctx
            .gpu
            .as_deref_mut()
            .map(|gpu| self.fields.upload(gpu, ctx.sim_step.offline()))
        {
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

    /// The liquid restarts in a new epoch, and a coupled pair together with
    /// a fresh rigid owner.
    fn clear_state(&mut self) {
        self.schedule.clear();
        self.coupled.reset();
        self.clock.restart();
    }
}

impl SharesSolids for MatterDomain {
    fn rigid_owner(&self) -> Option<&LiquidRigidOwner> {
        self.coupled.owner.as_ref()
    }

    fn rigid_owner_mut(&mut self) -> Option<&mut LiquidRigidOwner> {
        self.coupled.owner.as_mut()
    }

    fn shared_solids_mut(&mut self) -> &mut SharedSolids {
        &mut self.observed
    }

    fn pushes_bodies(&self) -> bool {
        true
    }

    fn take_observed_push(&mut self) -> Option<ObservedPush> {
        let scale = self.coupled.push.take()?;
        let reaction = self.reaction.clone()?;
        Some(Box::new(move |rows, impulses| {
            decode(scale, rows, reaction_words(Some(&reaction)), impulses)
        }))
    }
}

impl PhysicsNode for MatterDomain {
    fn fluid_domain_snapshot(&self) -> Option<manifold_core::fluid_domain::FluidDomainSnapshot> {
        let epoch = self.published.map_or(0, |values| values[EPOCH] as u64);
        Some(manifold_water_liquid::gizmos::domain_snapshot(
            self.layout,
            self.coupled.failed,
            self.role_pending,
            epoch,
            &self.setup_report,
            self.coupled
                .owner
                .as_ref()
                .map_or_else(|| self.observed.uncoupled(), LiquidRigidOwner::uncoupled),
        ))
    }

    /// The pool and each slot's initial column as the fill seeds them (whole
    /// cells), then bodies and forces.
    fn write_physics_gizmos(&self, out: &mut Vec<PhysicsGizmo>) {
        let Some(layout) = self.layout else {
            return;
        };
        let (pool_cells, columns) = self.fill_cells;
        // The f32 cell size, as `fill_region` counts cells.
        let dx = layout.cell_size as f32;
        let max: [f32; 3] = std::array::from_fn(|i| layout.min[i] + layout.size[i]);
        if pool_cells > 0 {
            let top = layout.min[1] + pool_cells as f32 * dx;
            out.push(PhysicsGizmo::aabb(
                PhysicsGizmoFamily::Fill,
                layout.min,
                [max[0], top, max[2]],
            ));
        }
        for column in columns
            .iter()
            .filter(|column| column.iter().all(|[lo, hi]| hi > lo))
        {
            let corner =
                |end: usize| std::array::from_fn(|i| layout.min[i] + column[i][end] as f32 * dx);
            out.push(PhysicsGizmo::aabb(
                PhysicsGizmoFamily::Fill,
                corner(0),
                corner(1),
            ));
        }
        manifold_water_liquid::gizmos::write_liquid_gizmos(
            &layout,
            &self.bodies,
            self.acceleration.as_ref(),
            out,
        );
    }

    fn set_coupled_physics(&mut self, enabled: bool) {
        if self.coupled.mode != enabled {
            self.coupled.mode = enabled;
            self.clear_state();
        }
    }

    fn set_coupled_rigid_inputs(
        &mut self,
        observation: Option<&RigidSceneObservation>,
        colliders: RigidImpulseTargets,
        error: Option<&str>,
        authored_sample_only: bool,
    ) {
        // A replay sample hands the scene over before this domain observes
        // the sample's Speed; `run` records it once Speed names the tick. A
        // pending one records nothing.
        if authored_sample_only {
            let scene = observation
                .filter(|_| error.is_none())
                .map(|observation| &observation.inputs);
            self.coupled.scenes.stage(scene);
            return;
        }
        self.set_coupled_physics(true);
        // Refill the retained inputs in place: a steady frame never reallocates.
        match (observation, &mut self.coupled.observation) {
            (Some(source), Some(retained)) => retained.clone_from(source),
            (source, retained) => *retained = source.cloned(),
        }
        self.coupled.colliders = colliders;
        self.coupled.error = error.map(str::to_owned);
    }

    fn coupled_rigid_frame(&self) -> Option<&RigidFrame> {
        let coupled = &self.coupled;
        if !coupled.mode
            || self.role_pending
            || coupled.failed
            || coupled.observation.is_none()
            || coupled.error.is_some()
        {
            return None;
        }
        let frame = coupled.owner.as_ref().map(LiquidRigidOwner::frame)?;
        Some(coupled.presented.shown(frame))
    }

    fn physics_impulse_epoch(&self) -> Option<u64> {
        self.impulses.epoch()
    }

    /// A pending liquid (roles still preparing, clock not started) makes a
    /// hit wait; a failed one refuses it.
    fn physics_impulse_readiness(&self) -> Result<bool, String> {
        if let Some(error) = self.impulses.failure() {
            return Err(error.into());
        }
        if self.coupled.failed {
            return Err("Matter impulses: the liquid failed; this hit is refused".into());
        }
        Ok(!self.role_pending && self.impulses.epoch().is_some())
    }

    /// A hit fired this frame is stamped at the liquid's simulated time, so
    /// it lands on the first tick of the next frame (the rigid half at the
    /// start of the next settled rigid tick).
    fn physics_impulse_stamp(
        &self,
        played: manifold_core::PlayedTime,
        sequence: u64,
        step: manifold_water_rigid::physics::SimStep,
    ) -> Result<manifold_physics::input::EventStamp, String> {
        if !self.physics_impulse_readiness()? {
            return Err("Matter impulses: the liquid is still preparing; fire waits for it".into());
        }
        let mut stamp = self.impulses.stamp(played, sequence)?;
        stamp.time = manifold_core::Seconds(
            self.clock
                .simulation_at(played, manifold_water_rigid::physics::live_load(step)),
        );
        Ok(stamp)
    }

    fn enqueue_physics_impulse(
        &mut self,
        stamp: manifold_physics::input::EventStamp,
        impulse: ResolvedNodeImpulse,
    ) -> Result<manifold_physics::TickStamp, String> {
        self.impulses
            .enqueue(stamp, impulse, self.coupled.owner.as_mut())
    }

    fn drain_physics_impulses(
        &mut self,
        consume: &mut dyn FnMut(manifold_physics::input::AppliedEvent<ResolvedNodeImpulse>),
    ) {
        self.impulses
            .drain_applied(consume, self.coupled.owner.as_mut());
    }

    fn drain_discarded_impulses(
        &mut self,
        consume: &mut dyn FnMut(manifold_physics::input::EventStamp),
    ) -> u64 {
        self.impulses
            .drain_discarded(consume, self.coupled.owner.as_mut())
    }
}

inventory::submit! { PhysicsNodeRegistration::new::<MatterDomain>() }

impl TickSampled for MatterDomain {
    fn request_physics_samples(
        &mut self,
        from: manifold_core::PlayedTime,
        until: manifold_core::PlayedTime,
        out: &mut Vec<manifold_core::PlayedTime>,
    ) {
        self.fields.request_samples(&self.clock, from, until, out);
        self.bodies.request_samples(&self.clock, from, until, out);
        if self.coupled.mode {
            self.coupled.scenes.request(&self.clock, from, until, out);
        }
    }
}

inventory::submit! { TickSampledRegistration::new::<MatterDomain>() }

/// The static meshes (bodies read through their trees) at the last tick:
/// a hash of their shapes, trees and which rows hold which, and, when only
/// where they stand changed since `last`, the world box round each moved
/// row's old and new box. `last` takes this call's rows. A row whose box is
/// not finite counts as a change of shape.
fn static_change(
    bodies: &LiquidBodies,
    last: &mut Vec<(usize, LiquidBody)>,
) -> (u64, Option<[[f32; 3]; 2]>) {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    let shapes = bodies.shapes();
    bytemuck::cast_slice::<LiquidShape, u8>(shapes).hash(&mut hasher);
    bodies.collider_nodes().len().hash(&mut hasher);
    bodies.collider_triangles().len().hash(&mut hasher);
    let rows = bodies.last_rows();
    let last_tick = &rows[rows.len() - bodies.count().min(rows.len())..];
    let now: Vec<(usize, LiquidBody)> = last_tick
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, row)| {
            let shape = row.accel_shape[3];
            shape >= 0.0 && shapes.get(shape as usize).is_some_and(|s| s.tree == 1)
        })
        .collect();
    for (index, row) in &now {
        index.hash(&mut hasher);
        row.accel_shape[3].to_bits().hash(&mut hasher);
    }
    let mut moved: Option<[[f32; 3]; 2]> = None;
    let mut unbounded = false;
    if last.len() == now.len() {
        for ((_, old), (_, new)) in last.iter().zip(&now) {
            if old.position_inv_mass[..3] == new.position_inv_mass[..3]
                && old.rotation == new.rotation
            {
                continue;
            }
            match (bodies.world_box(old), bodies.world_box(new)) {
                (Some(a), Some(b)) => {
                    let joined = moved.get_or_insert(a);
                    for [lo, hi] in [a, b] {
                        for axis in 0..3 {
                            joined[0][axis] = joined[0][axis].min(lo[axis]);
                            joined[1][axis] = joined[1][axis].max(hi[axis]);
                        }
                    }
                }
                _ => unbounded = true,
            }
        }
    }
    if unbounded {
        // A moved row with no finite box: its pose joins the hash, so every
        // held tile is rebuilt.
        for (_, row) in &now {
            bytemuck::bytes_of(row).hash(&mut hasher);
        }
    }
    *last = now;
    (hasher.finish(), moved.filter(|_| !unbounded))
}

impl MatterDomain {
    /// Keep the reaction and the provided body, shape and atlas buffers
    /// current.
    fn upload_bodies(
        &mut self,
        ctx: &mut EffectNodeContext<'_, '_>,
        rows_fresh: bool,
        ticks: bool,
    ) {
        let Some(gpu) = ctx.gpu.as_deref_mut() else {
            return;
        };
        // Sized for every body a liquid holds, so a pending reaction never
        // moves when roles come and go.
        let reaction = self.reaction.get_or_insert_with(|| {
            let bytes = MAX_LIQUID_BODIES * REACTION_WORDS as usize * 4;
            let buffer = gpu.device.create_buffer(bytes as u64, BufferUse::Upload);
            // SAFETY: new shared buffer, not yet visible to the GPU.
            unsafe { buffer.write(0, &vec![0u8; bytes]) };
            buffer
        });
        // A coupled tick sums its reaction from zero; the words stay put
        // until the next tick so the owner reads them once the frame retires.
        // A Matter that takes another liquid's solids sums its push over the
        // frame, which that owner reads at its next settle.
        if ticks && (self.coupled.owner.is_some() || self.coupled.push.is_some()) {
            gpu.native_enc.clear_buffer(reaction);
        }
        if self
            .fill_table
            .as_ref()
            .is_none_or(|(rows, _)| *rows != self.fill_rows)
        {
            let bytes = (FILL_ROW_WORDS * MAX_MATTER_SLOTS * 4) as u64;
            let buffer = gpu.device.create_buffer(bytes, BufferUse::Upload);
            // SAFETY: a new shared buffer no GPU work has seen yet.
            unsafe { buffer.write(0, bytemuck::cast_slice(&self.fill_rows)) };
            self.fill_table = Some((self.fill_rows, buffer));
        }
        if let Err(error) =
            self.body_buffers
                .upload(gpu, &self.bodies, rows_fresh, "node.matter_domain.bodies")
        {
            ctx.error(error);
        }
    }

    /// This frame's outputs, or None while a collider's distance lattice is
    /// still building (the liquid holds and the clock does not advance).
    fn compute(
        &mut self,
        ctx: &EffectNodeContext<'_, '_>,
        roles: &[Option<FluidRole>],
        fills: Result<[Option<InitialFill>; MAX_MATTER_SLOTS], String>,
    ) -> Result<Option<[f32; OUTPUTS.len()]>, String> {
        let geometry = fills
            .map_err(|reason| format!("Matter: {reason}"))
            .and_then(|fills| {
                let columns = fills.map(|fill| fill.map(|fill| fill.column));
                matter_geometry(
                    |name, default| ctx.scalar_or_param(name, default),
                    ctx.params,
                    ctx.inputs.transform("domain"),
                    &columns,
                )
                .map(|geometry| {
                    (
                        geometry,
                        columns,
                        fills.map(|fill| fill.and_then(|fill| fill.shape)),
                    )
                })
            });
        let (geometry, columns, shaped) = match geometry {
            Ok(found) => found,
            Err(reason) => {
                self.setup_report.refuse(&reason);
                return Err(reason);
            }
        };
        let MatterGeometry {
            layout, setup, fit, ..
        } = geometry;
        let cell_size = geometry.cell_size();
        self.setup_report.accept(
            fit,
            geometry
                .misfit
                .and_then(|slot| columns[slot])
                .map(|column| (column.pos, column.scale)),
        );
        let speed = ctx.scalar_or_param("speed", 1.0);
        let live = [
            "gravity_x",
            "gravity",
            "gravity_z",
            "wave",
            "snow_wave",
            "diffusivity",
            "density",
            "wall_friction",
            "hardened",
        ]
        .map(|name| ctx.scalar_or_param(name, 0.0));
        if !speed.is_finite()
            || !(0.0..=4.0).contains(&speed)
            || live.iter().any(|v| !v.is_finite())
        {
            return Err(
                "Matter: Speed must be between 0 and 4, and gravity and the materials must be finite"
                    .into(),
            );
        }
        let offline = ctx.sim_step.offline();
        if let Some(error) = self.coupled.host_error.take() {
            return Err(error);
        }
        // A warmup pump commits and waits on its own command buffer, which
        // never signals the content frame clock, so every tick before its
        // frame has finished: it settles them without the clock.
        let clock = if self.coupled.mode {
            ctx.gpu
                .as_deref()
                .filter(|gpu| !gpu.preparing)
                .and_then(|gpu| gpu.device.frame_clock())
        } else {
            None
        };
        let (wall_faces, open_faces) = face_masks(setup.bounds, closed_faces(ctx.params));
        self.coupled.walls = geometry.domain_walls(wall_faces);
        self.coupled.capacity = MAX_LIQUID_BODIES;
        // The heaviest material the chain holds: the bodies' coupling law
        // takes it.
        let density = ctx.scalar_or_param("density", SAND_DENSITY).max(1.0);
        if self.coupled.mode && !self.observe_rigid(ctx.played(), speed, density)? {
            return Ok(None);
        }
        let coupled_geometries = self
            .coupled
            .owner
            .as_ref()
            .map_or_else(|| self.observed.geometries(), LiquidRigidOwner::geometries);
        if self
            .bodies
            .prepare(roles, coupled_geometries, cell_size, offline)?
            == BodiesStatus::Pending
        {
            return Ok(None);
        }
        let setup_changed = self.setup != Some(setup);
        let restart = setup_changed || self.coupled.owner_fresh;
        // Finish the prior reaction before reusing its buffer. Live and
        // offline share per-tick exchanges; only the clock limits live work.
        if !restart
            && let (Some(owner), Some(_)) = (&mut self.coupled.owner, &self.coupled.observation)
        {
            let reaction = self.reaction.as_ref();
            let scale = self.coupled.scale;
            // This frame's clear is encoded after this read.
            let settled = owner.settle_ready(
                self.coupled.scenes.endpoint(owner.completed() + 1),
                |stamp| {
                    clock.as_ref().is_none_or(|clock| {
                        clock.is_complete(stamp) || (clock.wait(stamp) && clock.is_complete(stamp))
                    })
                },
                |_, rows, impulses| {
                    let scale =
                        scale.ok_or("Matter coupling: the pending tick has no reaction scale")?;
                    decode(scale, rows, reaction_words(reaction), impulses)
                },
            );
            if let Err(error) = settled {
                // A dead reaction or a failed step: the pair restarts
                // with a fresh rigid owner, the error reported.
                self.coupled.owner = None;
                return Err(error);
            }
            if let Some(start) = self.coupled.scenes.reanchored_start(owner.completed()) {
                owner.reanchor_start(start)?;
            }
        }
        // Bodies that joined couple between ticks, never by a restart.
        if !restart
            && let Some(owner) = self.coupled.owner.as_mut()
            && owner.pending().is_none()
            && owner.wants_rebind()
        {
            owner.rebind()?;
            if self
                .bodies
                .prepare(roles, owner.geometries(), cell_size, offline)?
                == BodiesStatus::Pending
            {
                return Ok(None);
            }
        }
        self.setup = Some(setup);
        self.layout = Some(layout);
        self.fill_cells = (geometry.pool_cells, geometry.columns);
        // A shaped fill's region row in a tick, -1 for a box or none: the
        // fill seeds only the box cells inside its shape.
        let regions = shaped.map(|port| {
            port.and_then(|port| self.bodies.region_of(port))
                .map_or(-1, |row| row as i32)
        });
        self.fill_rows = fill_table(
            layout.cells,
            geometry.pool_cells,
            &geometry.columns,
            regions,
            setup.points_per_cell,
        );
        let frame = self.clock.advance(
            ctx.played(),
            ctx.sim_step.interval.0,
            speed,
            ctx.scalar_or_param("reset", 0.0),
            restart,
            offline,
        );
        if frame.numerical_error {
            ctx.sim_metrics.record_simulation(0.0, 0.0, false, true);
        }
        if frame.restarted && self.coupled.owner.is_some() && !self.coupled.owner_fresh {
            self.rebuild_owner()?;
        }
        self.coupled.owner_fresh = false;
        if let Some(owner) = self.coupled.owner.as_mut() {
            ctx.sim_metrics.merge(&owner.rigid_mut().take_metrics());
        }
        if let Some(owner) = &self.coupled.owner {
            let scene = self
                .coupled
                .observation
                .as_ref()
                .map(|observation| &observation.inputs);
            self.coupled.scenes.settle(&self.clock, &frame, scene);
            self.coupled.scenes.prune_before(owner.completed());
        }
        self.impulses.observe_frame(ctx.played(), &frame)?;
        self.bodies.settle(roles, &self.clock, &frame);
        let first_tick = fields::first_tick(&frame);
        // A restart runs no tick yet still publishes the first tick's rows: the
        // fill seeds around the colliders' starting poses. Otherwise a frame
        // without ticks keeps the last rows as the bodies' poses.
        let row_ticks = if frame.restarted {
            frame.ticks.max(1)
        } else {
            frame.ticks
        };
        self.rows_fresh = row_ticks > 0;
        let coupled_rows = self
            .coupled
            .owner
            .as_ref()
            .map_or_else(|| self.observed.rows(), LiquidRigidOwner::rows);
        let rows = if row_ticks > 0 {
            let rows = self
                .bodies
                .rows(first_tick, frame.ticks, coupled_rows)?
                .len() as f32;
            self.body_rows = rows;
            let (hash, moved) = static_change(&self.bodies, &mut self.static_rows);
            if hash != self.static_hash {
                self.static_hash = hash;
                self.static_key = (self.static_key + 1.0) % 16_777_216.0;
            } else if let Some(moved) = moved {
                self.moved_box = moved;
                self.moved_key = (self.moved_key + 1.0) % 16_777_216.0;
            }
            rows
        } else {
            self.body_rows
        };
        let dynamic_count = coupled_rows
            .iter()
            .filter(|row| takes_reaction(row))
            .count();
        // Another water's bodies move on through each later tick of this frame.
        if row_ticks > 0 && self.coupled.owner.is_none() && !self.observed.rows().is_empty() {
            let mut seconds = 0.0;
            for tick in 1..frame.ticks as usize {
                seconds += frame
                    .interval(tick as u64 - 1)
                    .map_or(0.0, |interval| interval.duration().0 as f32);
                self.bodies
                    .set_coupled_rows(tick, self.observed.rows_after(seconds))?;
            }
        }
        self.observed.used();

        // D4: substeps from the CFL rule over the materials the chain holds.
        // Snow stiffens as it packs: live, the step budgets the packing the
        // last read tick held with a quarter to spare; offline the most it can
        // reach, so an export repeats. A point whose stiffness outruns the
        // step keeps the share it has room for (matter_material.wgsl
        // `matter_capped`), so a late read softens it for a frame and never
        // blows it up.
        let v_est = free_fall_speed(f64::from(layout.size[1]));
        let sand = MatterMaterial::sand(1.0, f64::from(SAND_FRICTION_ANGLE), 0.0);
        let built = ctx.scalar_or_param("wave", sand.wave_speed(f64::from(SAND_DENSITY)) as f32);
        let packing = if offline {
            SNOW_HARDENING_MAX
        } else {
            let held = f64::from(ctx.scalar_or_param("hardened", SNOW_HARDENING_MAX as f32));
            (held * 1.25).clamp(1.0, SNOW_HARDENING_MAX)
        };
        let wave = built.max(ctx.scalar_or_param("snow_wave", 0.0) * packing.sqrt() as f32);
        let body_limit = match &self.coupled.owner {
            Some(owner) => live_body_limit(owner, cell_size, wave, density),
            None => taken_body_limit(&self.observed, cell_size, wave, density),
        };
        let diffusivity = f64::from(ctx.scalar_or_param("diffusivity", 0.0));
        let viscous =
            (diffusivity > 0.0).then(|| viscous_limit(diffusivity, f64::from(cell_size)) as f32);
        let nominal_substeps =
            substeps_per_tick(cell_size, wave, v_est as f32, body_limit, viscous);
        let wave_reach = ACOUSTIC_CFL * f64::from(cell_size);
        // The share of its stiffness the stiffest material keeps at the cap.
        let room = stiffness_room(
            wave_reach,
            TICK / f64::from(nominal_substeps.min(MAX_SUBSTEPS)),
            v_est,
            f64::from(wave),
        );
        let limited = room < 1.0;
        if limited != self.limited {
            self.limited = limited;
            if limited {
                log::warn!(
                    "[matter] the stiffest material needs more than {MAX_SUBSTEPS} substeps at this cell size; it runs at {:.0}% of its stiffness",
                    room * 100.0
                );
            }
        }
        // A frame without ticks reports what one Sim Rate interval runs, so
        // the substep count and unit hold steady between ticks.
        let duration = if frame.ticks == 0 {
            Some(ctx.sim_step.interval.0)
                .filter(|interval| interval.is_finite() && *interval > 0.0)
                .unwrap_or(TICK)
        } else {
            frame.duration().0
        };
        let requested = substeps_for_interval(duration as f32, nominal_substeps);
        let substeps = requested.min(MAX_SUBSTEPS);
        let schedule_capped =
            schedule_intervals(&mut self.schedule, &frame, nominal_substeps, cell_size);
        let unit = self.schedule.first().map_or_else(
            || momentum_unit(cell_size, duration / f64::from(substeps)),
            |record| record.scalars[1].1,
        );
        // Taking another liquid's solids, this frame's push reaches the
        // bodies through that liquid's owner.
        self.coupled.push = (self.coupled.owner.is_none() && frame.ticks > 0 && dynamic_count > 0)
            .then(|| ReactionScale {
                unit,
                cell_size,
                offset: self.bodies.count() - self.observed.rows().len(),
            });
        let mut display_time = frame.display_time;
        if let Some(owner) = &mut self.coupled.owner {
            if frame.ticks > 0 {
                if first_tick != owner.completed() {
                    return Err(format!(
                        "Matter coupling: fluid ticks {first_tick}+{} do not follow rigid tick {}",
                        frame.ticks,
                        owner.completed()
                    ));
                }
                let pending = PendingTick {
                    tick: first_tick,
                    stamp: clock.as_ref().map_or(0, FrameClock::stamp),
                    interval: frame.interval(0).expect("pending interval"),
                    offline: frame.offline,
                };
                let scale = ReactionScale {
                    unit,
                    cell_size,
                    offset: self.bodies.count() - owner.rows().len(),
                };
                owner.set_pending(pending);
                self.coupled.scale = Some(scale);
                if frame.ticks > 1 {
                    self.coupled.exchange = Some(Exchange { pending, scale });
                }
            }
            // The liquid and the bodies are shown at the clock's display
            // time, one tick behind, never past the tick Box3D has settled:
            // the scene takes the bodies' frame before the region runs this
            // frame's ticks.
            display_time = display_time.min(owner.completed_time().0);
            self.coupled
                .presented
                .present(owner.completed_time().0, owner.frame(), display_time);
        }
        self.coupled.transport = Some(ctx.played());
        let field = self.fields.prepare(
            geometry.force_lattice(),
            self.acceleration.as_ref(),
            &self.clock,
            &frame,
            &self.impulses,
        )?;

        let (scene, kill) = within_key_span(
            self.scene(&geometry, &columns, frame.epoch),
            geometry.setup.cell_size,
        )?;
        let simulated: f64 = (0..u64::from(frame.ticks))
            .map(|tick| {
                frame
                    .interval(tick)
                    .map_or(0.0, |interval| interval.duration().0)
            })
            .sum();
        let walls = wall_velocity_outputs(
            if restart || frame.restarted {
                None
            } else {
                self.last_bounds
            },
            geometry.bounds_box,
            simulated,
        );
        self.last_bounds = setup.bounds.then_some(geometry.bounds_box);
        let per_frame = [
            ("closed_faces", wall_faces as f32),
            ("open_faces", open_faces as f32),
            ("kill_height", kill),
            ("gravity_x", ctx.scalar_or_param("gravity_x", 0.0)),
            ("gravity", ctx.scalar_or_param("gravity", -9.81)),
            ("gravity_z", ctx.scalar_or_param("gravity_z", 0.0)),
            ("ticks", frame.ticks as f32),
            ("substeps_per_tick", substeps as f32),
            ("epoch", frame.epoch as f32),
            ("simulation_time", frame.simulation_time as f32),
            ("interval_duration", frame.duration().0 as f32),
            ("target_time", frame.target_time as f32),
            (
                "step_cap_hit",
                if !offline && (schedule_capped || (frame.ticks == 0 && requested >= MAX_SUBSTEPS))
                {
                    1.0
                } else {
                    0.0
                },
            ),
            ("display_time", display_time as f32),
            ("dropped_seconds", frame.dropped_seconds as f32),
            ("wave_reach", wave_reach as f32),
            ("speed_estimate", v_est as f32),
            (
                "wall_friction",
                ctx.scalar_or_param("wall_friction", sand.wall_friction() as f32),
            ),
            (
                "limited_by_substeps",
                if limited { room as f32 } else { 0.0 },
            ),
            ("momentum_unit", unit),
            ("body_count", self.bodies.count() as f32),
            ("body_rows", rows),
            ("first_tick", first_tick as f32),
            ("dynamic_count", dynamic_count as f32),
            ("static_key", self.static_key),
            ("moved_key", self.moved_key),
            ("moved_lo_x", self.moved_box[0][0]),
            ("moved_lo_y", self.moved_box[0][1]),
            ("moved_lo_z", self.moved_box[0][2]),
            ("moved_hi_x", self.moved_box[1][0]),
            ("moved_hi_y", self.moved_box[1][1]),
            ("moved_hi_z", self.moved_box[1][2]),
        ];
        let mut values = [0.0; OUTPUTS.len()];
        let mut written = 0u128;
        for (name, value) in geometry
            .outputs()
            .into_iter()
            .chain(bounds_outputs(scene))
            .chain(walls)
            .chain(per_frame)
            .chain(field.outputs())
        {
            let slot = OUTPUTS
                .iter()
                .position(|output| *output == name)
                .expect("every output has a slot");
            values[slot] = value;
            written |= 1 << slot;
        }
        debug_assert_eq!(
            written,
            (1u128 << OUTPUTS.len()) - 1,
            "every output written once"
        );
        Ok(Some(values))
    }

    /// The Bounds box the frame publishes and the kill height: with Bounds on
    /// the box as placed and no kill height; off, the box round every body
    /// and the Initial Fill, and the height a point falling past the lowest
    /// solid (or the fill's floor when the epoch began, with no body) is
    /// removed at.
    fn scene(
        &mut self,
        geometry: &MatterGeometry,
        fills: &[Option<Transform>; MAX_MATTER_SLOTS],
        epoch: u32,
    ) -> ([[f32; 3]; 2], f32) {
        if geometry.setup.bounds {
            return (geometry.bounds_box, NO_KILL_HEIGHT);
        }
        let floor = self.source_floor.of_epoch(epoch, || {
            lowest_source(fills.iter().flatten().copied(), std::iter::empty())
        });
        let v0 =
            (geometry.setup.cell_size.powi(3) / f64::from(geometry.setup.points_per_cell)) as f32;
        let kill = kill_height(
            self.bodies
                .body_boxes()
                .map(|(fixed, placed)| (fixed, placed[0][1])),
            floor,
            RADIUS_SCALE * v0.cbrt(),
        );
        // A body that has fallen wholly below the kill height is past every
        // grain, so it no longer stretches the scene.
        let placed = self
            .bodies
            .body_boxes()
            .map(|(_, placed)| placed)
            .filter(|placed| placed[1][1] >= kill)
            .chain(
                fills
                    .iter()
                    .flatten()
                    .map(|fill| box_about(fill.pos, fill.scale)),
            );
        (union_box(placed).unwrap_or(geometry.bounds_box), kill)
    }

    /// Take the paired world's observation for this frame: check it shares
    /// the liquid's clock, and build a new rigid owner (restarting the pair)
    /// on its reset or a change of bodies. False while the world's inputs are
    /// still pending.
    fn observe_rigid(
        &mut self,
        played: manifold_core::PlayedTime,
        speed: f32,
        density: f32,
    ) -> Result<bool, String> {
        let Coupling {
            observation,
            colliders,
            error,
            previous_reset,
            owner,
            owner_fresh,
            epochs,
            walls,
            capacity,
            step,
            ..
        } = &mut self.coupled;
        if let Some(error) = error {
            return Err(error.clone());
        }
        let Some(observation) = observation.as_ref() else {
            return Ok(false);
        };
        if speed != observation.speed {
            return Err(format!(
                "Matter coupling: liquid and rigid bodies require the same Speed and shared World control (liquid {speed}, rigid {})",
                observation.speed
            ));
        }
        if (observation.transport.seconds() - played.seconds()).abs() > 1e-9 {
            return Err(
                "Matter coupling: rigid observation transport does not match liquid transport"
                    .into(),
            );
        }
        CoupledRigidInputs {
            scene: &observation.inputs,
            colliders,
            density: f64::from(density),
        }
        .validate()?;
        let reset_edge = previous_reset.is_some_and(|previous| previous != observation.reset);
        *previous_reset = Some(observation.reset);
        // A body that joined is admitted into the running world; only a
        // change it cannot take live starts a new epoch.
        let changed = owner.as_ref().is_none_or(|owner| {
            !owner.matches(&observation.inputs, *walls, &*colliders, *capacity)
        });
        let admitted = !reset_edge
            && changed
            && match owner.as_mut() {
                Some(owner) => owner.admit(&observation.inputs, *walls, &*colliders, *capacity)?,
                None => false,
            };
        if reset_edge || (changed && !admitted) {
            *epochs += 1;
            let mut built = LiquidRigidOwner::new(
                &observation.inputs,
                *walls,
                &*colliders,
                *capacity,
                *epochs,
                owner.as_ref(),
            )?;
            built.set_step(*step);
            *owner = Some(built);
            *owner_fresh = true;
        }
        Ok(true)
    }

    /// Exchange before tick `tick` of this frame (counted from its first):
    /// the GPU has finished tick `tick − 1`, so its reaction words are final
    /// and the body rows are free to rewrite.
    fn exchange_tick(&mut self, record: MatterInterval, exchange: Exchange) -> Result<(), String> {
        let tick = record.timing.ordinal;
        let scale = record.reaction_scale(exchange.scale);
        let owner = self
            .coupled
            .owner
            .as_mut()
            .ok_or("Matter coupling: no coupled rigid world")?;
        let reaction = self
            .reaction
            .as_ref()
            .ok_or("Matter coupling: the reaction array is missing")?;
        // A tick running this frame started by now, so the tick before it
        // has its end sampled.
        let end = self.coupled.scenes.endpoint(owner.completed() + 1);
        owner.settle_ready(
            end,
            |_| true,
            |_, rows, impulses| {
                decode(
                    exchange.scale,
                    rows,
                    reaction_words(Some(reaction)),
                    impulses,
                )
            },
        )?;
        let (offset, rows) = self.bodies.set_coupled_rows(tick as usize, owner.rows())?;
        let bodies = self
            .body_buffers
            .bodies()
            .ok_or("Matter coupling: the body rows are missing")?;
        let bytes: &[u8] = bytemuck::cast_slice(rows);
        if offset + bytes.len() as u64 > bodies.size {
            return Err("Matter coupling: the body rows outgrew their buffer".into());
        }
        // SAFETY: shared storage in bounds (checked above); the GPU has
        // completed every command that touched it, and the next reader is
        // encoded after this write.
        unsafe { bodies.write(offset, bytes) };
        reaction.zero_fill();
        owner.set_pending(PendingTick {
            tick: exchange.pending.tick + u64::from(tick),
            interval: record.interval(),
            ..exchange.pending
        });
        self.coupled.scale = Some(scale);
        if let Some(exchange) = &mut self.coupled.exchange {
            exchange.scale = scale;
        }
        Ok(())
    }

    /// A restart the liquid's own clock found (its reset, a backward seek):
    /// the rigid world restarts with it, keeping its hulls.
    fn rebuild_owner(&mut self) -> Result<(), String> {
        let observation = self
            .coupled
            .observation
            .as_ref()
            .ok_or("Matter coupling: the rigid observation is missing")?;
        self.coupled.epochs += 1;
        let mut owner = LiquidRigidOwner::new(
            &observation.inputs,
            self.coupled.walls,
            &self.coupled.colliders,
            self.coupled.capacity,
            self.coupled.epochs,
            self.coupled.owner.as_ref(),
        )?;
        owner.set_step(self.coupled.step);
        self.coupled.owner = Some(owner);
        Ok(())
    }
}

#[cfg(test)]
mod sim_rate_tests {
    use super::*;
    use manifold_core::{PlayedTime, Seconds};
    use manifold_physics::{clock::SimulationClock, stepping::StepInterval};

    #[test]
    fn matter_interval_schedule_matches_separate_speed_history_frames() {
        let run = |grouped| {
            let mut clock = SimulationClock::default();
            clock.advance(PlayedTime::from_played(0.0), TICK, 0.25, 0.0, false, true);
            clock.observe_speed(PlayedTime::from_played(TICK), 4.0);
            let mut schedule = Vec::new();
            let mut result = Vec::new();
            for transport in if grouped {
                vec![2.0 * TICK]
            } else {
                vec![TICK, 2.0 * TICK]
            } {
                let frame = clock.advance(
                    PlayedTime::from_played(transport),
                    TICK,
                    4.0,
                    0.0,
                    false,
                    true,
                );
                schedule_intervals(&mut schedule, &frame, 100, 0.0625);
                for record in &schedule {
                    result.push((
                        record.interval(),
                        record.timing.iterations,
                        crate::matter::substep_duration(
                            record.timing.duration().0 as f32,
                            record.timing.iterations,
                        )
                        .to_bits(),
                        record.scalars[1].1.to_bits(),
                    ));
                }
            }
            result
        };
        let separate = run(false);
        assert_eq!(run(true), separate);
        assert_eq!(separate.len(), 2);
        assert_eq!(separate[0].1, 25);
        assert_eq!(separate[1].1, MAX_SUBSTEPS);
        assert_ne!(separate[0].3, separate[1].3);
    }

    #[test]
    fn matter_clock_outputs_follow_interval_prefix_and_clear_on_reset() {
        let mut clock = SimulationClock::default();
        clock.advance(PlayedTime::from_played(0.0), TICK, 0.25, 0.0, false, true);
        clock.observe_speed(PlayedTime::from_played(TICK), 4.0);
        let frame = clock.advance(
            PlayedTime::from_played(2.0 * TICK),
            TICK,
            4.0,
            0.0,
            false,
            true,
        );
        let mut domain = MatterDomain::new();
        assert!(schedule_intervals(
            &mut domain.schedule,
            &frame,
            100,
            0.0625
        ));
        let first = domain.schedule[0];
        let second = domain.schedule[1];
        assert_eq!(second.timing.first_iteration, first.timing.iterations);
        assert_eq!(first.timing.total_iterations, 153);
        assert_eq!(second.timing.total_iterations, 153);
        domain.coupled.exchange = Some(Exchange {
            pending: PendingTick {
                tick: 7,
                interval: first.interval(),
                offline: true,
                stamp: 0,
            },
            scale: ReactionScale {
                unit: first.scalars[1].1,
                cell_size: 0.0625,
                offset: 0,
            },
        });
        for iteration in 0..153 {
            let expected = if iteration < 25 { first } else { second };
            let output = Primitive::substep_clock_interval(&domain, iteration).unwrap();
            assert_eq!(output.timing, expected.timing);
            assert_eq!(output.scalars, expected.scalars.as_slice());
            assert_eq!(
                Primitive::substep_host_sync(&domain, iteration),
                iteration == 25
            );
        }
        assert!(Primitive::substep_clock_interval(&domain, 153).is_none());
        let held = clock.advance(
            PlayedTime::from_played(2.0 * TICK),
            TICK,
            4.0,
            0.0,
            false,
            true,
        );
        let capacity = domain.schedule.capacity();
        schedule_intervals(&mut domain.schedule, &held, 100, 0.0625);
        assert!(domain.schedule.is_empty());
        assert_eq!(domain.schedule.capacity(), capacity);
        schedule_intervals(&mut domain.schedule, &frame, 100, 0.0625);
        Primitive::clear_state(&mut domain);
        assert!(Primitive::substep_clock_interval(&domain, 0).is_none());
        assert!(domain.coupled.exchange.is_none());
    }

    #[test]
    fn matter_interval_reaction_scale_keeps_previous_decode_and_next_unit() {
        use manifold_water_rigid::physics::RigidBody;
        let previous = ReactionScale {
            unit: 64.0,
            cell_size: 0.0625,
            offset: 3,
        };
        let record = MatterInterval {
            timing: SubstepInterval {
                start: Seconds(TICK),
                end: Seconds(2.0 * TICK),
                ordinal: 1,
                first_iteration: 25,
                iterations: 128,
                total_iterations: 153,
            },
            scalars: [("substeps_per_tick", 128.0), ("momentum_unit", 256.0)],
        };
        let exchange = Exchange {
            pending: PendingTick {
                tick: 7,
                interval: record.interval(),
                offline: true,
                stamp: 0,
            },
            scale: previous,
        };
        let next = record.reaction_scale(exchange.scale);
        assert_eq!(
            exchange.scale, previous,
            "completed reaction uses its original unit"
        );
        assert_eq!(
            next,
            ReactionScale {
                unit: 256.0,
                ..previous
            }
        );

        let mut scene = RigidSceneInputs {
            gravity: [0.0; 3],
            ..RigidSceneInputs::default()
        };
        scene.bodies[0] = Some(RigidBody {
            transform: Transform {
                pos: [0.0, 1.0, 0.0],
                scale: [0.4; 3],
                ..Transform::default()
            },
            mass: 50.0,
            ..RigidBody::default()
        });
        let mut owner = LiquidRigidOwner::new(
            &scene,
            DomainWalls::default(),
            &RigidImpulseTargets::body(0).unwrap(),
            MAX_LIQUID_BODIES,
            3,
            None,
        )
        .unwrap();
        let mut words = [0i32; 4 * REACTION_WORDS as usize];
        for (tick, scale) in [exchange.scale, next].into_iter().enumerate() {
            // The same +1 m/s reaction is encoded with different units. Decoding
            // either with the other interval's unit gives the wrong velocity.
            words[previous.offset * REACTION_WORDS as usize] = (16_777_216.0 / scale.unit) as i32;
            owner.set_pending(PendingTick {
                tick: tick as u64,
                interval: StepInterval::new(
                    Seconds(tick as f64 * TICK),
                    Seconds((tick + 1) as f64 * TICK),
                ),
                offline: true,
                stamp: 0,
            });
            owner
                .settle_ready(
                    Some(&scene),
                    |_| true,
                    |_, rows, impulses| decode(scale, rows, Some(&words), impulses),
                )
                .unwrap();
            assert!((owner.rows()[0].linear_velocity[0] - (tick + 1) as f32).abs() < 1e-4);
        }
    }

    #[test]
    fn sim_rate_matter_keeps_stability_subdivisions() {
        use crate::matter::{substep_duration, substeps_for_interval};
        use manifold_physics::{SimRate, clock::SimulationClock};
        for rate in SimRate::ALL {
            let run = |offline| {
                let mut clock = SimulationClock::default();
                let mut result = Vec::new();
                for frame in 0..=60 {
                    let accepted = clock.advance(
                        PlayedTime::from_played(f64::from(frame) / 60.0),
                        rate.interval(),
                        1.0,
                        0.0,
                        false,
                        offline,
                    );
                    for ordinal in 0..u64::from(accepted.ticks) {
                        let interval = accepted.interval(ordinal).unwrap().duration().0 as f32;
                        let steps = substeps_for_interval(interval, 34);
                        let dt = substep_duration(interval, steps);
                        assert_eq!(steps, 34 * 60 / rate.hz());
                        assert!((dt * steps as f32 - interval).abs() < 1e-7);
                        result.push((steps, dt.to_bits()));
                    }
                }
                result
            };
            let live = run(false);
            assert_eq!(live.len(), rate.hz() as usize);
            assert_eq!(live, run(true));
        }
    }

    /// A pending liquid makes a hit wait for it; only a failed one refuses
    /// (BUG-mo4oa (scene force trigger skipped)).
    #[test]
    fn matter_pending_liquid_makes_hits_wait() {
        use manifold_physics::clock::{SimulationClock, TICK};
        use manifold_water_rigid::node::PhysicsNode;
        let mut domain = MatterDomain::new();
        assert_eq!(
            domain.physics_impulse_readiness(),
            Ok(false),
            "the clock has not started"
        );
        let frame = SimulationClock::default().advance(
            PlayedTime::from_played(0.0),
            TICK,
            1.0,
            0.0,
            false,
            false,
        );
        domain
            .impulses
            .observe_frame(PlayedTime::from_played(0.0), &frame)
            .unwrap();
        assert_eq!(domain.physics_impulse_readiness(), Ok(true));
        domain.role_pending = true;
        assert_eq!(
            domain.physics_impulse_readiness(),
            Ok(false),
            "a role is still preparing"
        );
        domain.coupled.failed = true;
        assert!(
            domain
                .physics_impulse_readiness()
                .unwrap_err()
                .contains("refused")
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(test)]
mod gizmo_tests {
    use super::*;
    use manifold_core::physics_gizmo::PhysicsGizmoShape;

    /// A fill that does not fit the domain gets the shared fit's defined
    /// result, the one the setup check reports: the pool caps at the top, a
    /// column past a wall fills what is inside, one wholly outside fills
    /// nothing (`manifold_core::liquid_setup`).
    #[test]
    fn matter_fill_caps_the_pool_and_clips_the_column() {
        let geometry = |fill: f32, column: Option<Transform>| {
            let read =
                |name: &str, default: f32| if name == "fill_height" { fill } else { default };
            let mut fills = [None; MAX_MATTER_SLOTS];
            fills[0] = column;
            matter_geometry(read, &ParamValues::default(), None, &fills).expect("fits or is fitted")
        };
        let top = geometry(4.0, None);
        assert_eq!(geometry(9.0, None).pool_cells, top.pool_cells);

        // The default 4 m domain spans x from -2 to 2.
        let at_wall = Transform {
            pos: [1.5, 1.0, 0.0],
            scale: [1.0; 3],
            ..Transform::default()
        };
        let past_wall = Transform {
            pos: [2.0, 1.0, 0.0],
            scale: [2.0, 1.0, 1.0],
            ..Transform::default()
        };
        assert_eq!(
            geometry(0.16, Some(past_wall)).columns[0],
            geometry(0.16, Some(at_wall)).columns[0]
        );
        let away = Transform {
            pos: [9.0, 1.0, 0.0],
            scale: [1.0; 3],
            ..Transform::default()
        };
        assert_eq!(geometry(0.16, Some(away)).columns[0], [[0; 2]; 3]);
    }

    /// Each slot's box seeds after the pool and the slots before it, its
    /// first point and count the run its draw publishes; a box under the
    /// pool top starts above it, and the count crosses as an exact f32 wire.
    #[test]
    fn each_slot_seeds_its_own_run_after_the_pool() {
        let mut columns = [[[0u32; 2]; 3]; MAX_MATTER_SLOTS];
        // 64³ cells, pool 3 cells. Slot 0: x 10..29, y 0..33 (above the
        // pool from 3), z 4..60; slot 2: a 5³ box; slot 1 empty.
        columns[0] = [[10, 29], [0, 33], [4, 60]];
        columns[2] = [[40, 45], [20, 25], [40, 45]];
        let rows = fill_table([64; 3], 3, &columns, [-1, -1, 7, -1, -1, -1, -1, -1], 8);
        let pool = 64 * 3 * 64 * 8;
        let slot0 = 19 * 30 * 56 * 8;
        assert_eq!(rows[0], [10, 29, 3, 33, 4, 60, u32::MAX, pool]);
        assert_eq!(box_points(&rows[1], 8), 0);
        assert_eq!(rows[2][6], 7);
        assert_eq!(rows[2][7], pool + slot0);
        assert_eq!(rows[3][7], pool + slot0 + 125 * 8);
        assert_eq!(
            fill_points([64; 3], 3, &columns, 8),
            Ok(pool + slot0 + 125 * 8)
        );
        let empty = [[[0, 0]; 3]; MAX_MATTER_SLOTS];
        assert_eq!(
            fill_points([256, 32, 256], 32, &empty, 8),
            Ok(EXACT_F32_COUNT)
        );
        let error = fill_points([256, 33, 256], 33, &empty, 8).unwrap_err();
        assert!(
            error.contains("exact f32 range") && error.contains("Cell Size"),
            "{error}"
        );
    }

    /// Two Sands' fills on one solver: the grid covers both, each keeps its
    /// own box, and the draws' runs tile every point.
    #[test]
    fn two_fills_share_one_grid() {
        let place = |x: f32| Transform {
            pos: [x, 1.0, 0.0],
            scale: [0.5; 3],
            ..Transform::default()
        };
        let mut fills = [None; MAX_MATTER_SLOTS];
        fills[0] = Some(place(-3.0));
        fills[1] = Some(place(3.0));
        let read = |_: &str, default: f32| default;
        let mut params = ParamValues::default();
        params.insert("bounds".into(), ParamValue::Bool(false));
        let geometry = matter_geometry(read, &params, None, &fills).expect("fills");
        let layout = geometry.layout;
        assert!(layout.min[0] < -3.25 && layout.min[0] + layout.size[0] > 3.25);
        assert!(
            geometry
                .columns
                .iter()
                .take(2)
                .all(|c| c.iter().all(|[lo, hi]| hi > lo))
        );
        let ranges = geometry.slot_ranges();
        assert_eq!(ranges[0][0], 0);
        assert_eq!(ranges[1][0], ranges[0][1]);
        assert_eq!(ranges[1][0] + ranges[1][1], geometry.points);
        assert_eq!(ranges[0][1], ranges[1][1]);
    }

    /// The fill gizmos are the cells the fill seeds: the pool up to its cell
    /// layers and the column within half a cell of the authored box.
    #[test]
    fn matter_fill_gizmos_are_the_seeded_cells() {
        let column = Transform {
            pos: [-1.25, 1.12, 0.0],
            scale: [1.18, 1.92, 3.5],
            ..Transform::default()
        };
        let read = |name: &str, default: f32| if name == "fill_height" { 0.16 } else { default };
        let mut fills = [None; MAX_MATTER_SLOTS];
        fills[0] = Some(column);
        let geometry = matter_geometry(read, &ParamValues::default(), None, &fills).expect("fills");
        let mut domain = MatterDomain::new();
        (domain.setup, domain.layout) = (Some(geometry.setup), Some(geometry.layout));
        domain.fill_cells = (geometry.pool_cells, geometry.columns);
        assert_eq!(
            domain.fluid_domain_snapshot().unwrap().accepted_layout,
            Some(geometry.layout)
        );
        let mut out = Vec::new();
        domain.write_physics_gizmos(&mut out);
        let boxes: Vec<_> = out
            .iter()
            .map(|gizmo| match (gizmo.family, gizmo.shape) {
                (PhysicsGizmoFamily::Fill, PhysicsGizmoShape::Box { center, half, .. }) => {
                    (center, half)
                }
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(boxes.len(), 2);
        let (layout, dx) = (geometry.layout, geometry.layout.cell_size as f32);
        let pool_top = boxes[0].0[1] + boxes[0].1[1];
        assert!((pool_top - (layout.min[1] + geometry.pool_cells as f32 * dx)).abs() < 1e-6);
        assert!((pool_top - layout.min[1] - 0.16).abs() <= 0.5 * dx);
        for axis in 0..3 {
            assert!(
                (boxes[1].0[axis] - column.pos[axis]).abs() <= 0.5 * dx,
                "axis {axis}: {:?}",
                boxes[1]
            );
        }
    }
}
