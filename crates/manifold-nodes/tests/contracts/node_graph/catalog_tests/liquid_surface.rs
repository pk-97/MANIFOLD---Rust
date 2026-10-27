use manifold_water_surface::primitives::testkit::surface as surface_nodes;
use std::borrow::Cow;

use manifold_node_engine::bindings::Slot;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_water_liquid::fluid_particles::{CellRange, FluidBlob};
use manifold_water_surface::primitives::particle_volume::ParticleVolume;

use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_surface::testkit::liquid_surface::*;

/// node.blob_bounds over `blobs`: the bounds the field consumers require,
/// produced the way the shipped surface group produces them.
pub(crate) fn blob_bounds(harness: &mut Harness, blobs: Slot) -> Slot {
    let (bounds, _) = harness.array::<f32>(&[], 2);
    // The executor prepares the reduction before its first run; so does this.
    let mut node = manifold_water_surface::primitives::blob_bounds::BlobBounds::new();
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

#[test]
fn fluid_mesh_grid_native_particle_field_matches_reference() {
    let layout = manifold_core::fluid_domain::domain_layout(None, 2.0, 2.0 / 8.0).unwrap();
    let mesh = manifold_water_liquid::lattice::LiquidLattice::from_layout(&layout).surface();
    // Odd cell count, even node count and native half-cell origin, including
    // sparse blob bounds and the expanded closing band at subdivision two.
    for band in [0.0, 0.5] {
        volume_distance_on_lattice(
            band,
            Lattice {
                center: mesh.bounds().pos,
                size: mesh.bounds().scale,
                cell: mesh.cell_size(),
            },
            mesh.nodes(),
        );
    }
}

/// A lone sphere: the level set is the exact signed distance to it inside
/// the cap band.
#[test]
fn fluid_particle_volume_is_the_distance_to_a_lone_sphere() {
    let mut harness = Harness::new();
    let lattice = Lattice {
        center: [0.0, 0.0, 0.0],
        size: [1.0, 1.0, 1.0],
        cell: 0.25,
    };
    let solid_nodes = [5u32, 5, 5];
    let centre = [0.03_f32, -0.02, 0.01];
    // Exact native marker coefficient; scale three exceeds the old bin cap.
    let (r, scale) = (0.31017524 * lattice.cell, 3.0_f32);
    let particles = [particle(centre, r, 1)];
    let shape = [("particle_scale", scale)];
    let (_, _, _, (_, ranges_slot, blobs_slot)) =
        sort_and_shape(&mut harness, &lattice, &particles, 1, &shape);
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
        &[
            ("blobs", blobs_slot),
            ("cell_ranges", ranges_slot),
            ("solid", solid_slot),
            ("bounds", bounds_slot),
        ],
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
        let ijk = [
            idx as u32 % nodes[0],
            (idx as u32 / nodes[0]) % nodes[1],
            idx as u32 / (nodes[0] * nodes[1]),
        ];
        let p: [f64; 3] = std::array::from_fn(|a| f64::from(min[a]) + f64::from(ijk[a]) * h);
        let distance = (0..3)
            .map(|a| (p[a] - f64::from(centre[a])).powi(2))
            .sum::<f64>()
            .sqrt()
            - radius;
        let expected = if native_support(
            ijk,
            centre.map(f64::from),
            radius,
            min.map(f64::from),
            [h; 3],
            0.0,
        ) {
            distance.min(band)
        } else {
            band
        };
        if expected < 0.0 {
            inside += 1;
        }
        assert!(
            (f64::from(value) - expected).abs() <= 2e-6,
            "node {ijk:?}: {value} vs {expected}"
        );
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

/// The volume never reads past the ranges it was wired: a bin grid larger than
/// cell_ranges holds is a named error before any dispatch.
#[test]
fn fluid_searchers_refuse_bins_past_their_ranges() {
    let mut harness = Harness::new();
    let lattice = Lattice {
        center: [0.0; 3],
        size: [2.0; 3],
        cell: 0.25,
    };
    let (sorted, _) = harness.array(&[particle([0.0; 3], 0.05, 1)], 1);
    let (short, _) = harness.array::<CellRange>(&[CellRange { start: 0, count: 1 }; 511], 511);
    let (blobs, _) = harness.array::<FluidBlob>(&[], 1);
    let errors = harness
        .run(
            &mut surface_nodes::shape_particle_blobs(),
            &[("sorted", sorted)],
            &[("blobs", blobs)],
            &params(&[("particle_scale", 3.0)]),
        )
        .1;
    assert!(errors.is_empty(), "{errors:?}");

    let solid_nodes = 9.0;
    let (solid, _) = harness.array(&[1.0_f32; 729], 729);
    let (levelset, _) = harness.array::<f32>(&[], 17 * 17 * 17);
    let volume = lattice.params(&[
        ("nodes_x", solid_nodes),
        ("nodes_y", solid_nodes),
        ("nodes_z", solid_nodes),
    ]);
    let bounds = blob_bounds(&mut harness, blobs);
    let (_, errors) = harness.run(
        &mut ParticleVolume::new(),
        &[
            ("blobs", blobs),
            ("cell_ranges", short),
            ("solid", solid),
            ("bounds", bounds),
        ],
        &[("levelset", levelset)],
        &volume,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.starts_with("Particle Volume") && e.contains("holds 511")),
        "{errors:?}"
    );
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
    let shape = [("particle_scale", 3.0)];
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
    node_params.insert(
        Cow::Borrowed("resolution_scale"),
        ParamValue::Float(scale as f32),
    );
    let volume_nodes: [Slot; 3] = std::array::from_fn(|_| harness.scalar());
    let bounds_slot = blob_bounds(&mut harness, blobs_slot);
    let (scalars, errors) = harness.run(
        &mut ParticleVolume::new(),
        &[
            ("blobs", blobs_slot),
            ("cell_ranges", ranges_slot),
            ("solid", solid_slot),
            ("bounds", bounds_slot),
        ],
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
        let value = scalars
            .iter()
            .find(|(s, _)| s == slot)
            .map(|(_, v)| v.clone());
        assert_eq!(value, Some(ParamValue::Float(expected as f32)));
    }
    let levelset: Vec<f32> = read(&levelset_buf, total);
    if band_extra > 0.0 {
        use manifold_water_surface::primitives::lattice_bricks::{LatticeBricks, brick_layout};
        let layout = brick_layout(solid_nodes, scale).unwrap();
        let (bricks, _) = harness.array::<u32>(&[], layout.words as usize);
        let (_, errors) = harness.run(
            &mut LatticeBricks::new(),
            &[
                ("blobs", blobs_slot),
                ("cell_ranges", ranges_slot),
                ("solid", solid_slot),
                ("bounds", bounds_slot),
            ],
            &[("bricks", bricks)],
            &node_params,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let (sparse, sparse_buf) = harness.array::<f32>(&[], total);
        let (_, errors) = harness.run(
            &mut ParticleVolume::new(),
            &[
                ("blobs", blobs_slot),
                ("cell_ranges", ranges_slot),
                ("solid", solid_slot),
                ("bounds", bounds_slot),
                ("bricks", bricks),
            ],
            &[("levelset", sparse)],
            &node_params,
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            read::<u32>(&sparse_buf, total),
            read::<u32>(&levelset_buf, total)
        );
    }
    let h: [f64; 3] = std::array::from_fn(|a| f64::from(lattice.size[a]) / f64::from(nodes[a] - 1));
    let band = 3.0
        * blobs
            .iter()
            .map(|b| f64::from(b.center_radius[3]))
            .fold(0.0, f64::max)
        + f64::from(band_extra)
        + if band_extra > 0.0 {
            h.iter().map(|v| v * v).sum::<f64>().sqrt()
        } else {
            0.0
        };
    let (mut inside, mut clamped, mut in_band) = (0, 0, 0);
    for (idx, &value) in levelset.iter().enumerate() {
        let ijk = [
            idx as u32 % nodes[0],
            (idx as u32 / nodes[0]) % nodes[1],
            idx as u32 / (nodes[0] * nodes[1]),
        ];
        let p: [f64; 3] = std::array::from_fn(|a| f64::from(min[a]) + f64::from(ijk[a]) * h[a]);
        let mut expected = band;
        for blob in &blobs[..particles.len()] {
            let reach = f64::from(blob.center_radius[3]);
            let extra = f64::from(band_extra)
                + if band_extra > 0.0 {
                    h.iter().map(|v| v * v).sum::<f64>().sqrt()
                } else {
                    0.0
                };
            if reach <= 0.0
                || !native_support(
                    ijk,
                    std::array::from_fn(|a| f64::from(blob.center_radius[a])),
                    reach,
                    min.map(f64::from),
                    h,
                    extra,
                )
            {
                continue;
            }
            let d: [f64; 3] = std::array::from_fn(|a| p[a] - f64::from(blob.center_radius[a]));
            let q = d.iter().map(|x| x * x).sum::<f64>().sqrt();
            expected = expected.min(q - reach);
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
    assert!(
        inside > 100,
        "the fixture has liquid ({inside} inside nodes)"
    );
    assert!(
        in_band > 100,
        "the fixture has nodes inside the cap band ({in_band})"
    );
    assert!(
        clamped > 100,
        "the solid covers part of the lattice ({clamped} nodes)"
    );
}

fn volume_distance_reference(band_extra: f32) {
    let lattice = Lattice {
        center: [0.0, 1.0, 0.0],
        size: [2.0, 2.0, 2.0],
        cell: 0.25,
    };
    let solid_nodes = [9u32, 9, 9];
    volume_distance_on_lattice(band_extra, lattice, solid_nodes);
}
