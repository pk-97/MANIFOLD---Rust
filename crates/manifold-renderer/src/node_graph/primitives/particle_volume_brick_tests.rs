//! Proofs for `node.particle_volume`'s cooperative pass-1 kernel
//! (PARTICLE_VOLUME_BRICK_GATHER_DESIGN.md section 5 (Oracle and proofs)).
//! The oracle is the generated kernel on the same inputs, compared bit for bit.

use super::*;

/// Deterministic LCG; the proofs need reproducible inputs, not quality.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n.max(1)
    }
}

type Window = Option<([i32; 3], [i32; 3])>;

/// The generated kernel's visit order: z, y, x, then k ascending.
fn dense_sequence(bins: [i32; 3], ranges: &[(u32, u32)], window: Window) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let Some((lo, hi)) = window else { return out };
    for z in lo[2]..=hi[2] {
        for y in lo[1]..=hi[1] {
            for x in lo[0]..=hi[0] {
                let bin = (x + bins[0] * (y + bins[1] * z)) as u32;
                let (start, count) = ranges[bin as usize];
                out.extend((start..start + count).map(|k| (bin, k)));
            }
        }
    }
    out
}

/// The cooperative kernel's schedule (particle_volume_brick_gather.wgsl),
/// step for step: union, slabs, 256-bin runs, exclusive prefix, chunked
/// staging by upper-bound search, per-lane row clipping.
fn cooperative_sequences(bins: [i32; 3], ranges: &[(u32, u32)], windows: &[Window], chunk: u32) -> Vec<Vec<(u32, u32)>> {
    let mut out = vec![Vec::new(); windows.len()];
    let identity = ([i32::MAX; 3], [i32::MIN; 3]);
    let (first, last): (Vec<[i32; 3]>, Vec<[i32; 3]>) = windows.iter().map(|w| w.unwrap_or(identity)).unzip();
    let u0: [i32; 3] = std::array::from_fn(|a| first.iter().map(|f| f[a]).min().unwrap());
    let u1: [i32; 3] = std::array::from_fn(|a| last.iter().map(|l| l[a]).max().unwrap());
    if (0..3).any(|a| u0[a] > u1[a]) {
        return out;
    }
    let w = (u1[0] - u0[0] + 1) as u32;
    let bins_in_slab = w * (u1[1] - u0[1] + 1) as u32;
    let runs = bins_in_slab.div_ceil(256);
    let union_bin = |run: u32, i: u32, z: i32| {
        let q = run * 256 + i;
        let x = u0[0] + (q % w) as i32;
        let y = u0[1] + (q / w) as i32;
        (x + bins[0] * (y + bins[1] * z)) as u32
    };
    for z in u0[2]..=u1[2] {
        for run in 0..runs {
            let base = run * 256;
            let counts: Vec<u32> = (0..256)
                .map(|t| if base + t < bins_in_slab { ranges[union_bin(run, t, z) as usize].1 } else { 0 })
                .collect();
            let mut prefix = vec![0u32; 257];
            for t in 0..256 {
                prefix[t + 1] = prefix[t] + counts[t];
            }
            let total = prefix[256];
            let mut chunk_base = 0;
            while chunk_base < total {
                let staged: Vec<(u32, u32)> = (0..chunk.min(256))
                    .filter(|&t| chunk_base + t < total)
                    .map(|t| {
                        let g = chunk_base + t;
                        let (mut lo, mut hi) = (0usize, 256usize);
                        while hi - lo > 1 {
                            let mid = (lo + hi) / 2;
                            if prefix[mid] <= g { lo = mid } else { hi = mid }
                        }
                        let bin = union_bin(run, lo as u32, z);
                        (bin, ranges[bin as usize].0 + (g - prefix[lo]))
                    })
                    .collect();
                let chunk_end = (chunk_base + chunk).min(total);
                for (lane, (f, l)) in first.iter().zip(&last).enumerate() {
                    if !(f[2] <= z && z <= l[2] && f[0] <= l[0]) {
                        continue;
                    }
                    for y in f[1].max(u0[1])..=l[1].min(u1[1]) {
                        let row = (y - u0[1]) as u32 * w;
                        let a = (row + (f[0] - u0[0]) as u32).max(base);
                        let b = (row + (l[0] - u0[0]) as u32).min(base + 255);
                        if a > b {
                            continue;
                        }
                        let lo = prefix[(a - base) as usize].max(chunk_base);
                        let hi = prefix[(b - base + 1) as usize].min(chunk_end);
                        for s in lo..hi {
                            out[lane].push(staged[(s - chunk_base) as usize]);
                        }
                    }
                }
                chunk_base += chunk;
            }
        }
    }
    out
}

#[test]
fn brick_gather_slot_sequence_equals_dense_window_sequence() {
    let mut rng = Lcg(0x5eed);
    let mut runs_split = 0;
    for case in 0..300 {
        let bins = [1 + rng.below(40) as i32, 1 + rng.below(40) as i32, 1 + rng.below(6) as i32];
        let n = (bins[0] * bins[1] * bins[2]) as usize;
        let mut start = 0;
        let ranges: Vec<(u32, u32)> = (0..n)
            .map(|_| {
                let count = match rng.below(10) {
                    0..=4 => 0,
                    5..=8 => 1 + rng.below(4),
                    _ => rng.below(400),
                };
                let range = (start, count);
                start += count;
                range
            })
            .collect();
        let windows: Vec<Window> = (0..256)
            .map(|_| {
                if rng.below(8) == 0 {
                    return None;
                }
                // Mostly nested windows; some empty on one axis.
                let lo: [i32; 3] = std::array::from_fn(|a| rng.below(bins[a] as u32) as i32);
                let hi: [i32; 3] = std::array::from_fn(|a| {
                    if rng.below(30) == 0 { lo[a] - 1 } else { (lo[a] + rng.below(12) as i32).min(bins[a] - 1) }
                });
                Some((lo, hi))
            })
            .collect();
        let chunk = [1, 7, 64, 128, 256][case % 5];
        let cooperative = cooperative_sequences(bins, &ranges, &windows, chunk);
        let u0x = windows.iter().flatten().map(|w| w.0[0]).min().unwrap_or(0);
        let u1x = windows.iter().flatten().map(|w| w.1[0]).max().unwrap_or(0);
        let u0y = windows.iter().flatten().map(|w| w.0[1]).min().unwrap_or(0);
        let u1y = windows.iter().flatten().map(|w| w.1[1]).max().unwrap_or(0);
        runs_split += usize::from((u1x - u0x + 1) * (u1y - u0y + 1) > 256);
        for (lane, window) in windows.iter().enumerate() {
            assert_eq!(cooperative[lane], dense_sequence(bins, &ranges, *window), "case {case}, lane {lane}, chunk {chunk}");
        }
    }
    assert!(runs_split > 50, "fixture must split slabs into several runs: {runs_split}");
}

/// Every type a binding or `Params` member names, spelled out structurally so
/// two modules compare without sharing handles.
fn describe(module: &naga::Module, ty: naga::Handle<naga::Type>) -> String {
    use naga::TypeInner;
    let t = &module.types[ty];
    match &t.inner {
        TypeInner::Struct { members, span } => {
            let fields: Vec<String> = members
                .iter()
                .map(|m| format!("{:?}@{}:{}", m.name, m.offset, describe(module, m.ty)))
                .collect();
            format!("struct {:?} span {span} {{{}}}", t.name, fields.join(", "))
        }
        TypeInner::Array { base, size, stride } => format!("array<{}, {size:?}> stride {stride}", describe(module, *base)),
        other => format!("{other:?}"),
    }
}

fn reflect(source: &str) -> Vec<String> {
    let module = naga::front::wgsl::parse_str(source).expect("kernel parses");
    let mut globals: Vec<String> = module
        .global_variables
        .iter()
        .filter_map(|(_, g)| {
            let binding = g.binding.as_ref()?;
            Some(format!("{}/{} {:?} {:?} {}", binding.group, binding.binding, g.name, g.space, describe(&module, g.ty)))
        })
        .collect();
    globals.sort();
    globals
}

#[test]
fn particle_volume_brick_kernel_abi_matches_codegen() {
    let generated = crate::node_graph::freeze::codegen::standalone_for_spec::<ParticleVolume>().expect("volume codegen");
    let generated = reflect(&generated);
    let brick = reflect(&brick_gather_source());
    assert_eq!(generated.len(), 8, "{generated:#?}");
    for (index, line) in generated.iter().enumerate() {
        assert!(line.starts_with(&format!("0/{index} ")), "bindings 0-7 in order: {line}");
    }
    assert_eq!(brick, generated);
    assert!(generated[0].contains("Params") && generated[0].contains("dispatch_count"));
    assert_eq!(std::mem::size_of::<VolumeUniforms>(), 80);
}

#[cfg(feature = "gpu-proofs")]
mod gpu {
    use crate::node_graph::primitives::liquid_surface_tests::{Harness, Lattice, blob_bounds, read};
    use super::*;
    use crate::node_graph::fluid_particles::{CellRange, FluidBlob, bin_counts};
    use crate::node_graph::freeze::codegen::{ENTRY, standalone_for_spec};
    use crate::node_graph::primitives::lattice_bricks::{brick_layout, compact_brick_words};
    use manifold_gpu::GpuBuffer;

    const CANARY: u32 = 0x7fc0_dead;

    fn generated(h: &Harness) -> GpuComputePipeline {
        let source = standalone_for_spec::<ParticleVolume>().expect("volume codegen");
        h.device.create_compute_pipeline(&source, ENTRY, "particle_volume.generated_oracle")
    }

    fn canary(buffer: &GpuBuffer) {
        let words = vec![CANARY; buffer.size as usize / 4];
        // SAFETY: shared buffer of this size; no GPU work in flight.
        unsafe { buffer.write(0, bytemuck::cast_slice(&words)) };
    }

    fn assert_bitwise(actual: &GpuBuffer, oracle: &GpuBuffer, what: &str) -> (usize, usize) {
        let words = actual.size as usize / 4;
        let a = read::<u32>(actual, words);
        let b = read::<u32>(oracle, words);
        if let Some(i) = a.iter().zip(&b).position(|(a, b)| a != b) {
            panic!(
                "{what}: word {i}: cooperative {} ({:#010x}) vs generated {} ({:#010x})",
                f32::from_bits(a[i]), a[i], f32::from_bits(b[i]), b[i]
            );
        }
        (a.iter().filter(|&&w| w != CANARY).count(), words)
    }

    /// A schedule with every third brick inactive and the border kept, the
    /// way `node.lattice_bricks` keeps it.
    fn schedule(solid_nodes: [u32; 3], scale: u32, salt: u32) -> Vec<u32> {
        let layout = brick_layout(solid_nodes, scale).expect("brick layout");
        let b = layout.bricks;
        let mask: Vec<u32> = (0..layout.count)
            .map(|id| {
                let p = [id % b[0], (id / b[0]) % b[1], id / (b[0] * b[1])];
                let border = (0..3).any(|a| p[a] == 0 || p[a] + 1 == b[a]);
                u32::from(border || !(id + salt).is_multiple_of(3))
            })
            .collect();
        compact_brick_words(&mask, b)
    }

    struct Inputs {
        blobs: GpuBuffer,
        ranges: GpuBuffer,
        solid: GpuBuffer,
        bricks: GpuBuffer,
        interior: GpuBuffer,
        bounds: GpuBuffer,
    }

    /// Pass 1 alone, as `run` dispatches it, through `pipeline` into `out`.
    fn pass1(h: &Harness, pipeline: &GpuComputePipeline, uniforms: VolumeUniforms, i: &Inputs, out: &GpuBuffer) {
        let uniforms = VolumeUniforms { brick_pass: 1, ..uniforms };
        let buffers = [&i.blobs, &i.ranges, &i.solid, &i.bricks, &i.interior, &i.bounds, out];
        let mut bindings = vec![GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) }];
        bindings.extend(buffers.iter().enumerate().map(|(n, b)| GpuBinding::Buffer { binding: n as u32 + 1, buffer: b, offset: 0 }));
        let mut encoder = h.device.create_encoder("particle volume pass 1 proof");
        liquid_bricks::dispatch(&mut encoder, pipeline, &bindings, Some(&i.bricks), 1, uniforms.dispatch_count, "pass 1 proof");
        encoder.commit_and_wait_completed();
    }

    /// Cooperative pass 1 against generated pass 1 on identical inputs; the
    /// whole output, canary-filled first, must match bit for bit, so storage
    /// neither kernel owns stays untouched too. Returns written words.
    fn compare_pass1(h: &Harness, oracle: &GpuComputePipeline, brick: &GpuComputePipeline, uniforms: VolumeUniforms, i: &Inputs, words: usize, what: &str) -> usize {
        let a = h.device.create_buffer_shared((words * 4) as u64);
        let b = h.device.create_buffer_shared((words * 4) as u64);
        canary(&a);
        canary(&b);
        pass1(h, brick, uniforms, i, &a);
        pass1(h, oracle, uniforms, i, &b);
        assert_bitwise(&a, &b, what).0
    }

    fn shared<T: bytemuck::Pod>(h: &Harness, values: &[T]) -> GpuBuffer {
        let bytes = std::mem::size_of_val(values).max(4) as u64;
        let buffer = h.device.create_buffer_shared(bytes);
        buffer.zero_fill();
        // SAFETY: shared buffer of this size; no GPU work in flight.
        unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
        buffer
    }

    fn uniforms_for(lattice: &Lattice, solid_nodes: [u32; 3], scale: u32, band: f32, interior_len: u32) -> VolumeUniforms {
        let bins = bin_counts(lattice.size, lattice.cell);
        let refined = refined_nodes(solid_nodes.map(|n| n as f32), scale);
        VolumeUniforms {
            center_x: lattice.center[0],
            center_y: lattice.center[1],
            center_z: lattice.center[2],
            size_x: lattice.size[0],
            size_y: lattice.size[1],
            size_z: lattice.size[2],
            nodes_x: solid_nodes[0] as f32,
            nodes_y: solid_nodes[1] as f32,
            nodes_z: solid_nodes[2] as f32,
            cell_size: lattice.cell,
            resolution_scale: scale as i32,
            bins_x: bins[0] as i32,
            bins_y: bins[1] as i32,
            bins_z: bins[2] as i32,
            band_extra: band,
            brick_pass: 1,
            interior_len,
            dispatch_count: refined.iter().product(),
            _pad0: 0,
            _pad1: 0,
        }
    }

    fn matrix() -> [Lattice; 3] {
        [
            Lattice { center: [0.0, 1.0, 0.0], size: [2.0, 2.5, 3.0], cell: 0.25 },
            Lattice { center: [13.25, -7.5, 3.75], size: [2.0, 2.5, 3.0], cell: 0.25 },
            Lattice { center: [1000.0, -1000.0, 1000.0], size: [0.25, 0.3125, 0.375], cell: 0.03125 },
        ]
    }

    /// D4 step one: the generated kernel before the helper refactor (frozen
    /// in testdata) against the generated kernel now, on the
    /// search-boundary matrix.
    #[test]
    fn particle_volume_shared_helpers_match_inline_body_bitwise() {
        let mut h = Harness::new();
        let before = include_str!("testdata/particle_volume_pre_helpers.generated.wgsl");
        let mut reference = ParticleVolume::new();
        reference.pipeline = Some(h.device.create_compute_pipeline(before, ENTRY, "particle_volume.pre_helpers"));
        let mut refactored = ParticleVolume::new();
        for (case, lattice) in matrix().iter().enumerate() {
            let nodes = [9, 11, 13];
            let (blobs, ranges) = super::super::gpu_tests::search_boundary_blobs(lattice, nodes);
            let (blobs_slot, _) = h.array(&blobs, blobs.len());
            let (ranges_slot, _) = h.array(&ranges, ranges.len());
            let bounds_slot = blob_bounds(&mut h, blobs_slot);
            let solid = vec![1.0_f32; nodes.iter().product::<u32>() as usize];
            let (solid_slot, _) = h.array(&solid, solid.len());
            let inputs = [("blobs", blobs_slot), ("cell_ranges", ranges_slot), ("solid", solid_slot), ("bounds", bounds_slot)];
            for scale in [1, 2, 3] {
                let total = refined_nodes(nodes.map(|n| n as f32), scale).iter().product::<u32>() as usize;
                let (new_slot, new_buffer) = h.array::<f32>(&[], total);
                let (old_slot, old_buffer) = h.array::<f32>(&[], total);
                for band in [0.0, 0.6 * lattice.cell] {
                    let params = lattice.params(&[
                        ("nodes_x", nodes[0] as f32), ("nodes_y", nodes[1] as f32), ("nodes_z", nodes[2] as f32),
                        ("resolution_scale", scale as f32), ("band_extra", band),
                    ]);
                    for (node, slot) in [(&mut refactored, new_slot), (&mut reference, old_slot)] {
                        let (_, errors) = h.run(node, &inputs, &[("levelset", slot)], &params);
                        assert!(errors.is_empty(), "case {case}, scale {scale}, band {band}: {errors:?}");
                    }
                    let what = format!("helpers: case {case}, scale {scale}, band {band}");
                    let (written, _) = assert_bitwise(&new_buffer, &old_buffer, &what);
                    assert_eq!(written, total);
                    let values = read::<f32>(&new_buffer, total);
                    assert!(values.iter().any(|&v| v < 0.0), "{what}: fixture must contain liquid");
                }
            }
        }
    }

    /// D4 step two (I3) on the same matrix, with interior unwired, native
    /// padding and solver padding, through a brick schedule.
    #[test]
    fn particle_volume_brick_gather_matches_codegen_bitwise() {
        let mut h = Harness::new();
        let oracle = generated(&h);
        let brick = h.device.create_compute_pipeline(&brick_gather_source(), "cs_main", "particle_volume.bricks proof");
        let mut written = 0;
        for (case, lattice) in matrix().iter().enumerate() {
            let nodes = [9u32, 11, 13];
            let (blobs, ranges) = super::super::gpu_tests::search_boundary_blobs(lattice, nodes);
            let (blobs_slot, _) = h.array(&blobs, blobs.len());
            let bounds_slot = blob_bounds(&mut h, blobs_slot);
            let bounds = read::<f32>(&h.buffer(bounds_slot), 2);
            let solid: Vec<f32> = (0..nodes.iter().product::<u32>()).map(|i| if i % 17 == 3 { -0.5 } else { 1.0 }).collect();
            for (kind, cells) in [("none", None), ("native", Some(nodes.map(|n| n - 4))), ("solver", Some(nodes.map(|n| n - 7)))] {
                let interior: Vec<f32> = cells.map_or(vec![0.0], |c| {
                    (0..c.iter().product::<u32>()).map(|i| (i % 7) as f32 * 0.05 - 0.2).collect()
                });
                let interior_len = cells.map_or(0, |c| c.iter().product::<u32>());
                for scale in [1, 2, 3] {
                    let inputs = Inputs {
                        blobs: shared(&h, &blobs),
                        ranges: shared(&h, &ranges),
                        solid: shared(&h, &solid),
                        bricks: shared(&h, &schedule(nodes, scale, case as u32)),
                        interior: shared(&h, &interior),
                        bounds: shared(&h, &bounds),
                    };
                    let total = refined_nodes(nodes.map(|n| n as f32), scale).iter().product::<u32>() as usize;
                    for band in [0.0, 0.6 * lattice.cell] {
                        let uniforms = uniforms_for(lattice, nodes, scale, band, interior_len);
                        let what = format!("case {case}, interior {kind}, scale {scale}, band {band}");
                        written += compare_pass1(&h, &oracle, &brick, uniforms, &inputs, total + 64, &what);
                    }
                }
            }
        }
        assert!(written > 0);
    }

    struct Tails {
        lattice: Lattice,
        solid_nodes: [u32; 3],
        blobs: Vec<FluidBlob>,
        ranges: Vec<CellRange>,
    }

    /// One bin holding 3·CHUNK+1 blobs, sparse bins elsewhere (runs whose
    /// total is zero), NaN/inf centres, blobs with reach ≤ 0, and stretched
    /// kernels, on a lattice whose edge bricks pass `dims`.
    fn tails(rng: &mut Lcg) -> Tails {
        let lattice = Lattice { center: [0.5, -0.25, 2.0], size: [2.6, 1.9, 2.2], cell: 0.1 };
        let solid_nodes = [22, 17, 20];
        let bins = bin_counts(lattice.size, lattice.cell);
        let min = lattice.min();
        let mut marked: Vec<([f32; 3], FluidBlob)> = Vec::new();
        let mut push = |marker: [f32; 3], centre: [f32; 3], reach: f32, stretch: f32| {
            let inv = if reach > 0.0 { 1.0 / reach } else { 1.0 };
            marked.push((marker, FluidBlob {
                center_radius: [centre[0], centre[1], centre[2], reach],
                shape_diag: [inv, inv * stretch, inv, 0.0],
                shape_off: [0.1 * inv, 0.0, -0.05 * inv, 0.0],
            }));
        };
        let crowded = [min[0] + 1.05, min[1] + 0.95, min[2] + 1.15];
        for i in 0..(3 * 128 + 1) {
            let jitter = |s: u32| (s % 97) as f32 / 97.0 * 0.09 - 0.045;
            let c = [crowded[0] + jitter(i * 7), crowded[1] + jitter(i * 13), crowded[2] + jitter(i * 31)];
            push(c, c, 0.03 + (i % 5) as f32 * 0.01, 1.0 + (i % 3) as f32);
        }
        for _ in 0..600 {
            let c: [f32; 3] = std::array::from_fn(|a| min[a] + (rng.below(10_000) as f32 / 10_000.0) * lattice.size[a] * 0.5);
            push(c, c, 0.02 + rng.below(6) as f32 * 0.01, 1.0);
        }
        let odd = [min[0] + 0.55, min[1] + 0.45, min[2] + 0.35];
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            push(odd, [value, odd[1], odd[2]], 0.05, 1.0);
            push(odd, [odd[0], value, odd[2]], 0.05, 1.0);
        }
        push(odd, odd, 0.0, 1.0);
        push(odd, odd, -0.05, 1.0);
        push(odd, odd, f32::NAN, 1.0);
        marked.sort_by_key(|(m, _)| lattice.bin(*m));
        let mut ranges = vec![CellRange { start: 0, count: 0 }; bins.iter().product::<u32>() as usize];
        for (index, (m, _)) in marked.iter().enumerate() {
            let r = &mut ranges[lattice.bin(*m)];
            if r.count == 0 {
                r.start = index as u32;
            }
            r.count += 1;
        }
        assert!(ranges.iter().any(|r| r.count > 3 * 128), "a bin must exceed three chunks");
        Tails { lattice, solid_nodes, blobs: marked.into_iter().map(|(_, b)| b).collect(), ranges }
    }

    #[test]
    fn particle_volume_brick_gather_chunk_tails_bitwise() {
        let h = Harness::new();
        let oracle = generated(&h);
        let brick = h.device.create_compute_pipeline(&brick_gather_source(), "cs_main", "particle_volume.bricks proof");
        let mut rng = Lcg(0x7a11);
        let t = tails(&mut rng);
        let solid: Vec<f32> = (0..t.solid_nodes.iter().product::<u32>()).map(|i| if i % 23 == 5 { -1.0 } else { 1.0 }).collect();
        for scale in [1u32, 2, 4, 8] {
            let refined = refined_nodes(t.solid_nodes.map(|n| n as f32), scale);
            let total: u32 = refined.iter().product();
            // Ten of 21 nodes in z at scale 1: the upper half of the last
            // brick layer is past `dims`, so it is an all-inactive half brick.
            assert!(scale > 1 || refined[2] % 8 == 4);
            let mut words = schedule(t.solid_nodes, scale, 1);
            // Bricks the blobs never reach stay active: empty bricks.
            let layout = brick_layout(t.solid_nodes, scale).unwrap();
            let all = compact_brick_words(&vec![1; layout.count as usize], layout.bricks);
            if scale <= 2 {
                words = all;
            }
            let base_inputs = |bounds: [f32; 2], words: &[u32]| Inputs {
                blobs: shared(&h, &t.blobs),
                ranges: shared(&h, &t.ranges),
                solid: shared(&h, &solid),
                bricks: shared(&h, words),
                interior: shared(&h, &[0.0f32]),
                bounds: shared(&h, &bounds),
            };
            // Reach of at least five bins at scale 1: union rects over 256 bins.
            let reach = [0.07, 0.5];
            let base = uniforms_for(&t.lattice, t.solid_nodes, scale, 0.0, 0);
            let inputs = base_inputs(reach, &words);
            let slack = total as usize + 512;
            for band in [0.0, 0.03] {
                let u = VolumeUniforms { band_extra: band, ..base };
                let written = compare_pass1(&h, &oracle, &brick, u, &inputs, slack, &format!("tails scale {scale} band {band}"));
                assert!(written > 0);
            }
            let short = VolumeUniforms { dispatch_count: total / 3 + 17, ..base };
            compare_pass1(&h, &oracle, &brick, short, &inputs, slack, &format!("dispatch_count below total, scale {scale}"));
            let no_bins = VolumeUniforms { bins_y: 0, ..base };
            compare_pass1(&h, &oracle, &brick, no_bins, &inputs, slack, &format!("bins < 1, scale {scale}"));
            let zero = base_inputs([0.0, 0.0], &words);
            compare_pass1(&h, &oracle, &brick, base, &zero, slack, &format!("bounds [0, 0], scale {scale}"));
        }
    }

    /// The narrow-band value cases, through a brick schedule (every brick
    /// active) so the cooperative kernel computes every node, against the
    /// CPU reference.
    #[test]
    fn gpu_flip_narrow_band_mesher_values_with_bricks() {
        super::super::gpu_tests::narrow_band_mesher_values(true);
    }
}
