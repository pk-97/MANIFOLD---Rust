//! GPU sheeting against `manifold_fluids::sheeter` (the CPU port proven
//! against FLIP's own sheeter) on the same level set and ordered markers.
use super::gpu_flip_sheeting::{FILL_THRESHOLD, GpuSheeting, SheetInputs};
use super::liquid_surface_tests::read;
use super::sort_particles_into_cells::{LIQUID_PARTICLE_READ, ParticleSorter, SortJob, SortLabels};
use super::prefix_scan::ScanLabels;
use super::whitewater_engine_gpu_tests::marker_phi;
use crate::node_graph::fluid_particles::FluidParticle;
use manifold_fluids::sheeter;
use manifold_gpu::{GpuBuffer, GpuDevice};

const LABELS: SortLabels = SortLabels {
    clear: "sheeting proof sort clear",
    count: "sheeting proof sort count",
    scan: ScanLabels { blocks: "sheeting proof scan blocks", add: "sheeting proof scan add" },
    ranges: "sheeting proof sort ranges",
    tail: "sheeting proof sort tail",
    scatter: "sheeting proof sort scatter",
    stabilise: "sheeting proof sort stabilise",
};

fn shared(device: &GpuDevice, bytes: &[u8]) -> GpuBuffer {
    let buffer = device.create_buffer_shared(bytes.len().max(16) as u64);
    buffer.zero_fill();
    // SAFETY: a fresh shared buffer sized for `bytes`; no GPU work in flight.
    unsafe { buffer.write(0, bytes) };
    buffer
}

fn copy_out(device: &GpuDevice, buffer: &GpuBuffer) -> Vec<u32> {
    let staged = device.create_buffer_shared(buffer.size);
    let mut enc = device.create_encoder("sheeting proof readback");
    enc.copy_buffer_to_buffer(buffer, &staged, buffer.size);
    enc.commit_and_wait_completed();
    read(&staged, (buffer.size / 4) as usize)
}

/// A clock plan (gpu_flip_step.wgsl ClockPlan): live, with `step_dt`.
fn live_plan(device: &GpuDevice, step_dt: f32) -> GpuBuffer {
    let mut words = [0u32; 12];
    words[0] = step_dt.to_bits();
    words[11] = 1;
    shared(device, bytemuck::cast_slice(&words))
}

/// Markers sorted by cell (stable), and the order and ranges the stage reads.
struct Sorted {
    particles: GpuBuffer,
    order: GpuBuffer,
    sorter: ParticleSorter,
    count: u32,
}

fn sort(device: &GpuDevice, markers: &[[f32; 3]], cells: [u32; 3], h: f32) -> Sorted {
    let records: Vec<FluidParticle> = markers
        .iter()
        .enumerate()
        .map(|(i, p)| FluidParticle { position_radius: [p[0], p[1], p[2], 0.1], velocity: [0.0; 3], id: i as u32 + 1 })
        .collect();
    let source = shared(device, bytemuck::cast_slice(&records));
    let particles = device.create_buffer_shared(source.size);
    let order = device.create_buffer_shared((4 * records.len()) as u64);
    let mut sorter = ParticleSorter::default();
    sorter.prepare(device);
    sorter.reserve_ranges(device, cells).expect("ranges");
    let mut enc = device.create_encoder("sheeting proof sort");
    sorter
        .encode(
            device,
            &mut enc,
            &SortJob {
                particles: &source,
                read: LIQUID_PARTICLE_READ,
                capacity: records.len() as u32,
                count: records.len() as u32,
                bin_min: [0.0; 3],
                inv_cell: 1.0 / h,
                bins: cells,
                sorted: Some(&particles),
                order: Some(&order),
                gate: None,
            },
            &LABELS,
        )
        .expect("sort");
    enc.commit_and_wait_completed();
    Sorted { particles, order, sorter, count: records.len() as u32 }
}

struct Run {
    /// Births: x, y, z, rank bits.
    births: Vec<[u32; 4]>,
    count: u32,
    /// Births whose recomputed claim failed in `place`.
    unresolved: u32,
    /// Per cell: bit o marks offset o a candidate, bit 8 + o a claimant.
    flags: Vec<u32>,
    /// Per half-cell: the winning rank.
    claims: Vec<u32>,
}

fn run(device: &GpuDevice, stage: &GpuSheeting, sorted: &Sorted, phi: &GpuBuffer, h: f32, plan: Option<&GpuBuffer>) -> Run {
    let inputs = SheetInputs {
        particles: &sorted.particles,
        order: &sorted.order,
        ranges: sorted.sorter.ranges().expect("ranges"),
        count: sorted.count,
        phi,
        origin: [0.0; 3],
        h,
        threshold: FILL_THRESHOLD,
    };
    let mut enc = device.create_encoder("sheeting proof");
    match plan {
        Some(plan) => stage.encode_gated(&mut enc, &inputs, plan),
        None => stage.encode(&mut enc, &inputs),
    }
    enc.commit_and_wait_completed();
    let (births, count) = stage.births();
    let words = copy_out(device, count);
    let births: Vec<u32> = copy_out(device, births);
    Run {
        births: births.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect(),
        count: words[0],
        unresolved: words[1],
        flags: copy_out(device, &stage.scratch()[6]),
        claims: copy_out(device, &stage.scratch()[5]),
    }
}

/// The half-cell site of a candidate centre, and its engine-order rank.
fn site_of(p: [f32; 3], cells: [u32; 3], h: f32) -> usize {
    let s = p.map(|c| (c / (0.5 * h)).floor() as usize);
    let c = [s[0] / 2, s[1] / 2, s[2] / 2];
    let cell = c[0] + cells[0] as usize * (c[1] + cells[1] as usize * c[2]);
    8 * cell + (s[0] & 1) * 4 + (s[1] & 1) * 2 + (s[2] & 1)
}

fn rank_of(site: usize, cells: [u32; 3]) -> u32 {
    let n = cells.map(|c| c as usize);
    let (cell, o) = (site / 8, site % 8);
    let q = [cell % n[0], cell / n[0] % n[1], cell / (n[0] * n[1])];
    let b = q.map(|c| c / 2);
    let bn = n.map(|c| c.div_ceil(2));
    let bucket = b[0] + bn[0] * (b[1] + bn[1] * b[2]);
    let in_bucket = (q[2] & 1) * 4 + (q[1] & 1) * 2 + (q[0] & 1);
    ((bucket * 8 + in_bucket) * 8 + o) as u32
}

/// GPU against the CPU port on one fixture: births, claimants (more than
/// births when projections compete), cells holding several thin markers.
fn matches_port(device: &GpuDevice, name: &str, markers: &[[f32; 3]], phi: &[f32], cells: [u32; 3], h: f32) -> (u32, u32, usize) {
    let trace = sheeter::trace_sheet_particles(markers, phi, cells, f64::from(h), FILL_THRESHOLD).expect("port");
    let sorted = sort(device, markers, cells, h);
    let phi_buffer = shared(device, bytemuck::cast_slice(phi));
    let mut stage = GpuSheeting::default();
    stage.prepare(device);
    stage.reserve(device, cells, 1 << 16).expect("reserve");
    let gpu = run(device, &stage, &sorted, &phi_buffer, h, None);

    // Candidates by identity. The port lists them as generated; the engine
    // visits them bucket by bucket (k, j, i) in that order within a bucket,
    // which is ascending rank when each bucket's ranks rise as generated.
    let want_sites: Vec<usize> = trace.candidates.iter().map(|&p| site_of(p, cells, h)).collect();
    let mut last = std::collections::HashMap::new();
    for &s in &want_sites {
        let rank = rank_of(s, cells);
        let previous = last.insert(rank / 64, rank);
        assert!(previous.is_none_or(|p| p < rank), "{name}: rank {rank} out of the engine's order in its bucket");
    }
    let mut got_sites: Vec<usize> = (0..8 * gpu.flags.len()).filter(|&s| gpu.flags[s / 8] & (1 << (s % 8)) != 0).collect();
    let mut sorted_want = want_sites.clone();
    sorted_want.sort_unstable();
    got_sites.sort_unstable();
    assert_eq!(got_sites, sorted_want, "{name}: candidate sites differ");

    // The accepted set by source candidate, and the births' positions.
    let want: Vec<(u32, [f32; 3])> =
        trace.seed_candidates.iter().zip(&trace.seeds).map(|(&c, &p)| (rank_of(site_of(trace.candidates[c], cells, h), cells), p)).collect();
    // The port seeds in visiting order, ascending rank; the GPU list is in
    // that order too, unsorted.
    assert!(want.windows(2).all(|w| w[0].0 < w[1].0), "{name}: the port seeds out of rank order");
    assert_eq!(gpu.count as usize, want.len(), "{name}: birth counts differ");
    let got: Vec<(u32, [f32; 3])> = gpu.births[..gpu.count as usize]
        .iter()
        .map(|b| (b[3], [b[0], b[1], b[2]].map(f32::from_bits)))
        .collect();
    assert_eq!(got.iter().map(|g| g.0).collect::<Vec<_>>(), want.iter().map(|w| w.0).collect::<Vec<_>>(), "{name}: births differ in identity or order");
    // Every winning claim resolves exactly once, into its own sub-cell.
    assert_eq!(gpu.unresolved, 0, "{name}: a recomputed claim failed");
    let inv_sub = 1.0 / (0.5 * f64::from(h));
    let mut held = std::collections::HashSet::new();
    for (rank, p) in &got {
        let s = p.map(|c| (f64::from(c) * inv_sub).floor() as usize);
        let cell = s[0] / 2 + cells[0] as usize * (s[1] / 2 + cells[1] as usize * (s[2] / 2));
        let index = 8 * cell + (s[0] & 1) + 2 * (s[1] & 1) + 4 * (s[2] & 1);
        assert_eq!(gpu.claims[index], *rank, "{name}: birth {rank} does not hold its sub-cell");
        assert!(held.insert(index), "{name}: two births in one sub-cell");
    }
    // And the other way: every sub-cell a claim won holds a birth of that
    // rank, so no winner is lost between the claim and the births.
    let claimed: std::collections::HashSet<usize> = (0..gpu.claims.len()).filter(|&i| gpu.claims[i] != u32::MAX).collect();
    assert_eq!(claimed.len(), got.len(), "{name}: claims won and births differ in number");
    assert!(claimed == held, "{name}: a won claim has no birth");
    let claimants: u32 = gpu.flags.iter().map(|f| (f >> 8).count_ones()).sum();
    // Several markers of one cell passing the depth walk: detect's shared flag.
    let mut thin_per_cell = std::collections::HashMap::new();
    for (_, p) in markers.iter().enumerate().filter(|(m, _)| trace.thin[*m]) {
        *thin_per_cell.entry(p.map(|c| (f64::from(c) / f64::from(h)).floor() as i64)).or_insert(0) += 1;
    }
    let shared_cells = thin_per_cell.values().filter(|&&n| n > 1).count();
    let worst = got.iter().zip(&want).flat_map(|(g, w)| (0..3).map(move |a| (g.1[a] - w.1[a]).abs())).fold(0.0f32, f32::max);
    // The port fuses multiply-adds as the engine build does; Metal fuses its own way.
    assert!(worst < 1e-5 * h, "{name}: birth positions differ by {worst}");
    assert!(gpu.count > 0, "{name}: no births");
    eprintln!(
        "SHEETING {name}: {} candidates, {claimants} claimants, {} births, {shared_cells} cells with several thin markers, worst position difference {worst:e} m",
        got_sites.len(),
        gpu.count
    );

    // One bounded timing: the GPU time of a command buffer holding only the
    // sheeting passes, on the warm stage, median of five.
    let inputs = SheetInputs {
        particles: &sorted.particles,
        order: &sorted.order,
        ranges: sorted.sorter.ranges().expect("ranges"),
        count: sorted.count,
        phi: &phi_buffer,
        origin: [0.0; 3],
        h,
        threshold: FILL_THRESHOLD,
    };
    let mut times = Vec::new();
    for _ in 0..5 {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut enc = device.create_encoder("sheeting timing");
        stage.encode(&mut enc, &inputs);
        enc.add_gpu_time_handler(move |seconds| {
            let _ = tx.send(seconds);
        });
        enc.commit_and_wait_completed();
        times.push(rx.recv().expect("GPU time"));
    }
    times.sort_by(f64::total_cmp);
    eprintln!("SHEETING {name}: GPU time median {:.3} ms over 5", times[2] * 1e3);
    (gpu.count, claimants, shared_cells)
}

#[test]
fn gpu_flip_sheeting_matches_the_port_on_a_splash() {
    let device = crate::test_device();
    let (markers, analytic, cells, dx) = sheeter::fixtures::splash();
    let h = dx as f32;
    let (births, claimants, shared) = matches_port(&device, "analytic", &markers, &analytic, cells, h);
    assert!(claimants > births, "projections compete for sub-cells: {claimants} claimants, {births} births");
    assert!(shared > 0, "several thin markers share a cell");
    matches_port(&device, "markers", &markers, &marker_phi(&markers, cells, h), cells, h);
}

/// An inactive clock slot leaves every buffer the stage owns as the last
/// active slot left it; a rerun repeats the births exactly; a short birth list
/// keeps the engine's first births and the true count.
#[test]
fn gpu_flip_sheeting_inactive_slot_and_capacity() {
    let device = crate::test_device();
    let (markers, phi, cells, dx) = sheeter::fixtures::splash();
    let h = dx as f32;
    let sorted = sort(&device, &markers, cells, h);
    let phi_buffer = shared(&device, bytemuck::cast_slice(&phi));
    let mut stage = GpuSheeting::default();
    stage.prepare(&device);
    stage.reserve(&device, cells, 1 << 16).unwrap();
    let full = run(&device, &stage, &sorted, &phi_buffer, h, Some(&live_plan(&device, 0.01)));
    assert!(full.count > 3);
    let snapshot = |stage: &GpuSheeting| -> Vec<Vec<u32>> {
        stage.scratch()[..9].iter().chain([stage.winners()]).map(|b| copy_out(&device, b)).collect()
    };
    let before = snapshot(&stage);
    let other: Vec<f32> = phi.iter().map(|v| -v).collect();
    let other = shared(&device, bytemuck::cast_slice(&other));
    run(&device, &stage, &sorted, &other, h, Some(&live_plan(&device, 0.0)));
    assert!(before == snapshot(&stage), "an inactive slot wrote to the stage");
    let again = run(&device, &stage, &sorted, &phi_buffer, h, None);
    assert!(again.births == full.births && again.count == full.count, "a rerun changed the births");

    let mut short = GpuSheeting::default();
    short.prepare(&device);
    short.reserve(&device, cells, 3).unwrap();
    let three = run(&device, &short, &sorted, &phi_buffer, h, None);
    assert_eq!(three.count, full.count, "a short list still counts every birth");
    assert_eq!(three.births[..3], full.births[..3], "a short list keeps the engine's first births");
    let ranks: Vec<u32> = full.births[..full.count as usize].iter().map(|b| b[3]).collect();
    assert!(ranks.windows(2).all(|w| w[0] < w[1]), "births in rank order");
}

/// Cell and half-cell indices are the engine's f64 floor at non-binary
/// spacing, on the floats either side of every cell and half-cell face, and
/// of their mirrors below zero, where interpolation and the walk sample. A
/// plain f32 product gets some of them wrong (f32 0.3 at 1.5 m: cell 5, where
/// the engine's is 4), so the probe can tell.
#[test]
fn gpu_flip_sheeting_indexing_is_the_engine_floor_at_non_binary_spacing() {
    let device = crate::test_device();
    let cells = [40u32; 3];
    for h in [0.3f32, 0.1, 0.7, 1.0] {
        let dx = f64::from(h);
        let mut points = vec![1.5f32];
        for k in 1..80 {
            let face = (f64::from(k) * 0.5 * dx) as f32;
            let mut x = face;
            for _ in 0..3 {
                x = x.next_down();
            }
            for _ in 0..7 {
                points.push(x);
                x = x.next_up();
            }
        }
        // Interpolation and the depth walk index points below the grid's low
        // faces: the same faces mirrored, down to a few cells under zero.
        let negative: Vec<f32> = points.iter().filter(|&&x| x < 4.0 * h).map(|&x| -x).collect();
        points.extend(negative);
        points.extend([-1.5 * h, -0.5 * h, -f32::MIN_POSITIVE, -1e-30]);
        // Past the exact range and non-finite: clamped to +-2^30, NaN to +2^30.
        let limit = (1u64 << 30) as f32 * h;
        for x in [4294967296.0f32, 1073741824.0, limit, limit.next_down(), limit.next_up(), 1e30, f32::MAX, f32::INFINITY] {
            points.extend([x, -x]);
        }
        points.push(f32::NAN);
        let markers: Vec<[f32; 3]> = points.iter().map(|&x| [x, x, x]).collect();
        let records: Vec<FluidParticle> = markers
            .iter()
            .map(|p| FluidParticle { position_radius: [p[0], p[1], p[2], 0.1], velocity: [0.0; 3], id: 1 })
            .collect();
        let particles = shared(&device, bytemuck::cast_slice(&records));
        let zeros = shared(&device, &vec![0u8; 4 * 64000]);
        let mut stage = GpuSheeting::default();
        stage.prepare(&device);
        stage.reserve(&device, cells, 2 * records.len() as u32).unwrap();
        let mut enc = device.create_encoder("sheeting index probe");
        let inputs = SheetInputs {
            particles: &particles,
            order: &zeros,
            ranges: &zeros,
            count: records.len() as u32,
            phi: &zeros,
            origin: [0.0; 3],
            h,
            threshold: FILL_THRESHOLD,
        };
        stage.encode_index_probe(&device, &mut enc, &inputs);
        enc.commit_and_wait_completed();
        let words = copy_out(&device, stage.births().0);
        // In contract: finite with both floors inside +-2^30, the engine's floor.
        // Policy: the clamp for everything else, deliberately not native's.
        let (mut naive_wrong, mut checked, mut policy) = (0, 0, 0);
        for (s, &x) in points.iter().enumerate() {
            let engine = |inv: f64| {
                let f = (f64::from(x) * inv).floor();
                if f.is_nan() { 1 << 30 } else { f.clamp(-1073741824.0, 1073741824.0) as i32 }
            };
            let (cell, sub) = (engine(1.0 / dx), engine(1.0 / (0.5 * dx)));
            assert_eq!(words[8 * s] as i32, cell, "h {h}: cell of {x}");
            assert_eq!(words[8 * s + 4] as i32, sub, "h {h}: half-cell of {x}");
            if x.is_finite() && cell.abs() < 1 << 30 && sub.abs() < 1 << 30 {
                let naive = (x * (1.0 / dx) as f32).floor() as i32;
                naive_wrong += usize::from(naive != cell);
                checked += 1;
            } else {
                policy += 1;
            }
        }
        eprintln!(
            "SHEETING INDEX h {h}: {checked} in-contract positions match the engine floor (a plain f32 product misses {naive_wrong}); {policy} saturation and NaN inputs match the clamp policy"
        );
        assert!(policy > 0, "the probe covers the clamp policy");
        if h == 0.3 {
            assert!(naive_wrong > 0, "the probe must be able to see the f32 error");
        }
    }
}

/// A flat sheet on a regular marker lattice (exact distance ties among the
/// nearest three) with a hole, at non-binary spacing; and the splash scaled to
/// the same spacings.
#[test]
fn gpu_flip_sheeting_matches_the_port_at_non_binary_spacing() {
    let device = crate::test_device();
    let (markers, analytic, cells, dx) = sheeter::fixtures::splash();
    for h in [0.3f32, 0.1, 0.7] {
        let s = h / dx as f32;
        let scaled: Vec<[f32; 3]> = markers.iter().map(|p| p.map(|c| c * s)).collect();
        let phi: Vec<f32> = analytic.iter().map(|v| v * s).collect();
        matches_port(&device, &format!("splash h {h}"), &scaled, &phi, cells, h);

        // Cell-centre phi |y − 8.25 h| − 0.75 h, markers four to a cell at
        // y 8.1 h, none within 1 h of the hole's centre: the oracle's holed
        // sheet (sheet_oracle.rs) scaled to h.
        let n = 16usize;
        let lattice_phi: Vec<f32> = (0..n * n * n)
            .map(|i| ((((i / n) % n) as f32 + 0.5) - 8.25).abs() * h - 0.75 * h)
            .collect();
        let mut sheet = Vec::new();
        for zi in 8..24 {
            for xi in 8..24 {
                let (x, z) = (0.25 + 0.5 * xi as f32, 0.25 + 0.5 * zi as f32);
                if (x - 8.0).hypot(z - 8.0) >= 1.0 {
                    sheet.push([x * h, 8.1 * h, z * h]);
                }
            }
        }
        matches_port(&device, &format!("lattice h {h}"), &sheet, &lattice_phi, [n as u32; 3], h);
    }
}

/// The depth walk below the grid's low x face: a level set rising along x,
/// so each marker walks toward −x, past zero, where native reads only
/// out-of-range corners (zero) and the walk ends. Markers swept across the
/// first three and a half cells, one to a cell: the near ones feel the zero
/// corners and turn thin, the far ones walk inside the grid and do not. The GPU's thin cells after detect
/// equal the port's thin markers' cells; the port is the native sheeter's
/// decisions bit for bit (sheet_oracle.rs), which exposes only seeds, and
/// nothing seeds this close to the border.
#[test]
fn gpu_flip_sheeting_depth_walk_below_zero_matches_the_port() {
    let device = crate::test_device();
    let cells = [16u32; 3];
    for h in [1.0f32, 0.3] {
        let n = 16usize;
        let phi: Vec<f32> = (0..n * n * n).map(|i| 0.3 * ((i % n) as f32 + 0.5 - 5.0) * h).collect();
        let markers: Vec<[f32; 3]> = (0..48)
            .map(|k| [(0.02 + 0.072 * k as f32) * h, (2 + k % 12) as f32 * h + 0.5 * h, (2 + k / 12) as f32 * h + 0.5 * h])
            .collect();
        let trace = sheeter::trace_sheet_particles(&markers, &phi, cells, f64::from(h), FILL_THRESHOLD).expect("port");
        let cell = |p: &[f32; 3]| {
            let c = p.map(|v| (f64::from(v) * (1.0 / f64::from(h))).floor() as usize);
            c[0] + n * (c[1] + n * c[2])
        };
        let mut want: Vec<usize> = markers.iter().zip(&trace.thin).filter(|(_, t)| **t).map(|(p, _)| cell(p)).collect();
        want.sort_unstable();
        let thin = want.len();
        assert!(thin > 0 && thin < markers.len(), "h {h}: the sweep crosses the walk's decision ({thin} thin)");
        let sorted = sort(&device, &markers, cells, h);
        let phi_buffer = shared(&device, bytemuck::cast_slice(&phi));
        let mut stage = GpuSheeting::default();
        stage.prepare(&device);
        stage.reserve(&device, cells, 16).unwrap();
        let mut enc = device.create_encoder("sheeting detect");
        stage.encode_detect(&mut enc, &SheetInputs {
            particles: &sorted.particles,
            order: &sorted.order,
            ranges: sorted.sorter.ranges().unwrap(),
            count: sorted.count,
            phi: &phi_buffer,
            origin: [0.0; 3],
            h,
            threshold: FILL_THRESHOLD,
        });
        enc.commit_and_wait_completed();
        let flags = copy_out(&device, &stage.scratch()[0]);
        let got: Vec<usize> = (0..flags.len()).filter(|&c| flags[c] != 0).collect();
        assert_eq!(got, want, "h {h}: thin cells differ");
        eprintln!("SHEETING WALK h {h}: {thin} of {} markers thin, cells agree", markers.len());
    }
}

/// Restore the pre-stage-1 selection and both per-site merges. The surrounding
/// indexing, arithmetic, draw and write stay shared; no production switch exists.
pub(super) fn reference_shader() -> String {
    let source = include_str!("shaders/gpu_flip_sheeting.wgsl");
    let start = source.find("// Phase 2:").unwrap();
    let end = source.find("fn bucket_dims()").unwrap();
    let mut source = format!(
        "{}{}\n@compute @workgroup_size(256)\nfn build_buckets() {{}}\n{}",
        &source[..start], OLD_SELECTION, &source[end..],
    );
    let row_read = "let row = bucket_flat(nb);\n                let n = sheet_b[row];";
    assert_eq!(source.matches(row_read).count(), 2);
    assert_eq!(source.matches("let np = selected[32u * row + m].xyz;").count(), 2);
    source = source.replace(row_read, "let n = fill_bucket(nb);")
        .replace("let np = selected[32u * row + m].xyz;", "let np = bucket[m].xyz;")
        .replace("cell_counts", "selected_count");
    source
}

/// CPU-only: validate both complete WGSL modules, including the test reference.
#[test]
fn sheeting_bucket_shaders_validate_on_cpu() {
    for source in [include_str!("shaders/gpu_flip_sheeting.wgsl").to_owned(), reference_shader()] {
        let module = naga::front::wgsl::parse_str(&source).expect("WGSL parse");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module).expect("WGSL validation");
    }
}

struct BucketFixture {
    counts: Vec<u32>,
    segments: Vec<[u32; 4]>,
    merged: Vec<Vec<[u32; 4]>>,
}

/// Shuffled input indices are distributed to cells, then stably sorted per
/// cell as ParticleSorter does. CPU reference uses a stable sort of each
/// bucket's concatenated lists, independent of the shader's eight-way merge.
fn bucket_fixture(cells: [usize; 3], saturated: bool) -> BucketFixture {
    let buckets = cells.map(|n| n.div_ceil(2));
    let n = cells.iter().product::<usize>();
    let rows = buckets.iter().product::<usize>();
    let mut indices: Vec<u32> = (0..4 * n as u32).collect();
    let mut rng = 0x1234_5678u32;
    for i in (1..indices.len()).rev() {
        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
        indices.swap(i, rng as usize % (i + 1));
    }
    let mut fixture = BucketFixture {
        counts: vec![0; n], segments: vec![[0xdead_beef; 4]; 32 * rows], merged: vec![Vec::new(); rows],
    };
    for c in 0..n {
        let q = [c % cells[0], c / cells[0] % cells[1], c / (cells[0] * cells[1])];
        let b = q.map(|v| v / 2);
        let row = b[0] + buckets[0] * (b[1] + buckets[1] * b[2]);
        let lane = (q[0] & 1) + 2 * (q[1] & 1) + 4 * (q[2] & 1);
        let count = if saturated { 4 } else { c % 5 };
        fixture.counts[c] = count as u32;
        let list = &mut indices[4 * c..4 * c + count];
        list.sort_unstable();
        for (m, &index) in list.iter().enumerate() {
            // High index bits include NaN encodings: .w is never numeric data.
            let marker = [(c as f32).to_bits(), (m as f32).to_bits(), (-0.0f32).to_bits(), 0x7fc0_0000 + index];
            fixture.segments[32 * row + 4 * lane + m] = marker;
            fixture.merged[row].push(marker);
        }
    }
    for row in &mut fixture.merged {
        row.sort_by_key(|p| p[3]);
    }
    fixture
}

#[test]
fn sheeting_bucket_fixture_covers_padding_and_saturation() {
    let fixture = bucket_fixture([5, 3, 7], true);
    assert_eq!(fixture.merged.iter().map(Vec::len).sum::<usize>(), 4 * 5 * 3 * 7);
    assert!(fixture.merged.iter().any(|row| row.len() == 32));
    assert_eq!(fixture.merged.last().unwrap().len(), 4);
    assert!(fixture.merged.iter().all(|row| row.windows(2).all(|w| w[0][3] < w[1][3])));
    assert_ne!(&fixture.segments[..32], fixture.merged[0].as_slice(), "merge must reorder unread segments");
    let empty = bucket_fixture([1, 1, 1], false);
    assert!(empty.merged[0].is_empty());
}

#[test]
fn gpu_flip_sheeting_bucket_rows_match_cpu_merge() {
    let device = crate::test_device();
    let mut stage = GpuSheeting::default();
    stage.prepare(&device);
    for cells in [[5u32, 3, 7], [4, 4, 4], [1, 1, 1]] {
        stage.reserve(&device, cells, 1).unwrap();
        for saturated in [true, false] {
            let fixture = bucket_fixture(cells.map(|n| n as usize), saturated);
            let counts = shared(&device, bytemuck::cast_slice(&fixture.counts));
            let segments = shared(&device, bytemuck::cast_slice(&fixture.segments));
            let dummy = shared(&device, &[0; 32]);
            let inputs = SheetInputs {
                particles: &dummy, order: &dummy, ranges: &dummy, count: 0,
                phi: &dummy, origin: [0.0; 3], h: 1.0, threshold: FILL_THRESHOLD,
            };
            let mut enc = device.create_encoder("sheeting bucket proof");
            enc.copy_buffer_to_buffer(&counts, &stage.scratch()[3], 4 * fixture.counts.len() as u64);
            enc.copy_buffer_to_buffer(&segments, &stage.scratch()[4], segments.size);
            stage.encode_buckets(&mut enc, &inputs);
            enc.commit_and_wait_completed();
            let lengths = copy_out(&device, &stage.scratch()[1]);
            let rows = copy_out(&device, &stage.scratch()[4]);
            for (r, want) in fixture.merged.iter().enumerate() {
                assert_eq!(lengths[r] as usize, want.len(), "{cells:?}, row {r}");
                let want: &[u32] = bytemuck::cast_slice(want);
                assert_eq!(&rows[128 * r..128 * r + want.len()], want, "{cells:?}, row {r}");
            }
        }
    }
}

/// Exact words at the stage boundary, including saved-velocity writes and
/// identity reservation. Repeated encodes exercise real GPU command replay;
/// rates/tick/substep change while bindings stay stable.
#[test]
fn gpu_flip_sheeting_rows_match_old_gpu_under_replay_and_fractional_rates() {
    use super::gpu_flip_sheeting::StepBirths;
    use super::particle_identity::{BirthReservation, ParticleIdentity};
    use crate::node_graph::fluid_particles::FaceSample;
    use manifold_gpu::GpuReplayCache;

    let device = crate::test_device();
    let (mut markers, analytic, cells, dx) = sheeter::fixtures::splash();
    // Break spatial input order without changing the geometry.
    let mut rng = 0x0005_1eed_u32;
    for i in (1..markers.len()).rev() {
        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
        markers.swap(i, rng as usize % (i + 1));
    }
    let h = dx as f32;
    let sorted = sort(&device, &markers, cells, h);
    let original = copy_out(&device, &sorted.particles);
    let capacity = sorted.count + 8000;
    let mut initial = vec![0u32; 8 * capacity as usize];
    initial[..original.len()].copy_from_slice(&original);
    let ranges = sorted.sorter.ranges().unwrap();
    let phi = shared(&device, bytemuck::cast_slice(&analytic));
    let faces = vec![FaceSample { velocity: [0.3, -0.2, 0.1, 0.0], weight: [1.0; 4] };
        cells.into_iter().map(|n| (n + 1) as usize).product()];
    let old = shared(&device, bytemuck::cast_slice(&faces));
    let plan = live_plan(&device, 0.01);
    let mut ids = ParticleIdentity::default();
    ids.prepare(&device);
    // Reference/direct, rows/direct, rows/replayed, reference/replayed.
    let mut lanes: Vec<_> = (0..4).map(|lane| {
        let mut stage = GpuSheeting::default();
        if lane == 0 || lane == 3 { stage.prepare_reference(&device); } else { stage.prepare(&device); }
        stage.reserve(&device, cells, capacity).unwrap();
        (stage, shared(&device, bytemuck::cast_slice(&initial)),
            shared(&device, bytemuck::cast_slice(&[5000u32, 1, 0, 0])),
            (lane >= 2).then(GpuReplayCache::default))
    }).collect();
    let mut full_count = 0;
    for (tick, &(rate, active)) in [(1.0, true), (0.25, true), (0.75, true), (0.5, false),
        (0.5, true), (1.0, true), (0.25, true), (0.75, true)].iter().enumerate() {
        let mut clock = [0u32; 12];
        clock[0] = if active { 0.01f32.to_bits() } else { 0 };
        clock[11] = 1;
        // SAFETY: shared plan, all previous command buffers completed.
        unsafe { plan.write(0, bytemuck::cast_slice(&clock)); }
        let mut outputs = Vec::new();
        for (stage, particles, identity, cache) in &mut lanes {
            // Same sorted input on every encode isolates this stage from later
            // solver passes; the stage's own scratch/stats retain their history.
            // SAFETY: sized shared buffers, no GPU work in flight.
            unsafe {
                particles.write(0, bytemuck::cast_slice(&initial));
                identity.write(0, bytemuck::cast_slice(&[5000u32, 1, 0, 0]));
            }
            let inputs = SheetInputs { particles, order: &sorted.order, ranges, count: sorted.count,
                phi: &phi, origin: [0.0; 3], h, threshold: FILL_THRESHOLD };
            let births = StepBirths { old: &old, identity, slots: capacity, rate,
                tick: tick as u32, substep: (tick % 3) as u32 };
            let mut enc = device.create_encoder("sheeting old GPU parity");
            let replay = cache.take().map(|c| enc.begin_replay(&device, c)).is_some();
            stage.encode_draw(&mut enc, &inputs, &plan, &births);
            ids.reserve(&mut enc, BirthReservation { particles, identity, ranges, scan: stage.winners(), plan: &plan,
                params: [capacity, cells.iter().product(), stage.ranks(), 1] });
            enc.compute_memory_barrier_buffers();
            stage.encode_write(&mut enc, &inputs, &plan, &births);
            if replay { *cache = Some(enc.end_replay()); }
            enc.commit_and_wait_completed();
            let mut count = copy_out(&device, stage.births().1);
            count.truncate(2); // Only these words are defined by the stage.
            assert_eq!(count[1], 0, "recomputed claimant failed");
            if active && tick == 0 {
                full_count = count[0];
                assert!(full_count > 100, "fixture must seed sheets");
            } else if active && rate < 1.0 {
                assert!(count[0] > 0 && count[0] < full_count, "fractional draw must retain and reject births");
            }
            let births = copy_out(&device, stage.births().0);
            outputs.push(vec![copy_out(&device, particles), copy_out(&device, identity),
                copy_out(&device, stage.stats()), copy_out(&device, stage.winners()),
                copy_out(&device, &stage.scratch()[5]), copy_out(&device, &stage.scratch()[6]),
                births[..4 * count[0].min(capacity) as usize].to_vec(), count]);
        }
        for lane in 1..outputs.len() {
            assert!(outputs[lane] == outputs[0], "tick {tick}, rate {rate}, active {active}, lane {lane}: output words differ");
        }
    }
    for (_, _, _, cache) in &lanes[2..] {
        let stats = cache.as_ref().unwrap().stats();
        assert!(stats.replayed > 0, "nothing replayed: {stats:?}");
    }
}

// Frozen selection/merge from 09813ff8f; test builds only.
const OLD_SELECTION: &str = r#"// Phase 2: the first four markers of each sheet cell, in input order, with
// -2h <= phi < 2h.
@compute @workgroup_size(256)
fn select_markers(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let c = gid.x;
    if c >= u.nx * u.ny * u.nz || atomicLoad(&sheet_a[c]) == 0u { return; }
    let range = ranges[c];
    var n = 0u;
    for (var s = range.start; s < range.start + range.count && n < MAX_SHEET_PARTICLES_PER_CELL; s = s + 1u) {
        let p = local(particles[s]);
        let value = sample(p);
        if value >= u.max_depth || value < -u.max_depth { continue; }
        selected[4u * c + n] = vec4<f32>(p, bitcast<f32>(order[s]));
        n = n + 1u;
    }
    selected_count[c] = n;
}

// One coarse 2-cell bucket's phase-2 markers in input order: the eight
// cells' lists (each in input order) merged.
var<private> bucket: array<vec4<f32>, 32>;
fn fill_bucket(b: vec3<i32>) -> u32 {
    var heads: array<u32, 8>;
    var counts: array<u32, 8>;
    var cells: array<u32, 8>;
    for (var l = 0; l < 8; l = l + 1) {
        let c = 2 * b + vec3<i32>(l & 1, (l >> 1) & 1, (l >> 2) & 1);
        heads[l] = 0u;
        counts[l] = 0u;
        if in_range(c) {
            cells[l] = flat(c);
            counts[l] = selected_count[cells[l]];
        }
    }
    var n = 0u;
    loop {
        var best = -1;
        var best_index = 0xffffffffu;
        for (var l = 0; l < 8; l = l + 1) {
            if heads[l] < counts[l] {
                let index = bitcast<u32>(selected[4u * cells[l] + heads[l]].w);
                if index < best_index { best_index = index; best = l; }
            }
        }
        if best < 0 { break; }
        bucket[n] = selected[4u * cells[best] + heads[best]];
        heads[best] = heads[best] + 1u;
        n = n + 1u;
    }
    return n;
}

"#;
