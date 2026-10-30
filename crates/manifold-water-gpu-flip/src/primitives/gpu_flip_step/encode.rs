//! One step's passes, in order (module doc of `gpu_flip_step`).

use super::*;
use manifold_water_liquid::primitives::sort_particles_into_cells::{Companion, SortCount};
use manifold_water_liquid::tile_storage::pool_len;
use manifold_water_liquid::world_tiles::{allocated_args_offset, exhausted_word, list_args_offset};

impl StepState {
    pub(super) fn encode(
        &mut self,
        device: &GpuDevice,
        enc: &mut GpuEncoder,
        step: &Step<'_>,
        clock_params: &GpuFlipClockParams,
    ) -> Result<(), String> {
        let posed_bytes = step.params.body_count.max(1) as u64 * size_of::<LiquidBody>() as u64;
        if self
            .posed
            .as_ref()
            .is_none_or(|posed| posed.size < posed_bytes)
        {
            self.posed = Some(allocate(device, posed_bytes)?);
        }
        let mobility_bytes = step.params.body_count.max(1) as u64 * MOBILITY_BYTES;
        if self
            .mobility
            .as_ref()
            .is_none_or(|mobility| mobility.size < mobility_bytes)
        {
            self.mobility = Some(allocate(device, mobility_bytes)?);
        }
        enc.set_profile_tag("gpu_flip.stage.prepare");
        let pipes = self
            .pipelines
            .as_ref()
            .expect("step pipelines built by prepare_pipelines at install");
        let (Some(l), Some(tiles), Some(sorted), Some(record), Some(affine), Some(affine_sorted)) = (
            self.lattice.as_ref(),
            self.tiles.as_ref(),
            self.sorted.as_ref(),
            self.slot_record.as_ref(),
            self.affine.as_ref(),
            self.affine_sorted.as_ref(),
        ) else {
            return Err("the step's storage was not reserved".into());
        };
        // The step's own faces, pooled: the `faces` output.
        let out_faces = &l.faces;
        let table = tiles.table();
        let pool = tiles.pool_tiles();
        let ring = tiles.reach().ring_max;
        // Every held slot's static corners are rebuilt on the first step, on
        // new lattice buffers and when the shapes buffer is a different one;
        // otherwise only the fresh slots' and a moving static body's.
        let shapes_key = (step.shapes.identity_key(), step.shapes.size);
        let p = StepParams {
            static_all: u32::from(self.pool_canon_pending || self.static_built != Some(shapes_key)),
            ..step.params
        };
        self.static_built = Some(shapes_key);
        let capacity = p.capacity;
        // Every sort covers the slots in use, from the slot record, and keys
        // each particle on its world cell's pool index. The particles' affines
        // move with them: `affine` beside `out` (and the particles the tick
        // starts from, the state's copy of `out`), `affine_sorted` beside
        // `sorted`.
        let sort_job = |particles, sorted, (from, to)| SortJob {
            particles,
            read: LIQUID_PARTICLE_READ,
            capacity,
            count: SortCount::Record {
                buffer: record,
                offset: 0,
            },
            bin_min: p.origin,
            inv_cell: p.inv_cell,
            bins: [0; 3],
            sorted: Some(sorted),
            order: None,
            companion: Some(Companion {
                from,
                to,
                words: AFFINE_WORDS,
            }),
            gate: Some(step.clock_plan),
        };
        // A copy of the params, so each Bytes binding borrows a value that
        // lives across its dispatch.
        let base = p;
        // The bodies are posed once at the step's end and every solid pass
        // reads that pose.
        let posed = self.posed.as_ref().expect("posed rows reserved above");
        let mobility = self.mobility.as_ref().expect("mobility reserved above");
        // The tick's birth words and its count of particles removed for want
        // of a slot start at zero before anything adds to them, whether or
        // not inflows or sheeting run this tick.
        if p.step_in_tick == 0 {
            enc.dispatch_compute(
                &pipes.birth_tally_reset,
                &[GpuBinding::Buffer {
                    binding: 22,
                    buffer: step.capped,
                    offset: step.tally,
                }],
                [1, 1, 1],
                "gpu_flip.step.birth_tally_reset",
            );
            enc.compute_memory_barrier_buffers();
        }
        if p.body_count > 0 {
            enc.dispatch_compute(
                &pipes.pose_bodies,
                &[
                    uniform(&base),
                    buffer(14, step.bodies),
                    buffer(15, step.shapes),
                    buffer(21, step.reaction),
                    buffer(46, step.clock_plan),
                    buffer(48, posed),
                    buffer(49, step.contacts),
                    buffer(50, mobility),
                    buffer(52, &l.static_scratch),
                ],
                [(p.body_count as u32).div_ceil(64), 1, 1],
                "gpu_flip.step.pose_bodies",
            );
            enc.compute_memory_barrier_buffers();
        }
        // Sheet seeding (BUG-j9l9w) reads the markers in input order; off, the
        // sort is exactly as it was.
        let sheeting = step.sheet_rate > 0.0;
        let sheet_order = if sheeting {
            Some(
                self.sheet_order
                    .as_ref()
                    .ok_or("sheet seeding storage was not reserved")?,
            )
        } else {
            None
        };
        let ghost = StepParams {
            ghost: u32::from(step.ghost),
            ..base
        };

        // Which tiles hold water this step, from the particles themselves:
        // the shared build holds every tile within the table's reach of a
        // particle's (design section 2), and the sort then keys each particle
        // on its cell's pool index, so the ranges live in the pool like every
        // other fine field. The cell passes run over the cell set C through
        // its triple; every other cell holds the canonical values (design
        // section 4 (The defined-value rule)): set when a slot is handed out,
        // restored by the retire when a slot leaves C. A new pool's canonical
        // element is written first.
        if self.pool_canon_pending {
            encode_pool_canon(enc, pipes, &base, l);
            self.pool_canon_pending = false;
        }
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        let points = match &self.box_points {
            Some((points, count)) => (points, count),
            None => (step.particles, record),
        };
        #[cfg(not(all(any(test, feature = "testkit"), feature = "gpu-proofs")))]
        let points = (step.particles, record);
        // Narrow Band's grid water holds its tiles beside the particles.
        let kept = self.narrow_band.kept(p.narrow_band);
        encode_tiles(
            enc,
            pipes,
            &base,
            l,
            tiles,
            points,
            kept,
            step.shared,
            step.clock_plan,
        );
        let narrow = p.narrow_band != 0;
        if narrow {
            self.narrow_band.encode_fresh(enc, pipes, &base, tiles);
        }
        // The solids at every held cell's corner: the Bounds Wall faces, which
        // are in the solid with the bodies as the engine's inverted domain
        // object is (a body flush with a wall then seals against it, where
        // the body's own lattice distance alone would leave a sub-cell open
        // channel between them). Open faces are no solids.
        encode_static_corners(enc, pipes, &base, l, tiles, step, posed);
        encode_solid_corners(enc, pipes, &base, l, tiles, step, posed);
        self.sorter.encode_pooled(
            device,
            enc,
            &SortJob {
                order: sheet_order,
                ..sort_job(step.particles, sorted, (affine, affine_sorted))
            },
            table,
            pool,
            &SORT_LABELS,
        )?;
        let ranges = self
            .sorter
            .ranges()
            .ok_or("the cell ranges were not reserved")?;
        // The slots in use are the sort's live end, for whatever reads the
        // identity's record and the slot record.
        let keys = pool_len(pool) as u32;
        self.identity_ops.settle(
            enc,
            ranges,
            keys,
            step.identity,
            step.clock_plan,
            Some(record),
        );
        encode_tile_tally(enc, pipes, &base, tiles, ranges, step);
        let by_rank = &tiles.lists().by_rank;
        let held = allocated_args_offset(ring);
        // A pass over a list's cells, 512 threads a slot: C (`list_args_offset(0)`)
        // or every held slot (`held`).
        let over = |enc: &mut GpuEncoder,
                    pipeline: &GpuComputePipeline,
                    offset: u64,
                    bindings: &[GpuBinding<'_>],
                    label: &str| {
            let mut bound = vec![
                uniform(&base),
                buffer(29, by_rank),
                buffer(46, step.clock_plan),
                buffer(51, table),
            ];
            bound.extend_from_slice(bindings);
            enc.dispatch_compute_indirect(pipeline, &bound, tiles.args(), offset, label);
        };
        // The water mask is φ < 0, so φ is built every step.
        let bodies = p.body_count > 0;
        let c = list_args_offset(0);
        over(
            enc,
            &pipes.distance,
            c,
            &[buffer(1, ranges), buffer(2, sorted), buffer(5, &l.phi)],
            "gpu_flip.step.distance",
        );
        // Narrow Band: the particles' φ united with the grid's water, and the
        // substep's distance the band's tests read.
        let band = narrow.then(|| {
            self.narrow_band
                .encode_union(enc, pipes, &base, tiles, step, &l.phi)
        });
        // Bound where the band's distance is read; off, no pass reads it.
        let band_distance = band.as_ref().map_or(&l.phi, |band| band.distance);
        over(
            enc,
            &pipes.gather,
            c,
            &[
                buffer(1, ranges),
                buffer(2, sorted),
                buffer(4, &l.g),
                buffer(74, affine_sorted),
            ],
            "gpu_flip.step.particles_to_faces",
        );
        if let Some(band) = &band {
            self.narrow_band
                .encode_combine(enc, pipes, &base, tiles, step, band, &l.g, &l.corners);
        }
        // The particles' own faces, extended past the water for the forces.
        extend(
            enc,
            pipes,
            step.clock_plan,
            &base,
            tiles,
            [&l.g, &l.a, &l.b],
            SAVED_CHAIN,
            "gpu_flip.step.extend_old",
        );
        over(
            enc,
            &pipes.gravity,
            held,
            &[
                buffer(3, &l.a),
                buffer(4, &l.f),
                buffer(12, step.forces),
                buffer(13, step.impulses),
                buffer(15, step.shapes),
                buffer(16, step.atlas),
                buffer(36, step.regions),
            ],
            "gpu_flip.step.forces",
        );
        // A wired Sand: its grains' share at the corners, then its drag on
        // the forced faces; `sand_porous` takes the shares after the solids.
        if let (Some(points), Some(share)) = (step.sand, self.sand_share.as_ref()) {
            enc.dispatch_compute(
                &pipes.sand_corners,
                &[
                    uniform(&base),
                    buffer(46, step.clock_plan),
                    buffer(51, table),
                    buffer(72, points),
                    buffer(73, share),
                ],
                [p.sand_count.div_ceil(256), 1, 1],
                "gpu_flip.step.sand_corners",
            );
            enc.compute_memory_barrier_buffers();
            over(
                enc,
                &pipes.sand_drag,
                held,
                &[buffer(4, &l.f), buffer(73, share)],
                "gpu_flip.step.sand_drag",
            );
            enc.compute_memory_barrier_buffers();
        }
        over(
            enc,
            &pipes.open,
            held,
            &[buffer(9, &l.corners), buffer(4, &l.s)],
            "gpu_flip.step.open_fractions",
        );
        if solid_velocity_needed(p.body_count) {
            self.solid_velocity_is_zero = false;
            over(
                enc,
                &pipes.solid_velocity,
                held,
                &[
                    buffer(10, &l.s),
                    buffer(4, &l.b),
                    buffer(15, step.shapes),
                    buffer(16, step.atlas),
                    buffer(27, step.collider_nodes),
                    buffer(28, step.collider_triangles),
                    buffer(45, &l.static_corners),
                    buffer(48, posed),
                ],
                "gpu_flip.step.solid_velocity",
            );
            // The samples carried out over the open faces, every solid alike
            // (meshlevelset.cpp normalizeVelocityGrid): an odd layer count
            // from the scratch ends in `l.v`.
            const _: () = assert!(SOLID_LAYERS % 2 == 1);
            for layer in 0..SOLID_LAYERS {
                let (from, to) = if layer % 2 == 0 {
                    (&l.b, &l.v)
                } else {
                    (&l.v, &l.b)
                };
                over(
                    enc,
                    &pipes.solid_extrapolate,
                    held,
                    &[buffer(3, from), buffer(4, to)],
                    "gpu_flip.step.solid_extrapolate",
                );
            }
        } else if !self.solid_velocity_is_zero {
            // Bounds walls have zero velocity and friction. The extrapolation
            // known-mask is unused by all remaining no-body consumers. Clear
            // after allocation or body removal; no-body consumers never write
            // this scratch, so later intervals can retain its zero contents.
            enc.clear_buffer(&l.v);
            self.solid_velocity_is_zero = true;
        }
        // A wired Sand closes the faces it lies on by its grains' share,
        // moving with it, once the solids' velocity is final
        // (forces.wgsl `sand_porous`). It writes the solid velocity, so the
        // next step without bodies clears that again.
        if let (Some(_), Some(share)) = (step.sand, self.sand_share.as_ref()) {
            enc.compute_memory_barrier_buffers();
            over(
                enc,
                &pipes.sand_porous,
                held,
                &[buffer(20, &l.s), buffer(4, &l.v), buffer(73, share)],
                "gpu_flip.step.sand_porous",
            );
            enc.compute_memory_barrier_buffers();
            self.solid_velocity_is_zero = false;
        }
        // The Bounds walls are in the solid with the bodies, so this runs
        // with or without bodies, as the engine's postProcessSignedDistanceField
        // does.
        over(
            enc,
            &pipes.phi_into_solids,
            c,
            &[buffer(9, &l.corners), buffer(5, &l.phi)],
            "gpu_flip.step.phi_into_solids",
        );
        over(
            enc,
            &pipes.water_from_phi,
            c,
            &[buffer(7, &l.phi), buffer(5, &l.water)],
            "gpu_flip.step.water_from_phi",
        );
        // Which water reaches air holds every step: the density source reads
        // it too. As the engine does, the solid velocity's zeroing is skipped
        // when the bodies are in the solve: their mass resolves the pocket.
        enc.set_profile_tag("gpu_flip.stage.pockets");
        encode_pockets(
            enc,
            pipes,
            &base,
            l,
            tiles,
            step.capped,
            step.tally,
            step.clock_plan,
        );
        over(
            enc,
            &pipes.pocket_pin,
            held,
            &[
                buffer(6, &l.water),
                buffer(23, &l.pocket),
                buffer(25, &l.pocket_label),
                buffer(5, &l.solve_water),
            ],
            "gpu_flip.step.pocket_pin",
        );
        if bodies && !step.dynamic {
            over(
                enc,
                &pipes.pocket_condition,
                held,
                &[buffer(4, &l.v), buffer(10, &l.s), buffer(23, &l.pocket)],
                "gpu_flip.step.pocket_condition",
            );
        }
        // Walls that only push (projection.wgsl release_walls), from the last
        // solve's pressure. The density projection writes its own answer into
        // that buffer, so with it on the walls hold as they always did.
        if !step.density {
            over(
                enc,
                &pipes.release,
                c,
                &[
                    buffer(3, &l.f),
                    buffer(5, &l.solve_water),
                    buffer(6, &l.water),
                    buffer(8, &l.pressure),
                    buffer(11, &l.v),
                    buffer(20, &l.s),
                    buffer(23, &l.pocket),
                ],
                "gpu_flip.step.release_walls",
            );
        }
        over(
            enc,
            &pipes.divergence,
            c,
            &[
                buffer(3, &l.f),
                buffer(5, &l.rhs),
                buffer(6, &l.water),
                buffer(10, &l.s),
                buffer(11, &l.v),
            ],
            "gpu_flip.step.divergence",
        );
        encode_pocket_mean(
            enc,
            pipes,
            &base,
            l.fine_pockets(),
            tiles,
            &l.rhs,
            0,
            step.capped,
            step.tally,
            step.clock_plan,
        );
        // At a coarse solve level the restricted right-hand side loses each
        // pocket's zero sum (design section 11), so the mean comes off once
        // more there; as a later substep's would, its flux adds to the word.
        let coarse_mean = |solve: usize| {
            move |enc: &mut GpuEncoder, rhs: &GpuBuffer, level: &WorldTiles, h: f32| {
                let params = StepParams {
                    cell_size: h,
                    step_in_tick: 1,
                    ..base
                };
                encode_pocket_mean(
                    enc,
                    pipes,
                    &params,
                    l.coarse_pockets(),
                    level,
                    rhs,
                    solve,
                    step.capped,
                    step.tally,
                    step.clock_plan,
                );
            }
        };
        enc.set_profile_tag("gpu_flip.stage.pressure");
        let water = Water {
            cell_size: p.cell_size,
            water: &l.solve_water,
            faces: &l.s,
            phi: step.ghost.then_some(&l.phi),
        };
        self.solver.set_clock_plan(step.clock_plan);
        self.solver.prepare(device, enc, &water, tiles)?;
        // The pockets at the solve level, read through the level's table the
        // prepare built.
        if step.level > 0 {
            let level = self.solver.level_tiles(step.level)?;
            encode_pocket_coarsen(enc, pipes, &base, l, tiles, level, step.clock_plan);
        }
        // Dynamic bodies join the solve as the engine's mass-aware PCG
        // (RigidFluidCoupling) has them, at the pose the solid takes.
        let coupled = Bodies {
            tiles,
            cell_size: p.cell_size,
            density: WATER_DENSITY,
            tick_seconds: p.tick_seconds,
            first: 0,
            count: p.body_count.max(0) as u32,
            water: &l.solve_water,
            open: &l.s,
            solid: &l.v,
            bodies: posed,
            mobility,
        };
        self.bodies.set_clock_plan(step.clock_plan);
        if step.dynamic {
            self.bodies.prepare(device, pool, coupled.count)?;
            #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
            if POISON.load(std::sync::atomic::Ordering::SeqCst) {
                self.bodies.poison(enc, &coupled);
            }
        }
        let passes = step.dynamic.then_some((&self.bodies, &coupled));
        let pressure_mean = coarse_mean(0);
        self.solver.solve(
            enc,
            &water,
            Solve {
                rhs: &l.rhs,
                pressure: &l.pressure,
                stop: step.pressure,
                bodies: passes,
                level: step.level,
                coarse_rhs: Some(&pressure_mean),
            },
            tiles,
        )?;
        let tally = step.tally;
        self.solver.tally(
            enc,
            step.pressure,
            step.capped,
            tally,
            0,
            step.params.step_in_tick == 0,
        )?;
        enc.set_profile_tag("gpu_flip.stage.project_extend");
        // φ binds the water array when the ghost rows are off; the pass never reads it then.
        let phi = if step.ghost { &l.phi } else { &l.water };
        let subtract = |enc: &mut GpuEncoder,
                        params: &StepParams,
                        phi: &GpuBuffer,
                        faces: &GpuBuffer,
                        label: &str| {
            enc.dispatch_compute_indirect(
                &pipes.subtract,
                &[
                    uniform(params),
                    buffer(29, by_rank),
                    buffer(46, step.clock_plan),
                    buffer(20, faces),
                    buffer(10, &l.s),
                    buffer(6, &l.water),
                    buffer(8, &l.pressure),
                    buffer(7, phi),
                    buffer(51, table),
                ],
                tiles.args(),
                held,
                label,
            );
        };
        subtract(enc, &ghost, phi, &l.f, "gpu_flip.step.project");
        // The engine's finishPressure: the pressure's impulse goes to the
        // bodies and their velocity change to the solid faces. The
        // constraint's friction below acts on the water only.
        if step.dynamic {
            self.bodies.react(
                enc,
                &coupled,
                self.solver.tiles()?,
                &l.pressure,
                step.reaction,
            )?;
        }
        extend(
            enc,
            pipes,
            step.clock_plan,
            &base,
            tiles,
            [&l.f, out_faces, &l.b],
            BAND_CHAIN,
            "gpu_flip.step.extend_new",
        );
        enc.compute_memory_barrier_buffers();
        // The engine constrains its velocity to the solids after the pressure
        // solve.
        over(
            enc,
            &pipes.constrain,
            held,
            &[buffer(20, out_faces), buffer(10, &l.s), buffer(11, &l.v)],
            "gpu_flip.step.constrain",
        );
        // The density projection (module doc): its pressure's gradient is
        // taken off a copy of the new faces in `l.f`, and the move reads the
        // difference as a displacement. Air sits at zero at its centres.
        enc.set_profile_tag("gpu_flip.stage.density");
        let spread = if step.density {
            enc.copy_buffer_to_buffer(out_faces, &l.f, out_faces.size);
            over(
                enc,
                &pipes.density,
                c,
                &[
                    buffer(1, ranges),
                    buffer(2, sorted),
                    buffer(6, &l.water),
                    buffer(9, &l.corners),
                    buffer(5, &l.rhs),
                    buffer(67, band_distance),
                ],
                "gpu_flip.step.density_source",
            );
            encode_pocket_mean(
                enc,
                pipes,
                &base,
                l.fine_pockets(),
                tiles,
                &l.rhs,
                1,
                step.capped,
                step.tally,
                step.clock_plan,
            );
            // The density solve uses the same pocket-only water mask as
            // pressure, with air at the cell centres and no body coupling.
            let flat = Water { phi: None, ..water };
            let density_mean = coarse_mean(1);
            self.solver.solve(
                enc,
                &flat,
                Solve {
                    rhs: &l.rhs,
                    pressure: &l.pressure,
                    stop: step.pressure,
                    bodies: None,
                    level: step.level,
                    coarse_rhs: Some(&density_mean),
                },
                tiles,
            )?;
            self.solver
                .tally(enc, step.pressure, step.capped, tally, 1, false)?;
            let plain = StepParams { ghost: 0, ..base };
            subtract(enc, &plain, &l.water, &l.f, "gpu_flip.step.density_project");
            extend(
                enc,
                pipes,
                step.clock_plan,
                &base,
                tiles,
                [&l.f, &l.f, &l.b],
                BAND_CHAIN,
                "gpu_flip.step.extend_spread",
            );
            &l.f
        } else {
            out_faces
        };
        // Sheet seeding (fluidsimulation.cpp _updateSheetSeeding), after the
        // engine's own marker update for this substep and before the FLIP
        // update: births join the move, so they take this substep's velocity
        // change and advection. Off (rate 0), nothing here is encoded.
        let mut sheet_depth = None;
        if sheeting {
            enc.set_profile_tag("gpu_flip.stage.sheeting");
            // The water budget's count starts over (marker_removal.wgsl).
            enc.dispatch_compute(
                &pipes.owed[0],
                &[
                    uniform(&base),
                    buffer(46, step.clock_plan),
                    buffer(71, self.sheeting.stats()),
                ],
                [1, 1, 1],
                "gpu_flip.step.owed_clear",
            );
            let ranges = self
                .sorter
                .ranges()
                .ok_or("the cell ranges were not reserved")?;
            let order = sheet_order.ok_or("sheet seeding storage was not reserved")?;
            let plan = self.sheeting.encode_gate(
                enc,
                step.clock_plan,
                ranges,
                base.particles,
                (step.capped, step.tally),
            );
            enc.compute_memory_barrier_buffers();
            // The sheeter's own distance, true to SHEET_DISTANCE_CELLS deep,
            // from the liquid level set the pressure solve read.
            let phi = self.sheet_distance.encode_gated(
                enc,
                &l.phi,
                Some(&l.corners),
                p.cell_size,
                plan,
                tiles,
            );
            sheet_depth = Some(phi);
            enc.compute_memory_barrier_buffers();
            let inputs = super::super::gpu_flip_sheeting::SheetInputs {
                particles: sorted,
                order,
                ranges,
                count: (record, 0),
                phi,
                h: p.cell_size,
                threshold: super::super::gpu_flip_sheeting::FILL_THRESHOLD,
            };
            let births = super::super::gpu_flip_sheeting::StepBirths {
                old: &l.a,
                identity: step.identity,
                capped: step.capped,
                capped_offset: step.tally,
                rate: step.sheet_rate,
                tick: p.tick_index.max(0) as u32,
                substep: p.step_in_tick.max(0) as u32,
            };
            self.sheeting
                .encode_draw(enc, &inputs, plan, &births, tiles);
            self.identity_ops.reserve(
                enc,
                manifold_water_liquid::primitives::particle_identity::BirthReservation {
                    particles: sorted,
                    identity: step.identity,
                    ranges,
                    scan: self.sheeting.winners(),
                    plan,
                    tile_counts: tiles.counts(),
                    step_record: Some(record),
                    params: [
                        base.particles,
                        pool_len(pool) as u32,
                        self.sheeting.ranks(),
                        exhausted_word(ring) as u32,
                    ],
                },
            );
            enc.compute_memory_barrier_buffers();
            self.sheeting
                .encode_write(enc, &inputs, plan, &births, tiles);
        }
        enc.set_profile_tag("gpu_flip.stage.move");
        enc.dispatch_compute_indirect(
            &pipes.advect,
            &[
                uniform(&base),
                buffer(2, sorted),
                buffer(9, &l.corners),
                buffer(3, out_faces),
                buffer(18, spread),
                buffer(19, step.out),
                buffer(75, affine),
                buffer(22, step.capped),
                buffer(15, step.shapes),
                buffer(16, step.atlas),
                buffer(36, step.regions),
                buffer(46, step.clock_plan),
                buffer(51, table),
                buffer(30, tiles.counts()),
                buffer(42, record),
                buffer(67, band_distance),
                // The band's interior, which the move's test never reads;
                // Vulkan refuses a named binding left out.
                buffer(65, band_distance),
            ],
            record,
            0,
            "gpu_flip.step.move",
        );
        if let Some(band) = &band {
            self.narrow_band
                .encode_advect(enc, pipes, &base, tiles, step, band, out_faces);
        }
        // Native removal precedes inflow. Stable cell ranks predate speed
        // removal: an extreme marker still consumes a cell quota place.
        enc.compute_memory_barrier_buffers();
        self.sorter.encode_pooled(
            device,
            enc,
            &sort_job(step.out, sorted, (affine, affine_sorted)),
            table,
            pool,
            &SORT_LABELS,
        )?;
        let ranges = self
            .sorter
            .ranges()
            .ok_or("the cell ranges were not reserved")?;
        self.identity_ops.settle(
            enc,
            ranges,
            keys,
            step.identity,
            step.clock_plan,
            Some(record),
        );
        enc.compute_memory_barrier_buffers();
        self.clock
            .as_ref()
            .expect("live clock prepared")
            .remove_extreme(enc, sorted, record, clock_params);
        enc.compute_memory_barrier_buffers();
        enc.dispatch_compute_indirect(
            &pipes.remove_crowded,
            &[
                uniform(&base),
                buffer(1, ranges),
                buffer(29, by_rank),
                buffer(38, sorted),
                buffer(46, step.clock_plan),
                buffer(51, table),
            ],
            tiles.args(),
            c,
            "gpu_flip.step.remove_crowded",
        );
        // The births the sheets drew, given back from crowded cells deep in
        // the water, so seeding keeps the water's amount.
        if let Some(depth) = sheet_depth {
            for (pipe, label) in [
                (&pipes.owed[1], "gpu_flip.step.owed_count"),
                (&pipes.owed[2], "gpu_flip.step.remove_owed"),
            ] {
                enc.compute_memory_barrier_buffers();
                enc.dispatch_compute_indirect(
                    pipe,
                    &[
                        uniform(&base),
                        buffer(1, ranges),
                        buffer(7, depth),
                        buffer(29, by_rank),
                        buffer(38, sorted),
                        buffer(46, step.clock_plan),
                        buffer(51, table),
                        buffer(71, self.sheeting.stats()),
                    ],
                    tiles.args(),
                    c,
                    label,
                );
            }
        }
        if let Some(band) = &band {
            enc.compute_memory_barrier_buffers();
            self.narrow_band.encode_reseed(
                enc,
                pipes,
                &self.identity_ops,
                &base,
                tiles,
                step,
                band,
                sorted,
                ranges,
                record,
                &l.corners,
                out_faces,
            );
        }
        enc.compute_memory_barrier_buffers();
        self.sorter.encode_pooled(
            device,
            enc,
            &sort_job(sorted, step.out, (affine_sorted, affine)),
            table,
            pool,
            &SORT_LABELS,
        )?;
        let ranges = self
            .sorter
            .ranges()
            .ok_or("the cell ranges were not reserved")?;
        self.identity_ops.settle(
            enc,
            ranges,
            keys,
            step.identity,
            step.clock_plan,
            Some(record),
        );
        // New inflow is first transferred and advected by the following
        // step. Each inflow enumerates its own site box (`region_sites`).
        let sites = p.emit_sites;
        if p.region_count > 0 && sites > 0 {
            let scan = self.emit_scan.buffer(device, sites as usize)?.clone();
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                &pipes.emit_flags,
                &[
                    uniform(&base),
                    buffer(1, ranges),
                    buffer(2, step.out),
                    buffer(15, step.shapes),
                    buffer(16, step.atlas),
                    buffer(27, step.collider_nodes),
                    buffer(28, step.collider_triangles),
                    buffer(36, step.regions),
                    buffer(44, step.region_sites),
                    buffer(45, &l.static_corners),
                    buffer(46, step.clock_plan),
                    buffer(48, posed),
                    buffer(37, &scan),
                    buffer(51, table),
                ],
                groups(u64::from(sites)),
                "gpu_flip.step.emit_flags",
            );
            enc.compute_memory_barrier_buffers();
            self.emit_scan.encode_labelled_gated(
                enc,
                sites as usize,
                ScanLabels {
                    blocks: "gpu_flip.step.emit_scan.blocks",
                    add: "gpu_flip.step.emit_scan.add",
                },
                step.clock_plan,
            );
            self.identity_ops.reserve(
                enc,
                manifold_water_liquid::primitives::particle_identity::BirthReservation {
                    particles: step.out,
                    identity: step.identity,
                    ranges,
                    scan: &scan,
                    plan: step.clock_plan,
                    tile_counts: tiles.counts(),
                    step_record: Some(record),
                    params: [
                        base.particles,
                        pool_len(pool) as u32,
                        sites,
                        exhausted_word(ring) as u32,
                    ],
                },
            );
            enc.dispatch_compute(
                &pipes.emit_write,
                &[
                    uniform(&base),
                    buffer(1, ranges),
                    buffer(15, step.shapes),
                    buffer(16, step.atlas),
                    buffer(36, step.regions),
                    buffer(44, step.region_sites),
                    buffer(46, step.clock_plan),
                    buffer(37, &scan),
                    buffer(38, step.out),
                    buffer(47, step.identity),
                ],
                groups(u64::from(sites)),
                "gpu_flip.step.emit_write",
            );
            enc.dispatch_compute(
                &pipes.emit_tally,
                &[
                    uniform(&base),
                    buffer(1, ranges),
                    buffer(37, &scan),
                    buffer(46, step.clock_plan),
                    buffer(47, step.identity),
                    GpuBinding::Buffer {
                        binding: 22,
                        buffer: step.capped,
                        offset: step.tally,
                    },
                ],
                [1, 1, 1],
                "gpu_flip.step.emit_tally",
            );
        }
        if p.narrow_band == narrow_band::ON {
            enc.compute_memory_barrier_buffers();
            self.narrow_band.encode_keep(enc, pipes, &base, tiles, step);
        }
        enc.compute_memory_barrier_buffers();
        Ok(())
    }
}
