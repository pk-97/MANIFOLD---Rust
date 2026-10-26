//! `node.gpu_flip_step` as a primitive: install, run and outputs.

use super::*;

impl Primitive for GpuFlipStep {
    fn prepare_pipelines(&mut self, device: &GpuDevice) {
        self.state.prepare_pipelines(device);
        self.state.history.prepare(device);
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(
            port,
            "faces" | "distance" | "substep_schedule" | "substep_u" | "substep_v" | "substep_w"
        )
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "faces" => self.state.faces.as_ref(),
            "distance" => self.state.distance.as_ref(),
            "substep_schedule" => self.state.history.schedule.as_ref(),
            "substep_u" | "substep_v" | "substep_w" => self.state.history.faces.as_ref().map(|f| {
                &f[match port {
                    "substep_u" => 0,
                    "substep_v" => 1,
                    _ => 2,
                }]
            }),
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
            "faces" | "distance" | "substep_schedule" | "substep_u" | "substep_v" | "substep_w" => {
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
            "identity_out" => Some(4),
            _ => None,
        }
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("reaction", "reaction_out"), ("identity", "identity_out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(lattice) = LiquidLattice::from_wires(ctx, NAME) else {
            return;
        };
        let solver = FlipSolverGrid::from_lattice(lattice);
        let cells = solver.cells();
        ctx.outputs.set_transform("grid_bounds", solver.bounds());
        for (port, value) in ["grid_nodes_x", "grid_nodes_y", "grid_nodes_z"]
            .into_iter()
            .zip(solver.nodes())
            .chain(
                ["face_cells_x", "face_cells_y", "face_cells_z"]
                    .into_iter()
                    .zip(cells),
            )
            .chain([("face_valid_layers", FACE_VALID_LAYERS)])
        {
            ctx.outputs
                .set_scalar(port, ParamValue::Float(value as f32));
        }
        if let Some(reason) = lattice_refusal(cells) {
            ctx.error(format!("{NAME}: {reason}. Lower Resolution."));
            return;
        }
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
        let count = match ctx.inputs.scalar("count") {
            Some(ParamValue::Float(count)) if count.is_finite() => {
                (count.max(0.0) as u32).min(capacity)
            }
            _ => capacity,
        };
        let steps = ctx.scalar_or_param("steps", 1.0).round().clamp(1.0, 64.0);
        // A graph saved without the domain's interval wire runs on the project's Sim Rate.
        let interval_duration =
            f64::from(ctx.scalar_or_param("interval_duration", ctx.sim_step.interval.0 as f32));
        let step_dt = (interval_duration / f64::from(steps)) as f32;
        // 0 (export, or a graph without the wire) measures the whole frame.
        let limit_interval = Some(ctx.scalar_or_param("limit_interval", 0.0))
            .filter(|limit| limit.is_finite() && *limit > 0.0)
            .unwrap_or(0.0);
        let flip = ctx.scalar_or_param("flip", 0.95).clamp(0.0, 1.0);
        let h = lattice.cell_size();
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
        let closed_faces = match read_closed_faces(ctx.scalar_or_param("closed_faces", 63.0)) {
            Ok(mask) => mask,
            Err(error) => {
                ctx.error(format!("{NAME}: {error}"));
                return;
            }
        };
        let level = match read_solve_level(ctx.scalar_or_param("solve_level", 0.0), cells) {
            Ok(level) => level,
            Err(error) => {
                ctx.error(format!("{NAME}: {error}"));
                return;
            }
        };
        let box_min = solver.min();
        let regions_in = ctx.inputs.array("regions");
        let region_count = match regions_in {
            Some(_) => ctx
                .scalar_or_param("region_count", 0.0)
                .round()
                .clamp(0.0, i32::MAX as f32) as i32,
            None => 0,
        };
        let band = band_layers(ENGINE_CFL).max(FACE_VALID_LAYERS);
        if let Err(error) = self.state.reserve(
            ctx.gpu_encoder().device,
            cells,
            u64::from(capacity),
            ring_max(band),
            region_count > 0,
            sheet_rate > 0.0,
        ) {
            ctx.error(format!(
                "{NAME}: a {}×{}×{} lattice with {capacity} particle slots needs storage the device cannot give: {error}. Lower Resolution.",
                cells[0], cells[1], cells[2]
            ));
            return;
        }
        let zeros = self.state.zeros.clone().expect("zeros prepared");
        let Some(identity) = ctx.inputs.array("identity").filter(|b| b.size >= 16) else {
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
        let (bodies, shapes, atlas, body_count, rows, shapes_len) =
            match (bodies_in, shapes_in, atlas_in) {
                (Some(bodies), Some(shapes), Some(atlas)) => {
                    let held =
                        (bodies.size / size_of::<LiquidBody>() as u64).min(i32::MAX as u64) as i32;
                    let shapes_len = (shapes.size / size_of::<LiquidShape>() as u64)
                        .min(u64::from(u32::MAX)) as u32;
                    // Rows are tick major: this tick's are the last of the prefix
                    // the solid passes read.
                    let through_tick = (tick_index - field.first_tick + 1)
                        .max(1)
                        .saturating_mul(body_count);
                    (
                        bodies,
                        shapes,
                        atlas,
                        body_count,
                        rows.min(held).min(through_tick),
                        shapes_len,
                    )
                }
                _ => (&zeros, &zeros, &zeros, 0, 0, 0),
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
        let mut step = Step {
            identity,
            params: StepParams {
                n: cells,
                capacity,
                box_min,
                cell_size: h,
                gravity,
                step_dt,
                field_nodes: field.nodes.map(|n| n as u32),
                field_spacing: field.spacing,
                tick_index,
                step_in_tick: 0,
                force_lattices: field.force_lattices,
                impulse_tick: field.impulse_tick,
                first_tick: field.first_tick,
                body_count,
                rows,
                tick_seconds: step_dt,
                // The share is per step, as the engine's `_ratioPICFLIP`, whatever
                // the step count.
                flip,
                box_offset: box_min.iter().fold(0.0_f32, |m, v| m.max(v.abs())),
                ghost: u32::from(ghost),
                particles: out_slots,
                shapes_len,
                // The whole density error each step: projected to rest, no
                // per-step share.
                rate: 1.0 / step_dt,
                closed_faces,
                region_count,
                region_rows,
                emit_jitter: 0.25 * inflow_jitter,
                solve_level: level as u32,
                all_tiles: u32::from(all_tiles()),
                extension_layers: band,
                ring_max: ring_max(band),
                live_impulse_stride: field
                    .nodes
                    .iter()
                    .map(|&n| n as usize)
                    .product::<usize>()
                    .saturating_mul(4)
                    .min(u32::MAX as usize) as u32,
                wall_inset: solver.wall_inset(),
                coupled_h: manifold_physics::coupled_motion::coupled_substep(
                    manifold_physics::Seconds(interval_duration),
                ),
                pool_tiles: pool_tiles(cells),
                alloc_ring: ALLOC_RING,
                _pad: 0,
            },
            clock_plan: &zeros,
            particles,
            out,
            capped,
            count,
            forces: field.forces.unwrap_or(&zeros),
            impulses: field.impulses.unwrap_or(&zeros),
            bodies,
            regions,
            shapes,
            atlas,
            reaction,
            contacts: ctx.inputs.array("contacts").unwrap_or(&zeros),
            dynamic,
            pressure,
            level,
            wall_inset: solver.wall_inset(),
            tally,
            band,
            ghost,
            density: ctx.scalar_or_param("volume_projection", 0.0) > 0.5,
            sheet_rate,
        };
        let clock_status = ctx.outputs.array("clock_status").cloned();
        let body_row_offset = u64::try_from(rows.saturating_sub(body_count).max(0))
            .unwrap_or(0)
            .saturating_mul(size_of::<LiquidBody>() as u64);
        // LiquidState supplies -1 unless its fenced stats describe these exact
        // incoming particles. Later intervals in the same frame, skipped
        // readbacks, reset/recovery, and an unwired custom graph take the full
        // GPU-adaptive path. Forces applied inside this step do not change the
        // first scheduler decision: it reads the incoming markers. Dynamic
        // body reactions later in the step do not retroactively change that
        // decision. Domain's initial obstacle bound includes current linear,
        // angular and external acceleration, with reaction cleared before this
        // first interval. A missing bound retains the full adaptive loop.
        let obstacle_speed = if obstacle_count == 0 {
            0.0
        } else {
            ctx.scalar_or_param("initial_obstacle_speed", -1.0)
        };
        let one_step = tick_index > 0
            && steps == 1.0
            && live_hit_count == 0
            && source_count == 0
            && one_step_cfl_safe(
                ctx.scalar_or_param("retired_max_speed", -1.0),
                obstacle_speed,
                interval_duration as f32,
                h,
            );
        let history_slots = if one_step {
            1
        } else {
            manifold_physics::stepping::LIVE_DEFAULT_MAX_STEPS + live_hit_count
        };
        log::debug!(target: "gpu_flip::schedule",
            "tick={tick_index} slots={history_slots} interval={interval_duration} h={h} marker={} obstacle={obstacle_speed} minimum={steps} hits={live_hit_count} sources={source_count}",
            ctx.scalar_or_param("retired_max_speed", -1.0));
        ctx.outputs
            .set_scalar("substep_count", ParamValue::Float(history_slots as f32));
        let gpu = ctx.gpu_encoder();
        self.state.history.prepare(gpu.device);
        if let Err(error) = self
            .state
            .history
            .reserve(gpu.device, step.params.n, history_slots)
        {
            ctx.error(format!("{NAME}: {error}"));
            return;
        }
        // The first substep reads the tick's particles, every later one the
        // last one's out. The reaction is in place, so the bodies feel every
        // substep.
        let clock_params = GpuFlipClockParams {
            frame_duration: interval_duration as f32,
            cell_size: h,
            cfl: ENGINE_CFL as f32,
            surface_condition: 1.0,
            surface_constant: 1.0,
            color_mixing_rate: 0.0,
            _pad_prediction: 0.0,
            limit_interval,
            min_frame_steps: steps as u32,
            max_frame_steps: manifold_physics::stepping::LIVE_DEFAULT_MAX_STEPS,
            flags: if tick_index == 0 {
                clock_flags::FIRST_SUBSTEP | clock_flags::FLUID_PRESENT_OR_GENERATING
            } else {
                clock_flags::FLUID_PRESENT_OR_GENERATING
            },
            interval_sequence: tick_index as u32,
            constant_force: [gravity[0], gravity[1], gravity[2], 0.0],
        };
        let mut last_clock_plan = zeros.clone();
        let encoded_steps = history_slots;
        // Stage tags only name profiled spans; an unprofiled encoder ignores
        // them, and they touch no dispatch, binding or replay key.
        gpu.native_enc.set_profile_tag("gpu_flip.stage.clock");
        {
            let clock = self.state.clock.as_ref().expect("live clock prepared");
            clock.begin_frame(gpu.native_enc, &clock_params);
        }
        for k in 0..encoded_steps as i32 {
            step.count = if k > 0 { out_slots } else { count };
            let particles_for_step = if k > 0 { out } else { particles };
            let params = StepParams {
                step_in_tick: k,
                tick_seconds: (k + 1) as f32 * step_dt,
                ..step.params
            };
            gpu.native_enc.set_profile_tag("gpu_flip.stage.clock");
            {
                let plan_buffer = {
                    let clock = self.state.clock.as_ref().expect("live clock prepared");
                    let plan = clock.dispatch(
                        gpu.native_enc,
                        GpuFlipClockInputs {
                            marker_particles: particles_for_step,
                            marker_count: step.count,
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
                            impulse_origin: step.params.box_min,
                            impulse_spacing: step.params.field_spacing,
                            body_rows: bodies,
                            body_rows_offset: body_row_offset,
                            body_reaction: reaction,
                        },
                        &clock_params,
                    );
                    plan.buffer().clone()
                };
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
                self.state.commit_mask(
                    gpu.native_enc,
                    &plan_buffer,
                    out,
                    self.state
                        .saved_particles
                        .as_ref()
                        .expect("particle mask prepared"),
                    "gpu_flip.step.mask.particles",
                );
                // Public faces and body reactions already gate every write
                // on the actual clock plan. Inactive slots leave them intact;
                // they need neither an active-step backup nor a restore pass.
                self.state.commit_mask(
                    gpu.native_enc,
                    &plan_buffer,
                    &self.state.lattice.as_ref().expect("lattice prepared").phi,
                    self.state
                        .saved_distance
                        .as_ref()
                        .expect("distance mask prepared"),
                    "gpu_flip.step.mask.distance",
                );
                self.state.commit_mask(
                    gpu.native_enc,
                    &plan_buffer,
                    step.capped,
                    self.state
                        .saved_capped
                        .as_ref()
                        .expect("capped mask prepared"),
                    "gpu_flip.step.mask.capped",
                );
                self.state.history.capture(
                    gpu.native_enc,
                    k as u32,
                    &plan_buffer,
                    self.state.faces.as_ref().expect("faces prepared"),
                );
                last_clock_plan = plan_buffer;
            }
        }
        if let Some(status) = clock_status {
            gpu.native_enc
                .copy_buffer_to_buffer(&last_clock_plan, &status, 32);
        }
    }
}
