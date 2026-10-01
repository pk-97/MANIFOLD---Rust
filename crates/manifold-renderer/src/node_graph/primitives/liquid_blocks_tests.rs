//! GPU value proofs for node.liquid_blocks against its CPU statement, and the
//! codegen region of the atom against its standalone dispatch.

use super::liquid_blocks::LiquidBlocks;
use super::liquid_surface_tests::{Harness, params, read};
use super::whitewater_grid_tests::run;
use crate::node_graph::effect_node::ParamValues;
use crate::node_graph::liquid::blocks::{BLOCK_LIQUID, BLOCK_SOLID, BLOCK_SURFACE, LIQUID_BLOCK_CELLS, block_lattice};

/// Unequal sides with partial edge blocks: 18 × 13 × 10 cells, 5 × 4 × 3 blocks.
pub(super) const NODES: [u32; 3] = [19, 14, 11];
const H: f32 = 0.25;

pub(super) fn cells() -> [u32; 3] {
    NODES.map(|n| n - 1)
}

fn index(p: [u32; 3], n: [u32; 3]) -> usize {
    (p[0] + n[0] * (p[1] + n[1] * p[2])) as usize
}

fn coords(i: usize, n: [u32; 3]) -> [u32; 3] {
    let i = i as u32;
    [i % n[0], (i / n[0]) % n[1], i / (n[0] * n[1])]
}

/// A ball of liquid of radius 3.1 cells, signed distance in metres, at a
/// point given in cells.
pub(super) fn ball(p: [f32; 3]) -> f32 {
    let centre = [6.2, 5.4, 4.3];
    let r = (0..3).map(|a| (p[a] - centre[a]).powi(2)).sum::<f32>().sqrt();
    (r - 3.1) * H
}

/// Refined level set of the ball at `s` nodes per cell.
pub(super) fn level(s: u32) -> Vec<f32> {
    let levels = cells().map(|n| n * s + 1);
    (0..levels.iter().product::<u32>() as usize)
        .map(|i| ball(coords(i, levels).map(|v| v as f32 / s as f32)))
        .collect()
}

/// Water where the cell centre is inside the ball, as the 1/0 lattice.
pub(super) fn water() -> Vec<f32> {
    let c = cells();
    (0..c.iter().product::<u32>() as usize)
        .map(|i| if ball(coords(i, c).map(|v| v as f32 + 0.5)) < 0.0 { 1.0 } else { 0.0 })
        .collect()
}

/// A wall filling x < 1.3 cells and the floor z < 0.6, on the solid nodes.
pub(super) fn solid() -> Vec<f32> {
    (0..NODES.iter().product::<u32>() as usize)
        .map(|i| {
            let p = coords(i, NODES);
            ((p[0] as f32 - 1.3).min(p[2] as f32 - 0.6)) * H
        })
        .collect()
}

/// The CPU statement of the map.
pub(super) fn blocks_cpu(water: &[f32], level: &[f32], solid: &[f32], s: u32) -> Vec<u32> {
    let c = cells();
    let levels = c.map(|n| n * s + 1);
    let b = block_lattice(c);
    (0..b.iter().product::<u32>() as usize)
        .map(|i| {
            let block = coords(i, b);
            let first = block.map(|v| v * LIQUID_BLOCK_CELLS);
            let end: [u32; 3] = std::array::from_fn(|a| (first[a] + LIQUID_BLOCK_CELLS).min(c[a]));
            let mut bits = 0;
            let range = |lo: [u32; 3], hi: [u32; 3]| {
                (lo[2]..hi[2]).flat_map(move |z| (lo[1]..hi[1]).flat_map(move |y| (lo[0]..hi[0]).map(move |x| [x, y, z])))
            };
            if range(first, end).any(|p| water[index(p, c)] > 0.0) {
                bits |= BLOCK_LIQUID;
            }
            if range(first, end.map(|v| v + 1)).any(|p| solid[index(p, NODES)] < 0.0) {
                bits |= BLOCK_SOLID;
            }
            let (lo, hi) = (first.map(|v| v * s), end.map(|v| v * s + 1));
            let (below, above) = range(lo, hi).fold((false, false), |(b, a), p| {
                let v = level[index(p, levels)];
                (b || v < 0.0, a || v >= 0.0)
            });
            if below && above {
                bits |= BLOCK_SURFACE;
            }
            bits
        })
        .collect()
}

pub(super) fn block_params(s: u32) -> ParamValues {
    let levels = cells().map(|n| (n * s + 1) as f32);
    params(&[
        ("nodes_x", NODES[0] as f32),
        ("nodes_y", NODES[1] as f32),
        ("nodes_z", NODES[2] as f32),
        ("level_nodes_x", levels[0]),
        ("level_nodes_y", levels[1]),
        ("level_nodes_z", levels[2]),
    ])
}

#[test]
fn liquid_blocks_match_cpu() {
    let blocks = block_lattice(cells()).iter().product::<u32>() as usize;
    assert_eq!(blocks, 60);
    let (water, solid) = (water(), solid());
    let mut harness = Harness::new();
    let (water_in, solid_in) = (harness.array(&water, water.len()), harness.array(&solid, solid.len()));
    for s in [1u32, 2, 3] {
        let level = level(s);
        let level_in = harness.array(&level, level.len());
        let got: Vec<u32> = run(
            &mut harness,
            &mut LiquidBlocks::new(),
            &[("water", water_in.0), ("level_set", level_in.0), ("solid", solid_in.0)],
            blocks,
            &block_params(s),
        );
        let want = blocks_cpu(&water, &level, &solid, s);
        assert_eq!(got, want, "s {s}");
        for bit in [BLOCK_LIQUID, BLOCK_SURFACE, BLOCK_SOLID] {
            let set = want.iter().filter(|&&w| w & bit != 0).count();
            assert!(set > 0 && set < blocks, "s {s}: bit {bit} is set in {set} of {blocks} blocks; the scene must exercise both");
        }
    }
}

#[test]
fn liquid_blocks_fused_matches_unfused() {
    use crate::node_graph::effect_node::NodeInstanceId;
    use crate::node_graph::freeze::classify::FusionKind;
    use crate::node_graph::freeze::codegen::{ENTRY, FusionRegion, InputSource, RegionNode, generate_fused};
    use crate::node_graph::primitive::PrimitiveSpec;
    use manifold_gpu::GpuBinding;

    let s = 2;
    let blocks = block_lattice(cells()).iter().product::<u32>() as usize;
    let (water, solid, level) = (water(), solid(), level(s));
    let mut harness = Harness::new();
    let water_in = harness.array(&water, water.len());
    let level_in = harness.array(&level, level.len());
    let solid_in = harness.array(&solid, solid.len());
    let unfused: Vec<u32> = run(
        &mut harness,
        &mut LiquidBlocks::new(),
        &[("water", water_in.0), ("level_set", level_in.0), ("solid", solid_in.0)],
        blocks,
        &block_params(s),
    );

    let region = FusionRegion {
        nodes: vec![RegionNode {
            node_id: NodeInstanceId(0),
            fusion_kind: FusionKind::Pointwise,
            body: LiquidBlocks::WGSL_BODY.expect("body"),
            params: LiquidBlocks::PARAMS,
            inputs: vec![InputSource::External(0), InputSource::External(1), InputSource::External(2)],
            input_access: LiquidBlocks::INPUT_ACCESS.to_vec(),
            node_inputs: LiquidBlocks::INPUTS,
            node_outputs: LiquidBlocks::OUTPUTS,
            node_includes: LiquidBlocks::WGSL_INCLUDES,
            derived_uniforms: LiquidBlocks::DERIVED_UNIFORMS,
            type_id: LiquidBlocks::TYPE_ID.to_string(),
            derived_camera_ext: None,
            output_storage: "rgba16float",
            stencil_fetch: false,
            quantize_f16: false,
        }],
        num_external_inputs: 3,
        outputs: vec![(NodeInstanceId(0), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(),
        camera_externals: 0,
        output_capacity: None,
    };
    let fused = generate_fused(&region).expect("node.liquid_blocks generates");
    assert!(naga::front::wgsl::parse_str(&fused.wgsl).is_ok(), "fused WGSL parses:\n{}", fused.wgsl);
    let p = block_params(s);
    let mut words: Vec<u32> = fused
        .param_order
        .iter()
        .map(|&(_, name)| match p.get(name) {
            Some(crate::node_graph::parameters::ParamValue::Float(v)) => v.to_bits(),
            _ => panic!("unexpected fused param {name}"),
        })
        .collect();
    while !words.len().is_multiple_of(4) {
        words.push(0);
    }
    let dst = harness.array::<u32>(&[], blocks);
    let pipeline = harness.device.create_compute_pipeline(&fused.wgsl, ENTRY, "liquid-blocks-fused");
    let mut enc = harness.device.create_encoder("liquid-blocks-fused");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::cast_slice(&words) },
            GpuBinding::Buffer { binding: 1, buffer: &water_in.1, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &level_in.1, offset: 0 },
            GpuBinding::Buffer { binding: 3, buffer: &solid_in.1, offset: 0 },
            GpuBinding::Buffer { binding: 4, buffer: &dst.1, offset: 0 },
        ],
        [(blocks as u32).div_ceil(256), 1, 1],
        "liquid-blocks-fused",
    );
    enc.commit_and_wait_completed();
    let fused_out: Vec<u32> = read(&dst.1, blocks);
    assert_eq!(unfused, blocks_cpu(&water, &level, &solid, s));
    assert_eq!(fused_out, unfused, "fused differs from standalone");
}

// ── node.surface_crossings with the map ────────────────────────────────────

use super::surface_crossings::SurfaceCrossings;
use crate::node_graph::whitewater::SurfaceCrossing;

fn crossings_params(s: u32) -> ParamValues {
    block_params(s)
}

fn same_bits(a: &[SurfaceCrossing], b: &[SurfaceCrossing]) -> usize {
    let bits = |c: &SurfaceCrossing| bytemuck::bytes_of(c).to_vec();
    a.iter().zip(b).filter(|(x, y)| bits(x) != bits(y)).count()
}

/// The map changes no bit of surface_crossings' output, at every refinement,
/// and the scene leaves blocks both with and without surface.
#[test]
fn surface_crossings_block_skip_is_bit_identical() {
    let c = cells();
    let total = c.iter().product::<u32>() as usize;
    let blocks = block_lattice(c).iter().product::<u32>() as usize;
    let (water, solid) = (water(), solid());
    let mut harness = Harness::new();
    let (water_in, solid_in) = (harness.array(&water, water.len()), harness.array(&solid, solid.len()));
    for s in [1u32, 2, 3] {
        let level = level(s);
        let level_in = harness.array(&level, level.len());
        let map: Vec<u32> = run(
            &mut harness,
            &mut LiquidBlocks::new(),
            &[("water", water_in.0), ("level_set", level_in.0), ("solid", solid_in.0)],
            blocks,
            &block_params(s),
        );
        let skipped = map.iter().filter(|&&b| b & BLOCK_SURFACE == 0).count();
        assert!(skipped > 0 && skipped < blocks, "s {s}: {skipped} of {blocks} blocks lack surface");
        let map_in = harness.array(&map, map.len());
        let off: Vec<SurfaceCrossing> =
            run(&mut harness, &mut SurfaceCrossings::new(), &[("level_set", level_in.0), ("solid", solid_in.0)], total, &crossings_params(s));
        let on: Vec<SurfaceCrossing> = run(
            &mut harness,
            &mut SurfaceCrossings::new(),
            &[("level_set", level_in.0), ("solid", solid_in.0), ("blocks", map_in.0)],
            total,
            &crossings_params(s),
        );
        assert_eq!(same_bits(&on, &off), 0, "s {s}: the map changed the output");
    }
}

/// A map shorter than the block lattice is a named refusal, never a full scan.
#[test]
fn surface_crossings_refuses_a_short_map() {
    let total = cells().iter().product::<u32>() as usize;
    let (solid, level) = (solid(), level(2));
    let mut harness = Harness::new();
    let solid_in = harness.array(&solid, solid.len());
    let level_in = harness.array(&level, level.len());
    let short = harness.array(&[0xFFFF_FFFFu32; 59], 59);
    let out = harness.array::<SurfaceCrossing>(&[], total);
    let (_, errors) = harness.run(
        &mut SurfaceCrossings::new(),
        &[("level_set", level_in.0), ("solid", solid_in.0), ("blocks", short.0)],
        &[("out", out.0)],
        &crossings_params(2),
    );
    assert!(errors.iter().any(|e| e.contains("block map holds 59 blocks")), "{errors:?}");
}

// No fused proof of surface_crossings with the map: a gather-only region
// dispatches over the shortest gathered input, so the 60-word map caps it
// at 60 cells. Both atoms size outputs from params and never fuse in a
// graph; BUG-iiv4 (gather-only region count anchor) tracks the codegen gap.

/// The fields one timing reads: the solid lattice's nodes, the level set's
/// refinement, and the water, level set and solid the map gathers.
struct Fields {
    nodes: [u32; 3],
    s: u32,
    level: Vec<f32>,
    water: Vec<f32>,
    solid: Vec<f32>,
}

/// Measurement, not a gate: prints surface_crossings' GPU time per
/// dispatch with and without the map, and the map's own cost, on a
/// 34³- then 70³-cell grid (one size step) at refinement 2 with a ball of
/// liquid in a corner. Asserts only that the two outputs agree.
#[test]
fn surface_crossings_block_skip_timing() {
    for side in [35, 71] {
        time_block_skip(&format!("ball, {side} nodes a side"), &ball_fields(side));
    }
}

fn ball_fields(side: u32) -> Fields {
    let side_f = side as f32;
    let nodes = [side; 3];
    let cells = nodes.map(|n| n - 1);
    let s = 2;
    let levels = cells.map(|n| n * s + 1);
    let ball = |p: [f32; 3]| ((0..3).map(|a| (p[a] - 0.26 * side_f).powi(2)).sum::<f32>().sqrt() - 0.17 * side_f) * H;
    Fields {
        nodes,
        s,
        level: (0..levels.iter().product::<u32>() as usize).map(|i| ball(coords(i, levels).map(|v| v as f32 / s as f32))).collect(),
        water: (0..cells.iter().product::<u32>() as usize)
            .map(|i| if ball(coords(i, cells).map(|v| v as f32 + 0.5)) < 0.0 { 1.0 } else { 0.0 })
            .collect(),
        solid: (0..nodes.iter().product::<u32>() as usize).map(|i| (coords(i, nodes)[2] as f32 - 0.6) * H).collect(),
    }
}

/// Measurement, not a gate: the shipped GPU FLIP Dam Break with its
/// Whitewater group at Resolution 64 then 128 (extents proven by
/// `liquid_block_extents_at_64_and_128` and the scene walks in
/// `gpu_flip_preset`), unfrozen so the fields can be held. At frames 30, 90
/// and 150 it captures the level set the Whitewater group reads, the solid
/// lattice and the particles, marks a cell water when a particle sits in it,
/// and times the map and surface_crossings on those fields.
#[test]
fn surface_crossings_block_skip_timing_on_dam_break() {
    for n in [64, 128] {
        for (label, fields) in dam_break_fields(n) {
            time_block_skip(&label, &fields);
        }
    }
}

fn dam_break_fields(n: usize) -> Vec<(String, Fields)> {
    use super::gpu_flip_preset::WaterScene;
    use super::whitewater_scene_tests::{Show, whitewater_render_def};
    use crate::node_graph::fluid_particles::FluidParticle;
    use crate::node_graph::liquid::lattice::LiquidLattice;

    const FRAMES: [usize; 3] = [30, 90, 150];
    let scene = WaterScene::dam_break(n);
    let lattice = LiquidLattice::from_layout(&scene.layout());
    let nodes = lattice.nodes();
    let cells = nodes.map(|v| v - 1);
    let s = scene.surface_scale as u32;
    let levels = cells.map(|c| c * s + 1);
    let level_len = levels.iter().map(|&v| v as usize).product::<usize>();
    let mut show = Show::new(whitewater_render_def(scene), (320, 180), false, &[]);
    // render_def bakes the scene's surface scale into the Liquid Surface
    // group, so the held level set is `levels` nodes a side.
    show.restart();
    let (frame_node, smooth) = (show.node_named("frame"), show.node_named("liquid_smooth_z"));
    let (min, h) = (lattice.min(), lattice.cell_size());
    let mut captured = Vec::new();
    for frame in 1..=*FRAMES.last().expect("frames") {
        let capture = FRAMES.contains(&frame);
        show.set_dump_all(capture);
        show.frame(false);
        if !capture {
            continue;
        }
        let particles: Vec<FluidParticle> = show.dumped(&frame_node, "particles_b", scene.particles() as usize);
        let mut water = vec![0.0f32; cells.iter().map(|&v| v as usize).product()];
        let mut outside = 0;
        for p in particles.iter().filter(|p| p.position_radius[3] > 0.0) {
            let at: [f32; 3] = std::array::from_fn(|a| ((p.position_radius[a] - min[a]) / h).floor());
            if (0..3).any(|a| at[a] < 0.0 || at[a] >= cells[a] as f32) {
                outside += 1;
                continue;
            }
            water[index(at.map(|v| v as u32), cells)] = 1.0;
        }
        let liquid = water.iter().filter(|&&w| w > 0.0).count();
        let label = format!("Dam Break {n} frame {frame} ({liquid} water cells, {outside} particles outside the cells)");
        let fields = Fields {
            nodes,
            s,
            level: show.dumped(&smooth, "smoothed", level_len),
            water,
            solid: show.dumped(&frame_node, "solid_b", nodes.iter().map(|&v| v as usize).product()),
        };
        captured.push((label, fields));
    }
    let errors = show.errors();
    assert!(errors.is_empty(), "Resolution {n}: the scene ran with errors: {errors:#?}");
    captured
}

fn time_block_skip(label: &str, f: &Fields) {
    let (nodes, s) = (f.nodes, f.s);
    let (level, water, solid) = (&f.level, &f.water, &f.solid);
    let cells = nodes.map(|n| n - 1);
    let levels = cells.map(|n| n * s + 1);
    let total = cells.iter().product::<u32>() as usize;
    let blocks = block_lattice(cells).iter().product::<u32>() as usize;
    let lattice_values = [nodes[0], nodes[1], nodes[2], levels[0], levels[1], levels[2]].map(|v| v as f32);
    let p = params(&[
        ("nodes_x", lattice_values[0]),
        ("nodes_y", lattice_values[1]),
        ("nodes_z", lattice_values[2]),
        ("level_nodes_x", lattice_values[3]),
        ("level_nodes_y", lattice_values[4]),
        ("level_nodes_z", lattice_values[5]),
    ]);
    let mut harness = Harness::new();
    let water_in = harness.array(water, water.len());
    let level_in = harness.array(level, level.len());
    let solid_in = harness.array(solid, solid.len());
    let map_out = harness.array::<u32>(&[], blocks);
    let crossings = harness.array::<SurfaceCrossing>(&[], total);
    let (_, e) = harness.run(&mut LiquidBlocks::new(), &[("water", water_in.0), ("level_set", level_in.0), ("solid", solid_in.0)], &[("out", map_out.0)], &p);
    assert!(e.is_empty(), "{e:?}");
    let map: Vec<u32> = read(&map_out.1, blocks);
    let surface = map.iter().filter(|&&b| b & BLOCK_SURFACE != 0).count();
    let (_, e) = harness.run(&mut SurfaceCrossings::new(), &[("level_set", level_in.0), ("solid", solid_in.0)], &[("out", crossings.0)], &p);
    assert!(e.is_empty(), "{e:?}");
    let off: Vec<SurfaceCrossing> = read(&crossings.1, total);
    let (_, e) = harness.run(
        &mut SurfaceCrossings::new(),
        &[("level_set", level_in.0), ("solid", solid_in.0), ("blocks", map_out.0)],
        &[("out", crossings.0)],
        &p,
    );
    assert!(e.is_empty(), "{e:?}");
    let on: Vec<SurfaceCrossing> = read(&crossings.1, total);
    assert_eq!(same_bits(&on, &off), 0, "the map changed the output");

    // GPU time: REPEAT dispatches in one command buffer, GPUEnd − GPUStart.
    use crate::node_graph::freeze::codegen::{ENTRY, standalone_for_spec};
    use manifold_gpu::GpuBinding;
    const REPEAT: u32 = 20;
    let lattice: Vec<u32> = lattice_values.iter().map(|v| v.to_bits()).collect();
    let gpu_ms = |harness: &Harness, wgsl: &str, words: &[u32], buffers: &[&manifold_gpu::GpuBuffer], count: u32| {
        let pipeline = harness.device.create_compute_pipeline(wgsl, ENTRY, "liquid-blocks-timing");
        let mut enc = harness.device.create_encoder("liquid-blocks-timing");
        let (tx, rx) = std::sync::mpsc::channel();
        enc.add_gpu_time_handler(move |seconds| {
            let _ = tx.send(seconds);
        });
        let mut bindings = vec![GpuBinding::Bytes { binding: 0, data: bytemuck::cast_slice(words) }];
        for (i, buffer) in buffers.iter().enumerate() {
            bindings.push(GpuBinding::Buffer { binding: i as u32 + 1, buffer, offset: 0 });
        }
        for _ in 0..REPEAT {
            enc.dispatch_compute(&pipeline, &bindings, [count.div_ceil(256), 1, 1], "liquid-blocks-timing");
        }
        enc.commit_and_wait_completed();
        rx.recv_timeout(std::time::Duration::from_secs(5)).expect("gpu time") * 1e3 / f64::from(REPEAT)
    };
    let map_words: Vec<u32> = lattice.iter().copied().chain([blocks as u32, 0]).collect();
    let map_ms = gpu_ms(&harness, &standalone_for_spec::<LiquidBlocks>().expect("wgsl"), &map_words, &[&water_in.1, &level_in.1, &solid_in.1, &map_out.1], blocks as u32);
    let crossings_wgsl = standalone_for_spec::<SurfaceCrossings>().expect("wgsl");
    let off_words: Vec<u32> = lattice.iter().copied().chain([0, total as u32]).collect();
    let on_words: Vec<u32> = lattice.iter().copied().chain([blocks as u32, total as u32]).collect();
    let off_ms = gpu_ms(&harness, &crossings_wgsl, &off_words, &[&level_in.1, &solid_in.1, &solid_in.1, &crossings.1], total as u32);
    let on_ms = gpu_ms(&harness, &crossings_wgsl, &on_words, &[&level_in.1, &solid_in.1, &map_out.1, &crossings.1], total as u32);
    let after: Vec<SurfaceCrossing> = read(&crossings.1, total);
    assert_eq!(same_bits(&after, &off), 0, "the timed dispatches changed the output");
    eprintln!(
        "liquid blocks timing, {label}, {cells:?} cells s {s}: {surface}/{blocks} blocks with surface; GPU ms per dispatch: map {map_ms:.3}, crossings off {off_ms:.3}, on {on_ms:.3}, saved net of map {:.3}",
        off_ms - on_ms - map_ms
    );
}
