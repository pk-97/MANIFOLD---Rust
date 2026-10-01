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

