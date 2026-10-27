use super::*;
use manifold_water_liquid::tile_storage::{TILE, tile_counts, tile_total};

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
        "remove_crowded_markers",
        "density_source",
        "faces_to_particles",
        "tiles_clear",
        "tiles_mark",
        "tiles_rings",
        "tiles_allocate",
        "tiles_fresh",
        "tiles_lists",
        "tiles_retire",
        "slot_load",
        "slot_open",
        "pool_canon",
        "pocket_accumulate_coarse",
        "pocket_remove_coarse",
        "pocket_leader_clear",
        "pocket_coarsen",
        "pocket_relabel",
    ] {
        assert!(entries.contains(&entry), "missing entry {entry}");
    }
}

#[test]
fn step_params_match_the_shader_uniform() {
    assert_eq!(size_of::<StepParams>(), 176);
    assert_eq!(std::mem::offset_of!(StepParams, box_offset), 100);
    assert_eq!(std::mem::offset_of!(StepParams, particles), 108);
    assert_eq!(std::mem::offset_of!(StepParams, live_impulse_stride), 152);
    assert_eq!(std::mem::offset_of!(StepParams, wall_inset), 156);
    assert_eq!(std::mem::offset_of!(StepParams, coupled_h), 160);
    assert_eq!(std::mem::offset_of!(StepParams, pool_tiles), 164);
    assert_eq!(std::mem::offset_of!(StepParams, alloc_ring), 168);
    assert_eq!(std::mem::offset_of!(StepParams, inv_cell), 172);
}

/// The tile table's bytes, by an independent count: five words a tile
/// (nearness, two ring halves, the list, the retired list), the slot table
/// (a word per tile and per pool slot, the pool as large as the box), the
/// free stack (a word per slot), the fresh list (a word per tile), the
/// counts and the triples; partial edge tiles counted whole; the extent's
/// hold grows by exactly that.
#[test]
fn gpu_flip_tile_table_bytes_follow_the_lattice() {
    assert_eq!(tile_counts([64, 64, 64]), [8, 8, 8]);
    assert_eq!(tile_counts([63, 100, 8]), [8, 13, 1]);
    assert_eq!(tile_counts([1, 9, 17]), [1, 2, 3]);
    assert_eq!(ring_max(14), 2, "64³: 14 layers reach the second ring");
    assert_eq!(ring_max(23), 3, "128³: 23 layers reach the third");
    assert_eq!(ring_max(FACE_VALID_LAYERS), 1);
    for (cells, tiles) in [
        ([64, 64, 64], 512u64),
        ([63, 100, 8], 104),
        ([128, 128, 128], 4096),
        ([1, 9, 17], 6),
    ] {
        for r in 1..5u32 {
            // Counts: |C|, the rings 1..=r + 1, the retired count, the parity,
            // then the free top, the exhausted flag and the
            // fresh count.
            let counts = u64::from(r) + 2 + 1 + 1 + 3;
            let triples = 3 * (u64::from(r) + 1 + 1);
            // The triples, the aligned replay range, the fresh triple.
            let arguments = triples + triples % 2 + 2 + 3;
            assert_eq!(
                tile_scratch_bytes(cells, r),
                (5 * tiles + 2 * tiles + tiles + tiles + counts + arguments) * 4,
                "{cells:?} ring_max {r}"
            );
            assert_eq!(
                scratch_bytes(cells, 1000, r) - tile_scratch_bytes(cells, r),
                scratch_bytes(cells, 1000, 1) - tile_scratch_bytes(cells, 1)
            );
        }
    }
}

#[test]
fn closed_faces_takes_the_six_bit_mask_and_refuses_the_rest() {
    assert_eq!(read_closed_faces(0.0), Ok(0));
    assert_eq!(read_closed_faces(63.0), Ok(63));
    assert_eq!(read_closed_faces(61.0), Ok(61));
    for bad in [64.0, -1.0, 2.5, f32::NAN, f32::INFINITY] {
        assert!(
            read_closed_faces(bad).unwrap_err().contains("Closed Faces"),
            "{bad}"
        );
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

/// The pool holds the whole box at Matter/FLIP resolution 64 and 128 (the
/// solver lattice is resolution + 3 cells a side) and what the budget buys
/// when it is short.
#[test]
fn pool_size_follows_the_budget() {
    let tile_bytes = u64::from(TILE).pow(3) * pooled_bytes_per_cell();
    assert_eq!(tile_bytes, 512 * 368);
    for resolution in [64u32, 128] {
        let cells = [resolution + 3; 3];
        assert_eq!(
            u64::from(pool_tiles(cells)),
            tile_total(cells),
            "resolution {resolution}"
        );
        assert!(tile_total(cells) * tile_bytes <= POOL_BUDGET_BYTES);
    }
    assert_eq!(pool_capacity(4913, tile_bytes, 10 * tile_bytes), 10);
    assert_eq!(
        pool_capacity(4913, tile_bytes, 10 * tile_bytes + tile_bytes - 1),
        10
    );
    assert_eq!(pool_capacity(4913, tile_bytes, u64::MAX / 2), 4913);
    assert_eq!(pool_capacity(7, tile_bytes, 10 * tile_bytes), 7);
}
