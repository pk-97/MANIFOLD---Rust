use manifold_node_engine::water::primitives::testkit as water_nodes;
use std::borrow::Cow;


use manifold_node_engine::water::primitives::particle_volume::ParticleVolume;
use manifold_node_engine::bindings::Slot;
use manifold_node_engine::exec::effect_node::ParamValues;
use manifold_node_engine::particles::{FluidParticle};
use manifold_node_engine::water::fluid_particles::{CellRange, FluidBlob};
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::primitive::Primitive;

use manifold_node_engine::testkit::liquid_surface::*;



/// node.blob_bounds over `blobs`: the bounds the field consumers require,
/// produced the way the shipped surface group produces them.
pub(crate) fn blob_bounds(harness: &mut Harness, blobs: Slot) -> Slot {
    let (bounds, _) = harness.array::<f32>(&[], 2);
    // The executor prepares the reduction before its first run; so does this.
    let mut node = manifold_node_engine::water::primitives::blob_bounds::BlobBounds::new();
    node.prepare_pipelines(&harness.device);
    let (_, errors) = harness.run(
        &mut node,
        &[("blobs", blobs)],
        &[("bounds", bounds)],
        &params(&[]),
    );
    assert!(errors.is_empty(), "{errors:?}");
    bounds
}



#[test]
fn fluid_fill_pits_expanded_band_matches_all_blobs() {
    volume_distance_reference(0.5);
}

/// Every dial that widens the surface at its maximum: Smoothing 3 passes,
/// Resolution Scale 4, Particle Scale 8. Water fills a padded 1 m lattice at
/// resolution 8 against the floor and four closed walls and, through the open
/// top, up to the lattice's top edge. After the clamp every padding node
/// (behind a closed wall) reads air, including borders. Open border samples
/// pass through, as in the native production mesher. The fixture must bring
/// liquid into solid padding before the final clamp.
#[test]
fn fluid_liquid_surface_keeps_padding_and_border_air_at_extreme_dials() {
    use manifold_node_engine::water::liquid::lattice::{LiquidLattice, PADDING_NODES};

    const OPEN_TOP: u32 = 63 & !(1 << 3);
    let mut harness = Harness::new();
    let layout = manifold_node_engine::scene::fluid_domain::domain_layout(None, 1.0, 8).expect("layout");
    let domain = LiquidLattice::from_layout(&layout);
    let (cell, solid_nodes, cells) = (domain.cell_size(), domain.nodes(), domain.cells());
    let bounds = domain.bounds();
    let lattice = Lattice { center: bounds.pos, size: bounds.scale, cell };
    let min = lattice.min();
    let solid = domain.wall_distance(OPEN_TOP);

    // Two particles per cell per axis: x and z across the authored box, y
    // from the floor through the open top's padding.
    let low: [f32; 3] = std::array::from_fn(|a| min[a] + PADDING_NODES as f32 * cell);
    let layers = [2 * cells[0], 2 * (cells[1] + PADDING_NODES), 2 * cells[2]];
    let mut rng = Rng::new(0xb0c0_4011);
    let mut particles = Vec::new();
    for k in 0..layers[2] {
        for j in 0..layers[1] {
            for i in 0..layers[0] {
                let position: [f32; 3] = std::array::from_fn(|a| {
                    let layer = [i, j, k][a] as f32;
                    low[a] + (layer + 0.5 + 0.3 * (rng.next_f32() - 0.5)) * 0.5 * cell
                });
                particles.push(particle(position, 0.25 * cell, particles.len() as u32 + 1));
            }
        }
    }
    let shape = [("particle_scale", 8.0), ("stretch", 1.0), ("smoothing", 0.0), ("isolated_scale", 1.0), ("min_neighbours", 8.0)];
    let (_, _, _, (_, ranges_slot, blobs_slot)) =
        sort_and_shape(&mut harness, &lattice, &particles, particles.len(), &shape);

    let scale = 4u32;
    let nodes = solid_nodes.map(|n| (n - 1) * scale + 1);
    let total = nodes.iter().product::<u32>() as usize;
    let capacity = solid.len() * (scale * scale * scale) as usize;
    let (solid_slot, _) = harness.array(&solid, solid.len());
    let (levelset_slot, _) = harness.array::<f32>(&[], capacity);
    let volume_nodes: [Slot; 3] = std::array::from_fn(|_| harness.scalar());
    let bounds_slot = blob_bounds(&mut harness, blobs_slot);
    let (_, errors) = harness.run(
        &mut ParticleVolume::new(),
        &[("blobs", blobs_slot), ("cell_ranges", ranges_slot), ("solid", solid_slot), ("bounds", bounds_slot)],
        &[
            ("levelset", levelset_slot),
            ("volume_nodes_x", volume_nodes[0]),
            ("volume_nodes_y", volume_nodes[1]),
            ("volume_nodes_z", volume_nodes[2]),
        ],
        &lattice.params(&[
            ("nodes_x", solid_nodes[0] as f32),
            ("nodes_y", solid_nodes[1] as f32),
            ("nodes_z", solid_nodes[2] as f32),
            ("resolution_scale", scale as f32),
        ]),
    );
    assert!(errors.is_empty(), "{errors:?}");
    let mut source = levelset_slot;
    let mut smoothed = None;
    for axis in 0..3 {
        let (stage, buffer) = harness.array::<f32>(&[], capacity);
        let smoothing = [
            ("nodes_x", nodes[0] as f32),
            ("nodes_y", nodes[1] as f32),
            ("nodes_z", nodes[2] as f32),
            ("passes", 3.0),
            ("axis", axis as f32),
        ];
        let (_, errors) = harness.run(&mut water_nodes::smooth_lattice(None), &[("levelset", source)], &[("smoothed", stage)], &params(&smoothing));
        assert!(errors.is_empty(), "{errors:?}");
        source = stage;
        smoothed = Some(buffer);
    }
    let (clamped_slot, clamped_buf) = harness.array::<f32>(&[], capacity);
    let (_, errors) = harness.run(
        &mut water_nodes::clamp_liquid_to_solids(None),
        &[("levelset", source), ("solid", solid_slot)],
        &[("clamped", clamped_slot)],
        &clamp_params(lattice.center, lattice.size, nodes, solid_nodes, cell),
    );
    assert!(errors.is_empty(), "{errors:?}");

    let smoothed: Vec<f32> = read(&smoothed.expect("three passes"), total);
    let clamped: Vec<f32> = read(&clamped_buf, total);
    let h: [f64; 3] = std::array::from_fn(|a| f64::from(lattice.size[a]) / f64::from(nodes[a] - 1));
    let (mut border, mut padding) = (0, 0);
    let (mut border_moved_before, mut padding_liquid_before) = (0, 0);
    for idx in 0..total {
        let ijk = [idx as u32 % nodes[0], (idx as u32 / nodes[0]) % nodes[1], idx as u32 / (nodes[0] * nodes[1])];
        let p: [f64; 3] = std::array::from_fn(|a| f64::from(min[a]) + f64::from(ijk[a]) * h[a]);
        let solid_value = solid_sample(&solid, solid_nodes, min, lattice.size, p);
        if (0..3).any(|a| ijk[a] == 0 || ijk[a] == nodes[a] - 1) {
            if solid_value < -1e-4 {
                assert!(clamped[idx] >= 0.0, "solid border node {ijk:?} reads liquid");
            } else if solid_value > 1e-4 {
                assert_eq!(clamped[idx].to_bits(), smoothed[idx].to_bits(), "open border {ijk:?}");
            }
            border += 1;
            border_moved_before += usize::from(smoothed[idx] < 0.0);
            continue;
        }
        if solid_value < -1e-4 {
            assert!(clamped[idx] >= 0.0, "padding node {ijk:?} reads liquid: {}", clamped[idx]);
            padding += 1;
            padding_liquid_before += usize::from(smoothed[idx] < 0.0);
        }
    }
    assert!(border > 1000 && padding > 1000, "border {border}, padding {padding}");
    assert!(
        padding_liquid_before > 0,
        "smoothing must reach the solid padding; liquid border samples ({border_moved_before}) and the unclamped surface must reach the padding ({padding_liquid_before})"
    );
}

#[test]
fn fluid_mesh_grid_native_particle_field_matches_reference() {
    let layout = manifold_node_engine::scene::fluid_domain::domain_layout(None, 2.0, 8).unwrap();
    let mesh = manifold_node_engine::water::liquid::lattice::LiquidLattice::from_layout(&layout).surface();
    // Odd cell count, even node count and native half-cell origin, including
    // sparse blob bounds and the expanded closing band at subdivision two.
    for band in [0.0, 0.5] {
        volume_distance_on_lattice(band, Lattice {
            center: mesh.bounds().pos, size: mesh.bounds().scale, cell: mesh.cell_size(),
        }, mesh.nodes());
    }
}

/// A lone sphere: the level set is the exact signed distance to it inside
/// the cap band.
#[test]
fn fluid_particle_volume_is_the_distance_to_a_lone_sphere() {
    let mut harness = Harness::new();
    let lattice = Lattice { center: [0.0, 0.0, 0.0], size: [1.0, 1.0, 1.0], cell: 0.25 };
    let solid_nodes = [5u32, 5, 5];
    let centre = [0.03_f32, -0.02, 0.01];
    // Exact native marker coefficient; scale three exceeds the old bin cap.
    let (r, scale) = (0.31017524 * lattice.cell, 3.0_f32);
    let particles = [particle(centre, r, 1)];
    let shape = [("particle_scale", scale), ("stretch", 4.0), ("smoothing", 0.0), ("isolated_scale", 1.0), ("min_neighbours", 6.0)];
    let (_, _, _, (_, ranges_slot, blobs_slot)) = sort_and_shape(&mut harness, &lattice, &particles, 1, &shape);
    // Everything outside the solid.
    let solid = vec![1.0_f32; solid_nodes.iter().product::<u32>() as usize];
    let (solid_slot, _) = harness.array(&solid, solid.len());
    let res = 4u32;
    let nodes = solid_nodes.map(|n| (n - 1) * res + 1);
    let total = nodes.iter().product::<u32>() as usize;
    let (levelset_slot, levelset_buf) = harness.array::<f32>(&[], total);
    let volume_nodes: [Slot; 3] = std::array::from_fn(|_| harness.scalar());
    let bounds_slot = blob_bounds(&mut harness, blobs_slot);
    let (_, errors) = harness.run(
        &mut ParticleVolume::new(),
        &[("blobs", blobs_slot), ("cell_ranges", ranges_slot), ("solid", solid_slot), ("bounds", bounds_slot)],
        &[
            ("levelset", levelset_slot),
            ("volume_nodes_x", volume_nodes[0]),
            ("volume_nodes_y", volume_nodes[1]),
            ("volume_nodes_z", volume_nodes[2]),
        ],
        &lattice.params(&[
            ("nodes_x", solid_nodes[0] as f32),
            ("nodes_y", solid_nodes[1] as f32),
            ("nodes_z", solid_nodes[2] as f32),
            ("resolution_scale", res as f32),
        ]),
    );
    assert!(errors.is_empty(), "{errors:?}");
    let levelset: Vec<f32> = read(&levelset_buf, total);
    let min = lattice.min();
    let h = f64::from(lattice.size[0]) / f64::from(nodes[0] - 1);
    let radius = f64::from(scale * r);
    let band = 3.0 * radius;
    let mut inside = 0;
    for (idx, &value) in levelset.iter().enumerate() {
        let ijk = [idx as u32 % nodes[0], (idx as u32 / nodes[0]) % nodes[1], idx as u32 / (nodes[0] * nodes[1])];
        let p: [f64; 3] = std::array::from_fn(|a| f64::from(min[a]) + f64::from(ijk[a]) * h);
        let distance = (0..3).map(|a| (p[a] - f64::from(centre[a])).powi(2)).sum::<f64>().sqrt() - radius;
        let expected = if native_support(ijk, centre.map(f64::from), radius, min.map(f64::from), [h; 3], 0.0) { distance.min(band) } else { band };
        if expected < 0.0 {
            inside += 1;
        }
        assert!((f64::from(value) - expected).abs() <= 2e-6, "node {ijk:?}: {value} vs {expected}");
    }
    assert!(inside > 20, "the sphere covers lattice nodes ({inside})");
}

/// The volume against a brute force over every blob, not just the node's
/// bins: it matches only if no blob the ±1-bin search misses comes within the
/// cap, which is the blob atom's reach contract.
#[test]
fn fluid_particle_volume_matches_brute_force_distance_and_solid_clamp() {
    volume_distance_reference(0.0);
}

/// Relaxation gathers every input, so it never fuses as a consumer. As a
/// producer it could head a region with a coincident mesh atom after it, but
/// no fused capacity shape covers a gathered anchor beside other gathers, so
/// the region builder refuses it. Pinned on the shipped Dam Break with a
/// rotate after the relax chain: once this fails, the fused-vs-unfused render
/// proof is owed (BUG-xwf1 (Liquid Surface mesh relaxation)).
#[test]
fn fluid_relax_surface_mesh_stays_standalone_in_the_fused_view() {
    use manifold_core::effect_graph_def::EffectGraphDef;
    use serde_json::{Value, json};

    let registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
    let json = manifold_nodes::bundled_presets::bundled_preset_json(&manifold_core::PresetTypeId::new(
        "WaterDamBreakGpuFlip",
    ))
    .expect("Dam Break bundled");
    let mut preset: Value = serde_json::from_str(&json).expect("Dam Break parses");
    let surface = manifold_node_engine::water::liquid::conformance::json_node_mut(&mut preset, "surface")
        .expect("the Liquid Surface group");
    let group = &mut surface["group"];
    let last = manifold_node_engine::water::liquid::conformance::json_node_mut(group, "liquid_normals")
        .expect("surface normals")["id"].clone();
    let out = group["nodes"].as_array().expect("group nodes").iter()
        .find(|node| node["typeId"] == "system.group_output")
        .unwrap_or_else(|| panic!("no system.group_output"))["id"].clone();
    let turn = json!(100);
    group["nodes"].as_array_mut().expect("group nodes").push(json!({
        "id": turn, "typeId": "node.rotate_3d", "nodeId": "liquid_turn",
        "params": {"angle_y": {"type": "Float", "value": 0.01}}
    }));
    let wires = group["wires"].as_array_mut().expect("group wires");
    let into_output = wires
        .iter_mut()
        .find(|w| w["fromNode"] == last && w["toNode"] == out)
        .expect("the relax chain feeds the group output");
    into_output["toNode"] = turn.clone();
    into_output["toPort"] = json!("in");
    wires.push(json!({"fromNode": turn, "fromPort": "out", "toNode": out, "toPort": "vertices"}));

    let def: EffectGraphDef = serde_json::from_value(preset).expect("the variant loads");
    // No region anywhere in the graph means the unfused graph renders, where
    // each relax pass is trivially its own dispatch.
    let Some(view) = manifold_node_engine::freeze::install::fuse_generator_view(&def, &registry) else {
        return;
    };
    let relaxes = view.def.nodes.iter().filter(|n| matches!(n.type_id.as_str(), "node.smooth_surface_mesh" | "node.surface_mesh_normals")).count();
    assert_eq!(relaxes, 2, "smoothing and normals stay their own dispatch");
    assert!(
        !view.def.nodes.iter().any(|n| n.wgsl_source.as_deref().is_some_and(|s| s.contains("sm_adj_cell_edge"))),
        "relaxation fused into a kernel: prove it renders like the unfused graph"
    );
}

/// A searcher never reads past the ranges it was wired: a bin grid larger than
/// cell_ranges holds, or one with an empty axis, is a named error before any
/// dispatch. Bins wired at 0 (the sort has no lattice yet) run nothing, silently.
#[test]
fn fluid_searchers_refuse_bins_past_their_ranges() {
    let mut harness = Harness::new();
    let lattice = Lattice { center: [0.0; 3], size: [2.0; 3], cell: 0.25 };
    let (sorted, _) = harness.array(&[particle([0.0; 3], 0.05, 1)], 1);
    let (short, _) = harness.array::<CellRange>(&[CellRange { start: 0, count: 1 }; 511], 511);
    let (blobs, blobs_buf) = harness.array::<FluidBlob>(&[], 1);
    let untouched = |harness: &Harness| read::<u8>(&harness.buffer(blobs), blobs_buf.size as usize).iter().all(|&b| b == 0);
    let shape = |harness: &mut Harness, ranges: Slot, extra_inputs: &[(&'static str, Slot)], params: &ParamValues| {
        let mut inputs = vec![("sorted", sorted), ("cell_ranges", ranges)];
        inputs.extend_from_slice(extra_inputs);
        harness.run(&mut water_nodes::shape_particle_blobs(), &inputs, &[("blobs", blobs)], params).1
    };

    let errors = shape(&mut harness, short, &[], &lattice.params(&[]));
    assert!(errors.iter().any(|e| e.contains("needs 512 cell ranges") && e.contains("holds 511")), "{errors:?}");
    assert!(untouched(&harness), "a refused search dispatches nothing");

    let (ranges, _) = harness.array::<CellRange>(&[CellRange { start: 0, count: 1 }; 512], 512);
    let errors = shape(&mut harness, ranges, &[], &lattice.params(&[("bins_y", 0.0)]));
    assert!(errors.iter().any(|e| e.contains("whole and at least 1")), "{errors:?}");
    assert!(untouched(&harness), "no bins, no dispatch");

    let zero: [Slot; 3] = std::array::from_fn(|_| harness.scalar_input(0.0));
    let wired = [("bins_x", zero[0]), ("bins_y", zero[1]), ("bins_z", zero[2])];
    let errors = shape(&mut harness, short, &wired, &lattice.params(&[]));
    assert!(errors.is_empty(), "{errors:?}");
    assert!(untouched(&harness), "a sort without a lattice leaves the search idle");

    // A graph saved before the bins wires: the sort's rule on the shared box,
    // checked the same way.
    let legacy = lattice.params(&[("bins_x", 0.0), ("bins_y", 0.0), ("bins_z", 0.0)]);
    let errors = shape(&mut harness, short, &[], &legacy);
    assert!(errors.iter().any(|e| e.contains("needs 512 cell ranges")), "{errors:?}");
    assert!(untouched(&harness), "a refused legacy search dispatches nothing");
    let errors = shape(&mut harness, ranges, &[], &legacy);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!untouched(&harness), "the fixture does dispatch when the bins fit");

    let solid_nodes = 9.0;
    let (solid, _) = harness.array(&[1.0_f32; 729], 729);
    let (levelset, _) = harness.array::<f32>(&[], 17 * 17 * 17);
    let volume = lattice.params(&[("nodes_x", solid_nodes), ("nodes_y", solid_nodes), ("nodes_z", solid_nodes)]);
    let bounds = blob_bounds(&mut harness, blobs);
    let (_, errors) = harness.run(
        &mut ParticleVolume::new(),
        &[("blobs", blobs), ("cell_ranges", short), ("solid", solid), ("bounds", bounds)],
        &[("levelset", levelset)],
        &volume,
    );
    assert!(errors.iter().any(|e| e.starts_with("Particle Volume") && e.contains("holds 511")), "{errors:?}");
}

fn volume_distance_on_lattice(band_extra: f32, lattice: Lattice, solid_nodes: [u32; 3]) {
    let mut harness = Harness::new();
    let min = lattice.min();
    let mut rng = Rng::new(0x1234_5678);
    let particles: Vec<FluidParticle> = (0..900u32)
        .map(|i| {
            let position = std::array::from_fn(|axis| {
                let spread = if axis == 1 { 0.6 } else { 1.4 };
                lattice.center[axis] - 0.5 * spread + spread * rng.next_f32()
            });
            particle(position, 0.05, i + 1)
        })
        .collect();
    let shape = [("particle_scale", 3.0), ("stretch", 4.0), ("smoothing", 0.9), ("isolated_scale", 0.6), ("min_neighbours", 6.0)];
    let (_, _, blobs, (_, ranges_slot, blobs_slot)) =
        sort_and_shape(&mut harness, &lattice, &particles, particles.len(), &shape);

    // Solid: the half-space below y = 0.7 (distance y − 0.7).
    let spacing = lattice.size[1] / (solid_nodes[1] - 1) as f32;
    let solid: Vec<f32> = (0..solid_nodes.iter().product::<u32>())
        .map(|i| {
            let j = (i / solid_nodes[0]) % solid_nodes[1];
            min[1] + j as f32 * spacing - 0.7
        })
        .collect();
    let (solid_slot, _) = harness.array(&solid, solid.len());
    let scale = 2u32;
    let nodes = solid_nodes.map(|n| (n - 1) * scale + 1);
    let total = nodes.iter().product::<u32>() as usize;
    let (levelset_slot, levelset_buf) = harness.array::<f32>(&[], total);
    let mut node_params = lattice.params(&[
        ("nodes_x", solid_nodes[0] as f32),
        ("nodes_y", solid_nodes[1] as f32),
        ("nodes_z", solid_nodes[2] as f32),
        ("resolution_scale", scale as f32),
        ("band_extra", band_extra),
    ]);
    node_params.insert(Cow::Borrowed("resolution_scale"), ParamValue::Float(scale as f32));
    let volume_nodes: [Slot; 3] = std::array::from_fn(|_| harness.scalar());
    let bounds_slot = blob_bounds(&mut harness, blobs_slot);
    let (scalars, errors) = harness.run(
        &mut ParticleVolume::new(),
        &[("blobs", blobs_slot), ("cell_ranges", ranges_slot), ("solid", solid_slot), ("bounds", bounds_slot)],
        &[
            ("levelset", levelset_slot),
            ("volume_nodes_x", volume_nodes[0]),
            ("volume_nodes_y", volume_nodes[1]),
            ("volume_nodes_z", volume_nodes[2]),
        ],
        &node_params,
    );
    assert!(errors.is_empty(), "{errors:?}");
    for (slot, expected) in volume_nodes.iter().zip(nodes) {
        let value = scalars.iter().find(|(s, _)| s == slot).map(|(_, v)| v.clone());
        assert_eq!(value, Some(ParamValue::Float(expected as f32)));
    }
    let levelset: Vec<f32> = read(&levelset_buf, total);
    if band_extra > 0.0 {
        use manifold_node_engine::water::primitives::lattice_bricks::{LatticeBricks, brick_layout};
        let layout = brick_layout(solid_nodes, scale).unwrap();
        let (bricks, _) = harness.array::<u32>(&[], layout.words as usize);
        let (_, errors) = harness.run(
            &mut LatticeBricks::new(),
            &[("blobs", blobs_slot), ("cell_ranges", ranges_slot), ("solid", solid_slot), ("bounds", bounds_slot)],
            &[("bricks", bricks)], &node_params,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let (sparse, sparse_buf) = harness.array::<f32>(&[], total);
        let (_, errors) = harness.run(
            &mut ParticleVolume::new(),
            &[("blobs", blobs_slot), ("cell_ranges", ranges_slot), ("solid", solid_slot), ("bounds", bounds_slot), ("bricks", bricks)],
            &[("levelset", sparse)], &node_params,
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(read::<u32>(&sparse_buf, total), read::<u32>(&levelset_buf, total));
    }
    let h: [f64; 3] = std::array::from_fn(|a| f64::from(lattice.size[a]) / f64::from(nodes[a] - 1));
    let band = 3.0 * blobs.iter().map(|b| f64::from(b.center_radius[3])).fold(0.0, f64::max) + f64::from(band_extra)
        + if band_extra > 0.0 { h.iter().map(|v| v*v).sum::<f64>().sqrt() } else { 0.0 };
    let (mut inside, mut clamped, mut in_band) = (0, 0, 0);
    for (idx, &value) in levelset.iter().enumerate() {
        let ijk = [idx as u32 % nodes[0], (idx as u32 / nodes[0]) % nodes[1], idx as u32 / (nodes[0] * nodes[1])];
        let p: [f64; 3] = std::array::from_fn(|a| f64::from(min[a]) + f64::from(ijk[a]) * h[a]);
        let mut expected = band;
        for blob in &blobs[..particles.len()] {
            let reach = f64::from(blob.center_radius[3]);
            let extra = f64::from(band_extra) + if band_extra > 0.0 { h.iter().map(|v| v*v).sum::<f64>().sqrt() } else { 0.0 };
            if reach <= 0.0 || !native_support(ijk, std::array::from_fn(|a| f64::from(blob.center_radius[a])), reach, min.map(f64::from), h, extra) {
                continue;
            }
            let d: [f64; 3] = std::array::from_fn(|a| p[a] - f64::from(blob.center_radius[a]));
            let g = blob_matrix(blob);
            let v: [f64; 3] = std::array::from_fn(|r| (0..3).map(|c| g[r][c] * d[c]).sum());
            let q = v.iter().map(|x| x * x).sum::<f64>().sqrt();
            expected = expected.min(reach * (q - 1.0));
        }
        // Solid distance is linear in y, so trilinear interpolation is exact.
        if p[1] - 0.7 < 0.0 {
            expected = expected.max(0.0);
            clamped += 1;
        }
        if expected < 0.0 {
            inside += 1;
        } else if expected > 0.0 && expected < band {
            in_band += 1;
        }
        assert!(
            (f64::from(value) - expected).abs() <= 2e-6,
            "node {ijk:?}: {value} vs {expected}"
        );
    }
    assert!(inside > 100, "the fixture has liquid ({inside} inside nodes)");
    assert!(in_band > 100, "the fixture has nodes inside the cap band ({in_band})");
    assert!(clamped > 100, "the solid covers part of the lattice ({clamped} nodes)");
}

fn volume_distance_reference(band_extra: f32) {
    let lattice = Lattice { center: [0.0, 1.0, 0.0], size: [2.0, 2.0, 2.0], cell: 0.25 };
    let solid_nodes = [9u32, 9, 9];
    volume_distance_on_lattice(band_extra, lattice, solid_nodes);
}
