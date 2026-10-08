//! GPU value proofs for the whitewater grid atoms against their CPU
//! statements (`whitewater_cpu`), and the one fused pair the chain holds:
//! the last node.nearest_crossing pass with node.crossing_distance
//! (`docs/GPU_WHITEWATER_DESIGN.md` section 3.7).

use super::crossing_distance::CrossingDistance;
use super::extend_lattice::ExtendLattice;
use super::lattice_curvature::LatticeCurvature;
use super::liquid_cells::LiquidCells;
use crate::testkit::liquid_surface::{Harness, params, read};
use super::nearest_crossing::NearestCrossing;
use super::surface_crossings::SurfaceCrossings;
use {crate::water::primitives::whitewater_cpu as cpu, super::whitewater_cpu::Grid, super::whitewater_cpu::Rng};
use crate::exec::effect_node::ParamValues;
use crate::water::whitewater::{KnownValue, NO_CROSSING, SurfaceCrossing};

/// Unequal sides, so a swapped axis shows.
const NODES: [u32; 3] = [9, 8, 7];
const H: f32 = 0.25;

fn grid_params(extra: &[(&'static str, f32)]) -> ParamValues {
    let mut all = vec![("nodes_x", NODES[0] as f32), ("nodes_y", NODES[1] as f32), ("nodes_z", NODES[2] as f32)];
    all.extend_from_slice(extra);
    params(&all)
}

/// A sphere of liquid (radius 2.2 cells) with a little noise, in metres,
/// sampled at `positions` given in grid cells.
fn sphere_level(position: [f32; 3], rng: &mut Rng) -> f32 {
    sphere_at(position, [4.1, 3.3, 2.7], 2.2, rng)
}

fn sphere_at(position: [f32; 3], centre: [f32; 3], radius: f32, rng: &mut Rng) -> f32 {
    let r = (0..3).map(|a| (position[a] - centre[a]).powi(2)).sum::<f32>().sqrt();
    (r - radius + 0.1 * (rng.unit() - 0.5)) * H
}

/// A wall filling x < 1.3 cells, as a distance in metres on the solid nodes.
fn wall_solid(grid: &Grid) -> Vec<f32> {
    let n = grid.nodes;
    (0..n.iter().product::<u32>()).map(|i| ((i % n[0]) as f32 - 1.3) * H).collect()
}

fn refined_level(grid: &Grid, s: u32, rng: &mut Rng) -> Vec<f32> {
    let levels = grid.cells.map(|n| n * s + 1);
    (0..levels.iter().product::<u32>())
        .map(|i| {
            let p = [i % levels[0], (i / levels[0]) % levels[1], i / (levels[0] * levels[1])];
            sphere_level(p.map(|v| v as f32 / s as f32), rng)
        })
        .collect()
}



fn crossing_close(a: SurfaceCrossing, b: SurfaceCrossing) -> bool {
    (0..3).all(|i| (a.crossing[i] - b.crossing[i]).abs() <= 1e-4 * b.crossing[i].abs().max(1.0) && (a.normal[i] - b.normal[i]).abs() <= 1e-5)
        && (a.level - b.level).abs() <= 1e-6
}

/// A unit normal half the time, else none.
fn random_normal(rng: &mut Rng) -> [f32; 3] {
    if rng.unit() < 0.5 {
        return [0.0; 3];
    }
    let v: [f32; 3] = std::array::from_fn(|_| rng.unit() - 0.5);
    let size = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-3);
    v.map(|x| x / size)
}

/// Each cell's crossing and centre level match the CPU statement; a
/// different crossing only on a tie, at the same distance.
#[test]
fn surface_crossings_match_cpu() {
    let grid = Grid::new(NODES);
    let solid = wall_solid(&grid);
    let mut harness = Harness::new();
    let solid_in = harness.array(&solid, solid.len());
    for s in [2u32, 3] {
        let level = refined_level(&grid, s, &mut Rng(0x5eed_0000 + u64::from(s)));
        let level_in = harness.array(&level, level.len());
        let levels = grid.cells.map(|n| (n * s + 1) as f32);
        let p = grid_params(&[("level_nodes_x", levels[0]), ("level_nodes_y", levels[1]), ("level_nodes_z", levels[2])]);
        let got: Vec<SurfaceCrossing> =
            run(&mut harness, &mut SurfaceCrossings::new(), &[("level_set", level_in.0), ("solid", solid_in.0)], grid.total(), &p);
        let (mut found, mut walled) = (0, 0);
        for (i, g) in got.iter().enumerate() {
            let c = grid.coords(i);
            let (want, best, runner_up) = cpu::surface_crossing(&grid, &level, &solid, s, c);
            found += usize::from(want.crossing[0] < NO_CROSSING);
            walled += usize::from(c[0] < 2 && want.crossing[0] == NO_CROSSING && want.level.abs() < H);
            if crossing_close(*g, want) {
                continue;
            }
            let local: [f32; 3] = std::array::from_fn(|a| (g.crossing[a] - c[a] as f32) * s as f32 - 0.5 * s as f32);
            let dd: f32 = local.iter().map(|v| v * v).sum();
            assert!(
                (runner_up - best).abs() <= 1e-4 && (dd - best).abs() <= 1e-4 && (g.level - want.level).abs() <= 1e-6,
                "s {s} cell {c:?}: GPU {g:?} CPU {want:?}"
            );
        }
        assert!(found > 40, "s {s}: the sphere crosses {found} cells");
        assert!(walled > 0, "s {s}: the wall hides a crossing");
    }
}

/// One pass takes the nearest of 27 cells' crossings, the own on a tie, at
/// step 1 and 2.
#[test]
fn nearest_crossing_matches_cpu() {
    let grid = Grid::new(NODES);
    let mut rng = Rng(0x00ea_7e57);
    let crossings: Vec<SurfaceCrossing> = (0..grid.total())
        .map(|i| {
            let c = grid.coords(i);
            let crossing = if rng.unit() < 0.2 { std::array::from_fn(|a| c[a] as f32 + rng.unit()) } else { [NO_CROSSING; 3] };
            SurfaceCrossing { crossing, level: rng.unit() - 0.5, normal: random_normal(&mut rng), pad0: 0.0 }
        })
        .collect();
    let mut harness = Harness::new();
    let input = harness.array(&crossings, crossings.len());
    for step in [1.0f32, 2.0] {
        let got: Vec<SurfaceCrossing> =
            run(&mut harness, &mut NearestCrossing::new(), &[("crossings", input.0)], grid.total(), &grid_params(&[("step", step)]));
        let mut moved = 0;
        for (i, g) in got.iter().enumerate() {
            let c = grid.coords(i);
            let want = cpu::nearest_crossing(&grid, &crossings, c, step);
            moved += usize::from(want.crossing != crossings[i].crossing);
            assert!(crossing_close(*g, want), "step {step} cell {c:?}: GPU {g:?} CPU {want:?}");
        }
        assert!(moved > 100, "step {step}: the pass moves {moved} crossings");
    }
}

fn random_crossings(grid: &Grid, rng: &mut Rng) -> Vec<SurfaceCrossing> {
    (0..grid.total())
        .map(|i| {
            let c = grid.coords(i);
            let crossing = if rng.unit() < 0.8 { std::array::from_fn(|a| c[a] as f32 + 6.0 * rng.unit() - 2.5) } else { [NO_CROSSING; 3] };
            // Some levels at 0 and some centres on their crossing, for the eps rule.
            let level = if rng.unit() < 0.1 { 0.0 } else { rng.unit() - 0.5 };
            let crossing = if rng.unit() < 0.05 { Grid::centre(c) } else { crossing };
            SurfaceCrossing { crossing, level, normal: random_normal(rng), pad0: 0.0 }
        })
        .collect()
}

/// The signed, held distance and FLIP's solid and eps rules.
#[test]
fn crossing_distance_matches_cpu() {
    let grid = Grid::new(NODES);
    let crossings = random_crossings(&grid, &mut Rng(0xd157_0001));
    let solid = wall_solid(&grid);
    let mut harness = Harness::new();
    let (input, solid_in) = (harness.array(&crossings, crossings.len()), harness.array(&solid, solid.len()));
    let got: Vec<f32> = run(
        &mut harness,
        &mut CrossingDistance::new(),
        &[("crossings", input.0), ("solid", solid_in.0)],
        grid.total(),
        &grid_params(&[("cell_size", H)]),
    );
    let (mut into_wall, mut held) = (0, 0);
    for (i, &g) in got.iter().enumerate() {
        let c = grid.coords(i);
        let want = cpu::crossing_distance(&grid, crossings[i], &solid, H, c);
        into_wall += usize::from(want == -0.5 * H);
        held += usize::from(want.abs() == 4.0 * H);
        assert!((g - want).abs() <= 1e-6, "cell {c:?}: GPU {g} CPU {want}");
    }
    assert!(into_wall > 5 && held > 20, "the fixture reaches the wall rule {into_wall} and the hold {held} times");
}

/// Air, liquid and solid, with the liquid shrunk off the air.
#[test]
fn liquid_cells_match_cpu() {
    let grid = Grid::new(NODES);
    let mut rng = Rng(0x11c0_e115);
    let distance: Vec<f32> = (0..grid.total()).map(|_| if rng.unit() < 0.7 { -rng.unit() } else { rng.unit() }).collect();
    let solid = wall_solid(&grid);
    let mut harness = Harness::new();
    let (input, solid_in) = (harness.array(&distance, distance.len()), harness.array(&solid, solid.len()));
    let got: Vec<u32> =
        run(&mut harness, &mut LiquidCells::new(), &[("distance", input.0), ("solid", solid_in.0)], grid.total(), &grid_params(&[]));
    let mut counts = [0; 3];
    for (i, &g) in got.iter().enumerate() {
        let c = grid.coords(i);
        let want = cpu::liquid_cell(&grid, &distance, &solid, c);
        counts[want as usize] += 1;
        assert_eq!(g, want, "cell {c:?}");
    }
    let shrunk = (0..grid.total()).filter(|&i| distance[i] < 0.0 && got[i] == 0).count();
    assert!(counts.iter().all(|&n| n > 10) && shrunk > 10, "kinds {counts:?}, {shrunk} shrunk");
}

/// FLIP's curvature where the band holds, unknown elsewhere.
#[test]
fn lattice_curvature_matches_cpu() {
    let grid = Grid::new([13, 12, 11]);
    let mut rng = Rng(0xc0e7_0001);
    let distance: Vec<f32> = (0..grid.total()).map(|i| sphere_at(Grid::centre(grid.coords(i)), [6.2, 5.4, 4.9], 3.1, &mut rng)).collect();
    let mut harness = Harness::new();
    let input = harness.array(&distance, distance.len());
    let p = params(&[
        ("nodes_x", grid.nodes[0] as f32),
        ("nodes_y", grid.nodes[1] as f32),
        ("nodes_z", grid.nodes[2] as f32),
        ("cell_size", H),
    ]);
    let got: Vec<KnownValue> = run(&mut harness, &mut LatticeCurvature::new(), &[("distance", input.0)], grid.total(), &p);
    let mut known = 0;
    for (i, g) in got.iter().enumerate() {
        let c = grid.coords(i);
        let want = cpu::lattice_curvature(&grid, &distance, H, c);
        known += usize::from(want.known > 0.0);
        assert_eq!(g.known, want.known, "cell {c:?}");
        assert!((g.value - want.value).abs() <= 1e-4 / H, "cell {c:?}: GPU {} CPU {}", g.value, want.value);
    }
    assert!(known > 50 && known < grid.total() / 2, "{known} cells known");
}

/// The last nearest-crossing pass and crossing_distance folded into one
/// kernel give what the two standalone kernels give, and the CPU. The
/// region is the one `whitewater_grid_chain_fuses_only_the_distance_pair`
/// finds: the pass reads its input by gather (an external), the distance
/// takes the pass's output in a register and gathers the solid.
#[test]
fn nearest_crossing_fused_with_crossing_distance_matches_unfused() {
    use crate::exec::effect_node::NodeInstanceId;
    use crate::freeze::classify::FusionKind;
    use crate::freeze::codegen::{ENTRY, FusionRegion, InputSource, RegionNode, generate_fused};
    use crate::primitive::PrimitiveSpec;
    use manifold_gpu::GpuBinding;

    let grid = Grid::new(NODES);
    let crossings = random_crossings(&grid, &mut Rng(0xf05e_0001));
    let solid = wall_solid(&grid);
    let mut harness = Harness::new();
    let (input, solid_in) = (harness.array(&crossings, crossings.len()), harness.array(&solid, solid.len()));
    let spread: Vec<SurfaceCrossing> = run(&mut harness, &mut NearestCrossing::new(), &[("crossings", input.0)], grid.total(), &grid_params(&[]));
    let spread_in = harness.array(&spread, spread.len());
    let unfused: Vec<f32> = run(
        &mut harness,
        &mut CrossingDistance::new(),
        &[("crossings", spread_in.0), ("solid", solid_in.0)],
        grid.total(),
        &grid_params(&[("cell_size", H)]),
    );

    let member = |n: u32, type_id: &str, body, params, inputs, input_access, node_inputs, node_outputs, node_includes, derived_uniforms| RegionNode {
        node_id: NodeInstanceId(n),
        fusion_kind: FusionKind::Pointwise,
        body,
        params,
        inputs,
        input_access,
        node_inputs,
        node_outputs,
        node_includes,
        derived_uniforms,
        type_id: type_id.to_string(),
        derived_camera_ext: None,
        output_storage: "rgba16float",
        stencil_fetch: false,
        quantize_f16: false,
    };
    let region = FusionRegion {
        nodes: vec![
            member(
                0,
                NearestCrossing::TYPE_ID,
                NearestCrossing::WGSL_BODY.expect("body"),
                NearestCrossing::PARAMS,
                vec![InputSource::External(0)],
                NearestCrossing::INPUT_ACCESS.to_vec(),
                NearestCrossing::INPUTS,
                NearestCrossing::OUTPUTS,
                NearestCrossing::WGSL_INCLUDES,
                NearestCrossing::DERIVED_UNIFORMS,
            ),
            member(
                1,
                CrossingDistance::TYPE_ID,
                CrossingDistance::WGSL_BODY.expect("body"),
                CrossingDistance::PARAMS,
                vec![InputSource::Node(NodeInstanceId(0)), InputSource::External(1)],
                CrossingDistance::INPUT_ACCESS.to_vec(),
                CrossingDistance::INPUTS,
                CrossingDistance::OUTPUTS,
                CrossingDistance::WGSL_INCLUDES,
                CrossingDistance::DERIVED_UNIFORMS,
            ),
        ],
        num_external_inputs: 2,
        outputs: vec![(NodeInstanceId(1), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(),
        camera_externals: 0,
        output_capacity: None,
    };
    let fused = generate_fused(&region).expect("nearest_crossing → crossing_distance fuses");
    assert!(naga::front::wgsl::parse_str(&fused.wgsl).is_ok(), "fused WGSL parses:\n{}", fused.wgsl);
    let mut words: Vec<u32> = fused
        .param_order
        .iter()
        .map(|&(node, name)| {
            let value = match name {
                "nodes_x" => NODES[0] as f32,
                "nodes_y" => NODES[1] as f32,
                "nodes_z" => NODES[2] as f32,
                "step" if node.0 == 0 => 1.0,
                "cell_size" if node.0 == 1 => H,
                other => panic!("unexpected fused param {other} on node {node:?}"),
            };
            value.to_bits()
        })
        .collect();
    while !words.len().is_multiple_of(4) {
        words.push(0);
    }
    let dst = harness.array::<f32>(&[], grid.total());
    let pipeline = harness.device.create_compute_pipeline(&fused.wgsl, ENTRY, "whitewater-distance-fused");
    let mut enc = harness.device.create_encoder("whitewater-distance-fused");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::cast_slice(&words) },
            GpuBinding::Buffer { binding: 1, buffer: &input.1, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &solid_in.1, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &dst.1, offset: 0 },
        ],
        [(grid.total() as u32).div_ceil(256), 1, 1],
        "whitewater-distance-fused",
    );
    enc.commit_and_wait_completed();
    let fused_out: Vec<f32> = read(&dst.1, grid.total());
    for (i, (f, u)) in fused_out.iter().zip(&unfused).enumerate() {
        let c = grid.coords(i);
        let want = cpu::crossing_distance(&grid, cpu::nearest_crossing(&grid, &crossings, c, 1.0), &solid, H, c);
        assert!((u - want).abs() <= 1e-6, "cell {c:?}: standalone {u} CPU {want}");
        assert_eq!(f.to_bits(), u.to_bits(), "cell {c:?}: fused {f} standalone {u}");
    }
}

/// Three passes, each against the CPU pass on the same input.
#[test]
fn extend_lattice_matches_cpu() {
    let grid = Grid::new(NODES);
    let mut rng = Rng(0xe7e7_0001);
    let mut values: Vec<KnownValue> = (0..grid.total())
        .map(|i| {
            let known = !grid.on_border(grid.coords(i)) && rng.unit() < 0.1;
            KnownValue { value: rng.unit() * 2.0 - 1.0, known: if known { 1.0 } else { 0.0 } }
        })
        .collect();
    let mut harness = Harness::new();
    for pass in 0..3 {
        let input = harness.array(&values, values.len());
        let got: Vec<KnownValue> = run(&mut harness, &mut ExtendLattice::new(), &[("values", input.0)], grid.total(), &grid_params(&[]));
        let want: Vec<KnownValue> = (0..grid.total()).map(|i| cpu::extend_lattice(&grid, &values, grid.coords(i))).collect();
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!(g.known == w.known && (g.value - w.value).abs() <= 1e-6, "pass {pass} cell {:?}: GPU {g:?} CPU {w:?}", grid.coords(i));
        }
        values = want;
    }
    let inside = (0..grid.total()).filter(|&i| !grid.on_border(grid.coords(i))).count();
    let known = values.iter().filter(|v| v.known > 0.0).count();
    assert!(known > inside / 2, "three passes fill {known} of the {inside} inner cells");
}

use crate::testkit::water_codegen::run;
