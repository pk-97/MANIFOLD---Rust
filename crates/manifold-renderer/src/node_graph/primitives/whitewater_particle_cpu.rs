//! CPU statements of the whitewater emitter atoms' contracts, line for line
//! with their WGSL bodies, for the GPU value proofs
//! (`docs/GPU_WHITEWATER_DESIGN.md` section 3.7). Each threshold test also
//! reports how close the value came to its threshold, so a proof can allow
//! the GPU to fall the other way on a hair's-breadth decision.

// The GPU proofs (`whitewater_particle_tests`, feature gpu-proofs) call every
// item here; a default test build compiles only the extent proof's face index.
#![cfg_attr(not(feature = "gpu-proofs"), allow(dead_code))]

use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::liquid::grid::face_dims;
use crate::node_graph::whitewater::{CELL_AIR, KnownValue};

/// A whitewater grid as the particle atoms read it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Box3 {
    pub cells: [u32; 3],
    pub center: [f32; 3],
    pub size: [f32; 3],
}

impl Box3 {
    pub fn cell_size(&self) -> f32 {
        self.size[0] / self.cells[0] as f32
    }

    pub fn position(&self, p: [f32; 3]) -> [f32; 3] {
        std::array::from_fn(|a| (p[a] - (self.center[a] - 0.5 * self.size[a])) * self.cells[a] as f32 / self.size[a])
    }

    fn in_grid(&self, c: [i32; 3]) -> bool {
        (0..3).all(|a| c[a] >= 0 && c[a] < self.cells[a] as i32)
    }

    fn index(&self, c: [i32; 3]) -> usize {
        let [nx, ny, _] = self.cells.map(|n| n as usize);
        c[0] as usize + nx * (c[1] as usize + ny * c[2] as usize)
    }
}

pub(super) fn hash(x: u32) -> u32 {
    let mut h = x;
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^= h >> 16;
    h
}

pub(super) fn random(slot: u32, seed: u32, epoch: u32, stream: u32) -> f32 {
    let h = hash(slot.wrapping_add(hash(seed.wrapping_add(hash(epoch.wrapping_mul(16).wrapping_add(stream))))));
    (h >> 8) as f32 * (1.0 / 16_777_216.0)
}

fn corner(c: usize) -> [i32; 3] {
    [(c & 1) as i32, ((c >> 1) & 1) as i32, ((c >> 2) & 1) as i32]
}

fn corner_weight(f: [f32; 3], c: usize) -> f32 {
    let o = corner(c);
    (0..3).map(|a| if o[a] == 1 { f[a] } else { 1.0 - f[a] }).product()
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a * (1.0 - t) + b * t
}

/// `node.jitter_particles` for slot `idx`.
pub(super) fn jitter(p: FluidParticle, idx: u32, cell_size: f32, seed: f32, epoch: f32) -> FluidParticle {
    if p.position_radius[3] <= 0.0 {
        return p;
    }
    let reach = 0.25 * (1.0 - 1e-3) * cell_size;
    let generation = epoch.round().max(0.0) as u32;
    let mut out = p;
    for a in 0..3 {
        out.position_radius[a] += reach * (2.0 * random(idx, seed.to_bits(), generation, a as u32) - 1.0);
    }
    out
}

/// The seam index of the `axis` face at whitewater face coordinates `f`, or
/// None where FLIP's padded array holds 0.
pub(super) fn face_index(f: [i32; 3], axis: usize, pad: i32, face_cells: [u32; 3]) -> Option<usize> {
    let dims = face_dims(face_cells, axis);
    let g: [i32; 3] = std::array::from_fn(|a| f[a] - pad);
    (0..3).all(|a| g[a] >= 0 && g[a] < dims[a] as i32).then(|| {
        let g = g.map(|v| v as usize);
        g[0] + dims[0] as usize * (g[1] + dims[1] as usize * g[2])
    })
}

/// `node.sample_faces_at_particles`.
pub(super) fn sample_faces(p: FluidParticle, faces: [&[f32]; 3], face_cells: [u32; 3], grid: &Box3) -> FluidParticle {
    if p.position_radius[3] <= 0.0 {
        return p;
    }
    let mut out = p;
    out.velocity = [0.0; 3];
    let q = grid.position([p.position_radius[0], p.position_radius[1], p.position_radius[2]]);
    if (0..3).any(|a| q[a] < 0.0 || q[a] >= grid.cells[a] as f32) {
        return out;
    }
    let pad = (grid.cells[0] as i32 - face_cells[0] as i32) / 2;
    for (axis, face) in faces.iter().enumerate() {
        let s: [f32; 3] = std::array::from_fn(|a| if a == axis { q[a] } else { q[a] - 0.5 });
        let lower = s.map(f32::floor);
        let f: [f32; 3] = std::array::from_fn(|a| s[a] - lower[a]);
        let base = lower.map(|v| v as i32);
        let mut sum = 0.0;
        for c in 0..8 {
            let o = corner(c);
            let at: [i32; 3] = std::array::from_fn(|a| base[a] + o[a]);
            if let Some(i) = face_index(at, axis, pad, face_cells) {
                sum += corner_weight(f, c) * face[i];
            }
        }
        out.velocity[axis] = sum;
    }
    out
}

/// `node.energy_potential`.
pub(super) fn energy(p: FluidParticle, min: f32, max: f32) -> f32 {
    if p.position_radius[3] <= 0.0 || max <= min {
        return 0.0;
    }
    let v = p.velocity;
    let e = (0.5 * (v[0] * v[0] + v[1] * v[1] + v[2] * v[2])).max(min).min(max);
    (e - min) / (max - min)
}

/// FLIP's wavecrest limits.
#[derive(Clone, Copy)]
pub(super) struct Crest {
    pub min_curvature: f32,
    pub max_curvature: f32,
    pub sharpness: f32,
}

/// `node.wavecrest_potential`, and the smallest gap between a thresholded
/// quantity and its threshold on the way there (in that quantity's units,
/// relative where it has a scale).
pub(super) fn wavecrest(
    p: FluidParticle,
    distance: &[f32],
    curvature: &[KnownValue],
    cells: &[u32],
    grid: &Box3,
    crest: Crest,
) -> (f32, f32) {
    let mut margin = f32::INFINITY;
    if p.position_radius[3] <= 0.0 {
        return (0.0, margin);
    }
    let h = grid.cell_size();
    let q = grid.position([p.position_radius[0], p.position_radius[1], p.position_radius[2]]);
    let s = q.map(|v| v - 0.5);
    let lower = s.map(f32::floor);
    let f: [f32; 3] = std::array::from_fn(|a| s[a] - lower[a]);
    let base = lower.map(|v| v as i32);
    let mut phi = [0.0f32; 8];
    let (mut d, mut k) = (0.0f32, 0.0f32);
    for (c, value) in phi.iter_mut().enumerate() {
        let o = corner(c);
        let at: [i32; 3] = std::array::from_fn(|a| base[a] + o[a]);
        let w = corner_weight(f, c);
        if grid.in_grid(at) {
            *value = distance[grid.index(at)];
            k += w * curvature[grid.index(at)].value;
        }
        d += w * *value;
    }
    margin = margin.min((d.abs() - 1.5 * h).abs() / h);
    if d.abs() >= 1.5 * h {
        return (0.0, margin);
    }
    let g = q.map(|v| v.floor() as i32);
    let mut air = false;
    for dz in -1..=1 {
        for dy in -1..=1 {
            for dx in -1..=1 {
                let n = [g[0] + dx, g[1] + dy, g[2] + dz];
                if (dx, dy, dz) != (0, 0, 0) && grid.in_grid(n) && cells[grid.index(n)] == CELL_AIR {
                    air = true;
                }
            }
        }
    }
    // The cell a particle sits in is a floor; a hair from a boundary may
    // floor the other way on the GPU.
    margin = margin.min(q.iter().map(|v| (v - v.round()).abs()).fold(f32::INFINITY, f32::min));
    if !air {
        return (0.0, margin);
    }
    let v = p.velocity;
    if v.iter().all(|c| c.abs() < 1e-6) {
        return (0.0, margin);
    }
    let k = k * h;
    margin = margin.min((k - crest.min_curvature).abs());
    if k < crest.min_curvature {
        return (0.0, margin);
    }
    let k = k.min(crest.max_curvature);
    let gx = lerp(lerp(phi[1] - phi[0], phi[3] - phi[2], f[1]), lerp(phi[5] - phi[4], phi[7] - phi[6], f[1]), f[2]);
    let gy = lerp(lerp(phi[2] - phi[0], phi[3] - phi[1], f[0]), lerp(phi[6] - phi[4], phi[7] - phi[5], f[0]), f[2]);
    let gz = lerp(lerp(phi[4] - phi[0], phi[5] - phi[1], f[0]), lerp(phi[6] - phi[2], phi[7] - phi[3], f[0]), f[1]);
    let grad = [gx, gy, gz];
    if grad.iter().all(|c| c.abs() < 1e-6) {
        return (0.0, margin);
    }
    let unit = |x: [f32; 3]| {
        let l = (x[0] * x[0] + x[1] * x[1] + x[2] * x[2]).sqrt();
        x.map(|c| c / l)
    };
    let (vn, n) = (unit(v), unit(grad));
    let cosine = vn[0] * n[0] + vn[1] * n[1] + vn[2] * n[2];
    margin = margin.min((cosine - crest.sharpness).abs());
    if cosine < crest.sharpness {
        return (0.0, margin);
    }
    ((k - crest.min_curvature) / (crest.max_curvature - crest.min_curvature), margin)
}

/// `node.emission_count` inputs besides the per-particle arrays.
#[derive(Clone, Copy)]
pub(super) struct Emission {
    pub rate: f32,
    pub points_per_cell: f32,
    pub ticks: f32,
    pub live_count: f32,
}

/// `node.emission_count` for slot `idx`, and how far the per-tick count sat
/// from a rounding edge.
pub(super) fn emission_count(p: FluidParticle, energy: f32, wavecrest: f32, idx: u32, e: Emission) -> (u32, f32) {
    let speed = (p.velocity[0] * p.velocity[0] + p.velocity[1] * p.velocity[1] + p.velocity[2] * p.velocity[2]).sqrt();
    if idx as f32 >= e.live_count || p.position_radius[3] <= 0.0 || e.points_per_cell <= 0.0 {
        return (0, f32::INFINITY);
    }
    if speed < 1e-3 || energy < 1e-6 || wavecrest <= 0.0 {
        return (0, (speed - 1e-3).abs());
    }
    let per_tick = e.rate * energy * wavecrest * (1.0 / 60.0) * 8.0 / e.points_per_cell;
    let edge = ((per_tick + 0.5) - (per_tick + 0.5).round()).abs();
    ((per_tick + 0.5).floor() as u32 * e.ticks.round().max(0.0) as u32, edge.min((speed - 1e-3).abs()))
}
