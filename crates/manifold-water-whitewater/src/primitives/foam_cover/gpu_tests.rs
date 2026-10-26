//! GPU proofs for `node.foam_cover` against the closed forms of its kernel.

use super::*;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};

const R: f32 = 0.01;
const H: f32 = 0.05;
const NODES: [u32; 3] = [33, 9, 33];
const SIZE: [f32; 3] = [32.0 * H, 8.0 * H, 32.0 * H];
const KERNEL_PEAK: f32 = 10.0 / (7.0 * std::f32::consts::PI * H * H);

fn disc_area() -> f32 {
    std::f32::consts::PI * R * R
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

/// The cover over the 33 x 9 x 33 lattice centred on the origin.
fn cover_of(particles: &[FluidParticle], strength: f32) -> Vec<f32> {
    let mut harness = Harness::new();
    let mut node = FoamCover::new();
    let (particle_slot, _) = harness.array(particles, particles.len());
    let (cover_slot, _) = harness.array::<f32>(&[], 1);
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
        &[("cover", cover_slot)],
        &run_params,
    );
    assert!(errors.is_empty(), "{errors:?}");
    read(
        &harness.buffer(cover_slot),
        NODES.iter().product::<u32>() as usize,
    )
}

/// Foam discs of radius R on a square grid of spacing H / 2 across the whole
/// x-z extent of the lattice, in the fifth node plane.
fn sheet() -> Vec<FluidParticle> {
    let a = H / 2.0;
    let per_axis = (SIZE[0] / a).round() as u32 + 1;
    let y = node_position([0, 4, 0])[1];
    let mut particles = Vec::new();
    for iz in 0..per_axis {
        for ix in 0..per_axis {
            let x = -0.5 * SIZE[0] + a * ix as f32;
            let z = -0.5 * SIZE[2] + a * iz as f32;
            particles.push(particle([x, y, z], R, particles.len() as u32));
        }
    }
    particles
}

fn tau(cover: f32) -> f32 {
    -(1.0 - cover).ln()
}

#[test]
fn a_regular_sheet_covers_the_closed_form_fraction() {
    let a = H / 2.0;
    let expected = disc_area() / (a * a);
    let sheet = sheet();
    for strength in [1.0, 2.0] {
        let cover = cover_of(&sheet, strength);
        let mut checked = 0;
        for iz in 4..=28 {
            for ix in 4..=28 {
                let t = tau(cover[node_index([ix, 4, iz])]);
                let want = strength * expected;
                assert!(
                    (t - want).abs() <= 0.01 * want,
                    "strength {strength} node ({ix}, 4, {iz}): tau {t}, expected {want}"
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 25 * 25);
    }
}

#[test]
fn nodes_beyond_two_smoothing_lengths_read_exactly_zero() {
    let cover = cover_of(&sheet(), 1.0);
    for y in [0, 1, 7, 8] {
        for z in 0..NODES[2] {
            for x in 0..NODES[0] {
                assert_eq!(cover[node_index([x, y, z])], 0.0, "node ({x}, {y}, {z})");
            }
        }
    }
}

#[test]
fn one_particle_on_a_node_follows_the_kernel() {
    let at = [16, 4, 16];
    let cover = cover_of(&[particle(node_position(at), R, 0)], 1.0);
    let peak = disc_area() * KERNEL_PEAK;
    let own = tau(cover[node_index(at)]);
    assert!(
        (own - peak).abs() <= 1e-5 * peak,
        "own node: tau {own}, expected {peak}"
    );
    // One spacing away the kernel is a quarter of its peak. The fixed-point
    // sum rounds to 1 part in 2^20, which a quarter-size term feels more.
    let neighbour = tau(cover[node_index([17, 4, 16])]);
    let want = 0.25 * peak;
    assert!(
        (neighbour - want).abs() <= 1e-4 * want,
        "neighbour: tau {neighbour}, expected {want}"
    );
}

#[test]
fn empty_slots_cover_nothing() {
    let empty: Vec<_> = (0..64)
        .map(|id| particle(node_position([16, 4, 16]), 0.0, id))
        .collect();
    let cover = cover_of(&empty, 1.0);
    assert!(cover.iter().all(|&c| c == 0.0));
}
