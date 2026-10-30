//! `node.gpu_flip_step` as a primitive: install, run and outputs.

use super::*;
use crate::primitives::gpu_flip_pressure::max_solve_level;
use manifold_node_engine::scene::world_tiles::lattice_origin;
use manifold_water_liquid::primitives::particle_identity::IDENTITY_BYTES;
use manifold_water_liquid::substep_history::PooledFaces;
use manifold_water_liquid::tile_pool::pool_tiles_wire;

impl Primitive for GpuFlipStep {
    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        self.state.prepare_pipelines(device);
        self.state.history.prepare(device);
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(
            port,
            "faces" | "distance" | "tiles" | "interior" | "substep_schedule" | "substep_faces"
        )
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "faces" => self.state.lattice.as_ref().map(|l| &l.faces),
            "distance" => self.state.lattice.as_ref().map(|l| &l.phi),
            "tiles" => self.state.tiles.as_ref().map(|t| t.table()),
            "interior" => self.state.narrow_band.interior(),
            "substep_schedule" => self.state.history.schedule.as_ref(),
            "substep_faces" => self.state.history.faces.as_ref(),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        match port {
            "out" => inputs
                .iter()
                .find(|(name, _)| *name == "particles")
                .map(|&(_, n)| n),
            // Provided storage: a one-record hint, sized to the lattice at run time.
            "faces" | "distance" | "tiles" | "interior" | "substep_schedule" | "substep_faces" => {
                Some(1)
            }
            "reaction_out" => inputs
                .iter()
                .find(|(name, _)| *name == "reaction")
                .map(|&(_, n)| n),
            "capped" => inputs
                .iter()
                .find(|(name, _)| *name == "particles")
                .map(|&(_, n)| n.saturating_mul(2).saturating_add(SOLVER_WORDS)),
            "clock_status" => Some(8),
            "identity_out" => Some((IDENTITY_BYTES / 4) as u32),
            _ => None,
        }
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("reaction", "reaction_out"), ("identity", "identity_out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let h = ctx.scalar_or_param("cell_size", 0.0625);
        if !(h.is_finite() && h > 0.0) {
            ctx.error(format!("{NAME}: cell_size is {h}; it must be positive"));
            return;
        }
        // The domain's pool is where the step starts; it runs on what its
        // water holds, up to the growth ceiling.
        let start = match pool_tiles_wire(|name, default| ctx.scalar_or_param(name, default)) {
            Ok(pool) => pool,
            Err(error) => {
                ctx.error(format!("{NAME}: {error}"));
                return;
            }
        };
        let wall_faces = match read_closed_faces(ctx.scalar_or_param("closed_faces", 0.0)) {
            Ok(mask) => mask,
            Err(error) => {
                ctx.error(format!("{NAME}: {error}"));
                return;
            }
        };
        let open_faces = match read_open_faces(ctx.scalar_or_param("open_faces", 0.0)) {
            Ok(mask) => mask,
            Err(error) => {
                ctx.error(format!("{NAME}: {error}"));
                return;
            }
        };
        let memory = ctx.gpu_encoder().device.modifier_memory_snapshot();
        let (next, refused) = self.state.next_pool(
            start,
            tiles::growth_ceiling(start, wall_faces, open_faces),
            memory,
        );
        if let Some(refused) = refused {
            ctx.error(format!("{NAME}: {refused}"));
        }
        let (pool, narrow_band) = self
            .state
            .narrow_band
            .plan(ctx.scalar_or_param("narrow_band", 0.0) > 0.5, next);
        let field = match ForceLatticeBinding::read(
            ctx,
            ctx.inputs.array("forces"),
            ctx.inputs.array("impulses"),
            NAME,
        ) {
            Ok(field) => field,
            Err(error) => {
                ctx.error(error);
                return;
            }
        };
        let (Some(particles), Some(out), Some(capped)) = (
            ctx.inputs.array("particles"),
            ctx.outputs.array("out"),
            ctx.outputs.array("capped"),
        ) else {
            return;
        };
        let particle_bytes = size_of::<FluidParticle>() as u64;
        let Ok(capacity) = u32::try_from(particles.size / particle_bytes) else {
            ctx.error(format!(
                "{NAME}: particle storage exceeds 32-bit slot addressing"
            ));
            return;
        };
        let out_slots = (out.size / particle_bytes).min(u64::from(capacity)) as u32;
        let sheet_rate = match read_sheet_fill_rate(ctx.scalar_or_param("sheet_fill_rate", 0.0)) {
            Ok(rate) => rate,
            Err(error) => {
                ctx.error(format!("{NAME}: {error}"));
                return;
            }
        };
        let tally = 2 * u64::from(capacity) * 4;
        if capped.size < tally + u64::from(SOLVER_WORDS) * 4 {
            ctx.error(format!("{NAME}: the capped array holds fewer than two words a particle slot and {SOLVER_WORDS} solver words"));
            return;
        }
        let steps = ctx.scalar_or_param("steps", 1.0).round().clamp(1.0, 64.0);
        // A graph saved without the domain's interval wire runs on the project's Sim Rate.
        let interval_duration =
            f64::from(ctx.scalar_or_param("interval_duration", ctx.sim_step.interval.0 as f32));
        let step_dt = (interval_duration / f64::from(steps)) as f32;
        // 0 (export, or a graph without the wire) measures the whole frame.
        let limit_interval = Some(ctx.scalar_or_param("limit_interval", 0.0))
            .filter(|limit| limit.is_finite() && *limit > 0.0)
            .unwrap_or(0.0);
        let gravity = [("gravity_x", 0.0), ("gravity_y", -9.81), ("gravity_z", 0.0)]
            .map(|(name, default)| ctx.scalar_or_param(name, default));
        let tick_index = ctx.scalar_or_param("tick_index", 0.0).round().max(0.0) as i32;
        let body_count = ctx
            .scalar_or_param("body_count", 0.0)
            .round()
            .clamp(0.0, MAX_LIQUID_BODIES as f32) as i32;
        let rows = ctx.scalar_or_param("rows", 0.0).round().max(0.0) as i32;
        let pressure =
            match read_max_iterations(ctx.scalar_or_param("max_iterations", MAX_ITERATIONS as f32))
                .and_then(|max| read_iterations(ctx.scalar_or_param("iterations", 0.0), max))
            {
                Ok(iterations) => iterations,
                Err(error) => {
                    ctx.error(format!("{NAME}: {error}"));
                    return;
                }
            };
        let ghost = ctx.scalar_or_param("ghost_fluid", 1.0) > 0.5;
        let [bounds_lo, bounds_hi] = [
            ["bounds_lo_x", "bounds_lo_y", "bounds_lo_z"],
            ["bounds_hi_x", "bounds_hi_y", "bounds_hi_z"],
        ]
        .map(|names| names.map(|name| ctx.scalar_or_param(name, 0.0)));
        let [wall_velocity_lo, wall_velocity_hi] = [
            [
                "wall_velocity_lo_x",
                "wall_velocity_lo_y",
                "wall_velocity_lo_z",
            ],
            [
                "wall_velocity_hi_x",
                "wall_velocity_hi_y",
                "wall_velocity_hi_z",
            ],
        ]
        .map(|names| names.map(|name| ctx.scalar_or_param(name, 0.0)));
        let kill_height = ctx.scalar_or_param("kill_height", f32::MIN);
        if !bounds_lo
            .iter()
            .chain(&bounds_hi)
            .chain(&wall_velocity_lo)
            .chain(&wall_velocity_hi)
            .all(|v| v.is_finite())
            || kill_height.is_nan()
        {
            ctx.error(format!(
                "{NAME}: Bounds, its wall velocities and Kill Height must be numbers"
            ));
            return;
        }
        // Refused against the domain's pool, as the domain refuses it; a pool
        // that follows the water may hold fewer levels, and the gradient runs
        // on the deepest it has.
        let level = match read_solve_level(ctx.scalar_or_param("solve_level", 0.0), start) {
            Ok(level) => level.min(max_solve_level(pool)),
            Err(error) => {
                ctx.error(format!("{NAME}: {error}"));
                return;
            }
        };
        let regions_in = ctx.inputs.array("regions");
        let region_sites_in = ctx.inputs.array("region_sites");
        let region_count = match regions_in {
            Some(_) => ctx
                .scalar_or_param("region_count", 0.0)
                .round()
                .clamp(0.0, i32::MAX as f32) as i32,
            None => 0,
        };
        // The most sites one tick's inflows enumerate, at most the sites the
        // wired rows' boxes hold; no emission without them.
        let emit_sites = match region_sites_in {
            Some(_) if region_count > 0 => ctx
                .scalar_or_param("region_site_total", 0.0)
                .round()
                .clamp(0.0, manifold_node_engine::ports::EXACT_F32_COUNT as f32)
                as u32,
            _ => 0,
        };
        let band = step_band();
        let sand = ctx.inputs.array("sand");
        let sand_count = sand.map_or(0, |points| {
            ctx.scalar_or_param("sand_count", 0.0).round().clamp(
                0.0,
                (points.size / size_of::<MatterGrain>() as u64) as f32,
            ) as u32
        });
        let gpu = ctx.gpu_encoder();
        if let Err(error) = self.state.reserve(
            gpu.device,
            gpu.native_enc,
            pool,
            u64::from(capacity),
            sheet_rate > 0.0,
            narrow_band,
            sand.is_some(),
            tick_index == 0,
        ) {
            ctx.error(format!(
                "{NAME}: a pool of {pool} tiles with {capacity} particle slots needs storage the device cannot give: {error}. Raise Cell Size."
            ));
            return;
        }
        let zeros = self.state.zeros.clone().expect("zeros prepared");
        let Some(identity) = ctx
            .inputs
            .array("identity")
            .filter(|b| b.size >= IDENTITY_BYTES)
        else {
            ctx.error("GPU FLIP Step: persistent liquid_state identity is required");
            return;
        };
        let clock_obstacles = ctx.inputs.array("clock_obstacles").unwrap_or(&zeros);
        let clock_sources = ctx.inputs.array("clock_sources").unwrap_or(&zeros);
        let obstacle_count = ctx
            .scalar_or_param("clock_obstacle_count", 0.0)
            .round()
            .max(0.0)
            .min(
                (clock_obstacles.size
                    / size_of::<manifold_water_liquid::bodies::GpuFlipBodyVertex>() as u64)
                    as f32,
            ) as u32;
        let source_count = ctx
            .scalar_or_param("clock_source_count", 0.0)
            .round()
            .max(0.0)
            .min(
                (clock_sources.size
                    / size_of::<manifold_water_liquid::bodies::GpuFlipBodyVertex>() as u64)
                    as f32,
            ) as u32;
        let live_hits = ctx.inputs.array("live_hits").unwrap_or(&zeros);
        let live_hit_count = ctx
            .scalar_or_param("live_hit_count", 0.0)
            .round()
            .max(0.0)
            .min((live_hits.size / 16) as f32) as u32;
        {
            let capacities = [capacity, obstacle_count.max(1), source_count.max(1)];
            if self.state.clock.is_none()
                || self
                    .state
                    .clock_capacities
                    .iter()
                    .zip(capacities)
                    .any(|(old, new)| *old < new)
            {
                self.state.clock = Some(GpuFlipClock::new(
                    ctx.gpu_encoder().device,
                    capacities[0],
                    capacities[1],
                    capacities[2],
                ));
                self.state.clock_capacities = capacities;
            }
        }
        let bodies_in = ctx.inputs.array("bodies");
        let shapes_in = ctx.inputs.array("shapes");
        let atlas_in = ctx.inputs.array("atlas");
        let nodes_in = ctx.inputs.array("collider_nodes");
        let triangles_in = ctx.inputs.array("collider_triangles");
        let (
            bodies,
            shapes,
            atlas,
            collider_nodes,
            collider_triangles,
            body_count,
            rows,
            shapes_len,
        ) = match (bodies_in, shapes_in, atlas_in, nodes_in, triangles_in) {
            (Some(bodies), Some(shapes), Some(atlas), Some(nodes), Some(triangles)) => {
                let held =
                    (bodies.size / size_of::<LiquidBody>() as u64).min(i32::MAX as u64) as i32;
                let shapes_len =
                    (shapes.size / size_of::<LiquidShape>() as u64).min(u64::from(u32::MAX)) as u32;
                // Rows are tick major: this tick's are the last of the prefix
                // the solid passes read.
                let through_tick = (tick_index - field.first_tick + 1)
                    .max(1)
                    .saturating_mul(body_count);
                (
                    bodies,
                    shapes,
                    atlas,
                    nodes,
                    triangles,
                    body_count,
                    rows.min(held).min(through_tick),
                    shapes_len,
                )
            }
            _ => (&zeros, &zeros, &zeros, &zeros, &zeros, 0, 0, 0),
        };
        let reaction_in = ctx.inputs.array("reaction");
        let dynamic = body_count > 0 && ctx.scalar_or_param("dynamic_bodies", 0.0) > 0.0;
        if dynamic && let Some(reason) = body_refusal(body_count as u32, reaction_in) {
            ctx.error(format!("{NAME}: {reason}"));
            return;
        }
        let reaction = reaction_in.unwrap_or(&zeros);
        let regions = regions_in.unwrap_or(&zeros);
        // The engine refuses a negative jitter (setMarkerParticleJitterFactor).
        let inflow_jitter = ctx.scalar_or_param("inflow_jitter", 0.0);
        if !(inflow_jitter.is_finite() && inflow_jitter >= 0.0) {
            ctx.error(format!(
                "{NAME}: Inflow Jitter must be 0 or more, not {inflow_jitter}"
            ));
            return;
        }
        let region_rows =
            (regions.size / size_of::<LiquidBody>() as u64).min(i32::MAX as u64) as i32;
        let region_sites = region_sites_in.unwrap_or(&zeros);
        let pool = self
            .state
            .tiles
            .as_ref()
            .expect("tiles reserved")
            .pool_tiles();
        let tension = surface_tension(h, gravity);
        let step = Step {
            identity,
            params: StepParams {
                bounds_lo,
                capacity,
                bounds_hi,
                cell_size: h,
                gravity,
                step_dt,
                field_nodes: field.nodes.map(|n| n as u32),
                field_spacing: field.spacing,
                origin: lattice_origin(h),
                inv_cell: 1.0 / h,
                tick_index,
                step_in_tick: 0,
                force_lattices: field.force_lattices,
                impulse_tick: field.impulse_tick,
                first_tick: field.first_tick,
                body_count,
                rows,
                tick_seconds: step_dt,
                _pad0: 0,
                ghost: u32::from(ghost),
                particles: out_slots,
                shapes_len,
                // The whole density error each step: projected to rest, no
                // per-step share.
                rate: 1.0 / step_dt,
                wall_faces,
                region_count,
                region_rows,
                emit_jitter: 0.25 * inflow_jitter,
                solve_level: level as u32,
                extension_layers: band,
                ring_max: ring_max(band),
                live_impulse_stride: field
                    .nodes
                    .iter()
                    .map(|&n| n as usize)
                    .product::<usize>()
                    .saturating_mul(4)
                    .min(u32::MAX as usize) as u32,
                open_faces,
                coupled_h: manifold_physics::coupled_motion::coupled_substep(
                    manifold_physics::Seconds(interval_duration),
                ),
                pool_tiles: pool,
                // The force and impulse lattices start at the domain's scene box,
                // which bounds_lo carries.
                field_min: bounds_lo,
                table_entries: manifold_water_liquid::tile_storage::table_entries(pool),
                emit_sites,
                kill_height,
                narrow_band,
                band_cells: narrow_band::band_cells() as f32,
                wall_velocity_lo,
                _pad_lo: 0,
                wall_velocity_hi,
                _pad_hi: 0,
                // The encode decides: it knows the buffers it last built from.
                static_all: 0,
                tension,
                sand_count,
                sand_drag: manifold_core::liquid_domain::SAND_WATER_DRAG,
            },
            clock_plan: &zeros,
            particles,
            out,
            capped,
            forces: field.forces.unwrap_or(&zeros),
            impulses: field.impulses.unwrap_or(&zeros),
            bodies,
            regions,
            region_sites,
            shapes,
            atlas,
            collider_nodes,
            collider_triangles,
            reaction,
            contacts: ctx.inputs.array("contacts").unwrap_or(&zeros),
            dynamic,
            pressure,
            level,
            tally,
            ghost,
            density: ctx.scalar_or_param("volume_projection", 0.0) > 0.5,
            sheet_rate,
            sand: sand.filter(|_| sand_count > 0),
        };
        let clock_status = ctx.outputs.array("clock_status").cloned();
        let body_row_offset = u64::try_from(rows.saturating_sub(body_count).max(0))
            .unwrap_or(0)
            .saturating_mul(size_of::<LiquidBody>() as u64);
        // The history holds the most substeps the clock can schedule. Export
        // encodes them all; live encodes the numerical steps the newest
        // retired tick's speed asks for (live_steps), and the hits. A tick
        // with a hit takes every numerical step: the retired speed is from
        // before the hit, and too few slots would stretch the last step over
        // the rest of the tick. The plan gates the slots the clock leaves no
        // time for on the GPU.
        let rule = history_slots(live_hit_count);
        let history_slots = encoded_slots(rule);
        let frame_clock = ctx
            .gpu_encoder()
            .device
            .frame_clock()
            .filter(|_| !ctx.sim_step.offline());
        let numerical_steps = match &frame_clock {
            Some(clock) if history_slots == rule => {
                let speed = self.state.poll_plans(clock);
                if live_hit_count > 0 {
                    manifold_physics::stepping::LIVE_DEFAULT_MAX_STEPS
                } else {
                    live_steps(speed, interval_duration as f32, h, steps as u32)
                }
            }
            _ => manifold_physics::stepping::LIVE_DEFAULT_MAX_STEPS,
        };
        let encoded_steps = (numerical_steps + live_hit_count).min(history_slots);
        log::debug!(target: "gpu_flip::schedule",
            "tick={tick_index} slots={encoded_steps}/{history_slots} interval={interval_duration} h={h} minimum={steps} hits={live_hit_count} sources={source_count}");
        ctx.outputs
            .set_scalar("substep_count", ParamValue::Float(encoded_steps as f32));
        ctx.outputs
            .set_scalar("pool_tiles", ParamValue::Float(pool as f32));
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        if let Err(error) = self
            .state
            .reserve_box_points(ctx.gpu_encoder().device, &step.params)
        {
            ctx.error(format!("{NAME}: {error}"));
            return;
        }
        // Only a wired history output has a slot; with no reader the rows,
        // their capture and the remap cost nothing.
        let keep_history = ctx.outputs.slot("substep_faces").is_some()
            || ctx.outputs.slot("substep_schedule").is_some();
        let gpu = ctx.gpu_encoder();
        if keep_history {
            self.state.history.prepare(gpu.device);
            if let Err(error) = self.state.history.reserve(gpu.device, pool, history_slots) {
                ctx.error(format!("{NAME}: {error}"));
                return;
            }
        }
        // The first substep reads the tick's particles, every later one the
        // last one's out. The reaction is in place, so the bodies feel every
        // substep.
        let clock_params = GpuFlipClockParams {
            frame_duration: interval_duration as f32,
            cell_size: h,
            cfl: ENGINE_CFL as f32,
            surface_condition: TENSION_STABILITY,
            surface_constant: tension,
            color_mixing_rate: 0.0,
            encoded_steps: numerical_steps,
            limit_interval,
            min_frame_steps: steps as u32,
            max_frame_steps: manifold_physics::stepping::LIVE_DEFAULT_MAX_STEPS,
            flags: if tick_index == 0 {
                clock_flags::FIRST_SUBSTEP | clock_flags::FLUID_PRESENT_OR_GENERATING
            } else {
                clock_flags::FLUID_PRESENT_OR_GENERATING
            } | if ghost && tension > 0.0 {
                clock_flags::SURFACE_TENSION
            } else {
                0
            },
            interval_sequence: tick_index as u32,
            constant_force: [gravity[0], gravity[1], gravity[2], 0.0],
        };
        let mut last_clock_plan = zeros.clone();
        let slot_record = self
            .state
            .slot_record
            .clone()
            .expect("slot record reserved");
        // Stage tags only name profiled spans; an unprofiled encoder ignores
        // them, and they touch no dispatch, binding or replay key.
        gpu.native_enc.set_profile_tag("gpu_flip.stage.clock");
        {
            let clock = self.state.clock.as_ref().expect("live clock prepared");
            clock.begin_frame(gpu.native_enc, &clock_params);
        }
        for k in 0..encoded_steps as i32 {
            let particles_for_step = if k > 0 { out } else { particles };
            let params = StepParams {
                step_in_tick: k,
                tick_seconds: (k + 1) as f32 * step_dt,
                ..step.params
            };
            gpu.native_enc.set_profile_tag("gpu_flip.stage.clock");
            {
                if k == 0 {
                    self.state.load_slot(gpu.native_enc, identity);
                }
                let plan_buffer = {
                    let clock = self.state.clock.as_ref().expect("live clock prepared");
                    let plan = clock.dispatch(
                        gpu.native_enc,
                        GpuFlipClockInputs {
                            marker_particles: particles_for_step,
                            marker_count: &slot_record,
                            obstacle_vertices: clock_obstacles,
                            obstacle_count,
                            source_vertices: clock_sources,
                            // Only the first numerical slot can use initial
                            // source prediction. Later slots use markers.
                            source_count: if k == 0 { source_count } else { 0 },
                            live_hits,
                            live_hit_count,
                            event_impulses: field.impulses.unwrap_or(&zeros),
                            impulse_stride: step.params.live_impulse_stride,
                            impulse_nodes: step.params.field_nodes,
                            impulse_origin: step.params.field_min,
                            impulse_spacing: step.params.field_spacing,
                            body_rows: bodies,
                            body_rows_offset: body_row_offset,
                            body_reaction: reaction,
                        },
                        &clock_params,
                    );
                    plan.buffer().clone()
                };
                self.state.open_slot(gpu.native_enc, &plan_buffer, identity);
                let iteration = Step {
                    params,
                    particles: particles_for_step,
                    clock_plan: &plan_buffer,
                    ..step
                };
                if let Err(error) =
                    self.state
                        .encode(gpu.device, gpu.native_enc, &iteration, &clock_params)
                {
                    ctx.error(format!("{NAME}: {error}"));
                    return;
                }
                gpu.native_enc.set_profile_tag("gpu_flip.stage.finish");
                if keep_history {
                    self.state.history.capture(
                        gpu.native_enc,
                        k as u32,
                        &plan_buffer,
                        &PooledFaces {
                            faces: &self.state.lattice.as_ref().expect("lattice prepared").faces,
                            table: self.state.tiles.as_ref().expect("tiles prepared").table(),
                            pool_tiles: pool,
                        },
                    );
                }
                last_clock_plan = plan_buffer;
            }
        }
        self.state.narrow_band.finish(narrow_band);
        // Tiles change slot, come and go between substeps: every captured
        // row moves onto the table the step publishes.
        if keep_history {
            self.state.history.remap(
                gpu.native_enc,
                self.state.tiles.as_ref().expect("tiles prepared").table(),
                encoded_steps,
            );
        }
        if let Some(status) = clock_status {
            gpu.native_enc
                .copy_buffer_to_buffer(&last_clock_plan, &status, 32);
        }
        if let Some(clock) = &frame_clock {
            self.state
                .read_plan(gpu.device, gpu.native_enc, &last_clock_plan, clock);
        }
    }
}
