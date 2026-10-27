//! Fill, inflow and outflow proofs.

use super::*;

pub(super) fn cpu_fill_hash(x: u32) -> u32 {
    let s = x.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let w = ((s >> ((s >> 28) + 4)) ^ s).wrapping_mul(277_803_737);
    (w >> 22) ^ w
}

#[test]
pub(super) fn gpu_flip_liquid_fill_places_pool_then_box() {
    let mut harness = Harness::new();
    // Sites are half cells: a 12 × 10 × 8 site lattice.
    let (pool, sites, seed, jitter) = (1u32, [[2u32, 8], [0, 4], [3, 9]], 7u32, 0.5f32);
    // 96 pool sites (12 × 1 × 8) and 90 box sites (6 × 3 × 5); the fill owns
    // storage for exactly those.
    let placed = 96 + 90;
    let out = harness.array::<FluidParticle>(&[], 1);
    let count = harness.scalar();
    // The fill reads the box the domain publishes: N cells from MIN.
    let step = params(&[
        ("fill_cells_x", N[0] as f32),
        ("fill_cells_y", N[1] as f32),
        ("fill_cells_z", N[2] as f32),
        ("cell_size", H),
        ("fill_min_x", MIN[0]),
        ("fill_min_y", MIN[1]),
        ("fill_min_z", MIN[2]),
        ("pool_sites", pool as f32),
        ("box_x0", sites[0][0] as f32),
        ("box_x1", sites[0][1] as f32),
        ("box_y0", sites[1][0] as f32),
        ("box_y1", sites[1][1] as f32),
        ("box_z0", sites[2][0] as f32),
        ("box_z1", sites[2][1] as f32),
        ("jitter", jitter),
        ("seed", seed as f32),
    ]);
    let mut fill = LiquidFill::new();
    let (scalars, errors) = harness.run(
        &mut fill,
        &[],
        &[("particles", out.0), ("count", count)],
        &step,
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert!(
        scalars
            .iter()
            .any(|(s, v)| *s == count && *v == ParamValue::Float(placed as f32)),
        "{scalars:?}"
    );
    let storage = harness.buffer(out.0);
    assert_eq!(
        storage.size,
        (placed * std::mem::size_of::<FluidParticle>()) as u64,
        "the fill's storage holds the fill"
    );
    let got: Vec<FluidParticle> = read(&storage, placed);
    // The box clipped above the pool and to the lattice: x 2..8, y 1..4, z 3..8.
    let mut cells: Vec<[u32; 3]> = Vec::new();
    for z in 0..2 * N[2] as u32 {
        for x in 0..2 * N[0] as u32 {
            cells.push([x, 0, z]);
        }
    }
    for z in 3..8 {
        for y in 1..4 {
            for x in 2..8 {
                cells.push([x, y, z]);
            }
        }
    }
    assert_eq!(cells.len(), placed);
    for (i, g) in got.iter().enumerate() {
        let s = cells[i];
        let key = (i as u32)
            .wrapping_mul(3)
            .wrapping_add(seed.wrapping_mul(2_654_435_761));
        for a in 0..3 {
            let unit = (cpu_fill_hash(key.wrapping_add(a as u32)) >> 8) as f32 / 16_777_216.0;
            let local = 0.25 + 0.5 * s[a] as f32 + 0.5 * jitter * (unit - 0.5);
            let want = MIN[a] + local * H;
            assert!(
                (g.position_radius[a] - want).abs() < 1e-5,
                "particle {i} axis {a}: {} vs {want}",
                g.position_radius[a]
            );
        }
        assert!((g.position_radius[3] - 0.31017524 * H).abs() < 1e-6);
        assert_eq!((g.velocity, g.id), ([0.0; 3], i as u32 + 1), "particle {i}");
    }
    // The domain run that sites a fill past its 4 m top reports it capped:
    // the report the cards' setup check reads.
    let mut domain = super::super::gpu_flip_domain::GpuFlipDomain::new();
    let _ = harness.run(
        &mut domain,
        &[],
        &[],
        &params(&[("fill_height", 9.0), ("cell_metres", 0.5)]),
    );
    let snapshot = manifold_water_rigid::node::PhysicsNode::fluid_domain_snapshot(&domain)
        .expect("a snapshot");
    assert!(snapshot.fit.fill_capped, "{snapshot:?}");
}

/// One region row over a box of whole cells, `lo` to `lo + cells` of the box
/// from `min`, its lattice holding −1 everywhere, so a point is inside
/// exactly when it is in the box (sites sit at quarter cells, never on a
/// face); its shape and atlas.
pub(super) fn region_box(
    min: [f32; 3],
    code: f32,
    lo: [usize; 3],
    cells: [u32; 3],
    velocity: [f32; 3],
) -> (LiquidBody, LiquidShape, Vec<u32>) {
    let dims = cells.map(|c| c + 1);
    let mut atlas = Vec::new();
    pack_distance_atlas(
        &vec![-1.0; dims.iter().product::<u32>() as usize],
        &mut atlas,
    );
    let shape = LiquidShape {
        origin_spacing: [0.0, 0.0, 0.0, H],
        dims_x: dims[0],
        dims_y: dims[1],
        dims_z: dims[2],
        atlas_offset: 0,
        scale_min: [1.0; 4],
    };
    let p: [f32; 3] = std::array::from_fn(|a| min[a] + lo[a] as f32 * H);
    let row = LiquidBody {
        position_inv_mass: [p[0], p[1], p[2], 0.0],
        rotation: [0.0, 0.0, 0.0, 1.0],
        angular_velocity: [0.0, 0.0, 0.0, code],
        inv_inertia_x: [velocity[0], velocity[1], velocity[2], 0.0],
        accel_shape: [0.0, 0.0, 0.0, 0.0],
        ..LiquidBody::default()
    };
    (row, shape, atlas)
}

pub(super) fn in_box(x: [f64; 3], lo: [usize; 3], cells: [u32; 3]) -> bool {
    (0..3).all(|a| {
        let q = (x[a] - f64::from(MIN[a])) / f64::from(H);
        q >= lo[a] as f64 && q <= (lo[a] + cells[a] as usize) as f64
    })
}

/// The box's half-cell sites, `2N` per axis.
pub(super) fn box_site_count() -> usize {
    N.iter().map(|n| 2 * n).product()
}

/// One region row's site box (gpu_flip_domain's `RegionSites`, 8 words:
/// first world half cell, first index in its tick, sites per axis, count):
/// `size` half cells from world half cell `first`, half cell J at
/// (J/2 − 1/4)·h.
pub(super) fn site_box(first: [i32; 3], size: [u32; 3], index: u32) -> [u32; 8] {
    let first = first.map(|j| j as u32);
    [
        first[0],
        first[1],
        first[2],
        index,
        size[0],
        size[1],
        size[2],
        size.iter().product(),
    ]
}

/// The site box over every half-cell site of the box, so emission thread
/// idx is box site idx, x fastest: the box's first world cell p holds half
/// cells 2p and 2p + 1.
pub(super) fn box_sites() -> [u32; 8] {
    site_box(
        box_origin(MIN, H).map(|p| 2 * p),
        N.map(|n| 2 * n as u32),
        0,
    )
}

pub(super) fn region_params() -> StepParams {
    StepParams {
        region_count: 1,
        region_rows: 1,
        shapes_len: 1,
        emit_sites: box_site_count() as u32,
        ..lattice()
    }
}

/// An inflow fills (fluidsimulation.cpp 8566-8603, 8771-8811): every empty
/// half-cell site inside it and outside the solid takes one particle at rest
/// in the inflow's velocity, written after the live prefix in site order; an
/// occupied site and a site past the pool's slots take none.
#[test]
pub(super) fn gpu_flip_inflow_emits_at_empty_sites_into_free_slots() {
    let (lo, cells, velocity) = ([1, 2, 1], [3, 2, 2], [0.4, -1.5, 0.2]);
    let (row, shape, atlas) = region_box(MIN, 2.0, lo, cells, velocity);
    let site = |j: [usize; 3]| -> [f64; 3] {
        std::array::from_fn(|a| f64::from(MIN[a]) + (0.25 + 0.5 * j[a] as f64) * f64::from(H))
    };
    let sites: [usize; 3] = N.map(|n| 2 * n);
    let site_count = box_site_count();
    let mut live: Vec<FluidParticle> = random_particles(0xe31, 120)
        .into_iter()
        .filter(|p| p.position_radius[3] > 0.0)
        .collect();
    // One particle on an inflow site, so the proof sees a taken site skipped.
    let held = site([2 * lo[0] + 1, 2 * lo[1], 2 * lo[2] + 1]);
    live.push(FluidParticle {
        position_radius: [held[0] as f32, held[1] as f32, held[2] as f32, 0.08],
        velocity: [0.0; 3],
        id: 999,
    });
    let (sorted, mut ranges) = cpu_sort(&live);
    // The sorter gives an empty cell the running start too (write_ranges), so
    // the last cell's end is the live count even when that cell is empty.
    let mut end = 0;
    for r in &mut ranges {
        r.start = end;
        end += r.count;
    }
    let taken = |j: [usize; 3]| {
        sorted.iter().any(|p| {
            (0..3).all(|a| {
                ((2.0 * (p.position_radius[a] - MIN[a]) / H).floor() as i64) == j[a] as i64
            })
        })
    };
    let mut want = Vec::new();
    for idx in 0..site_count {
        let j = [
            idx % sites[0],
            (idx / sites[0]) % sites[1],
            idx / (sites[0] * sites[1]),
        ];
        want.push(u32::from(in_box(site(j), lo, cells) && !taken(j)));
    }
    let flagged = want.iter().filter(|&&f| f == 1).count();
    let inside = (0..site_count).filter(|&idx| {
        in_box(
            site([
                idx % sites[0],
                (idx / sites[0]) % sites[1],
                idx / (sites[0] * sites[1]),
            ]),
            lo,
            cells,
        )
    });
    let inside = inside.count();
    assert!(
        inside > flagged && flagged > 0,
        "the fixture holds taken and empty inflow sites: {flagged} of {inside} empty"
    );
    let params = region_params();
    let got: Vec<u32> = Pass::new()
        .bind_ranges(&ranges)
        .bind(2, &sorted, Layout::Dense)
        .bind(44, &box_sites(), Layout::Dense)
        .bind(48, &[LiquidBody::default()], Layout::Dense)
        .bind(15, &[shape], Layout::Dense)
        .bind(16, &atlas, Layout::Dense)
        .bind(36, &[row], Layout::Dense)
        .run("emit_flags", &params, 37, site_count, site_count);
    assert_eq!(got, want, "the flags are the empty inflow sites");

    // Two short of room: a full pool emits fewer, never errors.
    let capacity = sorted.len() + flagged - 2;
    let scan: Vec<u32> = want
        .iter()
        .scan(0, |sum, &f| {
            *sum += f;
            Some(*sum)
        })
        .collect();
    let mut pool = sorted.clone();
    pool.resize(capacity + 4, FluidParticle::default());
    // The identity after the birth stage granted the room: ids from 1000, and
    // `n` the live prefix plus the births that fit.
    let identity = [0u32, 0, 1000, 0, 1, 1, 1, capacity as u32];
    let written: Vec<FluidParticle> = Pass::new()
        .bind_ranges(&ranges)
        .bind(15, &[shape], Layout::Dense)
        .bind(16, &atlas, Layout::Dense)
        .bind(36, &[row], Layout::Dense)
        .bind(37, &scan, Layout::Dense)
        .bind(38, &pool, Layout::Dense)
        .bind(44, &box_sites(), Layout::Dense)
        .bind(47, &identity, Layout::Dense)
        .run("emit_write", &params, 38, pool.len(), site_count);
    assert_eq!(
        written[..sorted.len()],
        sorted[..],
        "the live prefix is untouched"
    );
    assert!(
        written[capacity..]
            .iter()
            .all(|p| *p == FluidParticle::default()),
        "nothing past the pool's slots"
    );
    let emitted: Vec<usize> = (0..site_count).filter(|&idx| want[idx] == 1).collect();
    for (slot, &idx) in (sorted.len()..capacity).zip(&emitted) {
        let x = site([
            idx % sites[0],
            (idx / sites[0]) % sites[1],
            idx / (sites[0] * sites[1]),
        ]);
        let p = written[slot];
        for a in 0..3 {
            close(p.position_radius[a], x[a], 1.0, "emitted position");
            close(
                p.velocity[a],
                f64::from(velocity[a]),
                1.0,
                "emitted velocity",
            );
        }
        close(
            p.position_radius[3],
            0.31017524 * f64::from(H),
            1.0,
            "emitted radius",
        );
        assert_eq!(p.id, 1000 + (slot - sorted.len()) as u32);
    }
    let live_after = written
        .iter()
        .filter(|p| p.position_radius[3] > 0.0)
        .count();
    assert_eq!(
        live_after, capacity,
        "the count is the live prefix plus the emitted, up to the pool"
    );

    // The tally: the births and the two the full pool refused, summed over
    // the tick's substeps into stats words 27 and 28 (the solver tail from
    // INFLOW_BIRTHS_TAIL), so a full pool is reported. The step runs
    // birth_tally_reset before the tick's first substep's births, which also
    // clears the sheet refusals and the particles removed for want of a tile
    // slot; the words are the whole solver tail.
    let tail = INFLOW_BIRTHS_TAIL as usize;
    let sheet = manifold_water_liquid::primitives::liquid_stats::SHEET_REFUSED_TAIL as usize;
    let solver = manifold_water_liquid::primitives::liquid_stats::SOLVER_WORDS as usize;
    let tally = |first_substep: bool, before: [u32; 2]| -> ([u32; 2], u32) {
        let mut words = vec![0u32; solver];
        words[tail..tail + 2].copy_from_slice(&before);
        words[sheet] = 3;
        if first_substep {
            words = Pass::new().bind(22, &words, Layout::Dense).run(
                "birth_tally_reset",
                &params,
                22,
                solver,
                1,
            );
        }
        let got: Vec<u32> = Pass::new()
            .bind_ranges(&ranges)
            .bind(37, &scan, Layout::Dense)
            .bind(47, &identity, Layout::Dense)
            .bind(22, &words, Layout::Dense)
            .run(
                "emit_tally",
                &StepParams {
                    step_in_tick: i32::from(!first_substep),
                    ..params
                },
                22,
                solver,
                1,
            );
        ([got[tail], got[tail + 1]], got[sheet])
    };
    let births = (capacity - sorted.len()) as u32;
    assert_eq!(
        tally(true, [5, 7]),
        ([births, 2], 0),
        "the tick's first substep starts its birth words at zero"
    );
    assert_eq!(
        tally(false, [5, 7]),
        ([5 + births, 9], 3),
        "a later substep adds to them"
    );

    // Jitter factor 1, the region a full cell deep everywhere: each emitted
    // particle moves uniformly up to a quarter cell each way, keyed by site
    // and substep (_jitterMarkerParticlePosition).
    // first_tick 3 too: the one region row is the tick's.
    let params = StepParams {
        emit_jitter: 0.25,
        tick_index: 3,
        first_tick: 3,
        step_in_tick: 1,
        ..params
    };
    let jittered: Vec<FluidParticle> = Pass::new()
        .bind_ranges(&ranges)
        .bind(15, &[shape], Layout::Dense)
        .bind(16, &atlas, Layout::Dense)
        .bind(36, &[row], Layout::Dense)
        .bind(37, &scan, Layout::Dense)
        .bind(38, &pool, Layout::Dense)
        .bind(44, &box_sites(), Layout::Dense)
        .bind(47, &identity, Layout::Dense)
        .run("emit_write", &params, 38, pool.len(), site_count);
    let substep = 3u32 * 64 + 1;
    let mut spread = 0.0f64;
    for (slot, &idx) in (sorted.len()..capacity).zip(&emitted) {
        let x = site([
            idx % sites[0],
            (idx / sites[0]) % sites[1],
            idx / (sites[0] * sites[1]),
        ]);
        let key = (idx as u32)
            .wrapping_mul(3)
            .wrapping_add(substep.wrapping_mul(2_654_435_761));
        for (a, &site_a) in x.iter().enumerate() {
            let unit = f64::from(cpu_fill_hash(key.wrapping_add(a as u32)) >> 8) / 16_777_216.0;
            let want = site_a + f64::from(H) * 0.25 * (2.0 * unit - 1.0);
            close(
                jittered[slot].position_radius[a],
                want,
                1.0,
                "jittered position",
            );
            spread = spread.max((want - site_a).abs() / f64::from(H));
        }
    }
    println!(
        "emit proof: {inside} inflow sites, {flagged} empty, {} written to a pool {} short, jitter spread {spread:.4} cells",
        capacity - sorted.len(),
        flagged + sorted.len() - capacity
    );
    assert!(
        spread > 0.2 && spread <= 0.25,
        "the draw reaches near a quarter cell and never past it: {spread}"
    );
}

/// Shared words on the device, for the birth reservation's inputs.
fn device_words(device: &GpuDevice, words: &[u32]) -> GpuBuffer {
    let buffer = device.create_buffer_shared((words.len() * 4) as u64);
    // SAFETY: a shared buffer of exactly these words; no GPU work in flight.
    unsafe { buffer.write(0, bytemuck::cast_slice(words)) };
    buffer
}

/// A full pool refuses an inflow's whole request. The birth reservation grants
/// no slot, so the write places no particle and the tally counts the request
/// as refused, not as births.
#[test]
pub(super) fn gpu_flip_inflow_into_a_pool_with_no_room_writes_nothing_and_refuses_the_request() {
    use manifold_water_liquid::primitives::particle_identity::{
        BirthReservation, ParticleIdentity,
    };

    let (lo, cells, velocity) = ([1, 2, 1], [3, 2, 2], [0.4, -1.5, 0.2]);
    let (row, shape, atlas) = region_box(MIN, 2.0, lo, cells, velocity);
    let site = |j: [usize; 3]| -> [f64; 3] {
        std::array::from_fn(|a| f64::from(MIN[a]) + (0.25 + 0.5 * j[a] as f64) * f64::from(H))
    };
    let site_count = box_site_count();
    let mut live: Vec<FluidParticle> = random_particles(0xe31, 120)
        .into_iter()
        .filter(|p| p.position_radius[3] > 0.0)
        .collect();
    let held = site([2 * lo[0] + 1, 2 * lo[1], 2 * lo[2] + 1]);
    live.push(FluidParticle {
        position_radius: [held[0] as f32, held[1] as f32, held[2] as f32, 0.08],
        velocity: [0.0; 3],
        id: 999,
    });
    let (sorted, mut ranges) = cpu_sort(&live);
    let mut end = 0;
    for r in &mut ranges {
        r.start = end;
        end += r.count;
    }
    let params = region_params();
    let flags: Vec<u32> = Pass::new()
        .bind_ranges(&ranges)
        .bind(2, &sorted, Layout::Dense)
        .bind(44, &box_sites(), Layout::Dense)
        .bind(48, &[LiquidBody::default()], Layout::Dense)
        .bind(15, &[shape], Layout::Dense)
        .bind(16, &atlas, Layout::Dense)
        .bind(36, &[row], Layout::Dense)
        .run("emit_flags", &params, 37, site_count, site_count);
    let scan: Vec<u32> = flags
        .iter()
        .scan(0, |sum, &f| {
            *sum += f;
            Some(*sum)
        })
        .collect();
    let requested = scan[site_count - 1];
    assert!(requested > 0, "the fixture has empty inflow sites");

    // The pool's slots are all live: no room for the request.
    let device = manifold_gpu::testkit::test_device();
    let live_end = sorted.len() as u32;
    let mut plan = [0u32; 12];
    plan[0] = 0.01f32.to_bits();
    plan[11] = 1;
    let plan = device_words(&device, &plan);
    let particles = device_words(&device, &vec![0u32; sorted.len() * 8]);
    let ranges_buf = device_words(&device, bytemuck::cast_slice(&ranges));
    let scan_buf = device_words(&device, &scan);
    let counts = device_words(&device, &[0u32; 8]);
    let identity = device_words(&device, &[1000, 0, 0, 0, 0, 0, 0, 0]);
    let mut ops = ParticleIdentity::default();
    ops.prepare(&device);
    let mut enc = device.create_encoder("inflow birth reservation, no room");
    ops.reserve(
        &mut enc,
        BirthReservation {
            particles: &particles,
            identity: &identity,
            ranges: &ranges_buf,
            scan: &scan_buf,
            plan: &plan,
            tile_counts: &counts,
            step_record: None,
            params: [live_end, ranges.len() as u32, site_count as u32, 5],
        },
    );
    enc.commit_and_wait_completed();
    let granted: Vec<u32> = read_staged(&device, &identity, 8);
    assert_eq!(granted[7], live_end, "no room: n stays the live end");
    assert_eq!(granted[3], 0, "no reset is requested");

    let written: Vec<FluidParticle> = Pass::new()
        .bind_ranges(&ranges)
        .bind(15, &[shape], Layout::Dense)
        .bind(16, &atlas, Layout::Dense)
        .bind(36, &[row], Layout::Dense)
        .bind(37, &scan, Layout::Dense)
        .bind(38, &sorted, Layout::Dense)
        .bind(44, &box_sites(), Layout::Dense)
        .bind(47, &granted, Layout::Dense)
        .run("emit_write", &params, 38, sorted.len(), site_count);
    assert_eq!(written, sorted, "no particle is written into a full pool");

    let tail = INFLOW_BIRTHS_TAIL as usize;
    let solver = manifold_water_liquid::primitives::liquid_stats::SOLVER_WORDS as usize;
    let words: Vec<u32> = Pass::new()
        .bind_ranges(&ranges)
        .bind(37, &scan, Layout::Dense)
        .bind(47, &granted, Layout::Dense)
        .bind(22, &vec![0u32; solver], Layout::Dense)
        .run("emit_tally", &params, 22, solver, 1);
    assert_eq!(words[tail], 0, "no inflow birth");
    assert_eq!(words[tail + 1], requested, "the whole request is refused");
}

/// An outflow empties (fluidsimulation.cpp 9011-9031): a particle whose new
/// position its distance holds below zero dies; every other keeps its radius.
#[test]
pub(super) fn gpu_flip_outflow_kills_the_particles_it_holds() {
    let (lo, cells) = ([2, 1, 0], [3, 3, 2]);
    let (row, shape, atlas) = region_box(MIN, 3.0, lo, cells, [0.0; 3]);
    let particles: Vec<FluidParticle> = random_particles(0xd7a, 300)
        .into_iter()
        .map(|mut p| {
            p.velocity = [0.0; 3];
            p
        })
        .collect();
    // Still faces: nothing moves, so the new position is the old one.
    let faces = vec![FaceSample::default(); face_len()];
    let step = StepParams {
        step_dt: 0.02,
        flip: 1.0,
        particles: particles.len() as u32,
        ..region_params()
    };
    let mut pass = Pass::new();
    let solid = pass.wall_corners(MIN, box_hi(), 63);
    let got: Vec<FluidParticle> = pass
        .bind(2, &particles, Layout::Dense)
        .bind(3, &faces, Layout::Records)
        .bind(9, &solid, Layout::Dense)
        .bind(17, &faces, Layout::Records)
        .bind(18, &faces, Layout::Records)
        .bind(22, &vec![0u32; 2 * particles.len()], Layout::Dense)
        .bind(15, &[shape], Layout::Dense)
        .bind(16, &atlas, Layout::Dense)
        .bind(36, &[row], Layout::Dense)
        .run(
            "faces_to_particles",
            &step,
            19,
            particles.len(),
            particles.len(),
        );
    let mut drained = 0;
    for (i, (g, p)) in got.iter().zip(&particles).enumerate() {
        if p.position_radius[3] <= 0.0 {
            assert_eq!(
                g.position_radius[3], p.position_radius[3],
                "unused slot {i} stays unused"
            );
            continue;
        }
        let x: [f64; 3] = std::array::from_fn(|a| f64::from(g.position_radius[a]));
        let dies = in_box(x, lo, cells);
        drained += usize::from(dies);
        let want = if dies { 0.0 } else { p.position_radius[3] };
        assert_eq!(g.position_radius[3], want, "particle {i} at {x:?}");
    }
    let alive = got.iter().filter(|p| p.position_radius[3] > 0.0).count();
    let before = particles
        .iter()
        .filter(|p| p.position_radius[3] > 0.0)
        .count();
    println!(
        "drain proof: {drained} drained of {} live",
        particles
            .iter()
            .filter(|p| p.position_radius[3] > 0.0)
            .count()
    );
    assert!(
        drained > 10,
        "the drain holds a share of the draw: {drained}"
    );
    assert_eq!(
        alive,
        before - drained,
        "the live count drops by exactly the drained"
    );
}
