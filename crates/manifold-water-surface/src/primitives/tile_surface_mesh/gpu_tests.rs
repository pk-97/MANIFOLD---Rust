//! GPU proofs for `node.tile_surface_mesh`: a sphere across tile faces meshes
//! closed and consistently wound, on the sphere, with outward normals, and the
//! vertices past the live triangles are zero frame to frame; the relax leaves a
//! flat surface flat and a one-particle drop a drop.

use super::*;
use manifold_node_engine::scene::world_tiles::{TILE, TILE_CELLS, pool_len};
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_liquid::testkit::world_box::{BoxTable, table_words_of};
use std::collections::BTreeMap;

const H: f32 = 0.1;
/// Off every node, so no level set value is zero, and near the world origin,
/// so the sphere crosses tile faces on every axis.
const CENTRE: [f32; 3] = [0.031, -0.017, 0.052];

/// The pool of every tile of `table` holding `phi` at its nodes, a free slot
/// of water after them, and the canonical element far outside; with the words
/// of its table and its pool.
fn field_pool(table: &BoxTable, phi: impl Fn([f32; 3]) -> f32) -> (Vec<f32>, Vec<u32>, u32) {
    let pool_tiles = table.pool_tiles + 1;
    let mut pool = vec![3.0 * H; pool_len(pool_tiles) as usize];
    for (tile, slot) in table.held() {
        for local in 0..TILE_CELLS {
            let l = [local % TILE, local / TILE % TILE, local / (TILE * TILE)];
            let p: [f32; 3] =
                std::array::from_fn(|a| ((tile[a] * TILE as i32 + l[a] as i32) as f32 - 0.5) * H);
            pool[(slot * TILE_CELLS + local) as usize] = phi(p);
        }
    }
    let free = (table.pool_tiles * TILE_CELLS) as usize;
    pool[free..free + TILE_CELLS as usize].fill(-1.0);
    (pool, table_words_of(pool_tiles, &table.held()), pool_tiles)
}

struct Frame {
    live: usize,
    asked: u32,
    vertices: Vec<MeshVertex>,
}

/// The sphere's signed distance.
fn sphere(radius: f32) -> impl Fn([f32; 3]) -> f32 {
    move |p| {
        (0..3)
            .map(|a| (p[a] - CENTRE[a]).powi(2))
            .sum::<f32>()
            .sqrt()
            - radius
    }
}

fn run(
    h: &mut Harness,
    node: &mut TileSurfaceMesh,
    table: &BoxTable,
    phi: impl Fn([f32; 3]) -> f32,
) -> Frame {
    let (pool, words, pool_tiles) = field_pool(table, phi);
    let (levelset, _) = h.array(&pool, pool.len());
    let (tiles, _) = h.array(&words, words.len());
    let (vertices, _) = h.array::<MeshVertex>(&[], 3);
    let (_, errors) = h.run(
        node,
        &[("levelset", levelset), ("tiles", tiles)],
        &[("vertices", vertices)],
        &params(&[("cell_size", H), ("pool_tiles", pool_tiles as f32)]),
    );
    assert!(errors.is_empty(), "{errors:?}");
    let mesh = node.mesh.as_ref().expect("the node made its buffer");
    let counts = read::<u32>(&mesh.counts, 3);
    Frame {
        live: counts[0] as usize,
        asked: counts[1],
        vertices: read::<MeshVertex>(&mesh.vertices, 3 * mesh.capacity as usize),
    }
}

/// A position to a lattice of 1e-5 m, so an edge's vertex built by two tiles
/// keys the same.
fn key(p: [f32; 3]) -> [i64; 3] {
    p.map(|x| (f64::from(x) * 1e5).round() as i64)
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|i| a[i] * b[i]).sum()
}

#[test]
fn a_sphere_across_tile_faces_meshes_closed_on_the_sphere() {
    // Nodes -10..=10 on every axis: tiles -2..=1, a tile face through the
    // sphere at node 0 and at node 8 or -8 on each side.
    let table = BoxTable::new([-10; 3], [21; 3]);
    let mut h = Harness::new();
    let mut node = TileSurfaceMesh::new();
    let radius = 0.6;
    let frame = run(&mut h, &mut node, &table, sphere(radius));
    assert_eq!(frame.live as u32, frame.asked, "the sphere fits the buffer");
    assert!(frame.live > 500, "{} triangles", frame.live);

    // Every directed edge once and its reverse once: closed, one winding.
    let mut edges = BTreeMap::new();
    let mut winding = [0usize; 2];
    for t in 0..frame.live {
        let v = &frame.vertices[3 * t..3 * t + 3];
        for corner in 0..3 {
            let a = key(v[corner].position);
            let b = key(v[(corner + 1) % 3].position);
            *edges.entry((a, b)).or_insert(0) += 1;
        }
        let face = cross(
            sub(v[1].position, v[0].position),
            sub(v[2].position, v[0].position),
        );
        let out = sub(v[0].position, CENTRE);
        winding[usize::from(dot(face, out) > 0.0)] += 1;
    }
    for (&(a, b), &n) in &edges {
        assert_eq!(n, 1, "edge {a:?} {b:?} used {n} times one way");
        assert_eq!(edges.get(&(b, a)), Some(&1), "edge {a:?} {b:?} has no twin");
    }
    assert!(
        winding[0] == 0 || winding[1] == 0,
        "mixed winding {winding:?}"
    );

    // On the sphere to a fifth of a cell, the relax pulling it in a little
    // (a curved surface's neighbour mean lies inside it), normals outward.
    for vertex in &frame.vertices[..3 * frame.live] {
        let d = sub(vertex.position, CENTRE);
        let r = dot(d, d).sqrt();
        assert!(
            r < radius + 0.02 * H && r > radius - 0.2 * H,
            "{:?} at {r}",
            vertex.position
        );
        assert!(
            dot(vertex.normal, d) / r > 0.99,
            "{:?} normal {:?}",
            vertex.position,
            vertex.normal
        );
        assert_eq!(vertex.color, [1.0; 4]);
    }
    // Past the live triangles, nothing.
    assert!(
        frame.vertices[3 * frame.live..]
            .iter()
            .all(|v| bytemuck::bytes_of(v).iter().all(|&b| b == 0))
    );

    // A smaller sphere next frame: fewer triangles, the rest zeroed.
    let smaller = run(&mut h, &mut node, &table, sphere(0.35));
    assert!(
        smaller.live < frame.live / 2,
        "{} {}",
        smaller.live,
        frame.live
    );
    assert!(
        smaller.vertices[3 * smaller.live..]
            .iter()
            .all(|v| bytemuck::bytes_of(v).iter().all(|&b| b == 0)),
        "last frame's triangles past the live ones are zero"
    );
}

/// The relax moves a vertex to its neighbours' mean, so a flat surface stays
/// flat: a tilted plane's vertices, away from the held tiles' edge, lie on it.
/// And it never deletes water: a ball the size of one particle's blob (0.9
/// cells), which smoothing the level set would erase, still meshes, inside
/// its ball.
#[test]
fn the_relax_keeps_planes_flat_and_drops_whole() {
    let table = BoxTable::new([-10; 3], [21; 3]);
    let mut h = Harness::new();
    let mut node = TileSurfaceMesh::new();
    let normal = {
        let n = [0.3f32, 1.0, -0.45];
        let l = dot(n, n).sqrt();
        n.map(|c| c / l)
    };
    let plane = run(&mut h, &mut node, &table, |p| dot(sub(p, CENTRE), normal));
    let inner: Vec<_> = plane.vertices[..3 * plane.live]
        .iter()
        .filter(|v| v.position.iter().all(|c| c.abs() < 0.6))
        .collect();
    assert!(inner.len() > 300, "{} inner vertices", inner.len());
    for v in inner {
        let off = dot(sub(v.position, CENTRE), normal);
        assert!(off.abs() < 1e-4 * H, "{:?} {off} off the plane", v.position);
    }

    let radius = 0.9 * H;
    let drop = run(&mut h, &mut node, &table, sphere(radius));
    assert!(drop.live >= 8, "{} triangles", drop.live);
    for v in &drop.vertices[..3 * drop.live] {
        let d = sub(v.position, CENTRE);
        assert!(dot(d, d).sqrt() < radius, "{:?} outside the drop", v.position);
    }
}
