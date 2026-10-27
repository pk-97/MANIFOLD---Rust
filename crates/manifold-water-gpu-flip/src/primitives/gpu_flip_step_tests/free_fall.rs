//! Water in the air: with nothing under it, it falls at g.

use super::slots::RestingPool;
use super::*;

/// Particles at rest at the fill's sub-sites `sites` (0-7) of box cell `at`.
fn clump(at: [usize; 3], sites: &[usize]) -> Vec<FluidParticle> {
    sites
        .iter()
        .enumerate()
        .map(|(id, &sub)| {
            let corner = [sub & 1, (sub >> 1) & 1, (sub >> 2) & 1];
            let position: [f32; 3] = std::array::from_fn(|a| {
                NODE_MIN[a] + (at[a] as f32 + 0.25 + 0.5 * corner[a] as f32) * H
            });
            FluidParticle {
                position_radius: [position[0], position[1], position[2], 0.31017524 * H],
                velocity: [0.0; 3],
                id: id as u32 + 1,
            }
        })
        .collect()
}

/// A lone particle and a cell of water, at rest in the air two cells above
/// the floor of the small box, fall freely: after four ticks of 1/60 s each
/// moves down at g t, within 5%.
#[test]
fn water_in_the_air_falls_at_g() {
    const TICKS: i32 = 4;
    let fall = 9.81 * TICKS as f32 / 60.0;
    let lone: &[usize] = &[0];
    let cell: &[usize] = &[0, 1, 2, 3, 4, 5, 6, 7];
    for (label, sites) in [("a lone particle", lone), ("a cell of water", cell)] {
        let mut pool = RestingPool::new(clump([2, 2, 1], sites), |live| live);
        for tick in 0..TICKS {
            pool.tick(tick);
        }
        for p in pool.left().particles {
            assert!(p.position_radius[3] > 0.0, "{label}: the particle is gone");
            let v = p.velocity[1];
            assert!(
                (v + fall).abs() <= 0.05 * fall,
                "{label}: falls at {v} m/s after {TICKS} ticks, free fall is {}",
                -fall
            );
        }
    }
}
