//! GPU FLIP's side of the water race (docs/GPU_FLIP_PRESSURE_SOLVE.md
//! section 6 (measures)): the Dam Break probes that report cost, what the
//! projection leaves undone, occupancy, packing, particle motion and the water
//! volume the surface holds, plus the density-source sweep and the settle
//! check. Minutes long, so opt-in: `--features water-race-probes`.
//! `gpu_flip_scenes_cover_every_dispatch` proves every array these graphs
//! allocate before any of them runs here.

use super::gpu_flip_preset::{PRESSURE_ITERATIONS, WaterScene};
use super::gpu_flip_scene_tests::{Run, divergence, particle_stats};
use super::gpu_flip_still::write_still;
use super::gpu_flip_volume::VolumeDrift;
use crate::node_graph::fluid_particles::FluidParticle;

/// How the live particles move: mean, 99th-percentile and top speed (m/s),
/// and the highest particle (m above the floor).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Motion {
    pub mean: f64,
    pub p99: f64,
    pub fastest: f64,
    pub highest: f64,
}

/// `particles` are (position and radius, velocity) pairs; radius 0 marks an
/// unused slot.
pub(crate) fn motion(particles: impl Iterator<Item = ([f32; 4], [f32; 3])>, floor: f64) -> Motion {
    let mut speeds = Vec::new();
    let mut highest = f64::MIN;
    for (p, v) in particles.filter(|(p, _)| p[3] > 0.0) {
        speeds.push(v.iter().map(|&c| f64::from(c).powi(2)).sum::<f64>().sqrt());
        highest = highest.max(f64::from(p[1]) - floor);
    }
    if speeds.is_empty() {
        return Motion::default();
    }
    speeds.sort_by(f64::total_cmp);
    let last = speeds.len() - 1;
    Motion {
        mean: speeds.iter().sum::<f64>() / speeds.len() as f64,
        p99: speeds[(speeds.len() * 99 / 100).min(last)],
        fastest: speeds[last],
        highest,
    }
}

/// Prints the fastest and highest particle over the run, and how fast the
/// water still moves over its last 30 frames (medians of the per-frame mean
/// and 99th percentile).
pub(crate) fn report_motion(label: &str, motion: &[Motion]) {
    let top = motion.iter().map(|m| m.fastest).fold(0.0, f64::max);
    let high = motion.iter().map(|m| m.highest).fold(0.0, f64::max);
    let tail = &motion[motion.len().saturating_sub(30)..];
    let settled = |f: fn(&Motion) -> f64| median(&tail.iter().map(f).collect::<Vec<_>>());
    println!("{label}: top speed {top:.2} m/s, highest particle {high:.2} m over the run");
    println!("{label}: last 30 frames speed mean {:.3} p99 {:.3} m/s", settled(|m| m.mean), settled(|m| m.p99));
}

/// How high the water throws, in scene metres (tank x, z in −2..2, floor at
/// `floor`, lid 4 m above it): the share of live particles above 2 m (the
/// column's top) and above 3 m, and the share within 10 cm of the lid. Of
/// those above 3 m, the share against each wall region (within 0.5 m of the
/// side walls z = ±2, the far wall x = 2, the column's wall x = −2) and in
/// the middle.
#[derive(Clone, Copy, Default)]
pub(crate) struct Splash {
    pub above_2: f64,
    pub above_3: f64,
    pub at_lid: f64,
    /// Side walls, far wall, column wall, middle.
    pub where_high: [f64; 4],
}

pub(crate) fn splash(positions: impl Iterator<Item = [f32; 4]>, floor: f64) -> Splash {
    let (mut live, mut above_2, mut above_3, mut at_lid) = (0usize, 0usize, 0usize, 0usize);
    let mut regions = [0usize; 4];
    for p in positions {
        live += 1;
        let height = f64::from(p[1]) - floor;
        above_2 += usize::from(height > 2.0);
        at_lid += usize::from(height > 3.9);
        if height > 3.0 {
            above_3 += 1;
            let (x, z) = (f64::from(p[0]), f64::from(p[2]));
            let region = if z.abs() > 1.5 { 0 } else if x > 1.5 { 1 } else if x < -1.5 { 2 } else { 3 };
            regions[region] += 1;
        }
    }
    let share = |k: usize, of: usize| k as f64 / of.max(1) as f64;
    Splash {
        above_2: share(above_2, live),
        above_3: share(above_3, live),
        at_lid: share(at_lid, live),
        where_high: regions.map(|k| share(k, above_3)),
    }
}

/// The water running up the side walls (within 25 cm of z = ±2): the share
/// of live particles above 2 m and above 3 m, the highest, the mean vertical
/// speed of those above 2 m (up positive), and how thick the sheet is above
/// 2.5 m as distance from its wall (mean and 90th percentile). BUG-h8or (lid
/// slabs): whether one solver's sheet runs up faster or thicker.
pub(crate) fn print_side_sheet(label: &str, frame: usize, particles: &[([f32; 4], [f32; 3])], floor: f64) {
    let near: Vec<(f64, f64, f64)> = particles
        .iter()
        .map(|(p, v)| (f64::from(p[1]) - floor, 2.0 - f64::from(p[2]).abs(), f64::from(v[1])))
        .filter(|&(_, gap, _)| gap < 0.25)
        .collect();
    let live = particles.len().max(1) as f64;
    let above = |y: f64| near.iter().filter(|p| p.0 > y).count() as f64 / live;
    let high: Vec<&(f64, f64, f64)> = near.iter().filter(|p| p.0 > 2.0).collect();
    let rising = high.iter().map(|p| p.2).sum::<f64>() / high.len().max(1) as f64;
    let highest = near.iter().map(|p| p.0).fold(0.0, f64::max);
    let mut gaps: Vec<f64> = near.iter().filter(|p| p.0 > 2.5).map(|p| p.1).collect();
    gaps.sort_by(f64::total_cmp);
    let mean_gap = gaps.iter().sum::<f64>() / gaps.len().max(1) as f64;
    let p90 = gaps.get(gaps.len() * 9 / 10).copied().unwrap_or(0.0);
    println!(
        "{label} sheet frame {frame:3}: above 2 m {:.3}%, above 3 m {:.3}%, highest {highest:.2} m, rising {rising:+.2} m/s, thickness above 2.5 m mean {:.1} p90 {:.1} cm",
        100.0 * above(2.0),
        100.0 * above(3.0),
        100.0 * mean_gap,
        100.0 * p90
    );
}

/// The water within 10 cm of the lid: how many particles, their mean
/// vertical speed (m/s, up positive), and how full their cells are on the
/// solver's own grid (particles sharing the cell: 1–2, 3–5, 6–8, 9 or more).
pub(crate) fn print_lid_layer(
    label: &str,
    frame: usize,
    particles: &[([f32; 4], [f32; 3])],
    origin: [f64; 3],
    cells: [usize; 3],
    h: f64,
    floor: f64,
) {
    let index = |p: &[f32; 4]| {
        let c: [usize; 3] = std::array::from_fn(|a| (((f64::from(p[a]) - origin[a]) / h).max(0.0) as usize).min(cells[a] - 1));
        c[0] + cells[0] * (c[1] + cells[1] * c[2])
    };
    let mut per_cell = vec![0u32; cells.iter().product()];
    for (p, _) in particles {
        per_cell[index(p)] += 1;
    }
    let (mut count, mut vy, mut fill) = (0usize, 0.0_f64, [0usize; 4]);
    for (p, v) in particles.iter().filter(|(p, _)| f64::from(p[1]) - floor > 3.9) {
        count += 1;
        vy += f64::from(v[1]);
        let k = per_cell[index(p)];
        fill[match k {
            0..=2 => 0,
            3..=5 => 1,
            6..=8 => 2,
            _ => 3,
        }] += 1;
    }
    println!(
        "{label} lid frame {frame:3}: {count} particles, mean vertical {:+.3} m/s, cells holding 1–2 / 3–5 / 6–8 / 9+: {fill:?}",
        vy / count.max(1) as f64
    );
}

/// The splash over time: the highest particle and the fastest.
pub(crate) fn print_height(label: &str, frame: usize, m: &Motion) {
    println!("{label} height frame {frame:3}: highest {:.2} m, top speed {:.2} m/s", m.highest, m.fastest);
}

pub(crate) fn print_splash(label: &str, frame: usize, s: &Splash) {
    let [side, far, back, middle] = s.where_high.map(|v| 100.0 * v);
    println!(
        "{label} splash frame {frame:3}: above 2 m {:.2}%, above 3 m {:.3}%, at the lid {:.3}%; above 3 m by side walls {side:.0}%, far wall {far:.0}%, column wall {back:.0}%, middle {middle:.0}%",
        100.0 * s.above_2,
        100.0 * s.above_3,
        100.0 * s.at_lid
    );
}

/// The water measure over a run, one [`Packing`] per frame: at frame 0, the
/// worst frame, and the median of the last 30.
pub(crate) fn report_water(label: &str, packed: &[Packing]) {
    let tail = &packed[packed.len().saturating_sub(30)..];
    for (name, f) in [("past rest", (|p: &Packing| p.crowded) as fn(&Packing) -> f64), ("missing inside", |p: &Packing| p.hollow)] {
        let worst = packed.iter().map(f).fold(0.0, f64::max);
        let settled = median(&tail.iter().map(f).collect::<Vec<_>>());
        println!("{label}: particles {name} {:.1}% at frame 0, worst {worst_pct:.1}%, last 30 frames {settled_pct:.1}%", 100.0 * f(&packed[0]), worst_pct = 100.0 * worst, settled_pct = 100.0 * settled);
    }
}

pub(crate) fn median(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

fn worst(v: &[f64]) -> f64 {
    v.iter().copied().fold(0.0_f64, f64::max)
}

fn particle_motion(particles: &[FluidParticle], min: [f64; 3]) -> Motion {
    motion(particles.iter().map(|p| (p.position_radius, p.velocity)), min[1])
}

/// The fraction of cells that are water and the fraction of 8³ blocks
/// holding any.
fn occupancy(water: &[f32], n: usize) -> (f64, f64) {
    let blocks = n.div_ceil(8);
    let mut touched = vec![false; blocks.pow(3)];
    let mut wet = 0usize;
    for (c, &w) in water.iter().enumerate() {
        if w <= 0.5 {
            continue;
        }
        let (i, j, k) = (c % n, (c / n) % n, c / (n * n));
        wet += 1;
        touched[i / 8 + blocks * (j / 8 + blocks * (k / 8))] = true;
    }
    (wet as f64 / water.len() as f64, touched.iter().filter(|&&t| t).count() as f64 / touched.len() as f64)
}

/// How the particles pack: per occupied cell, how many particles (8 is the
/// fill's density); how many sit on a wall (within a hundredth of a cell)
/// or near the lid; the mean height.
fn report_packing(particles: &[FluidParticle], min: [f64; 3], n: usize, h: f64) {
    let mut per_cell = vec![0u32; n * n * n];
    let (mut on_wall, mut high, mut height) = (0usize, 0usize, 0.0_f64);
    let side = n as f64 * h;
    for p in particles.iter().filter(|p| p.position_radius[3] > 0.0) {
        let local: [f64; 3] = std::array::from_fn(|a| f64::from(p.position_radius[a]) - min[a]);
        if local.iter().any(|&x| x < 0.01 * h || x > side - 0.01 * h) {
            on_wall += 1;
        }
        if local[1] > side - 0.5 {
            high += 1;
        }
        height += local[1];
        let c: [usize; 3] = std::array::from_fn(|a| ((local[a] / h) as usize).min(n - 1));
        per_cell[c[0] + n * (c[1] + n * c[2])] += 1;
    }
    let occupied: Vec<u32> = per_cell.into_iter().filter(|&c| c > 0).collect();
    let mut histogram = [0usize; 6];
    for &c in &occupied {
        histogram[match c {
            1..=4 => 0,
            5..=7 => 1,
            8 => 2,
            9..=12 => 3,
            13..=24 => 4,
            _ => 5,
        }] += 1;
    }
    let live = particles.iter().filter(|p| p.position_radius[3] > 0.0).count();
    println!(
        "GPU FLIP packing: {live} particles in {} cells, {:.2} per cell (the fill is 8), mean height {:.3} m",
        occupied.len(),
        live as f64 / occupied.len() as f64,
        height / live as f64
    );
    println!("GPU FLIP packing: cells holding 1–4 / 5–7 / 8 / 9–12 / 13–24 / 25+: {histogram:?}; {on_wall} on a wall, {high} near the lid");
}

fn gpu_flip_packing(particles: &[FluidParticle], min: [f64; 3], n: usize, h: f64) -> Packing {
    let live = particles.iter().filter(|p| p.position_radius[3] > 0.0);
    packing(live.map(|p| p.position_radius), min, [n; 3], h)
}

/// How far the particles sit from the fill's 8 a cell, over the solver's own
/// cells. Both solvers seed 8 a cell, so it reads the same for each, and it
/// needs no mesher: thin sheets lose mesh volume but not this.
#[derive(Clone, Copy, Default)]
pub(crate) struct Packing {
    /// Particles past 8 in their cell, as a share of the live count: the
    /// water compressed (I8, the accuracy guard).
    pub crowded: f64,
    /// Particles missing below 8 in interior cells (the cell and its six
    /// neighbours all hold water), as a share of the live count: the water
    /// spread out, which the mesher reads as volume gained.
    pub hollow: f64,
}

/// [`Packing`] for `positions` binned into `cells` of edge `h` from `origin`.
pub(crate) fn packing(positions: impl Iterator<Item = [f32; 4]>, origin: [f64; 3], cells: [usize; 3], h: f64) -> Packing {
    let mut per_cell = vec![0u32; cells.iter().product()];
    let index = |c: [usize; 3]| c[0] + cells[0] * (c[1] + cells[1] * c[2]);
    let mut live = 0usize;
    for p in positions {
        let c: [usize; 3] = std::array::from_fn(|a| (((f64::from(p[a]) - origin[a]) / h).max(0.0) as usize).min(cells[a] - 1));
        per_cell[index(c)] += 1;
        live += 1;
    }
    let rest = super::gpu_flip_preset::REST_PER_CELL as u32;
    let crowded: usize = per_cell.iter().map(|&c| c.saturating_sub(rest) as usize).sum();
    let mut hollow = 0usize;
    for z in 1..cells[2].saturating_sub(1) {
        for y in 1..cells[1].saturating_sub(1) {
            for x in 1..cells[0].saturating_sub(1) {
                let count = per_cell[index([x, y, z])];
                let near = [[x - 1, y, z], [x + 1, y, z], [x, y - 1, z], [x, y + 1, z], [x, y, z - 1], [x, y, z + 1]];
                if count > 0 && near.iter().all(|&c| per_cell[index(c)] > 0) {
                    hollow += rest.saturating_sub(count) as usize;
                }
            }
        }
    }
    let share = |k: usize| k as f64 / live.max(1) as f64;
    Packing { crowded: share(crowded), hollow: share(hollow) }
}

/// How far the water has broken apart in one frame: the live particles
/// binned into cells of edge `h`, and the occupied cells joined across faces
/// into bodies. A sheet that holds together stays one body; one that tears
/// leaves drops.
#[derive(Clone, Copy, Default)]
pub(crate) struct Breakup {
    /// The share of live particles outside the largest body.
    pub detached: f64,
    /// How many bodies.
    pub pieces: usize,
}

/// [`Breakup`] for `positions` binned into `cells` of edge `h` from `origin`.
pub(crate) fn breakup(positions: impl Iterator<Item = [f32; 4]>, origin: [f64; 3], cells: [usize; 3], h: f64) -> Breakup {
    let mut per_cell = vec![0u32; cells.iter().product()];
    let index = |c: [usize; 3]| c[0] + cells[0] * (c[1] + cells[1] * c[2]);
    let mut live = 0usize;
    for p in positions {
        let c: [usize; 3] = std::array::from_fn(|a| (((f64::from(p[a]) - origin[a]) / h).max(0.0) as usize).min(cells[a] - 1));
        per_cell[index(c)] += 1;
        live += 1;
    }
    let mut seen = vec![false; per_cell.len()];
    let (mut pieces, mut largest, mut stack) = (0usize, 0usize, Vec::new());
    for start in 0..per_cell.len() {
        if per_cell[start] == 0 || seen[start] {
            continue;
        }
        pieces += 1;
        let mut held = 0usize;
        seen[start] = true;
        stack.push(start);
        while let Some(c) = stack.pop() {
            held += per_cell[c] as usize;
            let p = [c % cells[0], (c / cells[0]) % cells[1], c / (cells[0] * cells[1])];
            for a in 0..3 {
                for side in [-1i64, 1] {
                    let q = p[a] as i64 + side;
                    if q < 0 || q >= cells[a] as i64 {
                        continue;
                    }
                    let mut r = p;
                    r[a] = q as usize;
                    let next = index(r);
                    if per_cell[next] > 0 && !seen[next] {
                        seen[next] = true;
                        stack.push(next);
                    }
                }
            }
        }
        largest = largest.max(held);
    }
    Breakup { detached: (live - largest) as f64 / live.max(1) as f64, pieces }
}

/// Sheet breakup over a run: the peak detached share and piece count with
/// their frames, and their means over the splash, frames 30–150.
pub(crate) fn report_breakup(label: &str, frames: &[Breakup]) {
    let peak = |f: fn(&Breakup) -> f64| {
        frames.iter().enumerate().map(|(i, b)| (f(b), i)).fold((0.0, 0), |m, v| if v.0 > m.0 { v } else { m })
    };
    let splash = &frames[30.min(frames.len())..151.min(frames.len())];
    let mean = |f: fn(&Breakup) -> f64| splash.iter().map(f).sum::<f64>() / splash.len().max(1) as f64;
    let (detached, at) = peak(|b| b.detached);
    let (pieces, pieces_at) = peak(|b| b.pieces as f64);
    println!(
        "{label}: breakup detached peak {:.2}% at frame {at}, mean {:.2}% over frames 30–150; pieces peak {pieces:.0} at frame {pieces_at}, mean {:.0}",
        100.0 * detached,
        100.0 * mean(|b| b.detached),
        mean(|b| b.pieces as f64)
    );
}

/// What one Dam Break run measured.
struct Record {
    gpu: Vec<f64>,
    cpu: Vec<f64>,
    /// Water volume drift per frame, skin-corrected (meshed runs only).
    volume: Vec<f64>,
    /// Per frame, how the particles move.
    motion: Vec<Motion>,
    /// Per frame, the [`Feel`] measures.
    feel: Vec<Feel>,
}

/// How the Dam Break wave meets the walls in one frame, as the parity audit
/// measured both solvers (floor at `floor`, the far wall at x = 2, the lid
/// 4 m up): the highest particle within 25 cm of the far wall (the run-up),
/// and how many particles sit within 10 cm of the lid.
#[derive(Clone, Copy, Default)]
pub(crate) struct Feel {
    pub runup: f64,
    pub at_lid: usize,
    /// The farthest x of a particle more than 10 cm above the 0.16 m pool:
    /// the collapsing column's front.
    pub front: f64,
    /// The share of live particles faster than the step's CFL guard.
    pub past_guard: f64,
}

/// [`Feel`] over (position and radius, velocity) pairs; `guard` is the
/// speed the CFL guard allows (m/s).
pub(crate) fn feel(particles: impl Iterator<Item = ([f32; 4], [f32; 3])>, floor: f64, guard: f64) -> Feel {
    let mut out = Feel { front: f64::NEG_INFINITY, ..Feel::default() };
    let (mut live, mut fast) = (0usize, 0usize);
    for (p, v) in particles.filter(|(p, _)| p[3] > 0.0) {
        let height = f64::from(p[1]) - floor;
        if p[0] > 1.75 {
            out.runup = out.runup.max(height);
        }
        out.at_lid += usize::from(height > 3.9);
        if height > 0.26 {
            out.front = out.front.max(f64::from(p[0]));
        }
        live += 1;
        fast += usize::from(v.iter().map(|&c| f64::from(c).powi(2)).sum::<f64>().sqrt() > guard);
    }
    out.past_guard = fast as f64 / live.max(1) as f64;
    out
}

/// Run-up at frames 59, 74 and 89, lid contact (particle-frames at the lid
/// over frames 50–130) and the last frame any particle is at the lid.
pub(crate) fn report_feel(label: &str, feel: &[Feel]) {
    let runup: Vec<String> = [59, 74, 89].iter().filter_map(|&f| feel.get(f)).map(|f| format!("{:.2}", f.runup)).collect();
    let contact: usize = feel.iter().take(131).skip(50).map(|f| f.at_lid).sum();
    let last = feel.iter().rposition(|f| f.at_lid > 0);
    println!("{label}: run-up at frames 59/74/89 {} m, lid contact {contact}, last frame at the lid {last:?}", runup.join(" / "));
    let front: Vec<String> = [6, 12, 18, 24].iter().filter_map(|&f| feel.get(f)).map(|f| format!("{:.2}", f.front)).collect();
    if let (Some(a), Some(b)) = (feel.get(6), feel.get(24)) {
        println!("{label}: front x at frames 6/12/18/24 {} m, {:.2} m/s over frames 6–24", front.join(" / "), (b.front - a.front) * 60.0 / 18.0);
    }
}

/// The Dam Break for `frames` frames: per-frame GPU and CPU encode ms, what
/// the projection left undone, occupancy, packing and particle motion; meshed, the
/// water volume the surface holds, and stills at frames 90 and 240. `label`
/// names the run in its lines, kept short because the tool output around
/// these probes cuts long ones. It asserts only what must hold for the
/// numbers to mean anything: no GPU fault, every particle alive and finite.
fn dam_break(scene: WaterScene, label: &str, frames: usize) -> Record {
    let mut run = Run::new(scene);
    let (n, h, min) = (run.n(), scene.cell_size(), scene.min());
    let mut record = Record { gpu: Vec::new(), cpu: Vec::new(), volume: Vec::new(), motion: Vec::new(), feel: Vec::new() };
    let (mut rms, mut max) = (Vec::new(), Vec::new());
    let (mut blocks_max, mut water_max) = (0.0_f64, 0.0_f64);
    let (mut raw, mut oracle, mut packed, mut broken) = (Vec::new(), None, Vec::new(), Vec::new());
    let mut per_cell: Vec<f64> = Vec::new();
    for frame in 0..frames {
        let (g, c) = run.frame();
        record.gpu.push(g);
        record.cpu.push(c);
        // A later substep's water mask stays inside the step node.
        if scene.steps == 1 {
            let water = run.water();
            let (r, m) = divergence(&run.faces(), &water, n, h);
            rms.push(r);
            max.push(m);
            let (fraction, blocks) = occupancy(&water, n);
            water_max = water_max.max(fraction);
            blocks_max = blocks_max.max(blocks);
        }
        if scene.surface {
            let measure = run.surface_measure();
            raw.push(measure.0);
            let oracle = oracle.get_or_insert_with(|| VolumeDrift::new(measure, run.particle_volume()));
            record.volume.push(oracle.drift(measure));
            if frame == 90 || frame == 240 {
                let name: String = label.chars().filter(|c| !c.is_whitespace()).map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
                write_still(&format!("{name}_frame{frame}"), run.surface().into_iter());
            }
        }
        let particles = run.particles();
        // The mesher-free volume: live particles over the φ < 0 cells.
        let live_count = particles.iter().filter(|p| p.position_radius[3] > 0.0).count();
        let phi_cells = run.water_of(&particles).iter().filter(|&&w| w > 0.5).count();
        per_cell.push(live_count as f64 / phi_cells.max(1) as f64);
        record.motion.push(particle_motion(&particles, min));
        let guard = scene.travel_cells() as f64 * h / scene.step_dt();
        record.feel.push(feel(particles.iter().map(|p| (p.position_radius, p.velocity)), min[1], guard));
        let pack = gpu_flip_packing(&particles, min, n, h);
        packed.push(pack);
        let live = particles.iter().filter(|p| p.position_radius[3] > 0.0).map(|p| p.position_radius);
        broken.push(breakup(live, min, [n; 3], h));
        let sheet = frame % 5 == 4 && frame < 90;
        if frame % 15 == 14 || sheet {
            let live: Vec<_> = particles.iter().filter(|p| p.position_radius[3] > 0.0).map(|p| (p.position_radius, p.velocity)).collect();
            let floor = min[1];
            if sheet {
                print_side_sheet(label, frame, &live, floor);
                print_height(label, frame, &record.motion[frame]);
            }
            if frame % 15 == 14 {
                print_splash(label, frame, &splash(live.iter().map(|p| p.0), floor));
                print_lid_layer(label, frame, &live, min, [n; 3], h, floor);
            }
        }
        if frame % 30 == 29 {
            let stats = particle_stats(&particles);
            assert_eq!((stats.live, stats.bad), (scene.particles() as usize, 0), "frame {frame}: particles lost or not finite");
            let m = record.motion[frame];
            println!("{label} frame {frame:3}: {g:.1} ms GPU, {c:.1} ms CPU, left rms {:.1e} max {:.1e} /s", rms.last().unwrap(), max.last().unwrap());
            println!("{label} frame {frame:3}: speed mean {:.2} p99 {:.2} top {:.2} m/s, highest {:.2} m", m.mean, m.p99, m.fastest, m.highest);
            if let Some(v) = record.volume.last() {
                println!("{label} frame {frame:3}: water volume {:+.2}%, raw mesh {:+.2}%", 100.0 * v, 100.0 * (raw[frame] / raw[0] - 1.0));
            }
            println!("{label} frame {frame:3}: {} particles, {:.3} a φ<0 cell ({:+.2}%)", stats.live, per_cell[frame], 100.0 * (per_cell[frame] / per_cell[0] - 1.0));
            println!("{label} frame {frame:3}: particles past rest {:.1}%, missing inside {:.1}%", 100.0 * pack.crowded, 100.0 * pack.hollow);
        }
    }
    report_packing(&run.particles(), min, n, h);
    report_water(label, &packed);
    report_breakup(label, &broken);
    println!("{label}: GPU {:.2} ms median, CPU encode {:.2} ms median", median(&record.gpu), median(&record.cpu));
    println!("{label}: left undone rms median {:.2e} worst {:.2e}; max median {:.2e} worst {:.2e} /s", median(&rms), worst(&rms), median(&max), worst(&max));
    println!("{label}: water at most {:.1}% of cells, {:.1}% of 8³ blocks", 100.0 * water_max, 100.0 * blocks_max);
    report_motion(label, &record.motion);
    report_feel(label, &record.feel);
    let spread: Vec<f64> = per_cell.iter().map(|c| (c / per_cell[0] - 1.0).abs()).collect();
    println!(
        "{label}: particles per φ<0 cell {:.3} at frame 0, {:.3} last, drift max {:.2}%, at the last frame {:+.2}%",
        per_cell[0],
        per_cell[frames - 1],
        100.0 * worst(&spread),
        100.0 * (per_cell[frames - 1] / per_cell[0] - 1.0)
    );
    let top = record.motion.iter().map(|m| m.fastest).fold(0.0, f64::max);
    println!("{label}: the top speed crosses {:.2} cells a step, {} steps a frame", top * scene.step_dt() / h, scene.steps);
    let guarded = record.feel.iter().map(|f| f.past_guard).fold(0.0, f64::max);
    println!(
        "{label}: CFL guard {} cells a step ({:.1} m/s), new extended {} layers; at most {:.3}% of particles faster in a frame",
        scene.travel_cells(),
        scene.travel_cells() as f64 * h / scene.step_dt(),
        scene.band_layers(),
        100.0 * guarded
    );
    if let Some(oracle) = oracle {
        let drift: Vec<f64> = record.volume.iter().map(|v| v.abs()).collect();
        println!("{label}: particles hold {:.4} m³, mesh {:.4} m³ at frame 0, skin {:.2} mm", run.particle_volume(), raw[0], 1000.0 * oracle.skin());
        println!("{label}: water volume drift max {:.2}%, at the last frame {:+.2}%", 100.0 * worst(&drift), 100.0 * record.volume[frames - 1]);
    }
    record
}

/// The race rows at a lattice: the step alone, then meshed; the difference
/// is the surface. The spread rate is the scene's default.
fn cost_probe(n: usize) {
    dam_break(WaterScene::race_dam_break(n), &format!("GPU FLIP step {n}³"), 300);
    dam_break(WaterScene::race_dam_break(n).with_surface(), &format!("GPU FLIP meshed {n}³"), 300);
}

#[test]
fn gpu_flip_cost_probe() {
    cost_probe(64);
}

#[test]
fn gpu_flip_cost_probe_refined() {
    cost_probe(128);
}

/// The 128³ splash against the solve's iteration count: if the fastest
/// particle and the highest splash fall as iterations rise, an
/// under-converged solve is feeding the splash energy; if not, the splash is
/// the scene's.
#[test]
fn gpu_flip_refined_splash_iterations() {
    for iterations in [4, 8, 12] {
        let scene = WaterScene::race_dam_break(128).with_surface().with_iterations(iterations);
        dam_break(scene, &format!("ITERATIONS {iterations} 128³"), 150);
    }
}

/// The 128³ splash as shipped, against the engine's at the same frames
/// (`gpu_flip_engine_race_refined`): p99 and top speed, peak height.
#[test]
fn gpu_flip_refined_splash() {
    dam_break(WaterScene::race_dam_break(128).with_surface(), "SPLASH 128³", 150);
}

/// The 128³ splash against the step length: four steps a frame, so a fast
/// particle crosses half as many cells per step as the two-layer face
/// extension covers.
#[test]
fn gpu_flip_refined_splash_causes() {
    let refined = WaterScene::race_dam_break(128).with_surface();
    dam_break(WaterScene { steps: 4, ..refined }, "SPLASH 4 steps 128³", 120);
}

/// The walls against the step count: the meshed 64³ Dam Break at two water
/// steps a frame and at one, with the run-up, lid contact and volume drift
/// lines, and each run's volume per frame in /tmp/flip_parity. The engine's
/// side is the parity audit's `/tmp/flip_feel/engine.csv`.
#[test]
fn gpu_flip_wall_feel_64() {
    std::fs::create_dir_all("/tmp/flip_parity").expect("out dir");
    for steps in [2, 1] {
        let label = format!("WALLS {steps} steps 64³");
        let record = dam_break(WaterScene::race_dam_break(64).with_steps(steps).with_surface(), &label, 300);
        let rows: Vec<String> = record.volume.iter().enumerate().map(|(f, v)| format!("{f},{v:.5}")).collect();
        std::fs::write(format!("/tmp/flip_parity/gpu_volume_steps_{steps}.csv"), format!("frame,volume_drift\n{}\n", rows.join("\n"))).expect("csv");
    }
}

/// The free surface on the meshed Dam Break, air at zero pressure on its
/// cell centres against the ghost-fluid surface, at `n`: sheet breakup,
/// volume and the race rows. The engine's breakup is `gpu_flip_engine_splash_64`
/// and `gpu_flip_engine_race_refined`.
fn ghost_fluid_race(n: usize) {
    // The ghost rows stiffen the surface cells the plain coarse levels never
    // see, so the ghost runs also show how far the shipped count leaves them.
    for (ghost, iterations) in [(false, PRESSURE_ITERATIONS), (true, PRESSURE_ITERATIONS), (true, 16)] {
        let scene = WaterScene { ghost_fluid: ghost, ..WaterScene::race_dam_break(n).with_surface() }.with_iterations(iterations);
        let label = format!("SURFACE {} {iterations} it {n}³", if ghost { "ghost" } else { "plain" });
        dam_break(scene, &label, 300);
    }
}

#[test]
fn gpu_flip_ghost_fluid_64() {
    ghost_fluid_race(64);
}

#[test]
fn gpu_flip_ghost_fluid_refined() {
    ghost_fluid_race(128);
}

/// The particle-to-face kernel on the meshed Dam Break as shipped: sheet
/// breakup and the race rows, read against the ghost rows of
/// `gpu_flip_ghost_fluid_64` and `_refined` and the engine's.
#[test]
fn gpu_flip_transfer_kernel_64() {
    dam_break(WaterScene::race_dam_break(64).with_surface(), "TRANSFER 64³", 300);
}

#[test]
fn gpu_flip_transfer_kernel_refined() {
    dam_break(WaterScene::race_dam_break(128).with_surface(), "TRANSFER 128³", 300);
}

/// 15 s of the meshed Dam Break at 64³: how still the pool is by the end.
#[test]
fn gpu_flip_dam_break_settles() {
    dam_break(WaterScene::race_dam_break(64).with_surface(), "SETTLE 64³", 900);
}
