//! One step's passes, in order (module doc of `gpu_flip_step`).

use super::*;

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
        let (Some(l), Some(tiles), Some(sorted), Some(out_faces)) = (
            self.lattice.as_ref(),
            self.tiles.as_ref(),
            self.sorted.as_ref(),
            self.faces.as_ref(),
        ) else {
            return Err("the step's storage was not reserved".into());
        };
        let cells = l.cells;
        let p = step.params;
        let capacity = p.capacity;
        // What reads this slot's activity beyond the passes that always did:
        // the clock plan, or zeros (always active) for the narrow band, whose
        // passes are not all clock-aware, and under the test lever.
        let gated = gating() && !step.narrow_enabled && !step.restore_narrow;
        let gate_plan = if gated {
            step.clock_plan
        } else {
            self.zeros.as_ref().expect("zero storage prepared")
        };
        let sort_job = |particles, sorted| SortJob {
            particles,
            read: LIQUID_PARTICLE_READ,
            capacity,
            count: step.count,
            bin_min: p.box_min,
            inv_cell: 1.0 / p.cell_size,
            bins: cells,
            sorted: Some(sorted),
            order: None,
            gate: Some(gate_plan),
        };
        // Copies of the params, so each Bytes binding borrows a value that
        // lives across its dispatch.  Narrow mode 1 is the one-time full
        // history initialization; mode 2 is the masked steady state.
        let narrow_initial = step.narrow_enabled && !self.narrow.initialized;
        let base = StepParams {
            particles: if step.narrow_enabled {
                capacity
            } else {
                p.particles
            },
            narrow_band: if step.narrow_enabled {
                if narrow_initial { 1 } else { 2 }
            } else {
                0
            },
            ..p
        };
        let nb_params = narrow_params(&base, self.narrow.initialized);
        if self.narrow_reset_pending
            && let Some(nb) = self.narrow.buffers.as_ref()
        {
            enc.clear_buffer(&nb.failure);
            self.narrow_reset_pending = false;
        }
        let corners = cells.map(|n| n + 1);
        // The box walls are in the solid with the bodies, as the engine's
        // inverted domain object is: a body flush with a wall then seals
        // against it, where the body's own lattice distance alone would leave
        // a sub-cell open channel between them. The engine's domain object
        // covers all six faces whichever are open, so this mask stays 63.
        // First, since emission seeds only outside the solid. The bodies are
        // posed once at the step's end and every solid pass reads that pose.
        let posed = self.posed.as_ref().expect("posed rows reserved above");
        let mobility = self.mobility.as_ref().expect("mobility reserved above");
        let zeros = self.zeros.as_ref().expect("zero storage prepared");
        if p.body_count > 0 {
            enc.dispatch_compute(
                &pipes.pose_bodies,
                &[
                    uniform(&base),
                    buffer(14, step.bodies),
                    buffer(21, step.reaction),
                    buffer(46, step.clock_plan),
                    buffer(48, posed),
                    buffer(49, step.contacts),
                    buffer(50, mobility),
                ],
                [(p.body_count as u32).div_ceil(64), 1, 1],
                "gpu_flip.step.pose_bodies",
            );
            enc.compute_memory_barrier_buffers();
        }
        encode_solid_distance(
            &mut self.solid,
            device,
            enc,
            &SolidDistanceJob {
                min: p.box_min,
                cell_size: p.cell_size,
                nodes: corners,
                closed_faces: 63,
                wall_inset: step.wall_inset,
                body_count: p.body_count,
                rows: p.body_count.min(p.rows),
                // Posed already: no time on, and a zero plan keeps it off.
                tick_seconds: 0.0,
                bodies: posed,
                shapes: step.shapes,
                atlas: step.atlas,
                out: &l.corners,
                clock_plan: zeros,
            },
            "gpu_flip.step.solid_distance",
        );
        // Sheet seeding (BUG-j9l9w) reads the markers in input order; off, the
        // sort is exactly as it was.
        let sheeting = step.sheet_rate > 0.0 && !step.narrow_enabled && !step.restore_narrow;
        let sheet_order = if sheeting {
            Some(
                self.sheet_order
                    .as_ref()
                    .ok_or("sheet seeding storage was not reserved")?,
            )
        } else {
            None
        };
        self.sorter.encode(
            device,
            enc,
            &SortJob {
                order: sheet_order,
                ..sort_job(step.particles, sorted)
            },
            &SORT_LABELS,
        )?;
        let cell_count: u64 = cells.iter().map(|&n| u64::from(n)).product();
        let face_count: u64 = cells.iter().map(|&n| u64::from(n) + 1).product();
        let ghost = StepParams {
            ghost: u32::from(step.ghost),
            ..base
        };
        let cells_groups = groups(cell_count);
        let face_groups = groups(face_count);

        if step.narrow_enabled {
            let nb = self
                .narrow
                .buffers
                .as_ref()
                .ok_or("narrow-band storage was not reserved")?;
            let nb_pipes = self
                .narrow
                .pipes
                .as_ref()
                .ok_or("narrow-band pipelines were not prepared")?;
            let ranges = self
                .sorter
                .ranges()
                .ok_or("the cell ranges were not reserved")?;
            if narrow_initial {
                // Dense narrow-band indexing bypasses the tile list at runtime,
                // but the compiled shader still requires its buffer binding.
                enc.dispatch_compute(
                    &pipes.band_distance,
                    &[
                        uniform(&base),
                        buffer(1, ranges),
                        buffer(2, sorted),
                        buffer(5, &l.phi),
                        buffer(29, &tiles.by_ring),
                        buffer(45, &nb.mask),
                        buffer(46, step.clock_plan),
                    ],
                    cells_groups,
                    "gpu_flip.step.narrow.initialize_distance",
                );
                enc.dispatch_compute(
                    &pipes.band_gather,
                    &[
                        uniform(&base),
                        buffer(1, ranges),
                        buffer(2, sorted),
                        buffer(4, &l.g),
                        buffer(29, &tiles.by_ring),
                        buffer(45, &nb.mask),
                        buffer(46, step.clock_plan),
                    ],
                    face_groups,
                    "gpu_flip.step.narrow.initialize_gather",
                );
                enc.compute_memory_barrier_buffers();
                extend(
                    enc,
                    pipes,
                    step.clock_plan,
                    &base,
                    face_groups,
                    None,
                    [&l.g, &nb.previous_faces, &l.b],
                    step.band,
                    "gpu_flip.step.narrow.initialize_faces",
                );
                narrow_redistance(
                    enc,
                    nb_pipes,
                    &nb_params,
                    &l.phi,
                    &nb.previous_phi,
                    cells,
                    "gpu_flip.step.narrow.initialize_distance",
                );
                enc.copy_buffer_to_buffer(&nb.previous_phi, &l.phi, l.phi.size);
            } else {
                enc.copy_buffer_to_buffer(&nb.phi, &nb.previous_phi, nb.phi.size);
            }
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                &nb_pipes.advect_phi,
                &[
                    narrow_uniform(&nb_params),
                    buffer(46, step.clock_plan),
                    buffer(1, &nb.previous_phi),
                    buffer(2, &nb.advected_phi),
                    buffer(3, &nb.previous_faces),
                ],
                cells_groups,
                "gpu_flip.step.narrow.advect_phi",
            );
            enc.dispatch_compute(
                &nb_pipes.advect_faces,
                &[
                    narrow_uniform(&nb_params),
                    buffer(46, step.clock_plan),
                    buffer(1, &nb.previous_phi),
                    buffer(3, &nb.previous_faces),
                    buffer(4, &nb.advected_faces),
                ],
                face_groups,
                "gpu_flip.step.narrow.advect_faces",
            );
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                &pipes.narrow_move,
                &[
                    uniform(&base),
                    buffer(46, step.clock_plan),
                    buffer(2, sorted),
                    buffer(3, &nb.previous_faces),
                    buffer(9, &l.corners),
                    buffer(17, &nb.previous_faces),
                    buffer(18, &nb.previous_faces),
                    buffer(19, &nb.particles),
                    buffer(22, step.capped),
                ],
                groups(u64::from(capacity)),
                "gpu_flip.step.narrow.move",
            );
            enc.compute_memory_barrier_buffers();
            self.sorter.encode(
                device,
                enc,
                &SortJob {
                    count: capacity,
                    ..sort_job(&nb.particles, sorted)
                },
                &SORT_LABELS,
            )?;
        }
        let ranges = self
            .sorter
            .ranges()
            .ok_or("the cell ranges were not reserved")?;

        if step.restore_narrow {
            let nb = self
                .narrow
                .buffers
                .as_ref()
                .ok_or("narrow-band restore has no storage")?;
            let nb_pipes = self
                .narrow
                .pipes
                .as_ref()
                .ok_or("narrow-band pipelines were not prepared")?;
            let scan_threads = cell_count
                .checked_mul(8)
                .ok_or("narrow-band scan extent overflow")?;
            let scan_words = usize::try_from(scan_threads)
                .map_err(|_| "narrow-band scan extent cannot be represented".to_string())?;
            let scan = self.narrow.scan.buffer(device, scan_words)?.clone();
            enc.copy_buffer_to_buffer(sorted, &nb.particles, nb.particles.size);
            let restore_params = narrow_params(&base, true);
            enc.dispatch_compute(
                &nb_pipes.restore_flags,
                &[
                    narrow_uniform(&restore_params),
                    buffer(46, step.clock_plan),
                    buffer(1, &nb.phi),
                    buffer(6, &l.corners),
                    buffer(8, &nb.previous_phi),
                    buffer(9, &nb.particles),
                    buffer(10, ranges),
                    buffer(11, &scan),
                ],
                cells_groups,
                "gpu_flip.step.narrow.restore_flags",
            );
            enc.compute_memory_barrier_buffers();
            self.narrow.scan.encode_labelled(
                enc,
                scan_words,
                ScanLabels {
                    blocks: "gpu_flip.step.narrow.restore_scan.blocks",
                    add: "gpu_flip.step.narrow.restore_scan.add",
                },
            );
            enc.dispatch_compute(
                &nb_pipes.status,
                &[
                    narrow_uniform(&restore_params),
                    buffer(46, step.clock_plan),
                    buffer(10, ranges),
                    buffer(11, &scan),
                    buffer(12, &nb.status),
                ],
                [1, 1, 1],
                "gpu_flip.step.narrow.restore_status",
            );
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                &pipes.narrow_latch,
                &[
                    narrow_uniform(&restore_params),
                    buffer(46, step.clock_plan),
                    buffer(43, &nb.status),
                    buffer(44, &nb.failure),
                ],
                [1, 1, 1],
                "gpu_flip.step.narrow.restore_latch",
            );
            enc.compute_memory_barrier_buffers();
            self.identity_ops.reserve(
                enc,
                manifold_water_liquid::primitives::particle_identity::BirthReservation {
                    particles: &nb.particles,
                    identity: step.identity,
                    ranges,
                    scan: &scan,
                    plan: step.clock_plan,
                    params: [capacity, cell_count as u32, scan_threads as u32, 2],
                },
            );
            enc.dispatch_compute(
                &nb_pipes.write,
                &[
                    narrow_uniform(&restore_params),
                    buffer(46, step.clock_plan),
                    buffer(47, step.identity),
                    buffer(3, &nb.previous_faces),
                    buffer(9, &nb.particles),
                    buffer(10, ranges),
                    buffer(11, &scan),
                    buffer(12, &nb.status),
                ],
                groups(scan_threads),
                "gpu_flip.step.narrow.restore_write",
            );
            enc.compute_memory_barrier_buffers();
            self.sorter.encode(
                device,
                enc,
                &SortJob {
                    count: capacity,
                    ..sort_job(&nb.particles, sorted)
                },
                &SORT_LABELS,
            )?;
            self.narrow_full_count = true;
        }
        let ranges = self
            .sorter
            .ranges()
            .ok_or("the cell ranges were not reserved")?;

        // Which tiles hold water this step, from the sort's bin counts. The
        // cell passes run over the cell set C through its indirect triple;
        // every other tile holds the canonical values (design section 4 (The
        // defined-value rule)): filled once per lattice, restored by the
        // retire when a tile leaves C.
        if !step.narrow_enabled {
            encode_tiles(
                enc,
                pipes,
                &base,
                tiles,
                ranges,
                step.capped,
                step.tally,
                gate_plan,
            );
        }
        if !step.narrow_enabled && !self.filled {
            enc.dispatch_compute(
                &pipes.tiles_fill,
                &canonical(l, vec![uniform(&base)]),
                face_groups,
                "gpu_flip.step.tiles.fill",
            );
            self.filled = true;
        }
        if !step.narrow_enabled {
            enc.dispatch_compute_indirect(
                &pipes.tiles_retire,
                &canonical(l, vec![uniform(&base), buffer(32, &tiles.retired)]),
                &tiles.args,
                retired_args_offset(tiles.ring_max),
                "gpu_flip.step.tiles.retire",
            );
        }
        #[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
        if POISON.load(std::sync::atomic::Ordering::SeqCst) {
            enc.dispatch_compute(
                &pipes.poison,
                &canonical(
                    l,
                    vec![
                        uniform(&base),
                        buffer(28, &tiles.rank),
                        buffer(30, &tiles.counts),
                    ],
                ),
                cells_groups,
                "gpu_flip.step.tiles.poison",
            );
        }
        let over_c = |enc: &mut GpuEncoder,
                      pipeline: &GpuComputePipeline,
                      bindings: Vec<GpuBinding<'_>>,
                      threads: [u32; 3],
                      label: &str| {
            let mut bound = vec![
                uniform(&base),
                buffer(29, &tiles.by_ring),
                buffer(46, step.clock_plan),
            ];
            bound.extend(bindings);
            if step.narrow_enabled {
                enc.dispatch_compute(pipeline, &bound, threads, label);
            } else {
                enc.dispatch_compute_indirect(pipeline, &bound, &tiles.args, 0, label);
            }
        };
        // The water mask is φ < 0, so φ is built every step.
        let bodies = p.body_count > 0;
        if step.narrow_enabled {
            let nb = self
                .narrow
                .buffers
                .as_ref()
                .ok_or("narrow-band storage was not reserved")?;
            let nb_pipes = self
                .narrow
                .pipes
                .as_ref()
                .ok_or("narrow-band pipelines were not prepared")?;
            enc.dispatch_compute(
                &nb_pipes.distance_support,
                &[
                    narrow_uniform(&nb_params),
                    buffer(46, step.clock_plan),
                    buffer(1, &nb.advected_phi),
                    buffer(6, &l.corners),
                    buffer(7, &nb.mask),
                    buffer(10, ranges),
                ],
                cells_groups,
                "gpu_flip.step.narrow.support",
            );
            enc.compute_memory_barrier_buffers();
            over_c(
                enc,
                &pipes.band_distance,
                vec![
                    buffer(1, ranges),
                    buffer(2, sorted),
                    buffer(5, &l.phi),
                    buffer(45, &nb.mask),
                ],
                cells_groups,
                "gpu_flip.step.narrow.distance",
            );
            enc.dispatch_compute(
                &nb_pipes.union,
                &[
                    narrow_uniform(&nb_params),
                    buffer(46, step.clock_plan),
                    buffer(1, &nb.advected_phi),
                    buffer(2, &nb.union_phi),
                    buffer(5, &l.phi),
                ],
                cells_groups,
                "gpu_flip.step.narrow.union",
            );
            enc.compute_memory_barrier_buffers();
            narrow_redistance(
                enc,
                nb_pipes,
                &nb_params,
                &nb.union_phi,
                &nb.phi,
                cells,
                "gpu_flip.step.narrow.redistance",
            );
            enc.copy_buffer_to_buffer(&nb.phi, &l.phi, nb.phi.size);
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                &nb_pipes.mask,
                &[
                    narrow_uniform(&nb_params),
                    buffer(46, step.clock_plan),
                    buffer(1, &nb.phi),
                    buffer(6, &l.corners),
                    buffer(7, &nb.mask),
                ],
                cells_groups,
                "gpu_flip.step.narrow.mask",
            );
            enc.compute_memory_barrier_buffers();
            over_c(
                enc,
                &pipes.band_gather,
                vec![
                    buffer(1, ranges),
                    buffer(2, sorted),
                    buffer(4, &l.g),
                    buffer(45, &nb.mask),
                ],
                face_groups,
                "gpu_flip.step.narrow.gather",
            );
        } else {
            over_c(
                enc,
                &pipes.distance,
                vec![buffer(1, ranges), buffer(2, sorted), buffer(5, &l.phi)],
                cells_groups,
                "gpu_flip.step.distance",
            );
            over_c(
                enc,
                &pipes.gather,
                vec![buffer(1, ranges), buffer(2, sorted), buffer(4, &l.g)],
                face_groups,
                "gpu_flip.step.particles_to_faces",
            );
        }
        // The saved faces: the particles' own, extended so FLIP's change is
        // measured wherever a particle samples.
        if step.narrow_enabled {
            extend(
                enc,
                pipes,
                step.clock_plan,
                &base,
                face_groups,
                None,
                [&l.g, &l.a, &l.b],
                step.band,
                "gpu_flip.step.narrow.extend_old",
            );
            let nb = self
                .narrow
                .buffers
                .as_ref()
                .ok_or("narrow-band storage was not reserved")?;
            let nb_pipes = self
                .narrow
                .pipes
                .as_ref()
                .ok_or("narrow-band pipelines were not prepared")?;
            enc.dispatch_compute(
                &nb_pipes.combine,
                &[
                    narrow_uniform(&nb_params),
                    buffer(46, step.clock_plan),
                    buffer(1, &nb.phi),
                    buffer(3, &nb.advected_faces),
                    buffer(4, &l.g),
                    buffer(6, &l.corners),
                ],
                face_groups,
                "gpu_flip.step.narrow.combine",
            );
            extend(
                enc,
                pipes,
                step.clock_plan,
                &base,
                face_groups,
                None,
                [&l.g, &l.g, &l.b],
                step.band,
                "gpu_flip.step.narrow.extend_combined",
            );
        } else {
            extend(
                enc,
                pipes,
                step.clock_plan,
                &base,
                face_groups,
                (!step.narrow_enabled).then_some(tiles),
                [&l.g, &l.a, &l.b],
                step.band,
                "gpu_flip.step.extend_old",
            );
        }
        let force_faces = if step.narrow_enabled { &l.g } else { &l.a };
        enc.dispatch_compute(
            &pipes.gravity,
            &[
                uniform(&base),
                buffer(3, force_faces),
                buffer(4, &l.f),
                buffer(12, step.forces),
                buffer(13, step.impulses),
                buffer(15, step.shapes),
                buffer(16, step.atlas),
                buffer(36, step.regions),
                buffer(46, step.clock_plan),
            ],
            face_groups,
            "gpu_flip.step.forces",
        );
        enc.dispatch_compute(
            &pipes.open,
            &[
                buffer(46, step.clock_plan),
                uniform(&base),
                buffer(9, &l.corners),
                buffer(4, &l.s),
            ],
            face_groups,
            "gpu_flip.step.open_fractions",
        );
        if solid_velocity_needed(p.body_count) {
            self.solid_velocity_is_zero = false;
            enc.dispatch_compute(
                &pipes.solid_velocity,
                &[
                    uniform(&base),
                    buffer(10, &l.s),
                    buffer(4, &l.b),
                    buffer(15, step.shapes),
                    buffer(16, step.atlas),
                    buffer(46, step.clock_plan),
                    buffer(48, posed),
                ],
                face_groups,
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
                enc.dispatch_compute(
                    &pipes.solid_extrapolate,
                    &[
                        uniform(&base),
                        buffer(3, from),
                        buffer(4, to),
                        buffer(46, gate_plan),
                    ],
                    face_groups,
                    "gpu_flip.step.solid_extrapolate",
                );
            }
        } else if !self.solid_velocity_is_zero {
            // Domain walls have zero velocity and friction. The extrapolation
            // known-mask is unused by all remaining no-body consumers. Clear
            // after allocation or body removal; no-body consumers never write
            // this scratch, so later intervals can retain its zero contents.
            enc.clear_buffer(&l.v);
            self.solid_velocity_is_zero = true;
        }
        // The tank walls are always in the solid, so this runs with or
        // without bodies, as the engine's postProcessSignedDistanceField does.
        over_c(
            enc,
            &pipes.phi_into_solids,
            vec![buffer(9, &l.corners), buffer(5, &l.phi)],
            cells_groups,
            "gpu_flip.step.phi_into_solids",
        );
        over_c(
            enc,
            &pipes.water_from_phi,
            vec![buffer(7, &l.phi), buffer(5, &l.water)],
            cells_groups,
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
            ranges,
            cells,
            step.capped,
            step.tally,
            gate_plan,
        );
        enc.dispatch_compute(
            &pipes.pocket_pin,
            &[
                uniform(&base),
                buffer(6, &l.water),
                buffer(23, &l.pocket),
                buffer(25, &l.pocket_label),
                buffer(5, &l.solve_water),
                buffer(46, gate_plan),
            ],
            cells_groups,
            "gpu_flip.step.pocket_pin",
        );
        if step.level > 0 {
            encode_pocket_coarsen(enc, pipes, &base, l, gate_plan);
        }
        if bodies && !step.dynamic {
            enc.dispatch_compute(
                &pipes.pocket_condition,
                &[
                    uniform(&base),
                    buffer(4, &l.v),
                    buffer(10, &l.s),
                    buffer(23, &l.pocket),
                    buffer(46, gate_plan),
                ],
                face_groups,
                "gpu_flip.step.pocket_condition",
            );
        }
        over_c(
            enc,
            &pipes.divergence,
            vec![
                buffer(3, &l.f),
                buffer(5, &l.rhs),
                buffer(6, &l.water),
                buffer(10, &l.s),
                buffer(11, &l.v),
            ],
            cells_groups,
            "gpu_flip.step.divergence",
        );
        encode_pocket_mean(
            enc,
            pipes,
            &base,
            l.fine_pockets(),
            &l.rhs,
            0,
            step.capped,
            step.tally,
            gate_plan,
        );
        // At a coarse solve level the restricted right-hand side loses each
        // pocket's zero sum (design section 11), so the mean comes off once
        // more there; as a later substep's would, its flux adds to the word.
        let coarse_mean = |solve: usize| {
            move |enc: &mut GpuEncoder, rhs: &GpuBuffer, lattice: [u32; 3], h: f32| {
                let params = StepParams {
                    n: lattice,
                    cell_size: h,
                    step_in_tick: 1,
                    ..base
                };
                encode_pocket_mean(
                    enc,
                    pipes,
                    &params,
                    l.coarse_pockets(),
                    rhs,
                    solve,
                    step.capped,
                    step.tally,
                    gate_plan,
                );
            }
        };
        enc.set_profile_tag("gpu_flip.stage.pressure");
        let water = Water {
            lattice: cells,
            cell_size: p.cell_size,
            water: &l.solve_water,
            faces: &l.s,
            phi: step.ghost.then_some(&l.phi),
        };
        self.solver.set_clock_plan(gate_plan);
        self.solver.prepare(device, enc, &water)?;
        // Dynamic bodies join the solve as the engine's mass-aware PCG
        // (RigidFluidCoupling) has them, at the pose the solid takes.
        let coupled = Bodies {
            lattice: cells,
            lattice_min: p.box_min,
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
            self.bodies.prepare(device, cells, coupled.count)?;
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
            enc.dispatch_compute(
                &pipes.subtract,
                &[
                    uniform(params),
                    buffer(46, step.clock_plan),
                    buffer(20, faces),
                    buffer(10, &l.s),
                    buffer(6, &l.water),
                    buffer(8, &l.pressure),
                    buffer(7, phi),
                ],
                face_groups,
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
            face_groups,
            (!step.narrow_enabled).then_some(tiles),
            [&l.f, out_faces, &l.b],
            step.band,
            "gpu_flip.step.extend_new",
        );
        enc.compute_memory_barrier_buffers();
        // The engine constrains its velocity and its saved velocity to the
        // solids after the pressure solve, so FLIP's change is measured
        // between two constrained fields.
        for (faces, label) in [
            (out_faces, "gpu_flip.step.constrain"),
            (&l.a, "gpu_flip.step.constrain_old"),
        ] {
            enc.dispatch_compute(
                &pipes.constrain,
                &[
                    buffer(46, step.clock_plan),
                    uniform(&base),
                    buffer(20, faces),
                    buffer(10, &l.s),
                    buffer(11, &l.v),
                ],
                face_groups,
                label,
            );
        }
        // The density projection (module doc): its pressure's gradient is
        // taken off a copy of the new faces in `l.f`, and the move reads the
        // difference as a displacement. Air sits at zero at its centres.
        enc.set_profile_tag("gpu_flip.stage.density");
        let spread = if step.density {
            enc.copy_buffer_to_buffer(out_faces, &l.f, out_faces.size);
            if step.narrow_enabled {
                let nb = self
                    .narrow
                    .buffers
                    .as_ref()
                    .ok_or("narrow-band storage was not reserved")?;
                enc.dispatch_compute(
                    &pipes.band_density,
                    &[
                        uniform(&base),
                        buffer(1, ranges),
                        buffer(2, sorted),
                        buffer(6, &l.water),
                        buffer(9, &l.corners),
                        buffer(5, &l.rhs),
                        buffer(45, &nb.mask),
                        buffer(46, step.clock_plan),
                    ],
                    cells_groups,
                    "gpu_flip.step.narrow.density_source",
                );
            } else {
                over_c(
                    enc,
                    &pipes.density,
                    vec![
                        buffer(1, ranges),
                        buffer(2, sorted),
                        buffer(6, &l.water),
                        buffer(9, &l.corners),
                        buffer(5, &l.rhs),
                    ],
                    cells_groups,
                    "gpu_flip.step.density_source",
                );
            }
            encode_pocket_mean(
                enc,
                pipes,
                &base,
                l.fine_pockets(),
                &l.rhs,
                1,
                step.capped,
                step.tally,
                gate_plan,
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
                face_groups,
                (!step.narrow_enabled).then_some(tiles),
                [&l.f, &l.f, &l.b],
                step.band,
                "gpu_flip.step.extend_spread",
            );
            &l.f
        } else {
            out_faces
        };
        // The tick's birth words start at zero before its first birth stage,
        // whether or not inflows or sheeting run this tick.
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
        // Sheet seeding (fluidsimulation.cpp _updateSheetSeeding), after the
        // engine's own marker update for this substep and before the FLIP
        // update: births join the move, so they take this substep's velocity
        // change and advection. Off (rate 0), nothing here is encoded.
        if sheeting {
            enc.set_profile_tag("gpu_flip.stage.sheeting");
            let ranges = self
                .sorter
                .ranges()
                .ok_or("the cell ranges were not reserved")?;
            let order = sheet_order.ok_or("sheet seeding storage was not reserved")?;
            // The sheeter's own distance, true to SHEET_DISTANCE_CELLS deep,
            // from the liquid level set the pressure solve read.
            let phi = self
                .sheet_distance
                .encode_gated(enc, &l.phi, p.cell_size, step.clock_plan);
            enc.compute_memory_barrier_buffers();
            let inputs = super::super::gpu_flip_sheeting::SheetInputs {
                particles: sorted,
                order,
                ranges,
                count: capacity,
                phi,
                origin: p.box_min,
                h: p.cell_size,
                threshold: super::super::gpu_flip_sheeting::FILL_THRESHOLD,
            };
            let births = super::super::gpu_flip_sheeting::StepBirths {
                old: &l.a,
                identity: step.identity,
                slots: base.particles,
                capped: step.capped,
                capped_offset: step.tally,
                rate: step.sheet_rate,
                tick: p.tick_index.max(0) as u32,
                substep: p.step_in_tick.max(0) as u32,
            };
            self.sheeting
                .encode_draw(enc, &inputs, step.clock_plan, &births);
            self.identity_ops.reserve(
                enc,
                manifold_water_liquid::primitives::particle_identity::BirthReservation {
                    particles: sorted,
                    identity: step.identity,
                    ranges,
                    scan: self.sheeting.winners(),
                    plan: step.clock_plan,
                    params: [
                        base.particles,
                        cells.iter().product(),
                        self.sheeting.ranks(),
                        1,
                    ],
                },
            );
            enc.compute_memory_barrier_buffers();
            self.sheeting
                .encode_write(enc, &inputs, step.clock_plan, &births);
        }
        enc.set_profile_tag("gpu_flip.stage.move");
        enc.dispatch_compute(
            &pipes.advect,
            &[
                uniform(&base),
                buffer(2, sorted),
                buffer(9, &l.corners),
                buffer(3, out_faces),
                buffer(17, &l.a),
                buffer(18, spread),
                buffer(19, step.out),
                buffer(22, step.capped),
                buffer(15, step.shapes),
                buffer(16, step.atlas),
                buffer(36, step.regions),
                buffer(46, step.clock_plan),
            ],
            groups(u64::from(base.particles)),
            "gpu_flip.step.move",
        );
        if step.narrow_enabled {
            let nb = self
                .narrow
                .buffers
                .as_ref()
                .ok_or("narrow-band storage was not reserved")?;
            let nb_pipes = self
                .narrow
                .pipes
                .as_ref()
                .ok_or("narrow-band pipelines were not prepared")?;
            enc.dispatch_compute(
                &nb_pipes.delete,
                &[
                    narrow_uniform(&nb_params),
                    buffer(46, step.clock_plan),
                    buffer(1, &nb.phi),
                    buffer(6, &l.corners),
                    buffer(9, step.out),
                ],
                groups(u64::from(capacity)),
                "gpu_flip.step.narrow.delete",
            );
            enc.compute_memory_barrier_buffers();
            self.sorter.encode(
                device,
                enc,
                &SortJob {
                    count: capacity,
                    ..sort_job(step.out, &nb.particles)
                },
                &SORT_LABELS,
            )?;
            let ranges = self
                .sorter
                .ranges()
                .ok_or("the cell ranges were not reserved")?;
            let scan_threads = cell_count
                .checked_mul(8)
                .ok_or("narrow-band scan extent overflow")?;
            let scan_words = usize::try_from(scan_threads)
                .map_err(|_| "narrow-band scan extent cannot be represented".to_string())?;
            let scan = self.narrow.scan.buffer(device, scan_words)?.clone();
            enc.dispatch_compute(
                &nb_pipes.flags,
                &[
                    narrow_uniform(&nb_params),
                    buffer(46, step.clock_plan),
                    buffer(1, &nb.phi),
                    buffer(6, &l.corners),
                    buffer(8, &nb.previous_phi),
                    buffer(9, &nb.particles),
                    buffer(10, ranges),
                    buffer(11, &scan),
                ],
                cells_groups,
                "gpu_flip.step.narrow.reseed_flags",
            );
            enc.compute_memory_barrier_buffers();
            self.narrow.scan.encode_labelled(
                enc,
                scan_words,
                ScanLabels {
                    blocks: "gpu_flip.step.narrow.reseed_scan.blocks",
                    add: "gpu_flip.step.narrow.reseed_scan.add",
                },
            );
            enc.dispatch_compute(
                &nb_pipes.status,
                &[
                    narrow_uniform(&nb_params),
                    buffer(46, step.clock_plan),
                    buffer(10, ranges),
                    buffer(11, &scan),
                    buffer(12, &nb.status),
                ],
                [1, 1, 1],
                "gpu_flip.step.narrow.reseed_status",
            );
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                &pipes.narrow_latch,
                &[
                    narrow_uniform(&nb_params),
                    buffer(46, step.clock_plan),
                    buffer(43, &nb.status),
                    buffer(44, &nb.failure),
                ],
                [1, 1, 1],
                "gpu_flip.step.narrow.reseed_latch",
            );
            enc.compute_memory_barrier_buffers();
            self.identity_ops.reserve(
                enc,
                manifold_water_liquid::primitives::particle_identity::BirthReservation {
                    particles: &nb.particles,
                    identity: step.identity,
                    ranges,
                    scan: &scan,
                    plan: step.clock_plan,
                    params: [capacity, cell_count as u32, scan_threads as u32, 2],
                },
            );
            enc.dispatch_compute(
                &nb_pipes.write,
                &[
                    narrow_uniform(&nb_params),
                    buffer(46, step.clock_plan),
                    buffer(47, step.identity),
                    buffer(3, out_faces),
                    buffer(9, &nb.particles),
                    buffer(10, ranges),
                    buffer(11, &scan),
                    buffer(12, &nb.status),
                ],
                groups(scan_threads),
                "gpu_flip.step.narrow.reseed_write",
            );
            enc.compute_memory_barrier_buffers();
            self.sorter.encode(
                device,
                enc,
                &SortJob {
                    count: capacity,
                    ..sort_job(&nb.particles, step.out)
                },
                &SORT_LABELS,
            )?;
            self.narrow_full_count = true;
            enc.copy_buffer_to_buffer(out_faces, &nb.previous_faces, out_faces.size);
            self.narrow.initialized = true;
            self.filled = false;
            if let Some(interior) = self.interior.as_ref() {
                enc.copy_buffer_to_buffer(&nb.phi, interior, nb.phi.size);
            }
        } else if let Some(interior) = self.interior.as_ref() {
            enc.dispatch_compute(
                &pipes.narrow_disabled,
                &[uniform(&base), buffer(5, interior)],
                cells_groups,
                "gpu_flip.step.narrow.disabled",
            );
        }
        // Native removal precedes inflow. Stable cell ranks predate speed
        // removal: an extreme marker still consumes a cell quota place.
        enc.compute_memory_barrier_buffers();
        self.sorter.encode(
            device,
            enc,
            &SortJob {
                count: base.particles,
                ..sort_job(step.out, sorted)
            },
            &SORT_LABELS,
        )?;
        enc.compute_memory_barrier_buffers();
        self.clock
            .as_ref()
            .expect("live clock prepared")
            .remove_extreme(enc, sorted, base.particles, clock_params);
        let ranges = self
            .sorter
            .ranges()
            .ok_or("the cell ranges were not reserved")?;
        enc.compute_memory_barrier_buffers();
        enc.dispatch_compute(
            &pipes.remove_crowded,
            &[
                uniform(&base),
                buffer(46, step.clock_plan),
                buffer(1, ranges),
                buffer(38, sorted),
            ],
            cells_groups,
            "gpu_flip.step.remove_crowded",
        );
        enc.compute_memory_barrier_buffers();
        self.sorter.encode(
            device,
            enc,
            &SortJob {
                count: base.particles,
                ..sort_job(sorted, step.out)
            },
            &SORT_LABELS,
        )?;
        // New inflow is first transferred and advected by the following step.
        if p.region_count > 0 {
            let sites = emit_sites(cells);
            let ranges = self
                .sorter
                .ranges()
                .ok_or("the cell ranges were not reserved")?;
            let scan = self.emit_scan.buffer(device, sites as usize)?.clone();
            let emission = StepParams {
                capacity: base.particles,
                ..base
            };
            enc.compute_memory_barrier_buffers();
            enc.dispatch_compute(
                &pipes.emit_flags,
                &[
                    uniform(&emission),
                    buffer(1, ranges),
                    buffer(2, step.out),
                    buffer(9, &l.corners),
                    buffer(15, step.shapes),
                    buffer(16, step.atlas),
                    buffer(36, step.regions),
                    buffer(46, step.clock_plan),
                    buffer(37, &scan),
                ],
                groups(sites),
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
                gate_plan,
            );
            self.identity_ops.reserve(
                enc,
                manifold_water_liquid::primitives::particle_identity::BirthReservation {
                    particles: step.out,
                    identity: step.identity,
                    ranges,
                    scan: &scan,
                    plan: step.clock_plan,
                    params: [emission.capacity, cells.iter().product(), sites as u32, 1],
                },
            );
            enc.dispatch_compute(
                &pipes.emit_write,
                &[
                    uniform(&emission),
                    buffer(1, ranges),
                    buffer(15, step.shapes),
                    buffer(16, step.atlas),
                    buffer(36, step.regions),
                    buffer(46, step.clock_plan),
                    buffer(37, &scan),
                    buffer(38, step.out),
                    buffer(47, step.identity),
                ],
                groups(sites),
                "gpu_flip.step.emit_write",
            );
            enc.dispatch_compute(
                &pipes.emit_tally,
                &[
                    uniform(&emission),
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
        enc.compute_memory_barrier_buffers();
        // A failure is sticky across narrow enable toggles and is cleared only
        // by an epoch/lattice reset.  Never-enabled dense steps use the
        // existing zero storage, so they do not acquire a new scratch word.
        let status = self
            .narrow
            .buffers
            .as_ref()
            .map(|buffers| &buffers.failure)
            .unwrap_or(self.zeros.as_ref().expect("zero storage prepared"));
        enc.dispatch_compute(
            &pipes.narrow_tally,
            &[
                uniform(&base),
                GpuBinding::Buffer {
                    binding: 22,
                    buffer: step.capped,
                    offset: step.tally,
                },
                buffer(43, status),
            ],
            [1, 1, 1],
            "gpu_flip.step.narrow.tally",
        );
        Ok(())
    }

}
