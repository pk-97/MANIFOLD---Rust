use manifold_node_engine::testkit::liquid_surface::{Harness, Lattice, read};
use crate::node_graph::catalog_tests::liquid_surface::blob_bounds;
    use manifold_node_engine::water::primitives::particle_volume::*;
    use manifold_node_engine::water::fluid_particles::{CellRange, FluidBlob, bin_counts};

    fn expected(
        lattice: &Lattice,
        solid_nodes: [usize; 3],
        interior: Option<&[f32]>,
        blob: Option<FluidBlob>,
    ) -> Vec<f32> {
        let levels = solid_nodes;
        let total = levels.iter().product();
        let band = blob.map_or(0.0, |b| 3.0 * b.center_radius[3]);
        let min = lattice.min();
        (0..total)
            .map(|idx| {
                let ijk = [
                    idx % levels[0],
                    (idx / levels[0]) % levels[1],
                    idx / (levels[0] * levels[1]),
                ];
                let p = std::array::from_fn(|axis| {
                    min[axis] + ijk[axis] as f32 * lattice.size[axis] / (levels[axis] - 1) as f32
                });
                let particle_phi = blob.map_or(band, |blob| {
                    let support = 1.5 * blob.center_radius[3];
                    if (0..3).any(|axis| {
                        let h = lattice.size[axis] / (levels[axis] - 1) as f32;
                        let lo = ((blob.center_radius[axis] - support - min[axis]) / h).floor() as i32;
                        let hi = ((blob.center_radius[axis] + support - min[axis]) / h).floor() as i32 + 1;
                        (ijk[axis] as i32) < lo || ijk[axis] as i32 > hi
                    }) { return band; }
                    let d: [f32; 3] = std::array::from_fn(|axis| p[axis] - blob.center_radius[axis]);
                    let v = [
                        blob.shape_diag[0] * d[0]
                            + blob.shape_off[0] * d[1]
                            + blob.shape_off[1] * d[2],
                        blob.shape_off[0] * d[0]
                            + blob.shape_diag[1] * d[1]
                            + blob.shape_off[2] * d[2],
                        blob.shape_off[1] * d[0]
                            + blob.shape_off[2] * d[1]
                            + blob.shape_diag[2] * d[2],
                    ];
                    let reach = blob.center_radius[3];
                    let distance = v.iter().map(|value| value * value).sum::<f32>().sqrt();
                    band.min(reach * (distance - 1.0))
                });
                manifold_node_engine::testkit::particle_volume::union(particle_phi, interior, p, min, lattice.size, levels)
            })
            .collect()
    }

    fn run_volume(
        lattice: &Lattice,
        solid_nodes: [usize; 3],
        interior: Option<&[f32]>,
        blob: Option<FluidBlob>,
    ) -> Vec<f32> {
        let mut harness = Harness::new();
        let blobs = blob.into_iter().collect::<Vec<_>>();
        let range_count = bin_counts(lattice.size, lattice.cell)
            .iter()
            .product::<u32>() as usize;
        let ranges = vec![
            CellRange {
                start: 0,
                count: blobs.len() as u32
            };
            range_count
        ];
        let (blobs_slot, _) = harness.array(&blobs, blobs.len().max(1));
        let (ranges_slot, _) = harness.array(&ranges, ranges.len());
        let solid = vec![1.0_f32; solid_nodes.iter().product()];
        let (solid_slot, _) = harness.array(&solid, solid.len());
        let interior_slot = interior.map(|values| harness.array(values, values.len().max(1)).0);
        // What node.blob_bounds reduces these blobs to.
        let bounds = blob.map_or([0.0; 2], |b| {
            [b.center_radius[3], 1.5 * b.center_radius[3] + b.shape_off[3]]
        });
        let (bounds_slot, _) = harness.array(&bounds, bounds.len());
        let levels = solid_nodes.map(|n| n as u32);
        let total = levels.iter().product::<u32>() as usize;
        let (levelset_slot, levelset_buf) = harness.array::<f32>(&[], total);
        let mut inputs = vec![
            ("blobs", blobs_slot),
            ("cell_ranges", ranges_slot),
            ("solid", solid_slot),
            ("bounds", bounds_slot),
        ];
        if let Some(slot) = interior_slot {
            inputs.push(("interior", slot));
        }
        let (_, errors) = harness.run(
            &mut ParticleVolume::new(),
            &inputs,
            &[("levelset", levelset_slot)],
            &lattice.params(&[
                ("nodes_x", levels[0] as f32),
                ("nodes_y", levels[1] as f32),
                ("nodes_z", levels[2] as f32),
                ("resolution_scale", 1.0),
            ]),
        );
        assert!(errors.is_empty(), "{errors:?}");
        read(&levelset_buf, total)
    }

    fn assert_matches(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len());
        for (idx, (&got, &want)) in actual.iter().zip(expected).enumerate() {
            assert!(
                (got - want).abs() <= 2e-5,
                "node {idx}: got {got}, expected {want}"
            );
        }
    }

    /// Markers determine the ranges; the shaped centre can move away from its
    /// original marker. Both the support and bin boundaries get their nearest
    /// representable neighbours, including markers clamped at the box edges.
    fn search_boundary_blobs(lattice: &Lattice, solid_nodes: [u32; 3]) -> (Vec<FluidBlob>, Vec<CellRange>) {
        let min = lattice.min();
        let h: [f32; 3] = std::array::from_fn(|a| lattice.size[a] / (solid_nodes[a] - 1) as f32);
        let neighbour = |v: f32, side| match side {
            0 => v.next_down(),
            1 => v,
            _ => v.next_up(),
        };
        let mut marked = Vec::new();
        let mut add = |marker: [f32; 3], center: [f32; 3], radius: f32| {
            let shift = std::array::from_fn::<_, 3, _>(|a| center[a] - marker[a]);
            let shift_length = shift.iter().map(|v| v * v).sum::<f32>().sqrt();
            assert!(shift_length <= 1.301 * lattice.cell);
            let inverse = if radius > 0.0 { radius.recip() } else { 1.0 };
            marked.push((marker, FluidBlob {
                center_radius: [center[0], center[1], center[2], radius],
                shape_diag: [inverse, inverse, inverse, 0.0],
                shape_off: [0.0, 0.0, 0.0, shift_length],
            }));
        };
        for (axis, radius_bins) in [0.1, 0.5, 1.5].into_iter().enumerate() {
            let radius = radius_bins * lattice.cell;
            for side in 0..3 {
                // Native support's upper/lower endpoint meets a lattice node.
                // The same physical nodes occur at all refinement scales.
                for sign in [-1.0, 1.0] {
                    let mut center = std::array::from_fn(|a| min[a] + 3.0 * h[a]);
                    center[axis] = neighbour(center[axis] + sign * 1.5 * radius, side);
                    let mut marker = center;
                    marker[axis] -= sign * 1.3 * lattice.cell;
                    add(marker, center, radius);
                }
                let mut marker = std::array::from_fn(|a| min[a] + 5.0 * h[a]);
                marker[axis] = neighbour(min[axis] + 4.0 * lattice.cell, side);
                let mut center = marker;
                center[axis] += 0.7 * lattice.cell;
                add(marker, center, radius);
            }
        }
        for side in 0..3 {
            for upper in [false, true] {
                let mut marker = lattice.center;
                marker[0] = neighbour(min[0] + if upper { lattice.size[0] } else { 0.0 }, side);
                let mut center = marker;
                center[0] += if upper { -1.3 } else { 1.3 } * lattice.cell;
                add(marker, center, 1.5 * lattice.cell);
            }
        }
        add(lattice.center, lattice.center, 0.0);
        add(min, min, -lattice.cell);
        marked.sort_by_key(|(marker, _)| lattice.bin(*marker));
        let bins = bin_counts(lattice.size, lattice.cell);
        let mut ranges = vec![CellRange { start: 0, count: 0 }; bins.iter().product::<u32>() as usize];
        for (index, (marker, _)) in marked.iter().enumerate() {
            let range = &mut ranges[lattice.bin(*marker)];
            if range.count == 0 { range.start = index as u32; }
            range.count += 1;
        }
        (marked.into_iter().map(|(_, blob)| blob).collect(), ranges)
    }

    #[test]
    fn gpu_flip_volume_tight_bounds_matches_original_search_exactly() {
        use manifold_node_engine::freeze::codegen::{ENTRY, standalone_for_spec};

        let mut old = standalone_for_spec::<ParticleVolume>().expect("volume standalone codegen");
        for (name, expression) in [
            ("first_bin", "max(home - vec3<i32>(reach_bins), vec3<i32>(0))"),
            ("last_bin", "min(home + vec3<i32>(reach_bins), bins - vec3<i32>(1))"),
        ] {
            let prefix = format!("let {name} = ");
            assert_eq!(old.matches(&prefix).count(), 1, "reference replacement must be unique: {name}");
            let assignment = old.lines().find(|line| line.trim_start().starts_with(&prefix)).unwrap().trim().to_owned();
            assert!(assignment.ends_with(';'), "assignment must occupy one line");
            old = old.replacen(&assignment, &format!("{prefix}{expression};"), 1);
        }
        let mut harness = Harness::new();
        let mut reference = ParticleVolume::new();
        reference.pipeline = Some(harness.device.create_compute_pipeline(&old, ENTRY, "particle_volume.original_search"));
        let mut optimized = ParticleVolume::new();
        for (case, lattice) in [
            Lattice { center: [0.0, 1.0, 0.0], size: [2.0, 2.5, 3.0], cell: 0.25 },
            Lattice { center: [13.25, -7.5, 3.75], size: [2.0, 2.5, 3.0], cell: 0.25 },
            Lattice { center: [1000.0, -1000.0, 1000.0], size: [0.25, 0.3125, 0.375], cell: 0.03125 },
        ].iter().enumerate() {
            let nodes = [9, 11, 13];
            let (blobs, ranges) = search_boundary_blobs(lattice, nodes);
            let (blobs_slot, _) = harness.array(&blobs, blobs.len());
            let (ranges_slot, _) = harness.array(&ranges, ranges.len());
            let bounds_slot = blob_bounds(&mut harness, blobs_slot);
            let bounds = read::<f32>(&harness.buffer(bounds_slot), 2);
            assert!(bounds[0] > 0.0 && bounds[1] > 1.5 * bounds[0]);
            let solid = vec![1.0_f32; nodes.iter().product::<u32>() as usize];
            let (solid_slot, _) = harness.array(&solid, solid.len());
            let inputs = [("blobs", blobs_slot), ("cell_ranges", ranges_slot), ("solid", solid_slot), ("bounds", bounds_slot)];
            for scale in [1, 2, 3] {
                let refined = nodes.map(|n| (n - 1) * scale + 1);
                let total = refined.iter().product::<u32>() as usize;
                let (actual_slot, actual_buffer) = harness.array::<f32>(&[], total);
                let (old_slot, old_buffer) = harness.array::<f32>(&[], total);
                for band in [0.0, 0.6 * lattice.cell] {
                    let params = lattice.params(&[
                        ("nodes_x", nodes[0] as f32), ("nodes_y", nodes[1] as f32), ("nodes_z", nodes[2] as f32),
                        ("resolution_scale", scale as f32), ("band_extra", band),
                    ]);
                    for (node, slot) in [(&mut optimized, actual_slot), (&mut reference, old_slot)] {
                        let (_, errors) = harness.run(node, &inputs, &[("levelset", slot)], &params);
                        assert!(errors.is_empty(), "case {case}, scale {scale}, band {band}: {errors:?}");
                    }
                    let actual = read::<f32>(&actual_buffer, total);
                    let original = read::<f32>(&old_buffer, total);
                    assert!(actual.iter().all(|v| v.is_finite()));
                    assert!(actual.iter().any(|&v| v < 0.0), "fixture must contain liquid");
                    assert!(actual.iter().any(|&v| v > 0.0 && v < 3.0 * bounds[0]), "fixture must contain intermediate distances");
                    for (index, (&got, &want)) in actual.iter().zip(&original).enumerate() {
                        assert_eq!(got.to_bits(), want.to_bits(), "case {case}, scale {scale}, band {band}, word {index}: {got} vs {want}");
                    }
                }
            }
        }
    }

    #[test]
    fn gpu_flip_narrow_band_mesher_values() {
        let deep = Lattice {
            center: [0.0; 3],
            size: [2.0; 3],
            cell: 2.0,
        };
        let deep_interior = vec![-3.0; 2 * 2 * 2];
        let deep_actual = run_volume(&deep, [9, 9, 9], Some(&deep_interior), None);
        assert_matches(
            &deep_actual,
            &expected(&deep, [9, 9, 9], Some(&deep_interior), None),
        );

        let surface_blob = FluidBlob {
            center_radius: [0.0, 0.0, 0.0, 0.5],
            shape_diag: [2.0, 2.0, 2.0, 0.0],
            shape_off: [0.0; 4],
        };
        let shallow = vec![-0.1; 2 * 2 * 2];
        let surface = run_volume(&deep, [9, 9, 9], Some(&shallow), Some(surface_blob));
        assert_matches(
            &surface,
            &expected(&deep, [9, 9, 9], Some(&shallow), Some(surface_blob)),
        );

        let off = run_volume(&deep, [9, 9, 9], None, Some(surface_blob));
        assert_matches(&off, &expected(&deep, [9, 9, 9], None, Some(surface_blob)));

        let rectangular = Lattice {
            center: [0.0; 3],
            size: [6.0, 4.0, 4.0],
            cell: 2.0,
        };
        let rectangular_nodes = [10, 9, 8];
        let physical_cells = rectangular_nodes.map(|n| n - 7);
        let rectangular_interior: Vec<f32> = (0..physical_cells.iter().product::<usize>())
            .map(|value| -6.0 + value as f32)
            .collect();
        let rectangular_actual =
            run_volume(&rectangular, rectangular_nodes, Some(&rectangular_interior), None);
        assert_matches(
            &rectangular_actual,
            &expected(&rectangular, rectangular_nodes, Some(&rectangular_interior), None),
        );
    }

    #[test]
    fn fluid_mesh_grid_native_interior_matches_cell_centred_plane() {
        for resolution in [8, 16] {
            let layout = manifold_node_engine::water::fluid::domain_layout(None, 2.0, resolution).unwrap();
            let mesh = manifold_node_engine::water::liquid::lattice::LiquidLattice::from_layout(&layout).surface();
            let lattice = Lattice { center: mesh.bounds().pos, size: mesh.bounds().scale, cell: mesh.cell_size() };
            let field: Vec<f32> = (0..resolution.pow(3)).map(|i| {
                layout.min[0] + (i % resolution) as f32 * mesh.cell_size() + 0.5 * mesh.cell_size() - 0.3
            }).collect();
            let actual = run_volume(&lattice, mesh.nodes().map(|n| n as usize), Some(&field), None);
            for (i, got) in actual.into_iter().enumerate() {
                let x = f64::from(mesh.min()[0]) + (i as u32 % mesh.nodes()[0]) as f64 * layout.cell_size;
                // Trilinear interpolation of a plane is analytic; outside
                // the cell-centre domain the engine extends the end sample.
                let lo = f64::from(layout.min[0]) + 0.5 * layout.cell_size;
                let hi = f64::from(layout.min[0]) + 2.0 - 0.5 * layout.cell_size;
                let want = (x.clamp(lo, hi) - 0.3 + layout.cell_size).min(0.0);
                assert!((f64::from(got) - want).abs() < 1e-6, "node {i}: {got} vs {want}");
            }
        }
    }
