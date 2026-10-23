//! Where particles come from and go: the fill, inflow emission and outflow removal.

use super::*;

fn cpu_fill_hash(x: u32) -> u32 {
    let s = x.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let w = ((s >> ((s >> 28) + 4)) ^ s).wrapping_mul(277_803_737);
    (w >> 22) ^ w
}

#[test]
fn gpu_flip_liquid_fill_places_pool_then_box() {
    let mut harness = Harness::new();
    // Sites are half cells: a 12 × 10 × 8 site lattice.
    let (pool, sites, seed, jitter) = (1u32, [[2u32, 8], [0, 4], [3, 9]], 7u32, 0.5f32);
    // 96 pool sites (12 × 1 × 8) and 90 box sites (6 × 3 × 5); the fill owns
    // storage for exactly those.
    let placed = 96 + 90;
    let out = harness.array::<FluidParticle>(&[], 1);
    let count = harness.scalar();
    // The fill reads the padded lattice the domain publishes: the box N at
    // MIN, grown by the padding on every side.
    let pad = PADDING_NODES as f32;
    let step = params(&[
        ("nodes_x", N[0] as f32 + 1.0 + 2.0 * pad),
        ("nodes_y", N[1] as f32 + 1.0 + 2.0 * pad),
        ("nodes_z", N[2] as f32 + 1.0 + 2.0 * pad),
        ("cell_size", H),
        ("lattice_min_x", MIN[0] - pad * H),
        ("lattice_min_y", MIN[1] - pad * H),
        ("lattice_min_z", MIN[2] - pad * H),
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
    let (scalars, errors) = harness.run(&mut fill, &[], &[("particles", out.0), ("count", count)], &step);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(scalars.iter().any(|(s, v)| *s == count && *v == ParamValue::Float(placed as f32)), "{scalars:?}");
    let storage = harness.buffer(out.0);
    assert_eq!(storage.size, (placed * std::mem::size_of::<FluidParticle>()) as u64, "the fill's storage holds the fill");
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
        let key = (i as u32).wrapping_mul(3).wrapping_add(seed.wrapping_mul(2_654_435_761));
        for a in 0..3 {
            let unit = (cpu_fill_hash(key.wrapping_add(a as u32)) >> 8) as f32 / 16_777_216.0;
            let local = 0.25 + 0.5 * s[a] as f32 + 0.5 * jitter * (unit - 0.5);
            let want = MIN[a] + local * H;
            assert!((g.position_radius[a] - want).abs() < 1e-5, "particle {i} axis {a}: {} vs {want}", g.position_radius[a]);
        }
        assert!((g.position_radius[3] - 0.31017524 * H).abs() < 1e-6);
        assert_eq!((g.velocity, g.id), ([0.0; 3], i as u32 + 1), "particle {i}");
    }
    // The domain run that sites a fill past its 4 m top reports it capped:
    // the report the cards' setup check reads (SCENE_SETUP_PROGRAMME.md B3).
    let mut domain = super::super::gpu_flip_domain::GpuFlipDomain::new();
    let _ = harness.run(&mut domain, &[], &[], &params(&[("fill_height", 9.0), ("resolution", 8.0)]));
    let snapshot = manifold_water_rigid::node::PhysicsNode::fluid_domain_snapshot(&domain).expect("a snapshot");
    assert!(snapshot.fit.fill_capped, "{snapshot:?}");
}

// ── Solids ─────────────────────────────────────────────────────────────────

/// One region row over a box of whole cells, `lo` to `lo + cells`, its
/// lattice holding −1 everywhere, so a point is inside exactly when it is in
/// the box (sites sit at quarter cells, never on a face); its shape and atlas.
pub(super) fn region_box(code: f32, lo: [usize; 3], cells: [u32; 3], velocity: [f32; 3]) -> (LiquidBody, LiquidShape, Vec<u32>) {
    let dims = cells.map(|c| c + 1);
    let mut atlas = Vec::new();
    pack_distance_atlas(&vec![-1.0; dims.iter().product::<u32>() as usize], &mut atlas);
    let shape = LiquidShape {
        origin_spacing: [0.0, 0.0, 0.0, H],
        dims_x: dims[0],
        dims_y: dims[1],
        dims_z: dims[2],
        atlas_offset: 0,
        scale_min: [1.0; 4],
    };
    let p: [f32; 3] = std::array::from_fn(|a| MIN[a] + lo[a] as f32 * H);
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

fn in_box(x: [f64; 3], lo: [usize; 3], cells: [u32; 3]) -> bool {
    (0..3).all(|a| {
        let q = (x[a] - f64::from(MIN[a])) / f64::from(H);
        q >= lo[a] as f64 && q <= (lo[a] + cells[a] as usize) as f64
    })
}

fn region_params() -> StepParams {
    StepParams { region_count: 1, region_rows: 1, shapes_len: 1, ..lattice() }
}

/// An inflow fills (fluidsimulation.cpp 8566-8603, 8771-8811): every empty
/// half-cell site inside it and outside the solid takes one particle at rest
/// in the inflow's velocity, written after the live prefix in site order; an
/// occupied site and a site past the pool's slots take none.
#[test]
fn gpu_flip_inflow_emits_at_empty_sites_into_free_slots() {
    let (lo, cells, velocity) = ([1, 2, 1], [3, 2, 2], [0.4, -1.5, 0.2]);
    let (row, shape, atlas) = region_box(2.0, lo, cells, velocity);
    let site = |j: [usize; 3]| -> [f64; 3] { std::array::from_fn(|a| f64::from(MIN[a]) + (0.25 + 0.5 * j[a] as f64) * f64::from(H)) };
    let sites: [usize; 3] = N.map(|n| 2 * n);
    let site_count: usize = sites.iter().product();
    let mut live: Vec<FluidParticle> =
        random_particles(0xe31, 120).into_iter().filter(|p| p.position_radius[3] > 0.0).collect();
    // One particle on an inflow site, so the proof sees a taken site skipped.
    let held = site([2 * lo[0] + 1, 2 * lo[1], 2 * lo[2] + 1]);
    live.push(FluidParticle { position_radius: [held[0] as f32, held[1] as f32, held[2] as f32, 0.08], velocity: [0.0; 3], id: 999 });
    let (sorted, mut ranges) = cpu_sort(&live);
    // The sorter gives an empty cell the running start too (write_ranges), so
    // the last cell's end is the live count even when that cell is empty.
    let mut end = 0;
    for r in &mut ranges {
        r.start = end;
        end += r.count;
    }
    let taken = |j: [usize; 3]| {
        sorted.iter().any(|p| (0..3).all(|a| ((2.0 * (p.position_radius[a] - MIN[a]) / H).floor() as i64) == j[a] as i64))
    };
    let mut want = Vec::new();
    for idx in 0..site_count {
        let j = [idx % sites[0], (idx / sites[0]) % sites[1], idx / (sites[0] * sites[1])];
        want.push(u32::from(in_box(site(j), lo, cells) && !taken(j)));
    }
    let flagged = want.iter().filter(|&&f| f == 1).count();
    let inside = (0..site_count).filter(|&idx| {
        in_box(site([idx % sites[0], (idx / sites[0]) % sites[1], idx / (sites[0] * sites[1])]), lo, cells)
    });
    let inside = inside.count();
    assert!(inside > flagged && flagged > 0, "the fixture holds taken and empty inflow sites: {flagged} of {inside} empty");
    let corners = vec![1.0f32; face_len()];
    let params = region_params();
    let got: Vec<u32> = Pass::new()
        .bind(1, &ranges)
        .bind(2, &sorted)
        .bind(9, &corners)
        .bind(15, &[shape])
        .bind(16, &atlas)
        .bind(36, &[row])
        .run("emit_flags", &params, 37, site_count, site_count);
    assert_eq!(got, want, "the flags are the empty inflow sites");

    // Two short of room: a full pool emits fewer, never errors.
    let capacity = sorted.len() + flagged - 2;
    let scan: Vec<u32> = want.iter().scan(0, |sum, &f| { *sum += f; Some(*sum) }).collect();
    let mut pool = sorted.clone();
    pool.resize(capacity + 4, FluidParticle::default());
    let params = StepParams { capacity: capacity as u32, ..params };
    let written: Vec<FluidParticle> = Pass::new()
        .bind(1, &ranges)
        .bind(15, &[shape])
        .bind(16, &atlas)
        .bind(36, &[row])
        .bind(37, &scan)
        .bind(38, &pool)
        .bind(47, &[0u32, 0, 1000, 0])
        .run("emit_write", &params, 38, pool.len(), site_count);
    assert_eq!(written[..sorted.len()], sorted[..], "the live prefix is untouched");
    assert!(written[capacity..].iter().all(|p| *p == FluidParticle::default()), "nothing past the pool's slots");
    let emitted: Vec<usize> = (0..site_count).filter(|&idx| want[idx] == 1).collect();
    for (slot, &idx) in (sorted.len()..capacity).zip(&emitted) {
        let x = site([idx % sites[0], (idx / sites[0]) % sites[1], idx / (sites[0] * sites[1])]);
        let p = written[slot];
        for a in 0..3 {
            close(p.position_radius[a], x[a], 1.0, "emitted position");
            close(p.velocity[a], f64::from(velocity[a]), 1.0, "emitted velocity");
        }
        close(p.position_radius[3], 0.31017524 * f64::from(H), 1.0, "emitted radius");
        assert_eq!(p.id, 1000 + (slot - sorted.len()) as u32);
    }
    let live_after = written.iter().filter(|p| p.position_radius[3] > 0.0).count();
    assert_eq!(live_after, capacity, "the count is the live prefix plus the emitted, up to the pool");

    // The tally: the births and the two the full pool refused, summed over
    // the tick's substeps from zero at its first, into stats words 28 and 29
    // (the solver tail from INFLOW_BIRTHS_TAIL), so a full pool is reported.
    let tail = INFLOW_BIRTHS_TAIL as usize;
    let tally = |step_in_tick: i32, before: [u32; 2]| -> [u32; 2] {
        let mut words = vec![0u32; tail + 2];
        words[tail..].copy_from_slice(&before);
        let got: Vec<u32> = Pass::new()
            .bind(1, &ranges)
            .bind(37, &scan)
            .bind(47, &[0u32, 0, 1000, 0])
            .bind(22, &words)
            .run("emit_tally", &StepParams { step_in_tick, ..params }, 22, tail + 2, 1);
        [got[tail], got[tail + 1]]
    };
    let births = (capacity - sorted.len()) as u32;
    assert_eq!(tally(0, [5, 7]), [births, 2], "the first substep starts the tick's tally at zero");
    assert_eq!(tally(1, [5, 7]), [5 + births, 9], "a later substep adds to it");

    // Jitter factor 1, the region a full cell deep everywhere: each emitted
    // particle moves uniformly up to a quarter cell each way, keyed by site
    // and substep (_jitterMarkerParticlePosition).
    // first_tick 3 too: the one region row is the tick's.
    let params = StepParams { emit_jitter: 0.25, tick_index: 3, first_tick: 3, step_in_tick: 1, ..params };
    let jittered: Vec<FluidParticle> = Pass::new()
        .bind(1, &ranges)
        .bind(15, &[shape])
        .bind(16, &atlas)
        .bind(36, &[row])
        .bind(37, &scan)
        .bind(38, &pool)
        .bind(47, &[0u32, 0, 1000, 0])
        .run("emit_write", &params, 38, pool.len(), site_count);
    let substep = 3u32 * 64 + 1;
    let mut spread = 0.0f64;
    for (slot, &idx) in (sorted.len()..capacity).zip(&emitted) {
        let x = site([idx % sites[0], (idx / sites[0]) % sites[1], idx / (sites[0] * sites[1])]);
        let key = (idx as u32).wrapping_mul(3).wrapping_add(substep.wrapping_mul(2_654_435_761));
        for (a, &site_a) in x.iter().enumerate() {
            let unit = f64::from(cpu_fill_hash(key.wrapping_add(a as u32)) >> 8) / 16_777_216.0;
            let want = site_a + f64::from(H) * 0.25 * (2.0 * unit - 1.0);
            close(jittered[slot].position_radius[a], want, 1.0, "jittered position");
            spread = spread.max((want - site_a).abs() / f64::from(H));
        }
    }
    println!("emit proof: {inside} inflow sites, {flagged} empty, {} written to a pool {} short, jitter spread {spread:.4} cells", capacity - sorted.len(), flagged + sorted.len() - capacity);
    assert!(spread > 0.2 && spread <= 0.25, "the draw reaches near a quarter cell and never past it: {spread}");
}

/// An outflow empties (fluidsimulation.cpp 9011-9031): a particle whose new
/// position its distance holds below zero dies; every other keeps its radius.
#[test]
fn gpu_flip_outflow_kills_the_particles_it_holds() {
    let (lo, cells) = ([2, 1, 0], [3, 3, 2]);
    let (row, shape, atlas) = region_box(3.0, lo, cells, [0.0; 3]);
    let particles: Vec<FluidParticle> = random_particles(0xd7a, 300)
        .into_iter()
        .map(|mut p| {
            p.velocity = [0.0; 3];
            p
        })
        .collect();
    // Still faces: nothing moves, so the new position is the old one.
    let faces = vec![FaceSample::default(); face_len()];
    let step = StepParams { step_dt: 0.02, flip: 1.0, particles: particles.len() as u32, ..region_params() };
    let got: Vec<FluidParticle> = Pass::new()
        .bind(2, &particles)
        .bind(3, &faces)
        .bind(9, &wall_solid(N, H))
        .bind(17, &faces)
        .bind(18, &faces)
        .bind(22, &vec![0u32; 2 * particles.len()])
        .bind(15, &[shape])
        .bind(16, &atlas)
        .bind(36, &[row])
        .run("faces_to_particles", &step, 19, particles.len(), particles.len());
    let mut drained = 0;
    for (i, (g, p)) in got.iter().zip(&particles).enumerate() {
        if p.position_radius[3] <= 0.0 {
            assert_eq!(g.position_radius[3], p.position_radius[3], "unused slot {i} stays unused");
            continue;
        }
        let x: [f64; 3] = std::array::from_fn(|a| f64::from(g.position_radius[a]));
        let dies = in_box(x, lo, cells);
        drained += usize::from(dies);
        let want = if dies { 0.0 } else { p.position_radius[3] };
        assert_eq!(g.position_radius[3], want, "particle {i} at {x:?}");
    }
    let alive = got.iter().filter(|p| p.position_radius[3] > 0.0).count();
    let before = particles.iter().filter(|p| p.position_radius[3] > 0.0).count();
    println!("drain proof: {drained} drained of {} live", particles.iter().filter(|p| p.position_radius[3] > 0.0).count());
    assert!(drained > 10, "the drain holds a share of the draw: {drained}");
    assert_eq!(alive, before - drained, "the live count drops by exactly the drained");
}

