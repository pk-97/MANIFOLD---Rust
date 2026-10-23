//! CPU tests of the step's parameters, layouts and shader text.

use super::*;
use super::lattice::{LATTICE_CELL_ARRAYS, mask_saved_bytes};
use super::pipelines::{band_source, step_source};
use super::pockets::{POCKET_ATOMIC_SITES, pocket_coarse_bytes, pocket_sum_bytes};

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
    for bad in [0.0, 0.4, -3.0, 900.6, 1000.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let refused = read_max_iterations(bad).expect_err("refused");
        assert!(refused.starts_with("Max Iterations must be 1 to 900"), "{refused}");
    }
    assert_eq!(read_iterations(0.0, 300), Ok(Stop::Converged(300)));
    assert_eq!(read_iterations(-2.0, 900), Ok(Stop::Converged(900)));
    assert_eq!(read_iterations(24.0, 10), Ok(Stop::Fixed(24)), "a fixed count ignores Max Iterations");
    assert_eq!(read_iterations(900.0, 1), Ok(Stop::Fixed(900)));
    assert!(read_iterations(901.0, 900).is_err());
    assert!(read_iterations(f32::NAN, 900).is_err());
}

#[test]
fn one_step_bound_requires_a_whole_reference_interval() {
    use manifold_physics::{Seconds, stepping::{CflPolicy, CflRestrictions}};
    for interval in [1.0f32 / 24.0, 1.0 / 60.0, 1.0 / 120.0] {
        for cell_size in [0.01f32, 0.1, 1.0] {
            let edge = ENGINE_CFL as f32 * cell_size / interval;
            for fraction in [0.0, 0.5, 0.98, 0.999, 1.0, 1.01, 2.0] {
                let speed = edge * fraction;
                let safe = one_step_cfl_safe(speed, 0.0, interval, cell_size);
                if safe {
                    let reference = CflPolicy { cell_size: f64::from(cell_size), ..Default::default() }
                        .duration(Seconds(f64::from(interval)), f64::from(speed), CflRestrictions::default());
                    assert!(reference.diagnostic.is_none());
                    assert_eq!(reference.value.0, f64::from(interval));
                }
                assert_eq!(safe, fraction < 0.99, "speed fraction {fraction}");
                assert_eq!(one_step_cfl_safe(0.0, speed, interval, cell_size), safe, "obstacles share CFL maximum");
            }
        }
    }
    for bad in [-1.0, f32::NAN, f32::INFINITY] {
        assert!(!one_step_cfl_safe(bad, 0.0, 1.0 / 60.0, 0.1));
        assert!(!one_step_cfl_safe(0.0, bad, 1.0 / 60.0, 0.1));
        assert!(!one_step_cfl_safe(0.0, 0.0, bad, 0.1));
        assert!(!one_step_cfl_safe(0.0, 0.0, 1.0 / 60.0, bad));
    }
    assert!(!one_step_cfl_safe(0.0, 0.0, 0.0, 0.1));
    assert!(!one_step_cfl_safe(0.0, 0.0, 1.0 / 60.0, 0.0));
}

#[test]
fn narrow_band_history_restores_once_and_resets_identity() {
    let mut state = StepState::default();
    let mut sync = |epoch, tick, enabled| state.sync_narrow_history(epoch, [0.0; 3], 1.0, [8; 3], 4096, tick, enabled);
    assert!(!sync(Some(1), 0, true));
    assert!(!sync(Some(1), 1, true));
    assert!(sync(Some(1), 2, false));
    assert!(!sync(Some(1), 3, false));
    assert!(!sync(Some(1), 4, true));
    assert!(!sync(Some(2), 0, false));
    assert!(!sync(None, 10, true));
    assert!(!sync(None, 0, false));
    assert!(state.narrow_reset_pending);
    assert!(!state.narrow_full_count);
    assert!(!state.narrow.initialized);
}

#[test]
fn narrow_band_mode_refuses_fractional_and_nonfinite_values() {
    assert_eq!(read_narrow_band(0.0), Ok(false));
    assert_eq!(read_narrow_band(1.0), Ok(true));
    for value in [-1.0, 0.5, 2.0, f32::NAN, f32::INFINITY] {
        assert!(read_narrow_band(value).is_err());
    }
}

/// I8: the liquid conformance suite checks codegen bodies for atomics;
/// the step's hand shader is checked here. Its only atomics are the
/// pockets' fixed-point sums.
#[test]
fn step_shader_atomics_are_allowlisted_integer_sums() {
    let stray = super::atomic_sites_outside(STEP_SHADER, POCKET_ATOMIC_SITES);
    assert!(stray.is_empty(), "atomics outside the allowlist: {stray:#?}");
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
    assert!(totals.windows(2).all(|w| w[0] == w[1]), "the words differ by order: {totals:?}");
    assert_eq!(value(totals[0]), want, "the carry sum equals the exact sum");
}

#[test]
fn step_shader_validates_with_every_entry() {
    let mask = naga::front::wgsl::parse_str(MASK_SHADER)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(MASK_SHADER)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&mask)
    .unwrap_or_else(|e| panic!("{e:?}"));        for source in [step_source(), band_source()] {
        let module = naga::front::wgsl::parse_str(&source).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let mut bindings = std::collections::HashSet::new();
        for (_, global) in module.global_variables.iter() {
            if let Some(binding) = &global.binding {
                assert!(bindings.insert((binding.group, binding.binding)), "duplicate shader binding {binding:?}");
            }
        }
        let entries: Vec<&str> = module.entry_points.iter().map(|e| e.name.as_str()).collect();
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
            "narrow_move",
            "tiles_classify",
            "tiles_rings",
            "tiles_lists",
            "tiles_fill",
            "tiles_retire",
            "pocket_leader_clear",
            "pocket_coarsen",
            "pocket_relabel",
        ] {
            assert!(entries.contains(&entry), "missing entry {entry}");
        }
    }
}

/// The shared extent counts every cell-sized lattice array, including
/// the separate coarse pocket leader, against its actual allocation.
#[test]
fn gpu_flip_lattice_buffers_are_held() {
    let source = include_str!("lattice.rs");
    let body = source.split("fn new(device: &GpuDevice, cells: [u32; 3]) -> Result<Self, String> {\n        let cell").nth(1).expect("LatticeBuffers::new");
    let body = body.split("\n    }\n").next().unwrap_or("");
    let arrays = body.matches("(device, cell)?").count() as u64;
    // The fine-label leader is counted by pocket_coarse_bytes, separately
    // from the seven fine solve arrays.
    assert_eq!(arrays, LATTICE_CELL_ARRAYS + 1, "cell arrays including the coarse pocket leader");
    for cells in [[64u32, 64, 64], [63, 100, 8], [128, 128, 128]] {
        let face = face_bytes(cells);
        let corners = cells.iter().map(|&n| u64::from(n) + 1).product::<u64>() * 4;
        let particles = 1000 * size_of::<FluidParticle>() as u64;
        let expected = particles
            + 7 * cell_bytes(cells)
            + corners
            + 6 * face
            + POCKET_GATE_WORDS * 4
            + pocket_sum_bytes(cells)
            + pocket_coarse_bytes(cells)
            + tile_scratch_bytes(cells, 2)
            + mask_saved_bytes(cells, 1000);
        assert_eq!(scratch_bytes(cells, 1000, 2), expected, "{cells:?}");
    }
}

#[test]
fn step_params_match_the_shader_uniform() {
    assert_eq!(size_of::<StepParams>(), 176);
    assert_eq!(std::mem::offset_of!(StepParams, box_offset), 100);
    assert_eq!(std::mem::offset_of!(StepParams, particles), 108);
    assert_eq!(std::mem::offset_of!(StepParams, live_impulse_stride), 156);
    assert_eq!(std::mem::offset_of!(StepParams, wall_inset), 160);
}

/// The tile table's bytes, by an independent count: five words a tile
/// (nearness, two ring halves, the list, the retired list), the counts
/// and the triples; partial edge tiles counted whole; the extent's hold
/// grows by exactly that.
#[test]
fn gpu_flip_tile_table_bytes_follow_the_lattice() {
    assert_eq!(tile_counts([64, 64, 64]), [8, 8, 8]);
    assert_eq!(tile_counts([63, 100, 8]), [8, 13, 1]);
    assert_eq!(tile_counts([1, 9, 17]), [1, 2, 3]);
    assert_eq!(ring_max(14), 2, "64³: 14 layers reach the second ring");
    assert_eq!(ring_max(23), 3, "128³: 23 layers reach the third");
    assert_eq!(ring_max(FACE_VALID_LAYERS), 1);
    for (cells, tiles) in [([64, 64, 64], 512u64), ([63, 100, 8], 104), ([128, 128, 128], 4096), ([1, 9, 17], 6)] {
        for r in 1..5u32 {
            let counts = u64::from(r) + 2 + 1 + 1;
            let triples = 3 * (u64::from(r) + 1 + 1 + 1);
            let arguments = triples + triples % 2 + 2;
            assert_eq!(tile_scratch_bytes(cells, r), (5 * tiles + counts + arguments) * 4, "{cells:?} ring_max {r}");
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
        assert!(read_closed_faces(bad).unwrap_err().contains("Closed Faces"), "{bad}");
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
