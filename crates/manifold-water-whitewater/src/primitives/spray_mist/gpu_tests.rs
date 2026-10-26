//! GPU proofs for `node.spray_mist` against the closed forms of its kernel.

use super::*;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};

const R: f32 = 0.01;
const H: f32 = 0.05;
const NODES: [u32; 3] = [33, 33, 33];
const SIZE: [f32; 3] = [32.0 * H, 32.0 * H, 32.0 * H];
const KERNEL_PEAK: f32 = 1.0 / (std::f32::consts::PI * H * H * H);

fn droplet_area() -> f32 {
    2.0 * std::f32::consts::PI * R * R
}

fn particle(position: [f32; 3], radius: f32, id: u32) -> FluidParticle {
    FluidParticle {
        position_radius: [position[0], position[1], position[2], radius],
        velocity: [0.0; 3],
        id,
    }
}

fn node_position(index: [u32; 3]) -> [f32; 3] {
    std::array::from_fn(|a| -0.5 * SIZE[a] + H * index[a] as f32)
}

fn node_index(index: [u32; 3]) -> usize {
    (index[0] + NODES[0] * (index[1] + NODES[1] * index[2])) as usize
}

/// The mist over the 33 x 33 x 33 lattice centred on the origin.
fn mist_of(particles: &[FluidParticle], strength: f32) -> Vec<f32> {
    let mut harness = Harness::new();
    let mut node = SprayMist::new();
    let (particle_slot, _) = harness.array(particles, particles.len());
    let (mist_slot, _) = harness.array::<f32>(&[], 1);
    let run_params = params(&[
        ("center_x", 0.0),
        ("center_y", 0.0),
        ("center_z", 0.0),
        ("size_x", SIZE[0]),
        ("size_y", SIZE[1]),
        ("size_z", SIZE[2]),
        ("nodes_x", NODES[0] as f32),
        ("nodes_y", NODES[1] as f32),
        ("nodes_z", NODES[2] as f32),
        ("strength", strength),
    ]);
    let (_, errors) = harness.run(
        &mut node,
        &[("particles", particle_slot)],
        &[("mist", mist_slot)],
        &run_params,
    );
    assert!(errors.is_empty(), "{errors:?}");
    read(
        &harness.buffer(mist_slot),
        NODES.iter().product::<u32>() as usize,
    )
}

/// Droplets of radius R on a cubic grid of spacing H / 2 across the whole
/// lattice.
fn cloud() -> Vec<FluidParticle> {
    let a = H / 2.0;
    let per_axis = (SIZE[0] / a).round() as u32 + 1;
    let mut particles = Vec::new();
    for iz in 0..per_axis {
        for iy in 0..per_axis {
            for ix in 0..per_axis {
                let p = [
                    -0.5 * SIZE[0] + a * ix as f32,
                    -0.5 * SIZE[1] + a * iy as f32,
                    -0.5 * SIZE[2] + a * iz as f32,
                ];
                particles.push(particle(p, R, particles.len() as u32));
            }
        }
    }
    particles
}

#[test]
fn a_uniform_cloud_has_the_closed_form_extinction() {
    let a = H / 2.0;
    let expected = droplet_area() / (a * a * a);
    let cloud = cloud();
    for strength in [1.0, 2.0] {
        let mist = mist_of(&cloud, strength);
        let want = strength * expected;
        let mut checked = 0;
        for iz in 4..=28 {
            for iy in 4..=28 {
                for ix in 4..=28 {
                    let got = mist[node_index([ix, iy, iz])];
                    assert!(
                        (got - want).abs() <= 0.01 * want,
                        "strength {strength} node ({ix}, {iy}, {iz}): {got}, expected {want}"
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 25 * 25 * 25);
    }
}

#[test]
fn nodes_beyond_two_smoothing_lengths_read_exactly_zero() {
    let at = [16, 16, 16];
    let mut position = node_position(at);
    position[0] += 0.3 * H;
    let mist = mist_of(&[particle(position, R, 0)], 1.0);
    let mut far = 0;
    for z in 0..NODES[2] {
        for y in 0..NODES[1] {
            for x in 0..NODES[0] {
                let apart = [x, y, z].map(|c| c.abs_diff(16));
                if apart.iter().any(|&d| d >= 4) {
                    assert_eq!(mist[node_index([x, y, z])], 0.0, "node ({x}, {y}, {z})");
                    far += 1;
                }
            }
        }
    }
    assert!(far > 0);
}

#[test]
fn one_particle_on_a_node_follows_the_kernel() {
    let at = [16, 16, 16];
    let mist = mist_of(&[particle(node_position(at), R, 0)], 1.0);
    let peak = droplet_area() * KERNEL_PEAK;
    let own = mist[node_index(at)];
    assert!(
        (own - peak).abs() <= 1e-5 * peak,
        "own node: {own}, expected {peak}"
    );
    // One spacing away the kernel is a quarter of its peak. The fixed-point
    // sum rounds to 1 part in 2^20, which a quarter-size term feels more.
    let neighbour = mist[node_index([17, 16, 16])];
    let want = 0.25 * peak;
    assert!(
        (neighbour - want).abs() <= 1e-4 * want,
        "neighbour: {neighbour}, expected {want}"
    );
}

#[test]
fn empty_slots_make_no_mist() {
    let empty: Vec<_> = (0..64)
        .map(|id| particle(node_position([16, 16, 16]), 0.0, id))
        .collect();
    let mist = mist_of(&empty, 1.0);
    assert!(mist.iter().all(|&m| m == 0.0));
}
