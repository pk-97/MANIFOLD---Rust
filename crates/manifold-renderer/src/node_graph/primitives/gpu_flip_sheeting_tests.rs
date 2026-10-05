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
const NO_SITE: u32 = u32::MAX;

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
    /// Per half-cell site: x, y, z, rank or a no-candidate mark.
    sites: Vec<[u32; 4]>,
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
    let count = copy_out(device, count)[0];
    let births: Vec<u32> = copy_out(device, births);
    let sites: Vec<u32> = copy_out(device, &stage.scratch()[6]);
    Run {
        births: births.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect(),
        count,
        sites: sites.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect(),
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

/// GPU against the CPU port on one fixture; returns the birth count.
fn matches_port(device: &GpuDevice, name: &str, markers: &[[f32; 3]], phi: &[f32], cells: [u32; 3], h: f32) -> u32 {
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
    let mut got_sites: Vec<usize> = (0..gpu.sites.len()).filter(|&s| gpu.sites[s][3] != NO_SITE).collect();
    let mut sorted_want = want_sites.clone();
    sorted_want.sort_unstable();
    got_sites.sort_unstable();
    assert_eq!(got_sites, sorted_want, "{name}: candidate sites differ");

    // The accepted set by source candidate, and the births' positions.
    let mut want: Vec<(u32, [f32; 3])> =
        trace.seed_sources.iter().zip(&trace.seeds).map(|(&c, &p)| (rank_of(site_of(c, cells, h), cells), p)).collect();
    want.sort_by_key(|w| w.0);
    assert_eq!(gpu.count as usize, want.len(), "{name}: birth counts differ");
    let mut got: Vec<(u32, [f32; 3])> = gpu.births[..gpu.count as usize]
        .iter()
        .map(|b| (b[3], [b[0], b[1], b[2]].map(f32::from_bits)))
        .collect();
    got.sort_by_key(|g| g.0);
    assert_eq!(got.iter().map(|g| g.0).collect::<Vec<_>>(), want.iter().map(|w| w.0).collect::<Vec<_>>(), "{name}: accepted sets differ");
    let worst = got.iter().zip(&want).flat_map(|(g, w)| (0..3).map(move |a| (g.1[a] - w.1[a]).abs())).fold(0.0f32, f32::max);
    // Same f32 projection; the GPU fuses multiply-adds the port does not.
    assert!(worst < 1e-5 * h, "{name}: birth positions differ by {worst}");
    assert!(gpu.count > 0, "{name}: no births");
    eprintln!(
        "SHEETING {name}: {} candidates, {} births, worst position difference {worst:e} m",
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
    gpu.count
}

#[test]
fn gpu_flip_sheeting_matches_the_port_on_a_splash() {
    let device = crate::test_device();
    let (markers, analytic, cells, dx) = sheeter::fixtures::splash();
    let h = dx as f32;
    matches_port(&device, "analytic", &markers, &analytic, cells, h);
    matches_port(&device, "markers", &markers, &marker_phi(&markers, cells, h), cells, h);
}

/// An inactive clock slot leaves every buffer the stage owns as the last
/// active slot left it; a short birth list keeps the first births and the true
/// count.
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
    let before: Vec<Vec<u32>> = stage.scratch()[..9].iter().map(|b| copy_out(&device, b)).collect();
    let other: Vec<f32> = phi.iter().map(|v| -v).collect();
    let other = shared(&device, bytemuck::cast_slice(&other));
    run(&device, &stage, &sorted, &other, h, Some(&live_plan(&device, 0.0)));
    let after: Vec<Vec<u32>> = stage.scratch()[..9].iter().map(|b| copy_out(&device, b)).collect();
    assert!(before == after, "an inactive slot wrote to the stage");

    let mut short = GpuSheeting::default();
    short.prepare(&device);
    short.reserve(&device, cells, 3).unwrap();
    let three = run(&device, &short, &sorted, &phi_buffer, h, None);
    assert_eq!(three.count, full.count, "a short list still counts every birth");
    for b in &three.births[..3] {
        assert!(full.births[..full.count as usize].contains(b), "{b:?} is not a birth");
    }
}
