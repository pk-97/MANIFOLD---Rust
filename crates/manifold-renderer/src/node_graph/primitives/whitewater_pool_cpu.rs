//! CPU statements of the GPU whitewater pool atoms, line for line with their
//! WGSL bodies (`docs/GPU_WHITEWATER_DESIGN.md` section 3.9), and the proof
//! that dropping FLIP's near-solid early-out changes no result in a domain
//! closed by solid. Each decision also reports how close it came to its
//! threshold, so a GPU proof can allow a hair's-breadth flip.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use super::whitewater_particle_cpu::{Box3, face_index};
use crate::node_graph::whitewater::WhitewaterParticle;

const BOX_INSET: f32 = 1.625;
const BOX_EPSILON: f32 = 0.5e-6;
const SOLID_BUFFER: f32 = 0.25;
const STEP: f32 = 0.5;
const MAX_RESOLVE: f32 = 5.0;
const MAX_VELOCITY: f32 = 1.1;
const NEAR_SOLID: u32 = 3;
const EPS: f32 = 1e-6;
pub(super) const DEAD: f32 = -1e6;
/// FLIP's `_solidLevelSetExactBand`, cells.
const EXACT_BAND: f32 = 3.0;
/// ceil(CFL / near-solid factor) feather passes.
const FEATHER: usize = 2;

/// The advect's settings, FLIP's defaults from [`Advect::flip`].
#[derive(Clone, Copy, Debug)]
pub(super) struct Advect {
    pub gravity: [f32; 3],
    pub dt: f32,
    pub foam_advection: f32,
    pub bubble_buoyancy: f32,
    pub bubble_drag: f32,
    pub spray_drag: f32,
    pub spray_drag_variance: f32,
    pub spray_restitution: f32,
    pub spray_friction: f32,
}

impl Advect {
    pub fn flip() -> Self {
        Self {
            gravity: [0.0, -9.81, 0.0],
            dt: 1.0 / 60.0,
            foam_advection: 1.0,
            bubble_buoyancy: 4.0,
            bubble_drag: 1.0,
            spray_drag: 0.0,
            spray_drag_variance: 0.25,
            spray_restitution: 0.2,
            spray_friction: 0.0,
        }
    }
}

/// The frame's fields the advect reads.
pub(super) struct Fields<'a> {
    pub faces: [&'a [f32]; 3],
    pub face_cells: [u32; 3],
    /// The solid lattice, `cells + 1` nodes a side.
    pub solid: &'a [f32],
}

fn corner(c: usize) -> [i32; 3] {
    [(c & 1) as i32, ((c >> 1) & 1) as i32, ((c >> 2) & 1) as i32]
}

fn corner_weight(f: [f32; 3], c: usize) -> f32 {
    let o = corner(c);
    (0..3).map(|a| if o[a] == 1 { f[a] } else { 1.0 - f[a] }).product()
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] + b[i])
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}

fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    a.map(|x| x * s)
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn length(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a * (1.0 - t) + b * t
}

/// FLIP's MAC trilinear at grid position q.
fn velocity(q: [f32; 3], f: &Fields<'_>, grid: &Box3) -> [f32; 3] {
    if (0..3).any(|a| q[a] < 0.0 || q[a] >= grid.cells[a] as f32) {
        return [0.0; 3];
    }
    let pad = (grid.cells[0] as i32 - f.face_cells[0] as i32) / 2;
    std::array::from_fn(|axis| {
        let s: [f32; 3] = std::array::from_fn(|a| if a == axis { q[a] } else { q[a] - 0.5 });
        let lower = s.map(f32::floor);
        let w: [f32; 3] = std::array::from_fn(|a| s[a] - lower[a]);
        let mut sum = 0.0;
        for c in 0..8 {
            let at: [i32; 3] = std::array::from_fn(|a| lower[a] as i32 + corner(c)[a]);
            if let Some(i) = face_index(at, axis, pad, f.face_cells) {
                sum += corner_weight(w, c) * f.faces[axis][i];
            }
        }
        sum
    })
}

fn node(solid: &[f32], nodes: [i32; 3], n: [i32; 3]) -> f32 {
    if (0..3).any(|a| n[a] < 0 || n[a] >= nodes[a]) {
        return 0.0;
    }
    solid[n[0] as usize + nodes[0] as usize * (n[1] as usize + nodes[1] as usize * n[2] as usize)]
}

fn corners(solid: &[f32], grid: &Box3, q: [f32; 3]) -> ([f32; 8], [f32; 3]) {
    let nodes = grid.cells.map(|c| c as i32 + 1);
    let lower = q.map(f32::floor);
    let f = std::array::from_fn(|a| q[a] - lower[a]);
    let phi = std::array::from_fn(|c| node(solid, nodes, std::array::from_fn(|a| lower[a] as i32 + corner(c)[a])));
    (phi, f)
}

/// The solid's distance at grid position q, metres.
fn solid_at(solid: &[f32], grid: &Box3, q: [f32; 3]) -> f32 {
    let (phi, f) = corners(solid, grid, q);
    (0..8).map(|c| corner_weight(f, c) * phi[c]).sum()
}

/// FLIP's trilinear gradient (interpolation.cpp:197), unscaled.
fn gradient(solid: &[f32], grid: &Box3, q: [f32; 3]) -> [f32; 3] {
    let (p, f) = corners(solid, grid, q);
    [
        mix(mix(p[1] - p[0], p[3] - p[2], f[1]), mix(p[5] - p[4], p[7] - p[6], f[1]), f[2]),
        mix(mix(p[2] - p[0], p[3] - p[1], f[0]), mix(p[6] - p[4], p[7] - p[5], f[0]), f[2]),
        mix(mix(p[4] - p[0], p[5] - p[1], f[0]), mix(p[6] - p[2], p[7] - p[3], f[0]), f[1]),
    ]
}

/// FLIP's `_nearSolidGrid`, as the bridge rebuilds it each frame: coarse
/// cells of three grid cells holding a node within the exact band of the
/// solid, feathered twice to the six neighbours.
pub(super) struct NearSolid {
    dims: [u32; 3],
    set: Vec<bool>,
}

impl NearSolid {
    pub fn build(solid: &[f32], grid: &Box3) -> Self {
        let dims = grid.cells.map(|c| c.div_ceil(NEAR_SOLID));
        let index = |c: [u32; 3]| (c[0] + dims[0] * (c[1] + dims[1] * c[2])) as usize;
        let mut set = vec![false; (dims[0] * dims[1] * dims[2]) as usize];
        let band = EXACT_BAND * grid.cell_size();
        let nodes = grid.cells.map(|c| c as i32 + 1);
        for k in 0..grid.cells[2] {
            for j in 0..grid.cells[1] {
                for i in 0..grid.cells[0] {
                    if node(solid, nodes, [i as i32, j as i32, k as i32]).abs() < band {
                        set[index([i, j, k].map(|n| n / NEAR_SOLID))] = true;
                    }
                }
            }
        }
        for _ in 0..FEATHER {
            let before = set.clone();
            for k in 0..dims[2] {
                for j in 0..dims[1] {
                    for i in 0..dims[0] {
                        if !before[index([i, j, k])] {
                            continue;
                        }
                        for n in 0..6 {
                            let mut c = [i as i64, j as i64, k as i64];
                            c[n / 2] += if n % 2 == 1 { 1 } else { -1 };
                            if (0..3).all(|a| c[a] >= 0 && c[a] < dims[a] as i64) {
                                set[index(c.map(|v| v as u32))] = true;
                            }
                        }
                    }
                }
            }
        }
        Self { dims, set }
    }

    fn get(&self, g: [i64; 3]) -> bool {
        self.set[(g[0] + self.dims[0] as i64 * (g[1] + self.dims[1] as i64 * g[2])) as usize]
    }
}

/// The near-solid cell of local position p, if it lies in the coarse grid.
fn near_solid_cell(p: [f32; 3], grid: &Box3) -> Option<[i64; 3]> {
    let h = grid.cell_size();
    let g = p.map(|x| (x / (NEAR_SOLID as f32 * h)).floor() as i64);
    (0..3).all(|a| g[a] >= 0 && g[a] < grid.cells[a].div_ceil(NEAR_SOLID) as i64).then_some(g)
}

/// The collision march and its resolve, local metres: the resolved position,
/// the spray bounce velocity when a usable normal was found, and the margin.
fn collide(
    p: [f32; 3],
    nextp: [f32; 3],
    v: [f32; 3],
    f: &Fields<'_>,
    grid: &Box3,
    s: Advect,
    near: Option<&NearSolid>,
) -> ([f32; 3], Option<[f32; 3]>, f32) {
    let h = grid.cell_size();
    let lo = [BOX_INSET * h + BOX_EPSILON; 3];
    let hi: [f32; 3] = std::array::from_fn(|a| grid.cells[a] as f32 * h - lo[a]);
    let inside = |x: [f32; 3]| (0..3).all(|a| x[a] >= lo[a] && x[a] < hi[a]);
    let box_gap = |x: [f32; 3]| (0..3).map(|a| (x[a] - lo[a]).abs().min((x[a] - hi[a]).abs())).fold(f32::INFINITY, f32::min);
    let solid = |x: [f32; 3]| solid_at(f.solid, grid, x.map(|c| c / h));
    let mut margin = f32::INFINITY;
    let travel = length(sub(nextp, p));
    let (Some(old_cell), Some(new_cell)) = (near_solid_cell(p, grid), near_solid_cell(nextp, grid)) else {
        return (nextp, None, margin);
    };
    if let Some(near) = near
        && !near.get(old_cell)
        && !near.get(new_cell)
    {
        return (nextp, None, margin);
    }
    if travel < EPS {
        return (nextp, None, (travel - EPS).abs());
    }
    let step = STEP * h;
    let ratio = travel / step;
    margin = margin.min((ratio - ratio.round()).abs() * step);
    let steps = ratio.ceil() as i32;
    let dir = scale(sub(nextp, p), 1.0 / travel);
    let mut last = p;
    let mut current = p;
    let mut hit = None;
    for i in 0..steps {
        current = if i == steps - 1 { nextp } else { add(p, scale(dir, (i + 1) as f32 * step)) };
        let phi = solid(current);
        margin = margin.min(phi.abs()).min(box_gap(current));
        if phi < 0.0 || !inside(current) {
            hit = Some(phi);
            break;
        }
        last = current;
    }
    let Some(hit_phi) = hit else { return (nextp, None, margin) };
    let reach = MAX_RESOLVE * h;
    let grad = gradient(f.solid, grid, current.map(|c| c / h));
    let mut bounce = None;
    let mut resolved;
    margin = margin.min((length(grad) - EPS).abs());
    if length(grad) > EPS {
        let n = scale(grad, 1.0 / length(grad));
        resolved = sub(current, scale(n, hit_phi - SOLID_BUFFER * h));
        let moved = length(sub(resolved, current));
        margin = margin.min(solid(resolved).abs()).min((moved - reach).abs());
        if solid(resolved) < 0.0 || moved > reach {
            resolved = last;
        }
        let u = scale(n, dot(v, n));
        bounce = Some(sub(scale(sub(v, u), 1.0 - s.spray_friction), scale(u, s.spray_restitution)));
    } else {
        resolved = last;
    }
    margin = margin.min(box_gap(resolved));
    if !inside(resolved) {
        let before = resolved;
        resolved = std::array::from_fn(|a| resolved[a].max(lo[a]).min(hi[a] - EPS));
        let moved = length(sub(resolved, before));
        margin = margin.min(solid(resolved).abs()).min((moved - reach).abs());
        if solid(resolved) < 0.0 || moved > reach {
            resolved = last;
        }
    }
    (resolved, bounce, margin)
}

/// `node.advect_whitewater` for one slot, and the smallest gap between a
/// decision and its threshold, metres. `near` applies FLIP's near-solid
/// early-out, which the atom drops.
pub(super) fn advect(
    particle: WhitewaterParticle,
    f: &Fields<'_>,
    grid: &Box3,
    s: Advect,
    near: Option<&NearSolid>,
) -> (WhitewaterParticle, f32) {
    let mut out = particle;
    if particle.kind > 2 || s.dt <= 0.0 {
        return (out, f32::INFINITY);
    }
    let h = grid.cell_size();
    let origin: [f32; 3] = std::array::from_fn(|a| grid.center[a] - 0.5 * grid.size[a]);
    let p = sub([particle.position_lifetime[0], particle.position_lifetime[1], particle.position_lifetime[2]], origin);
    let v = particle.velocity;
    let g = s.gravity;
    let spray = particle.kind == 2;
    let mut nextv = if spray {
        let factor = particle.id as f32 / 255.0;
        let mind = (s.spray_drag - s.spray_drag * s.spray_drag_variance).max(0.0);
        let maxd = s.spray_drag + s.spray_drag * s.spray_drag_variance;
        let drag = mind + (1.0 - factor) * (maxd - mind);
        add(add(v, scale(g, s.dt)), scale(v, -drag * s.dt))
    } else {
        let vmac = velocity(p.map(|c| c / h), f, grid);
        if particle.kind == 0 {
            let push: [f32; 3] = std::array::from_fn(|a| -s.bubble_buoyancy * g[a] + s.bubble_drag * (vmac[a] - v[a]) / s.dt);
            add(v, scale(push, s.dt))
        } else {
            scale(vmac, s.foam_advection)
        }
    };
    let nextp = add(p, scale(nextv, s.dt));
    if !length(sub(nextp, p)).is_finite() {
        out.position_lifetime[3] = DEAD;
        return (out, f32::INFINITY);
    }
    let (resolved, bounce, mut margin) = collide(p, nextp, v, f, grid, s, near);
    if spray && let Some(b) = bounce {
        nextv = add(b, scale(g, s.dt));
    }
    let speed = length(sub(resolved, p)) * (1.0 / s.dt);
    let limit = MAX_VELOCITY * length(nextv);
    margin = margin.min((speed - limit).abs() * s.dt);
    if speed > limit {
        out.position_lifetime[3] = DEAD;
    }
    let world = add(resolved, origin);
    out.position_lifetime = [world[0], world[1], world[2], out.position_lifetime[3]];
    out.velocity = nextv;
    (out, margin)
}

/// `node.retype_whitewater` for one slot, and the smallest gap between a
/// deciding value and its threshold, in cells.
pub(super) fn retype(particle: WhitewaterParticle, f: &Fields<'_>, distance: &[f32], cells: &[u32], grid: &Box3) -> (WhitewaterParticle, f32) {
    let mut out = particle;
    if particle.kind > 2 {
        return (out, f32::INFINITY);
    }
    let h = grid.cell_size();
    let q = grid.position([particle.position_lifetime[0], particle.position_lifetime[1], particle.position_lifetime[2]]);
    let lo = BOX_INSET + BOX_EPSILON / h;
    let mut margin = (0..3).map(|a| (q[a] - lo).abs().min((q[a] - (grid.cells[a] as f32 - lo)).abs())).fold(f32::INFINITY, f32::min);
    let kind = if (0..3).any(|a| q[a] < lo || q[a] >= grid.cells[a] as f32 - lo) {
        2
    } else {
        let s = q.map(|v| v - 0.5);
        let lower = s.map(f32::floor);
        let w: [f32; 3] = std::array::from_fn(|a| s[a] - lower[a]);
        let mut d = 0.0;
        for c in 0..8 {
            let at: [i32; 3] = std::array::from_fn(|a| lower[a] as i32 + corner(c)[a]);
            if grid.in_grid(at) {
                d += corner_weight(w, c) * distance[grid.index(at)];
            }
        }
        margin = margin.min((d.abs() - h).abs() / h);
        let mut kind = if d > -h && d < h {
            1
        } else if d < -h {
            0
        } else {
            2
        };
        if particle.kind == 1 && kind == 0 {
            margin = margin.min((d + 2.0 * h).abs() / h);
            if d > -2.0 * h {
                kind = 1;
            }
        }
        if kind != 0 {
            let g = q.map(|v| v.floor() as i32);
            margin = margin.min(q.iter().map(|v| (v - v.round()).abs()).fold(f32::INFINITY, f32::min));
            let air = (-1..=1).any(|dz| {
                (-1..=1).any(|dy| {
                    (-1..=1).any(|dx| {
                        let n = [g[0] + dx, g[1] + dy, g[2] + dz];
                        (dx, dy, dz) != (0, 0, 0) && grid.in_grid(n) && cells[grid.index(n)] == 0
                    })
                })
            });
            if !air {
                kind = 0;
            }
        }
        kind
    };
    if particle.kind == 0 && kind != 0 {
        out.velocity = velocity(q, f, grid);
    }
    out.kind = kind;
    (out, margin)
}

/// `node.age_whitewater`'s settings, FLIP's defaults from [`Age::flip`].
#[derive(Clone, Copy, Debug)]
pub(super) struct Age {
    pub dt: f32,
    pub bubble: f32,
    pub foam: f32,
    pub spray: f32,
}

impl Age {
    pub fn flip() -> Self {
        Self { dt: 1.0 / 60.0, bubble: 0.333, foam: 1.0, spray: 2.0 }
    }
}

/// `node.age_whitewater` for one slot.
pub(super) fn age(particle: WhitewaterParticle, s: Age) -> WhitewaterParticle {
    let mut out = particle;
    if particle.kind > 2 {
        return out;
    }
    out.position_lifetime[3] -= [s.bubble, s.foam, s.spray][particle.kind as usize] * s.dt;
    out
}

/// `node.preserve_foam`'s settings, FLIP's defaults from [`Preserve::flip`]
/// (off there; on here, since a proof needs it on).
#[derive(Clone, Copy, Debug)]
pub(super) struct Preserve {
    pub dt: f32,
    pub rate: f32,
    pub min_density: f32,
    pub max_density: f32,
}

impl Preserve {
    pub fn flip() -> Self {
        Self { dt: 1.0 / 60.0, rate: 0.75, min_density: 20.0, max_density: 45.0 }
    }
}

/// FLIP's `_updateFoamPreservation` over a pool: foam, dead or alive, in each
/// cell of side `h` from `origin`, then each foam particle's gain. A position
/// outside the `cells` grid or not finite counts nowhere and gains nothing
/// (FLIP indexes past its grid there; the tick removes every such particle).
pub(super) fn preserve(pool: &[WhitewaterParticle], origin: [f32; 3], h: f32, cells: [u32; 3], s: Preserve) -> Vec<WhitewaterParticle> {
    let cell = |p: &WhitewaterParticle| -> Option<usize> {
        let g: [f32; 3] = std::array::from_fn(|a| ((p.position_lifetime[a] - origin[a]) / h).floor());
        (0..3)
            .all(|a| g[a].is_finite() && g[a] >= 0.0 && g[a] < cells[a] as f32)
            .then(|| g[0] as usize + cells[0] as usize * (g[1] as usize + cells[1] as usize * g[2] as usize))
    };
    let mut density = vec![0u32; (cells[0] * cells[1] * cells[2]) as usize];
    for p in pool.iter().filter(|p| p.kind == 1) {
        if let Some(c) = cell(p) {
            density[c] += 1;
        }
    }
    let inv = 1.0 / (s.max_density - s.min_density).max(1e-6);
    pool.iter()
        .map(|p| {
            let mut out = *p;
            if p.kind == 1
                && let Some(c) = cell(p)
            {
                let d = ((density[c] as f32 - s.min_density) * inv).clamp(0.0, 1.0);
                out.position_lifetime[3] += s.rate * d * s.dt;
            }
            out
        })
        .collect()
}

/// Fixtures shared by the CPU proof here and the GPU proofs.
pub(super) mod fixture {
    use super::super::whitewater_cpu::Rng;
    use super::*;
    use crate::node_graph::liquid::grid::face_len;
    use crate::node_graph::whitewater::{WHITEWATER_EMPTY, WHITEWATER_ID_LIMIT};

    /// Large enough that near-solid cells clear of the walls and the ball
    /// exist, so the early-out has somewhere to fire.
    pub const NODES: [u32; 3] = [49, 45, 41];
    pub const FACE_CELLS: [u32; 3] = [46, 42, 38];
    pub const H: f32 = 0.05;
    pub const ORIGIN: [f32; 3] = [-1.0, 0.5, 2.0];

    pub fn grid() -> Box3 {
        let cells = NODES.map(|n| n - 1);
        let size: [f32; 3] = std::array::from_fn(|a| cells[a] as f32 * H);
        Box3 { cells, center: std::array::from_fn(|a| ORIGIN[a] + 0.5 * size[a]), size }
    }

    /// A tank closed on every side, its walls one cell thick, with a ball
    /// of radius 3 cells inside: positive in the open, metres.
    pub fn tank() -> Vec<f32> {
        let cells = NODES.map(|n| n - 1);
        let ball: [f32; 3] = std::array::from_fn(|a| 0.45 * cells[a] as f32 * H);
        let mut solid = Vec::with_capacity((NODES[0] * NODES[1] * NODES[2]) as usize);
        for k in 0..NODES[2] {
            for j in 0..NODES[1] {
                for i in 0..NODES[0] {
                    let x = [i, j, k].map(|n| n as f32 * H);
                    let wall = (0..3).map(|a| (x[a] - H).min(cells[a] as f32 * H - H - x[a])).fold(f32::INFINITY, f32::min);
                    solid.push(wall.min(length(sub(x, ball)) - 3.0 * H));
                }
            }
        }
        solid
    }

    /// Face velocities up to `speed` either way.
    pub fn faces(rng: &mut Rng, speed: f32) -> [Vec<f32>; 3] {
        std::array::from_fn(|axis| (0..face_len(FACE_CELLS, axis)).map(|_| speed * (2.0 * rng.unit() - 1.0)).collect())
    }

    /// Particles across the grid, a tenth of the slots empty, every type,
    /// velocities up to `speed` in any direction.
    pub fn pool(rng: &mut Rng, slots: usize, speed: f32) -> Vec<WhitewaterParticle> {
        let g = grid();
        (0..slots)
            .map(|_| {
                let p: [f32; 3] = std::array::from_fn(|a| ORIGIN[a] + g.size[a] * rng.unit());
                let empty = rng.unit() < 0.1;
                let lifetime = 0.5 + rng.unit();
                let d: [f32; 3] = std::array::from_fn(|_| 2.0 * rng.unit() - 1.0);
                let velocity = scale(d, speed * rng.unit() / length(d).max(1e-3));
                WhitewaterParticle {
                    position_lifetime: [p[0], p[1], p[2], lifetime],
                    velocity,
                    kind: if empty { WHITEWATER_EMPTY } else { (rng.unit() * 3.0) as u32 % 3 },
                    id: (rng.unit() * WHITEWATER_ID_LIMIT as f32) as u32 % WHITEWATER_ID_LIMIT,
                    ..Default::default()
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::whitewater_cpu::Rng;
    use super::fixture::*;
    use super::*;

    /// GPU_WHITEWATER_DESIGN.md section 3.9: in a domain closed by solid,
    /// with no particle travelling more than 5 cells a tick, the advect with
    /// FLIP's near-solid early-out and without it agree bit for bit. The
    /// early-out skips the march only where no solid lies within reach.
    #[test]
    fn whitewater_near_solid_drop_changes_nothing() {
        let mut rng = Rng(0x9ea5_0011);
        let g = grid();
        let solid = tank();
        let near = NearSolid::build(&solid, &g);
        let speed = 5.0 * H * 60.0;
        let faces = faces(&mut rng, speed / 3f32.sqrt());
        let f = Fields { faces: faces.each_ref().map(Vec::as_slice), face_cells: FACE_CELLS, solid: &solid };
        let settings = Advect { spray_drag: 0.5, spray_friction: 0.1, ..Advect::flip() };
        let (mut skipped, mut moved, mut stopped) = (0, 0, 0);
        for particle in pool(&mut rng, 20_000, speed) {
            let (with, _) = advect(particle, &f, &g, settings, Some(&near));
            let (without, _) = advect(particle, &f, &g, settings, None);
            assert_eq!(bytemuck::bytes_of(&with), bytemuck::bytes_of(&without), "{particle:?}");
            if particle.kind > 2 {
                continue;
            }
            let h = g.cell_size();
            let origin: [f32; 3] = std::array::from_fn(|a| g.center[a] - 0.5 * g.size[a]);
            let local = |x: [f32; 4]| [x[0] - origin[0], x[1] - origin[1], x[2] - origin[2]];
            let cells = |x: [f32; 3]| near_solid_cell(x, &g).filter(|c| near.get(*c)).is_none();
            let plain = local(particle.position_lifetime);
            let free = add(plain, scale(without.velocity, settings.dt));
            // Foam keeps its velocity through a collision, so its free
            // endpoint is known from the result.
            if particle.kind == 1 && cells(plain) && cells(free) {
                skipped += 1;
            }
            moved += usize::from(length(sub(local(without.position_lifetime), plain)) > 0.5 * h);
            stopped += usize::from(without.position_lifetime[3] == DEAD);
        }
        println!("{skipped} foam early-outs, {moved} moved half a cell, {stopped} killed");
        assert!(skipped > 200 && moved > 2000 && stopped > 100, "{skipped} {moved} {stopped}");
    }
}
