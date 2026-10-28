//! The kernel's solid on display tiles across the origin: the walls against
//! their CPU twin `bounds_distance` at every held element, a posed body
//! against its analytic distance, and a free slot and the canonical element
//! reading 1e30.

use manifold_node_engine::bindings::Slot;
use manifold_node_engine::scene::world_tiles::{TILE_CELLS, pool_len};
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_liquid::bodies::{LiquidBody, LiquidShape, pack_distance_atlas};
use manifold_water_liquid::bounds::bounds_distance;
use manifold_water_liquid::testkit::world_box::table_words_of;

use super::LiquidSolidDistance;

/// Wall faces −X +Y −Z +Z (bits −X +X −Y +Y −Z +Z); +X and −Y are Open.
const MIXED_WALLS: u32 = 0b11_1001;
const H: f32 = 0.25;
const POOL: u32 = 5;
/// Four tiles round the origin; slot 4 is free.
const HELD: &[([i32; 3], u32)] = &[
    ([-1, -1, -1], 0),
    ([0, 0, 0], 1),
    ([-1, 0, -1], 2),
    ([0, -1, 0], 3),
];
const LO: [f32; 3] = [-0.6, -0.55, -0.7];
const HI: [f32; 3] = [0.35, 0.4, 0.45];
const FAR: f32 = 1.0e30;

fn elements() -> usize {
    pool_len(POOL) as usize
}

/// Where a pool element sits: its node at (p − ½)·h; `None` for a free slot
/// and the canonical element.
fn position_of(element: usize) -> Option<[f32; 3]> {
    let slot = (element / TILE_CELLS as usize) as u32;
    let (tile, _) = HELD.iter().find(|(_, s)| *s == slot)?;
    let local = element % TILE_CELLS as usize;
    let cell = [
        tile[0] * 8 + (local % 8) as i32,
        tile[1] * 8 + (local / 8 % 8) as i32,
        tile[2] * 8 + (local / 64) as i32,
    ];
    Some(cell.map(|c| (c as f32 - 0.5) * H))
}

struct Rig {
    harness: Harness,
    node: LiquidSolidDistance,
    bodies: Slot,
    shapes: Slot,
    atlas: Slot,
    nodes: Slot,
    triangles: Slot,
    tiles: Slot,
    out: Slot,
}

impl Rig {
    /// A rig over `bodies`, `shapes` and the packed `atlas`.
    fn new(bodies: &[LiquidBody], shapes: &[LiquidShape], atlas: &[u32]) -> Self {
        let mut harness = Harness::new();
        let (bodies, _) = harness.array(bodies, bodies.len());
        let (shapes, _) = harness.array(shapes, shapes.len());
        let (atlas, _) = harness.array(atlas, atlas.len());
        let (nodes, _) = harness.array::<manifold_node_engine::mesh::Vec4Vertex>(&[], 1);
        let (triangles, _) = harness.array::<manifold_node_engine::mesh::Vec4Vertex>(&[], 1);
        let words = table_words_of(POOL, HELD);
        let (tiles, _) = harness.array(&words, words.len());
        let (out, _) = harness.array::<f32>(&[], 1);
        Self {
            harness,
            node: LiquidSolidDistance::new(),
            bodies,
            shapes,
            atlas,
            nodes,
            triangles,
            tiles,
            out,
        }
    }

    /// The solid of every pool element with `walls` closed round [`LO`, `HI`]
    /// and `body_count` bodies in `rows` rows.
    fn solid(&mut self, walls: u32, body_count: u32, rows: u32) -> Vec<f32> {
        let set = params(&[
            ("cell_size", H),
            ("pool_tiles", POOL as f32),
            ("closed_faces", walls as f32),
            ("bounds_lo_x", LO[0]),
            ("bounds_lo_y", LO[1]),
            ("bounds_lo_z", LO[2]),
            ("bounds_hi_x", HI[0]),
            ("bounds_hi_y", HI[1]),
            ("bounds_hi_z", HI[2]),
            ("body_count", body_count as f32),
            ("rows", rows as f32),
        ]);
        let (_, errors) = self.harness.run(
            &mut self.node,
            &[
                ("bodies", self.bodies),
                ("shapes", self.shapes),
                ("atlas", self.atlas),
                ("collider_nodes", self.nodes),
                ("collider_triangles", self.triangles),
                ("tiles", self.tiles),
            ],
            &[("solid", self.out)],
            &set,
        );
        assert!(errors.is_empty(), "{errors:?}");
        read(&self.harness.buffer(self.out), elements())
    }
}

#[test]
fn liquid_solid_distance_walls_match_bounds_distance() {
    let mut rig = Rig::new(&[], &[], &[]);
    for walls in [MIXED_WALLS, 0] {
        let got = rig.solid(walls, 0, 0);
        let (mut inside, mut past) = (0, 0);
        for (e, &have) in got.iter().enumerate() {
            let want = position_of(e).map_or(FAR, |x| bounds_distance(x, LO, HI, walls));
            assert!(
                (have - want).abs() <= 1.0e-5 * want.abs().max(1.0),
                "walls {walls:#b} element {e}: {have} vs {want}"
            );
            inside += usize::from(position_of(e).is_some() && want > 0.0 && want < FAR);
            past += usize::from(want < 0.0);
        }
        if walls == 0 {
            // No Wall face reads free space everywhere, held or not.
            assert!(got.iter().all(|&v| v == FAR));
        } else {
            assert!(
                inside > 100 && past > 100,
                "{inside} inside, {past} past a Wall"
            );
        }
    }
}

#[test]
fn a_posed_body_and_the_walls_make_the_solid() {
    // A sphere of radius 0.3 in a 16³ lattice at 0.1 m, centred on its
    // origin, posed at `centre`.
    let (radius, spacing, dims) = (0.3_f32, 0.1_f32, 16_u32);
    let origin = -0.5 * spacing * (dims - 1) as f32;
    let mut values = Vec::new();
    for z in 0..dims {
        for y in 0..dims {
            for x in 0..dims {
                let p = [x, y, z].map(|c| origin + c as f32 * spacing);
                values.push((p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt() - radius);
            }
        }
    }
    let mut atlas = Vec::new();
    pack_distance_atlas(&values, &mut atlas);
    let shape = LiquidShape {
        origin_spacing: [origin, origin, origin, spacing],
        dims_x: dims,
        dims_y: dims,
        dims_z: dims,
        atlas_offset: 0,
        scale_min: [1.0; 4],
        ..Default::default()
    };
    let centre = [0.1, -0.05, 0.05];
    let body = LiquidBody {
        position_inv_mass: [centre[0], centre[1], centre[2], 0.0],
        rotation: [0.0, 0.0, 0.0, 1.0],
        accel_shape: [0.0, 0.0, 0.0, 0.0],
        ..LiquidBody::default()
    };
    let mut rig = Rig::new(&[body], &[shape], &atlas);
    let got = rig.solid(MIXED_WALLS, 1, 1);
    let reach = 0.5 * spacing * (dims - 1) as f32 - 0.05;
    let mut compared = 0;
    for (e, &have) in got.iter().enumerate() {
        let Some(x) = position_of(e) else {
            assert_eq!(have, FAR, "element {e}");
            continue;
        };
        let apart = [0, 1, 2].map(|a| x[a] - centre[a]);
        let rho = (apart[0] * apart[0] + apart[1] * apart[1] + apart[2] * apart[2]).sqrt();
        // Inside the lattice and outside the sphere, the trilinear sphere is
        // within a centimetre of the analytic one.
        if apart.iter().any(|c| c.abs() > reach) || rho < 0.3 {
            continue;
        }
        let want = bounds_distance(x, LO, HI, MIXED_WALLS).min(rho - radius);
        assert!(
            (have - want).abs() <= 0.012,
            "element {e} at {x:?}: {have} vs {want}"
        );
        compared += 1;
    }
    assert!(compared > 50, "{compared} elements compared");
}
