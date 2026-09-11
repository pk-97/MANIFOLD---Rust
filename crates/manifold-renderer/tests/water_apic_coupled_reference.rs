//! End-to-end f64 APIC liquid reference.
//!
//! This deliberately stays a small CPU oracle.  The transfer and fractional
//! projection implementations are the adjacent reference modules; this file
//! only couples their stages and records physical metrics for review.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

#[path = "water_linear_apic_reference.rs"]
mod apic;
#[path = "water_fractional_projection_reference.rs"]
mod projection;

const GRAVITY: f64 = -9.81;
const EXTRAPOLATION_LAYERS: usize = 5;
const REST_SECONDS: usize = 60;
const WAVE_AMPLITUDE: f64 = 0.01;
const WAVE_PERIOD_TOLERANCE: f64 = 0.10;
const WAVE_AMPLITUDE_RETENTION: f64 = 0.50;
const DT_HALF_MAX_INITIAL_RMS: f64 = 0.10;
const OFFLINE_H: f64 = 0.03125;
const OFFLINE_DT: f64 = 1. / 120.;
const WATER_PARTICLE_RECORD_BYTES: usize = 96;
const WATER_PARTICLE_DENSITY: f64 = 1000.;

#[derive(Clone, Copy, Debug)]
struct Config {
    n: [usize; 3],
    h: f64,
    origin: [f64; 3],
    basin_min: [f64; 3],
    basin_max: [f64; 3],
    water_depth: f64,
    particles_per_cell: usize,
    density: f64,
}

const BASIN_MIN: [f64; 3] = [0.125; 3];
const BASIN_MAX: [f64; 3] = [1.375, 1.375, 0.875];
const WORLD_MAX: [f64; 3] = [1.5, 1.5, 1.0];

fn config_for_h(h: f64) -> Config {
    Config {
        n: std::array::from_fn(|a| (WORLD_MAX[a] / h).round() as usize),
        h,
        origin: [0.; 3],
        basin_min: BASIN_MIN,
        basin_max: BASIN_MAX,
        water_depth: 0.375,
        particles_per_cell: 8,
        density: 1000.,
    }
}

fn default_config() -> Config {
    config_for_h(0.125)
}

/// Write a deterministic cache of the coupled reference simulation for the
/// renderer's `WaterParticle` playback path. The output directory must not
/// already exist so a failed or stale export cannot be mistaken for a fresh
/// cache.
pub(crate) fn export_offline_cache(
    output_dir: &Path,
    frame_count: usize,
    fps: f64,
) -> Result<(), String> {
    if frame_count == 0 {
        return Err("frame count must be at least one".to_string());
    }
    if !fps.is_finite() || fps <= 0. {
        return Err("fps must be finite and greater than zero".to_string());
    }
    let steps_f = 1. / (fps * OFFLINE_DT);
    let steps = steps_f.round();
    if !steps.is_finite() || steps < 1. || (steps_f - steps).abs() > 1e-9 {
        return Err(format!(
            "fps={fps} does not map to an integral number of {OFFLINE_DT}s simulation steps"
        ));
    }
    let steps_per_frame = usize::try_from(steps as u128)
        .map_err(|_| "steps per frame do not fit in usize".to_string())?;
    let config = config_for_h(OFFLINE_H);
    let mut sim = Simulation::with_config(config, true);
    validate_export_state(&sim)?;
    if output_dir.exists() {
        return Err(format!(
            "refusing to overwrite existing cache {}",
            output_dir.display()
        ));
    }
    fs::create_dir(output_dir)
        .map_err(|error| format!("create cache directory {}: {error}", output_dir.display()))?;

    let mut previous_positions: Vec<[f64; 3]> = sim.particles.iter().map(|p| p.p).collect();
    let initial = serialize_frame(&sim, &previous_positions)?;
    write_frame(output_dir, 0, &initial)?;
    let mut residual = 0.;
    print_export_progress(0, &sim, residual, frame_count);
    let mut next_progress_time = 1.;

    for frame in 1..frame_count {
        previous_positions.clear();
        previous_positions.extend(sim.particles.iter().map(|p| p.p));
        for _ in 0..steps_per_frame {
            residual = sim
                .step(OFFLINE_DT)
                .map_err(|error| format!("simulation fault at frame {frame}: {error}"))?;
            validate_export_state(&sim)?;
        }
        let bytes = serialize_frame(&sim, &previous_positions)?;
        write_frame(output_dir, frame, &bytes)?;
        if sim.time + OFFLINE_DT * 0.5 >= next_progress_time {
            print_export_progress(frame, &sim, residual, frame_count);
            next_progress_time += 1.;
        }
    }

    let manifest = format!(
        "{{\n  \"version\": 1,\n  \"layout\": \"WaterParticle96LE\",\n  \"count\": {},\n  \"fps\": {},\n  \"h\": {},\n  \"dt\": {},\n  \"basinBounds\": {{\"min\": {}, \"max\": {}}}\n}}\n",
        sim.particles.len(),
        fps,
        OFFLINE_H,
        OFFLINE_DT,
        format_vec3(config.basin_min),
        format_vec3(config.basin_max),
    );
    fs::write(output_dir.join("manifest.json"), manifest)
        .map_err(|error| format!("write cache manifest: {error}"))?;
    Ok(())
}

fn validate_export_state(sim: &Simulation) -> Result<(), String> {
    if !sim.time.is_finite() {
        return Err("invalid state: simulation time is non-finite".to_string());
    }
    if sim.particles.is_empty() || sim.particles.len() != sim.initial_particles {
        return Err(format!(
            "invalid state: particle count changed ({} -> {})",
            sim.initial_particles,
            sim.particles.len()
        ));
    }
    for (index, particle) in sim.particles.iter().enumerate() {
        if !particle.m.is_finite()
            || particle.m <= 0.
            || particle
                .p
                .iter()
                .chain(particle.v.iter())
                .chain(particle.c.iter().flatten())
                .any(|value| !value.is_finite())
        {
            return Err(format!("invalid state: non-finite particle {index}"));
        }
        if particle.p.iter().enumerate().any(|(axis, &value)| {
            value < sim.config.basin_min[axis] || value > sim.config.basin_max[axis]
        }) {
            return Err(format!(
                "invalid state: particle {index} is outside the basin"
            ));
        }
    }
    Ok(())
}

fn serialize_frame(sim: &Simulation, previous_positions: &[[f64; 3]]) -> Result<Vec<u8>, String> {
    if previous_positions.len() != sim.particles.len() {
        return Err("invalid state: previous-position count does not match particles".to_string());
    }
    let byte_len = sim
        .particles
        .len()
        .checked_mul(WATER_PARTICLE_RECORD_BYTES)
        .ok_or_else(|| "frame is too large to serialize".to_string())?;
    let mut bytes = Vec::with_capacity(byte_len);
    for (particle, previous) in sim.particles.iter().zip(previous_positions) {
        for &value in &particle.p {
            push_f32(&mut bytes, value)?;
        }
        push_f32(&mut bytes, particle.m)?;
        for &value in &particle.v {
            push_f32(&mut bytes, value)?;
        }
        push_f32(&mut bytes, WATER_PARTICLE_DENSITY)?;
        for row in &particle.c {
            for &value in row {
                push_f32(&mut bytes, value)?;
            }
            push_f32(&mut bytes, 0.)?;
        }
        for &value in previous {
            push_f32(&mut bytes, value)?;
        }
        push_f32(&mut bytes, 0.)?;
    }
    debug_assert_eq!(bytes.len(), byte_len);
    Ok(bytes)
}

fn push_f32(bytes: &mut Vec<u8>, value: f64) -> Result<(), String> {
    let value = value as f32;
    if !value.is_finite() {
        return Err("invalid state: value cannot be represented as finite f32".to_string());
    }
    bytes.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_frame(output_dir: &Path, frame: usize, bytes: &[u8]) -> Result<(), String> {
    let path = output_dir.join(format!("frame_{frame:06}.bin"));
    let mut file =
        File::create(&path).map_err(|error| format!("create {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(())
}

fn format_vec3(values: [f64; 3]) -> String {
    format!("[{},{},{}]", values[0], values[1], values[2])
}

fn print_export_progress(frame: usize, sim: &Simulation, residual: f64, frame_count: usize) {
    let metric = sim.metrics(residual);
    println!(
        "offline-water frame={frame}/{last} t={:.3}s rms_speed={:.6e} max_speed={:.6e} mean_y={:.6e} projection_residual={:.6e}",
        metric.time,
        metric.rms_speed,
        metric.max_speed,
        metric.mean_y,
        metric.projection_residual,
        last = frame_count - 1,
    );
}

impl Config {
    fn cell_position(self, c: [usize; 3]) -> [f64; 3] {
        std::array::from_fn(|a| self.origin[a] + self.h * (c[a] as f64 + 0.5))
    }

    fn basin_solid_cell(self, c: [usize; 3]) -> bool {
        let p = self.cell_position(c);
        p.iter()
            .enumerate()
            .any(|(a, &x)| x < self.basin_min[a] || x > self.basin_max[a])
    }

    fn grid_index(self, axis: usize, x: f64) -> usize {
        ((x - self.origin[axis]) / self.h).round() as usize
    }

    fn particle_mass(self) -> f64 {
        self.density * self.h.powi(3) / self.particles_per_cell as f64
    }
}

#[derive(Clone, Copy, Debug)]
struct Metrics {
    time: f64,
    rms_speed: f64,
    max_speed: f64,
    mean_y: f64,
    particle_volume: f64,
    occupied_cell_volume_proxy: f64,
    projection_residual: f64,
    wave_amplitude: f64,
}

struct Simulation {
    config: Config,
    particles: Vec<apic::Particle>,
    grid: apic::Grid,
    projection_grid: projection::Grid,
    reference_x: Vec<f64>,
    reference_y: Vec<f64>,
    top_layer: Vec<bool>,
    wave_number: f64,
    initial_particles: usize,
    time: f64,
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    dot(
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]],
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]],
    )
    .sqrt()
}

fn particles(config: Config, wave: bool) -> Vec<apic::Particle> {
    let x_start = config.grid_index(0, config.basin_min[0]);
    let x_end = config.grid_index(0, config.basin_max[0]);
    let y_start = config.grid_index(1, config.basin_min[1]);
    let y_end = config.grid_index(1, config.basin_min[1] + config.water_depth);
    let z_start = config.grid_index(2, config.basin_min[2]);
    let z_end = config.grid_index(2, config.basin_max[2]);
    let mut result = Vec::with_capacity(
        (x_end - x_start) * (y_end - y_start) * (z_end - z_start) * config.particles_per_cell,
    );
    let offsets = [0.25, 0.75];
    let width = config.basin_max[0] - config.basin_min[0];
    let wave_number = std::f64::consts::PI / width;
    let amplitude = WAVE_AMPLITUDE;
    let depth = config.water_depth;
    for k in z_start..z_end {
        for j in y_start..y_end {
            for i in x_start..x_end {
                for &oz in &offsets {
                    for &oy in &offsets {
                        for &ox in &offsets {
                            let mut p = [
                                config.origin[0] + config.h * (i as f64 + ox),
                                config.origin[1] + config.h * (j as f64 + oy),
                                config.origin[2] + config.h * (k as f64 + oz),
                            ];
                            let mode = (wave_number * (p[0] - config.basin_min[0])).sin();
                            let cosine_mode = (wave_number * (p[0] - config.basin_min[0])).cos();
                            let v = [0.; 3];
                            if wave {
                                let y = p[1] - config.basin_min[1];
                                let denominator = (wave_number * depth).sinh();
                                // Divergence-free linear gravity-wave mode:
                                // div(xi)=0, xi_y=0 at the basin floor, and
                                // the side-wall normal displacement vanishes.
                                p[0] -= amplitude * mode * (wave_number * y).cosh() / denominator;
                                p[1] += amplitude * cosine_mode * (wave_number * y).sinh()
                                    / denominator;
                            }
                            result.push(apic::Particle {
                                p,
                                v,
                                c: [[0.; 3]; 3],
                                m: config.particle_mass(),
                            });
                        }
                    }
                }
            }
        }
    }
    result
}

impl Simulation {
    fn new(wave: bool) -> Self {
        Self::with_config(default_config(), wave)
    }

    fn with_config(config: Config, wave: bool) -> Self {
        let mut projection_grid = projection::Grid::new(config.n, config.h);
        let dims = projection_grid.n;
        for a in 0..3 {
            let d = projection_grid.dims(a);
            for z in 0..d[2] {
                for y in 0..d[1] {
                    for x in 0..d[0] {
                        let c = [x, y, z];
                        let face_index = projection_grid.face(a, c);
                        let mut closed = c[a] == 0 || c[a] == dims[a];
                        let lo = if c[a] > 0 {
                            let mut q = c;
                            q[a] -= 1;
                            Some(q)
                        } else {
                            None
                        };
                        let hi = if c[a] < dims[a] { Some(c) } else { None };
                        closed |= lo.is_some_and(|q| config.basin_solid_cell(q));
                        closed |= hi.is_some_and(|q| config.basin_solid_cell(q));
                        let face_position = config.origin[a] + config.h * c[a] as f64;
                        closed |= face_position <= config.basin_min[a]
                            || face_position >= config.basin_max[a];
                        if closed {
                            projection_grid.open[a][face_index] = 0.;
                        }
                    }
                }
            }
        }
        let reference = particles(config, false);
        let reference_x = reference.iter().map(|p| p.p[0]).collect();
        let reference_y = reference.iter().map(|p| p.p[1]).collect();
        let top_layer = reference
            .iter()
            .map(|p| p.p[1] >= config.basin_min[1] + config.water_depth - config.h)
            .collect();
        let particles = particles(config, wave);
        let initial_particles = particles.len();
        let wave_number = std::f64::consts::PI / (config.basin_max[0] - config.basin_min[0]);
        Self {
            config,
            grid: apic::Grid::new(config.n, config.h, config.origin),
            projection_grid,
            reference_x,
            reference_y,
            top_layer,
            wave_number,
            particles,
            initial_particles,
            time: 0.,
        }
    }

    fn update_liquid_sdf(&mut self) {
        let h = self.config.h;
        let radius = 3.0_f64.sqrt() * 0.5 * h;
        let phi_cap = 3.0 * h;
        let epsilon = 0.005 * h;
        self.projection_grid.phi.fill(phi_cap);
        // A particle farther than the cap plus its sphere radius cannot alter
        // phi. Updating only this bounded neighborhood preserves the same
        // capped sphere-union proxy while making the refined run tractable.
        let cutoff = phi_cap + radius;
        for particle in &self.particles {
            let ranges: [(usize, usize); 3] = std::array::from_fn(|a| {
                let lo = ((particle.p[a] - cutoff - self.config.origin[a]) / h - 0.5).ceil();
                let hi = ((particle.p[a] + cutoff - self.config.origin[a]) / h - 0.5).floor();
                let lo = (lo as isize).max(0) as usize;
                let hi = (hi as isize).min(self.config.n[a] as isize - 1).max(0) as usize;
                (lo, hi)
            });
            for z in ranges[2].0..=ranges[2].1 {
                for y in ranges[1].0..=ranges[1].1 {
                    for x in ranges[0].0..=ranges[0].1 {
                        let c = [x, y, z];
                        let i = self.projection_grid.cell(c);
                        let p = self.config.cell_position(c);
                        self.projection_grid.phi[i] =
                            self.projection_grid.phi[i].min(distance(p, particle.p) - radius);
                    }
                }
            }
        }
        for i in 0..self.projection_grid.phi.len() {
            let c = self.projection_grid.coords(i);
            let mut phi = self.projection_grid.phi[i];
            // Match ParticleLevelSet::postProcessSignedDistanceField: extend
            // near-surface liquid into solid cells before conditioning phi.
            if phi < 0.5 * h && self.config.basin_solid_cell(c) {
                phi = -0.5 * h;
            }
            if phi.abs() < epsilon {
                phi = if phi > 0. { epsilon } else { -epsilon };
            }
            self.projection_grid.phi[i] = phi;
        }
    }

    // Free-slip sampling ghosts for this grid-aligned static box. The
    // pressure solve still uses zero normal wall flux. Reflecting tangential
    // components evenly and normal components oddly supplies that boundary
    // condition to the particle interpolation stencil. Setting every sample
    // inside a solid to zero would impose tangential drag near the wall.
    fn extend_basin_sampling_velocities(&mut self) {
        for a in 0..3 {
            let d = self.grid.dims(a);
            let old = self.grid.velocity[a].clone();
            for z in 0..d[2] {
                for y in 0..d[1] {
                    for x in 0..d[0] {
                        let c = [x, y, z];
                        let mut reflected = c;
                        let mut sign = 1.;
                        for k in 0..3 {
                            let offset = if a == k { 0. } else { 0.5 };
                            let position = c[k] as f64 + offset;
                            let lo =
                                (self.config.basin_min[k] - self.config.origin[k]) / self.config.h;
                            let hi =
                                (self.config.basin_max[k] - self.config.origin[k]) / self.config.h;
                            if position < lo {
                                reflected[k] = (2. * lo - position - offset).round() as usize;
                                if a == k {
                                    sign = -sign;
                                }
                            } else if position > hi {
                                reflected[k] = (2. * hi - position - offset).round() as usize;
                                if a == k {
                                    sign = -sign;
                                }
                            }
                        }
                        let i = self.grid.index(a, c);
                        let j = self.grid.index(a, reflected);
                        self.grid.velocity[a][i] = sign * old[j];
                    }
                }
            }
        }
    }

    fn step(&mut self, dt: f64) -> Result<f64, &'static str> {
        self.grid.transfer(&self.particles)?;
        let mut valid = std::array::from_fn(|a| {
            self.grid.weight[a]
                .iter()
                .map(|&m| m > 0.)
                .collect::<Vec<_>>()
        });
        self.grid.extrapolate(&mut valid, EXTRAPOLATION_LAYERS);
        let mut velocity = std::array::from_fn(|a| self.grid.velocity[a].clone());
        for va in &mut velocity[1..2] {
            for u in va {
                *u += GRAVITY * dt;
            }
        }
        self.update_liquid_sdf();
        let projection = self.projection_grid.project(&mut velocity, 1e-11, 512)?;
        let mut pressure_valid = std::array::from_fn(|a| vec![false; velocity[a].len()]);
        for a in 0..3 {
            let d = self.projection_grid.dims(a);
            for z in 0..d[2] {
                for y in 0..d[1] {
                    for x in 0..d[0] {
                        let c = [x, y, z];
                        let f = self.projection_grid.face(a, c);
                        if self.projection_grid.open[a][f] == 0. {
                            velocity[a][f] = self.projection_grid.solid[a][f];
                            continue;
                        }
                        let lo = if c[a] > 0 {
                            let mut q = c;
                            q[a] -= 1;
                            Some(self.projection_grid.cell(q))
                        } else {
                            None
                        };
                        let hi = if c[a] < self.projection_grid.n[a] {
                            Some(self.projection_grid.cell(c))
                        } else {
                            None
                        };
                        pressure_valid[a][f] = lo.is_some_and(|i| self.projection_grid.phi[i] < 0.)
                            || hi.is_some_and(|i| self.projection_grid.phi[i] < 0.);
                    }
                }
            }
        }
        self.grid.velocity = velocity;
        self.grid
            .extrapolate(&mut pressure_valid, EXTRAPOLATION_LAYERS);
        for a in 0..3 {
            let d = self.projection_grid.dims(a);
            for z in 0..d[2] {
                for y in 0..d[1] {
                    for x in 0..d[0] {
                        let c = [x, y, z];
                        let f = self.projection_grid.face(a, c);
                        if self.projection_grid.open[a][f] == 0. {
                            self.grid.velocity[a][f] = self.projection_grid.solid[a][f];
                        }
                    }
                }
            }
        }
        self.extend_basin_sampling_velocities();
        let old_positions: Vec<_> = self.particles.iter().map(|p| p.p).collect();
        for (particle, old) in self.particles.iter_mut().zip(old_positions) {
            let (v, c) = self.grid.gather(particle.p)?;
            particle.v = v;
            particle.c = c;
            let proposed = self.grid.advect(particle.p, dt)?;
            particle.p = collide_segment(self.config, old, proposed);
        }
        self.time += dt;
        Ok(projection.residual)
    }

    fn metrics(&self, residual: f64) -> Metrics {
        let mut sum_speed_sq = 0.;
        let mut max_speed: f64 = 0.;
        let mut mean_y = 0.;
        for p in &self.particles {
            let speed = dot(p.v, p.v).sqrt();
            sum_speed_sq += speed * speed;
            max_speed = max_speed.max(speed);
            mean_y += p.p[1];
        }
        mean_y /= self.particles.len() as f64;
        let mut wave_numerator = 0.;
        let mut wave_denominator = 0.;
        for (((particle, &base_x), &base_y), &top) in self
            .particles
            .iter()
            .zip(&self.reference_x)
            .zip(&self.reference_y)
            .zip(&self.top_layer)
        {
            if !top {
                continue;
            }
            let mode = (self.wave_number * (base_x - self.config.basin_min[0])).cos();
            wave_numerator += (particle.p[1] - base_y) * mode;
            wave_denominator += mode * mode;
        }
        let occupied_cell_volume_proxy = self
            .projection_grid
            .phi
            .iter()
            .enumerate()
            .filter(|&(i, &phi)| {
                phi < 0. && !self.config.basin_solid_cell(self.projection_grid.coords(i))
            })
            .count() as f64
            * self.config.h.powi(3);
        Metrics {
            time: self.time,
            rms_speed: (sum_speed_sq / self.particles.len() as f64).sqrt(),
            max_speed,
            mean_y,
            particle_volume: self.particles.len() as f64 * (self.config.h / 2.0).powi(3),
            occupied_cell_volume_proxy,
            projection_residual: residual,
            wave_amplitude: wave_numerator / wave_denominator,
        }
    }
}

fn collide_segment(config: Config, old: [f64; 3], proposed: [f64; 3]) -> [f64; 3] {
    let mut p = proposed;
    for a in 0..3 {
        let lo = config.basin_min[a];
        let hi = config.basin_max[a];
        if p[a] < lo || p[a] > hi {
            let wall = if p[a] < lo { lo } else { hi };
            let denom = proposed[a] - old[a];
            let t = if denom.abs() > f64::EPSILON {
                ((wall - old[a]) / denom).clamp(0., 1.)
            } else {
                0.
            };
            for d in 0..3 {
                p[d] = old[d] + t * (proposed[d] - old[d]);
            }
            p[a] = wall;
        }
        p[a] = p[a].clamp(lo, hi);
    }
    p
}

fn assert_invariants(sim: &Simulation, residual: f64) {
    assert_eq!(sim.particles.len(), sim.initial_particles);
    assert!(
        residual.is_finite() && residual < 1e-8,
        "projection residual={residual}"
    );
    for p in &sim.particles {
        assert!(p.p.iter().chain(p.v.iter()).all(|x| x.is_finite()));
        assert!(
            p.p.iter()
                .enumerate()
                .all(|(a, &x)| { x >= sim.config.basin_min[a] && x <= sim.config.basin_max[a] })
        );
    }
}

fn print_metric(label: &str, metric: Metrics) {
    println!(
        "{label} t={:.3}s rms_speed={:.9e} max_speed={:.9e} mean_y={:.9e} wave_amplitude={:.9e} particle_volume={:.9e} occupied_cell_volume_proxy={:.9e} projection_residual={:.9e}",
        metric.time,
        metric.rms_speed,
        metric.max_speed,
        metric.mean_y,
        metric.wave_amplitude,
        metric.particle_volume,
        metric.occupied_cell_volume_proxy,
        metric.projection_residual
    );
}

fn wave_period(sim: &Simulation) -> f64 {
    let depth = sim.config.water_depth;
    let omega = (GRAVITY.abs() * sim.wave_number * (sim.wave_number * depth).tanh()).sqrt();
    2.0 * std::f64::consts::PI / omega
}

fn wave_history(config: Config, dt: f64, duration: f64) -> (Simulation, Vec<Metrics>) {
    let mut sim = Simulation::with_config(config, true);
    let steps = (duration / dt).round() as usize;
    let mut history = Vec::with_capacity(steps + 1);
    history.push(sim.metrics(0.));
    let mut residual;
    for _ in 0..steps {
        residual = sim.step(dt).expect("coupled wave step");
        assert_invariants(&sim, residual);
        history.push(sim.metrics(residual));
    }
    (sim, history)
}

fn peak_envelope(history: &[Metrics], start: f64, end: f64) -> f64 {
    history
        .iter()
        .filter(|metric| metric.time >= start && metric.time <= end)
        .map(|metric| metric.wave_amplitude.abs())
        .fold(0., f64::max)
}

fn measured_period(history: &[Metrics]) -> Option<f64> {
    let mut crossings = Vec::new();
    for pair in history.windows(2) {
        let a = pair[0].wave_amplitude;
        let b = pair[1].wave_amplitude;
        if a * b < 0. {
            let fraction = a.abs() / (a.abs() + b.abs());
            crossings.push(pair[0].time + fraction * (pair[1].time - pair[0].time));
        }
    }
    crossings
        .windows(2)
        .next()
        .map(|pair| 2.0 * (pair[1] - pair[0]))
}

fn modal_rms_difference(coarse: &[Metrics], refined: &[Metrics], refined_stride: usize) -> f64 {
    assert!(refined.len() > refined_stride * (coarse.len() - 1));
    let sum = coarse
        .iter()
        .enumerate()
        .map(|(i, x)| (x.wave_amplitude - refined[i * refined_stride].wave_amplitude).powi(2))
        .sum::<f64>();
    (sum / coarse.len() as f64).sqrt()
}

#[test]
fn coupled_settle_reference() {
    let mut sim = Simulation::new(false);
    let mut residual = 0.;
    for frame in 1..=REST_SECONDS * 60 {
        residual = sim.step(1. / 60.).expect("coupled APIC step");
        assert_invariants(&sim, residual);
        if frame % 600 == 0 {
            print_metric("settle", sim.metrics(residual));
        }
    }
    let final_metrics = sim.metrics(residual);
    print_metric("settle-final", final_metrics);
    assert!(
        final_metrics.rms_speed < 0.01,
        "late rest RMS={}",
        final_metrics.rms_speed
    );
}

#[test]
fn coupled_linear_wave_reference() {
    let probe = Simulation::new(true);
    let predicted_period = wave_period(&probe);
    let (sim, history) = wave_history(default_config(), 1. / 60., 2.0 * predicted_period);
    print_metric("wave-initial", history[0]);
    for metric in history.iter().skip(1).step_by(60) {
        print_metric("wave", *metric);
    }
    let initial_amplitude = history[0].wave_amplitude.abs();
    let one_period_index = (predicted_period * 60.0).round() as usize;
    let one_period = history[one_period_index];
    print_metric("wave-final", *history.last().unwrap());
    let measured = measured_period(&history).expect("two wave zero crossings");
    println!(
        "wave-dispersion predicted_period={predicted_period:.9e} measured_period={measured:.9e} initial_amplitude={initial_amplitude:.9e} amplitude_after_one_period={:.9e}",
        one_period.wave_amplitude.abs()
    );
    assert!((measured / predicted_period - 1.0).abs() <= WAVE_PERIOD_TOLERANCE);
    assert!(one_period.wave_amplitude.abs() >= WAVE_AMPLITUDE_RETENTION * initial_amplitude);
    assert_invariants(&sim, history.last().unwrap().projection_residual);
}

#[test]
fn coupled_linear_wave_dt_half_comparison() {
    let probe = Simulation::new(true);
    let duration = 2.0 * wave_period(&probe);
    let (coarse, coarse_history) = wave_history(default_config(), 1. / 60., duration);
    let (fine, fine_history) = wave_history(default_config(), 1. / 120., duration);
    let (quarter, quarter_history) = wave_history(default_config(), 1. / 240., duration);
    let coarse_metric = *coarse_history.last().unwrap();
    let fine_metric = *fine_history.last().unwrap();
    let quarter_metric = *quarter_history.last().unwrap();
    print_metric("wave-dt", coarse_metric);
    print_metric("wave-dt-half", fine_metric);
    print_metric("wave-dt-quarter", quarter_metric);
    let initial_amplitude = coarse_history[0].wave_amplitude.abs();
    let half_rms = modal_rms_difference(&coarse_history, &fine_history, 2);
    let fine_quarter_rms = modal_rms_difference(&fine_history, &quarter_history, 2);
    println!(
        "wave-dt-half comparison initial_amplitude={initial_amplitude:.9e} modal_rms_half={half_rms:.9e} modal_rms_fine_quarter={fine_quarter_rms:.9e} final_mean_y_delta={:.9e}",
        (coarse_metric.mean_y - fine_metric.mean_y).abs(),
    );
    assert!(half_rms <= DT_HALF_MAX_INITIAL_RMS * initial_amplitude);
    assert_invariants(&coarse, coarse_metric.projection_residual);
    assert_invariants(&fine, fine_metric.projection_residual);
    assert_invariants(&quarter, quarter_metric.projection_residual);
}

#[test]
fn coupled_linear_wave_spatial_refinement() {
    let coarse_config = default_config();
    let fine_config = config_for_h(0.0625);
    let probe = Simulation::with_config(coarse_config, true);
    let predicted_period = wave_period(&probe);
    let duration = 2.0 * predicted_period;
    let (coarse, coarse_history) = wave_history(coarse_config, 1. / 60., duration);
    let (fine, fine_history) = wave_history(fine_config, 1. / 120., duration);
    let coarse_period = measured_period(&coarse_history).expect("coarse wave zero crossings");
    let fine_period = measured_period(&fine_history).expect("fine wave zero crossings");
    let coarse_initial_peak = peak_envelope(&coarse_history, 0., predicted_period);
    let fine_initial_peak = peak_envelope(&fine_history, 0., predicted_period);
    let coarse_peak_after_one_period = peak_envelope(&coarse_history, predicted_period, duration);
    let fine_peak_after_one_period = peak_envelope(&fine_history, predicted_period, duration);
    let aligned_rms = modal_rms_difference(&coarse_history, &fine_history, 2);
    println!(
        "wave-spatial predicted_period={predicted_period:.9e} coarse_period={coarse_period:.9e} fine_period={fine_period:.9e} coarse_initial_peak={coarse_initial_peak:.9e} fine_initial_peak={fine_initial_peak:.9e} coarse_peak_after_one_period={coarse_peak_after_one_period:.9e} fine_peak_after_one_period={fine_peak_after_one_period:.9e} aligned_waveform_rms={aligned_rms:.9e}"
    );
    assert_invariants(&coarse, coarse_history.last().unwrap().projection_residual);
    assert_invariants(&fine, fine_history.last().unwrap().projection_residual);
}

#[test]
fn coupled_resolved_wave_acceptance() {
    let config = config_for_h(0.03125);
    let predicted = wave_period(&Simulation::with_config(config, true));
    let duration = 2. * predicted;
    let (_, coarse) = wave_history(config, 1. / 60., duration);
    let (_, fine) = wave_history(config, 1. / 120., duration);
    let (_, quarter) = wave_history(config, 1. / 240., duration);
    let initial = coarse[0].wave_amplitude.abs();
    let period = measured_period(&fine).expect("resolved wave zero crossings");
    let retention = peak_envelope(&fine, predicted, duration) / initial;
    let half_error = modal_rms_difference(&coarse, &fine, 2) / initial;
    let quarter_error = modal_rms_difference(&fine, &quarter, 2) / initial;
    println!(
        "resolved h={} predicted={predicted} period={period} retention={retention} dt_half_error={half_error} dt_quarter_error={quarter_error}",
        config.h
    );
    assert!(
        (period / predicted - 1.).abs() <= WAVE_PERIOD_TOLERANCE,
        "resolved dispersion"
    );
    assert!(retention >= WAVE_AMPLITUDE_RETENTION, "resolved peak loss");
    assert!(
        half_error <= DT_HALF_MAX_INITIAL_RMS,
        "resolved timestep dependence"
    );
    assert!(
        quarter_error <= half_error,
        "timestep refinement must improve agreement"
    );
}

#[test]
fn basin_sampling_preserves_tangent_and_zero_normal_at_wall() {
    let mut sim = Simulation::new(false);
    sim.grid.velocity[0].fill(2.);
    for a in 0..3 {
        for f in 0..sim.grid.velocity[a].len() {
            if sim.projection_grid.open[a][f] == 0. {
                sim.grid.velocity[a][f] = 0.;
            }
        }
    }
    sim.extend_basin_sampling_velocities();
    let at_floor = [0.75, sim.config.basin_min[1], 0.5];
    let (v, c) = sim.grid.gather(at_floor).unwrap();
    assert!((v[0] - 2.).abs() < 1e-12);
    assert!(v[1].abs() < 1e-12);
    assert!(c[0][1].abs() < 1e-12);
}

#[test]
fn offline_frame_serialization_is_finite_and_96_bytes_per_particle() {
    let config = config_for_h(0.125);
    let sim = Simulation::with_config(config, true);
    let previous_positions: Vec<_> = sim.particles.iter().map(|particle| particle.p).collect();
    let bytes = serialize_frame(&sim, &previous_positions).expect("initial frame serializes");
    assert_eq!(
        bytes.len(),
        sim.particles.len() * WATER_PARTICLE_RECORD_BYTES
    );
    assert!(
        bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
            .all(|value| value.is_finite()),
        "all serialized values are finite"
    );
    let first = sim.particles.first().unwrap();
    let read = |offset: usize| f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    assert_eq!(read(0), first.p[0] as f32);
    assert_eq!(read(4), first.p[1] as f32);
    assert_eq!(read(8), first.p[2] as f32);
    assert_eq!(read(12), config.particle_mass() as f32);
    assert_eq!(read(28), WATER_PARTICLE_DENSITY as f32);
    assert_eq!(read(32), first.c[0][0] as f32);
    assert_eq!(read(80), first.p[0] as f32);
    assert_eq!(read(92), 0.);
}

#[test]
fn offline_export_rejects_invalid_timing_before_creating_files() {
    let path = Path::new("/invalid-offline-water-cache");
    assert!(export_offline_cache(path, 0, 30.).is_err());
    assert!(export_offline_cache(path, 1, f64::NAN).is_err());
    assert!(export_offline_cache(path, 1, 29.).is_err());
}
