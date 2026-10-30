use super::*;
use manifold_water_liquid::bounds::ALL_FACES;
use manifold_water_liquid::tile_pool::{POOL_BUDGET_BYTES, pool_capacity};
use manifold_water_liquid::tile_storage::TILE_CELLS;

/// A live tick encodes the substeps the retired speed asks for at the CFL,
/// with half again to spare: 10 m/s at 1/16 m over 1/60 s wants 0.53 of a
/// step and gets 1, 40 m/s wants 2.1 and gets 4. Never under the minimum or
/// over every slot; no reading, or a broken one, takes every slot.
#[test]
fn live_steps_follow_the_retired_speed() {
    let max = manifold_physics::stepping::LIVE_DEFAULT_MAX_STEPS;
    let at = |speed, minimum| live_steps(speed, 1.0 / 60.0, 0.0625, minimum);
    assert_eq!(at(Some(10.0), 1), 1);
    assert_eq!(at(Some(40.0), 1), 4);
    assert_eq!(at(Some(0.0), 1), 1);
    assert_eq!(at(Some(10.0), 3), 3);
    assert_eq!(at(Some(1e6), 1), max);
    assert_eq!(at(Some(10.0), 64), max);
    assert_eq!(at(None, 1), max);
    assert_eq!(at(Some(f32::NAN), 1), max);
    assert_eq!(at(Some(f32::INFINITY), 1), max);
    // A coarser cell or a shorter interval asks for fewer.
    assert_eq!(live_steps(Some(40.0), 1.0 / 120.0, 0.0625, 1), 2);
    assert_eq!(live_steps(Some(40.0), 1.0 / 60.0, 0.125, 1), 2);
}

/// A Wall or an Open face keeps the pool to the domain's; with neither the
/// water may grow its pool to what the budget buys, never below the domain's.
#[test]
fn only_unbounded_water_grows_past_the_domain_pool() {
    let budget = pool_capacity(
        u64::MAX,
        u64::from(TILE_CELLS) * pooled_bytes_per_cell(),
        POOL_BUDGET_BYTES,
    );
    assert!(budget > 2744, "the budget buys {budget} tiles");
    for (walls, open) in [(ALL_FACES, 0), (0, ALL_FACES), (0b100, 0b111011), (0, 0b1)] {
        assert_eq!(
            growth_ceiling(2744, walls, open),
            2744,
            "{walls:b} {open:b}"
        );
    }
    assert_eq!(growth_ceiling(2744, 0, 0), budget);
    assert_eq!(growth_ceiling(budget + 1, 0, 0), budget + 1);
}

/// Max Iterations rounds and is refused outside 1..=MAX_ITERATIONS or
/// non-finite; Auto converges within it; a fixed count runs as saved even
/// past it.
#[test]
fn max_iterations_bounds_auto_only() {
    assert_eq!(MAX_ITERATIONS, 900);
    assert_eq!(read_max_iterations(900.0), Ok(900));
    assert_eq!(read_max_iterations(1.0), Ok(1));
    assert_eq!(read_max_iterations(37.4), Ok(37));
    assert_eq!(read_max_iterations(0.6), Ok(1));
    for bad in [
        0.0,
        0.4,
        -3.0,
        900.6,
        1000.0,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
    ] {
        let refused = read_max_iterations(bad).expect_err("refused");
        assert!(
            refused.starts_with("Max Iterations must be 1 to 900"),
            "{refused}"
        );
    }
    assert_eq!(read_iterations(0.0, 300), Ok(Stop::Converged(300)));
    assert_eq!(read_iterations(-2.0, 900), Ok(Stop::Converged(900)));
    assert_eq!(
        read_iterations(24.0, 10),
        Ok(Stop::Fixed(24)),
        "a fixed count ignores Max Iterations"
    );
    assert_eq!(read_iterations(900.0, 1), Ok(Stop::Fixed(900)));
    assert!(read_iterations(901.0, 900).is_err());
    assert!(read_iterations(f32::NAN, 900).is_err());
}

/// The pocket sums' two-word add with carry (gpu_flip_step/pockets.wgsl
/// pocket_fixed, pocket_add, pocket_value) gives the exact sum of the
/// fixed-point values in any order.
#[test]
fn pocket_carry_sum_is_exact_in_any_order() {
    const SCALE: f64 = 65536.0;
    let fixed = |x: f64| -> (u32, u32) {
        let v = (x * SCALE).round() as i64;
        (v as u32, (v >> 32) as u32)
    };
    let add = |words: &mut (u32, u32), q: (u32, u32)| {
        let (low, carry) = words.0.overflowing_add(q.0);
        words.0 = low;
        words.1 = words.1.wrapping_add(q.1).wrapping_add(u32::from(carry));
    };
    let value = |w: (u32, u32)| ((i64::from(w.1 as i32) << 32) | i64::from(w.0)) as f64 / SCALE;
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    // Mixed signs and magnitudes, some past 2^32 in fixed point.
    let values: Vec<f64> = (0..20_000)
        .map(|i| {
            let unit = (next() % 2_000_001) as f64 / 1.0e6 - 1.0;
            unit * if i % 7 == 0 { 1.0e5 } else { 3.0 }
        })
        .collect();
    let want: f64 = values.iter().map(|&x| (x * SCALE).round()).sum::<f64>() / SCALE;
    let mut order: Vec<usize> = (0..values.len()).collect();
    let mut totals = Vec::new();
    for _ in 0..3 {
        for i in (1..order.len()).rev() {
            order.swap(i, (next() % (i as u64 + 1)) as usize);
        }
        let mut words = (0u32, 0u32);
        for &i in &order {
            add(&mut words, fixed(values[i]));
        }
        totals.push(words);
    }
    assert!(
        totals.windows(2).all(|w| w[0] == w[1]),
        "the words differ by order: {totals:?}"
    );
    assert_eq!(value(totals[0]), want, "the carry sum equals the exact sum");
}

#[test]
fn step_shader_validates_with_every_entry() {
    let source = step_source();
    let module = naga::front::wgsl::parse_str(&source)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{e:?}"));
    let mut bindings = std::collections::HashSet::new();
    for (_, global) in module.global_variables.iter() {
        if let Some(binding) = &global.binding {
            assert!(
                bindings.insert((binding.group, binding.binding)),
                "duplicate shader binding {binding:?}"
            );
        }
    }
    let entries: Vec<&str> = module
        .entry_points
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    for entry in [
        "particles_to_faces",
        "extend_faces",
        "face_gravity",
        "open_fractions",
        "solid_face_velocity",
        "phi_into_solids",
        "water_from_phi",
        "divergence",
        "particle_distance",
        "subtract_pressure",
        "constrain_solid_faces",
        "release_walls",
        "remove_crowded_markers",
        "owed_clear",
        "owed_count",
        "remove_owed_markers",
        "density_source",
        "faces_to_particles",
        "tiles_fresh",
        "tiles_retire",
        "solid_corners",
        "tiles_tally",
        "slot_load",
        "slot_open",
        "pool_canon",
        "pocket_accumulate",
        "pocket_remove",
        "pocket_leader_clear",
        "pocket_coarsen",
        "pocket_relabel",
    ] {
        assert!(entries.contains(&entry), "missing entry {entry}");
    }
}

#[test]
fn step_params_match_the_shader_uniform() {
    assert_eq!(size_of::<StepParams>(), 256);
    assert_eq!(std::mem::offset_of!(StepParams, bounds_hi), 16);
    assert_eq!(std::mem::offset_of!(StepParams, origin), 64);
    assert_eq!(std::mem::offset_of!(StepParams, inv_cell), 76);
    assert_eq!(std::mem::offset_of!(StepParams, tick_index), 80);
    assert_eq!(std::mem::offset_of!(StepParams, ghost), 116);
    assert_eq!(std::mem::offset_of!(StepParams, live_impulse_stride), 160);
    assert_eq!(std::mem::offset_of!(StepParams, open_faces), 164);
    assert_eq!(std::mem::offset_of!(StepParams, pool_tiles), 172);
    assert_eq!(std::mem::offset_of!(StepParams, field_min), 176);
    assert_eq!(std::mem::offset_of!(StepParams, table_entries), 188);
    assert_eq!(std::mem::offset_of!(StepParams, emit_sites), 192);
    assert_eq!(std::mem::offset_of!(StepParams, kill_height), 196);
    assert_eq!(std::mem::offset_of!(StepParams, wall_velocity_lo), 208);
    assert_eq!(std::mem::offset_of!(StepParams, wall_velocity_hi), 224);
    assert_eq!(std::mem::offset_of!(StepParams, sand_count), 248);
    assert_eq!(std::mem::offset_of!(StepParams, sand_drag), 252);
    assert_eq!(
        StepParams::default().kill_height,
        f32::MIN,
        "free space kills nothing"
    );
}

/// The tile rings the band reaches, and the world tiles' bytes: the
/// table's own figure for the pool, which the step's scratch holds once.
#[test]
fn gpu_flip_world_tile_bytes_follow_the_pool() {
    assert_eq!(ring_max(14), 2, "64³: 14 layers reach the second ring");
    assert_eq!(ring_max(23), 3, "128³: 23 layers reach the third");
    assert_eq!(ring_max(FACE_VALID_LAYERS), 1);
    for cells in [[64, 64, 64], [60, 97, 5], [1, 9, 17]] {
        let pool = pool_tiles(cells, 0);
        assert_eq!(
            tile_scratch_bytes(pool),
            manifold_water_liquid::world_tiles::held_bytes(pool, step_reach().ring_max),
            "{cells:?}"
        );
        assert_eq!(
            scratch_bytes(pool, 1000) - scratch_bytes(pool, 1),
            999 * size_of::<FluidParticle>() as u64,
            "{cells:?}: the slots add only the sorted particles"
        );
    }
}
#[test]
fn face_masks_take_the_six_bit_mask_and_refuse_the_rest() {
    for (read, name) in [
        (
            read_closed_faces as fn(f32) -> Result<u32, String>,
            "Closed Faces",
        ),
        (read_open_faces, "Open Faces"),
    ] {
        assert_eq!(read(0.0), Ok(0));
        assert_eq!(read(63.0), Ok(63));
        assert_eq!(read(61.0), Ok(61));
        for bad in [64.0, -1.0, 2.5, f32::NAN, f32::INFINITY] {
            assert!(read(bad).unwrap_err().contains(name), "{name} {bad}");
        }
    }
}

/// The band is the engine's ⌈√3 · CFL⌉ + 3, never under the
/// face grid's guarantee.
#[test]
fn band_layers_match_configured_engine_cfl() {
    assert_eq!(band_layers(3), 9);
    assert_eq!(band_layers(1), 5);
    assert_eq!(band_layers(5), 12, "the engine's 12 layers at its CFL 5");
    assert_eq!(band_layers(8), 17);
    assert_eq!(band_layers(0), 3);
    assert_eq!(band_layers(ENGINE_CFL), 12);
}

/// The pool holds every tile the step's table may hold for a box wherever
/// it sits on the world lattice: with every face a Wall, the most tiles any
/// placement's wall clip spans (each axis swept a third of a cell at a time
/// over a whole tile), and no ring; with none, those tiles widened by the
/// table's ring on both sides; and what the budget buys when it is short.
#[test]
fn pool_size_follows_the_box_the_walls_and_the_budget() {
    let ring = step_reach().ring_max;
    let h = 0.0625_f32;
    for size in [[0.5_f32, 0.5, 0.5], [0.81, 0.5, 1.97], [4.0, 2.53, 0.6]] {
        let layout = manifold_core::fluid_domain::domain_layout(
            Some(manifold_core::fluid_domain::DomainBox {
                pos: [0.0; 3],
                rot_euler: [0.0; 3],
                scale: size,
                billboard: false,
            }),
            4.0,
            f64::from(h),
        )
        .expect("a box of whole cells");
        let most: [u32; 3] = std::array::from_fn(|a| {
            (0..24)
                .map(|third| {
                    let mut params: StepParams = bytemuck::Zeroable::zeroed();
                    params.bounds_lo[a] = third as f32 * h / 3.0 - 0.37;
                    params.bounds_hi[a] = params.bounds_lo[a] + size[a];
                    params.inv_cell = 1.0 / h;
                    params.wall_faces = ALL_FACES;
                    let [lo, hi] = tiles::wall_clip(&params).expect("Walls clip");
                    (hi[a] - lo[a] + 1) as u32
                })
                .max()
                .expect("placements")
        });
        let cells = layout.cells;
        assert_eq!(
            pool_tiles(cells, ALL_FACES),
            most.iter().product::<u32>(),
            "{size:?}"
        );
        assert_eq!(
            pool_tiles(cells, 0),
            most.iter().map(|&t| t + 2 * ring).product::<u32>(),
            "{size:?}"
        );
        // An Open floor widens the y axis by one ring, below.
        assert_eq!(
            pool_tiles(cells, ALL_FACES & !0b100),
            most[0] * (most[1] + ring) * most[2],
            "{size:?}"
        );
    }
    let tile_bytes = u64::from(TILE_CELLS) * pooled_bytes_per_cell();
    assert_eq!(pool_capacity(4913, tile_bytes, 10 * tile_bytes), 10);
    assert_eq!(
        pool_capacity(4913, tile_bytes, 10 * tile_bytes + tile_bytes - 1),
        10
    );
    assert_eq!(pool_capacity(4913, tile_bytes, u64::MAX / 2), 4913);
    assert_eq!(pool_capacity(7, tile_bytes, 10 * tile_bytes), 7);
    let huge = [1024u32; 3];
    assert_eq!(
        u64::from(pool_tiles(huge, 0)),
        POOL_BUDGET_BYTES / tile_bytes,
        "a box past the budget gets what the budget buys"
    );
}
