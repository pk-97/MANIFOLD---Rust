//! Engine value and fusion proofs on world tiles: the pooled upwind sweep and
//! surface distance against the CPU rules over a dense domain, the pool atoms'
//! substep and outflow paths, and the substep history across table changes.
//! A domain is the tiles a table holds plus a ring of absent tiles round
//! them, so a cell of a held tile has every neighbour in the dense array, an
//! absent tile holding the canonical value as the table presents it.
use super::advect_whitewater::AdvectWhitewater;
use super::whitewater_cpu::CANONICAL_SOLID;
use super::whitewater_engine_cpu::{reinitialize, upwind};
use super::whitewater_particle_cpu::{Box3, face_index};
use manifold_gpu::GpuBuffer;
use manifold_node_engine::freeze::codegen::InputSource;
use manifold_node_engine::scene::world_tiles::pool_len;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_node_engine::testkit::fused_atoms::{fused, member, run};
use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_water_liquid::primitives::{
    offset_lattice::OffsetLattice, upwind_distance::UpwindDistance,
    whitewater_distance::SurfaceDistance,
};
use manifold_water_liquid::substep_history::{PooledFaces, SubstepHistory};
use manifold_water_liquid::testkit::world_box::{BoxTable, pool_index_under, table_words_of};
use manifold_water_liquid::whitewater::WhitewaterParticle;
use {
    super::whitewater_pool_cpu::Advect, super::whitewater_pool_cpu::Fields,
    crate::primitives::whitewater_pool_cpu as cpu,
    manifold_node_engine::exec::effect_node::NodeInstanceId,
};

/// The tiles a table holds and a ring of absent tiles round them as a dense
/// box, its first cell the ring's.
struct Domain {
    words: Vec<u32>,
    pool_tiles: u32,
    tile_min: [i32; 3],
    /// Tiles a side, ring included.
    tiles: [usize; 3],
}

impl Domain {
    fn new(table: &BoxTable) -> Self {
        Self {
            words: table.words.clone(),
            pool_tiles: table.pool_tiles,
            tile_min: table.tile_min.map(|t| t - 1),
            tiles: table.tiles.map(|t| t as usize + 2),
        }
    }

    fn dims(&self) -> [usize; 3] {
        self.tiles.map(|t| 8 * t)
    }

    /// The slot the table gives domain tile `tile`, if it holds it.
    fn slot(&self, tile: [usize; 3]) -> Option<usize> {
        let cell = std::array::from_fn(|a| (self.tile_min[a] + tile[a] as i32) * 8);
        let at = pool_index_under(&self.words, self.pool_tiles, cell);
        (at + 1 < pool_len(self.pool_tiles) as usize).then_some(at / 512)
    }

    /// Every cell of the domain out of a pooled field: a held tile's cells as
    /// its slot has them, an absent tile's the canonical element.
    fn lift<T: Copy>(&self, pool: &[T]) -> Vec<T> {
        let [nx, ny, nz] = self.dims();
        let mut dense = vec![pool[pool.len() - 1]; nx * ny * nz];
        for t in 0..self.tiles.iter().product::<usize>() {
            let tile = [
                t % self.tiles[0],
                t / self.tiles[0] % self.tiles[1],
                t / (self.tiles[0] * self.tiles[1]),
            ];
            let Some(slot) = self.slot(tile) else {
                continue;
            };
            for l in 0..512 {
                let c = [
                    tile[0] * 8 + l % 8,
                    tile[1] * 8 + l / 8 % 8,
                    tile[2] * 8 + l / 64,
                ];
                dense[c[0] + nx * (c[1] + ny * c[2])] = pool[slot * 512 + l];
            }
        }
        dense
    }

    /// The domain tile of dense index `i`.
    fn tile_of(&self, i: usize) -> usize {
        let [nx, ny, _] = self.dims();
        let c = [i % nx / 8, i / nx % ny / 8, i / (nx * ny) / 8];
        c[0] + self.tiles[0] * (c[1] + self.tiles[1] * c[2])
    }
}

/// What `SurfaceDistance` computes over `domain` from the dense `source`:
/// the sweeps run in every cell of a held tile that has a cell within 2h of
/// the surface, or shares a face with a tile that does; every other cell
/// reads 5h.
fn tile_surface_distance(domain: &Domain, source: &[f32], h: f32) -> Vec<f32> {
    let tiles = domain.tiles;
    let held: Vec<bool> = (0..tiles.iter().product::<usize>())
        .map(|t| {
            domain
                .slot([
                    t % tiles[0],
                    t / tiles[0] % tiles[1],
                    t / (tiles[0] * tiles[1]),
                ])
                .is_some()
        })
        .collect();
    let mut flagged = vec![false; held.len()];
    for (i, d) in source.iter().enumerate() {
        let t = domain.tile_of(i);
        flagged[t] |= held[t] && d.abs() < 2.0 * h;
    }
    let strides = [1, tiles[0], tiles[0] * tiles[1]];
    let valid: Vec<bool> = (0..source.len())
        .map(|i| {
            let t = domain.tile_of(i);
            held[t] && (flagged[t] || strides.iter().any(|s| flagged[t - s] || flagged[t + s]))
        })
        .collect();
    let mut out = reinitialize(source, domain.dims(), h, &valid);
    for (d, valid) in out.iter_mut().zip(valid) {
        if !valid {
            *d = 5.0 * h;
        }
    }
    out
}

#[test]
fn whitewater_engine_distance_values_and_fusion() {
    let mut h = Harness::new();
    // 10 x 9 x 8 cells across tiles -1 and 0 on every axis.
    let table = BoxTable::new([-5, -2, -9], [10, 9, 8]);
    let pool_tiles = table.pool_tiles;
    let len = pool_len(pool_tiles) as usize;
    let domain = Domain::new(&table);
    let count = 10 * 9 * 8;
    let dense: Vec<f32> = (0..count)
        .map(|i| {
            let (x, y, z) = ((i % 10) as f32, (i / 10 % 9) as f32, (i / 90) as f32);
            0.2 * x - 0.7 + 0.05 * y - 0.03 * z
        })
        .collect();
    let input = table.to_pool(&dense, 5.0);
    let valid = table.to_pool(&vec![1u32; count], 0);
    let levelset = h.array(&input, len);
    let flags = h.array(&valid, len);
    let tiles = h.array(&table.words, table.words.len());
    let values = [
        ("cell_size", 1.0),
        ("pool_tiles", pool_tiles as f32),
        ("offset", 0.125),
    ];
    let got: Vec<f32> = run(
        &mut h,
        &mut UpwindDistance::new(),
        &[
            ("levelset", levelset.0),
            ("valid", flags.0),
            ("tiles", tiles.0),
        ],
        len,
        &params(&values),
    );
    let before = domain.lift(&input);
    let valid_cells: Vec<bool> = domain.lift(&valid).iter().map(|&v| v == 1).collect();
    let want = upwind(&before, domain.dims(), 1.0, &valid_cells);
    for (i, (a, b)) in domain.lift(&got).iter().zip(&want).enumerate() {
        assert!((a - b).abs() < 2e-6, "cell {i}: GPU {a} CPU {b}");
    }
    assert!(
        want.iter().zip(&before).filter(|(a, b)| a != b).count() > count / 2,
        "the sweep moved the box"
    );
    let intermediate = h.array(&got, len);
    let standalone: Vec<f32> = run(
        &mut h,
        &mut OffsetLattice::new(),
        &[("levelset", intermediate.0)],
        len,
        &params(&values),
    );
    let external = InputSource::External;
    let combined: Vec<f32> = fused(
        &mut h,
        vec![
            member::<UpwindDistance>(0, vec![external(0), external(1), external(2)]),
            member::<OffsetLattice>(1, vec![InputSource::Node(NodeInstanceId(0))]),
        ],
        &[&levelset.1, &flags.1, &tiles.1],
        len,
        &values,
    );
    assert_eq!(combined, standalone);

    let mut stage = SurfaceDistance::default();
    stage.prepare(&h.device);
    stage.reserve(&h.device, pool_tiles).unwrap();
    let mut corner = vec![8.0; count];
    corner[0] = 0.1;
    for dense in [dense, corner, vec![-5.0; count]] {
        let pooled = table.to_pool(&dense, 5.0);
        let got = surface_on_gpu(&mut h, &stage, &table, &pooled, &tiles.1, 1.0, None);
        let want = tile_surface_distance(&domain, &domain.lift(&pooled), 1.0);
        for (i, (a, b)) in domain.lift(&got).iter().zip(&want).enumerate() {
            assert!((a - b).abs() < 2e-6, "cell {i}: stage {a} CPU {b}");
        }
        // Whatever the field, no cell of an absent tile or of the canonical
        // element reads anything but 5h.
        assert_eq!(got[len - 1], 5.0);
    }
    // The corner cell alone is on the surface: its tile and the three tiles
    // it shares a face with sweep, every other tile reads 5h.
    let mut corner = vec![8.0; count];
    corner[0] = 0.1;
    let pooled = table.to_pool(&corner, 5.0);
    let want = tile_surface_distance(&domain, &domain.lift(&pooled), 1.0);
    let swept = want.iter().filter(|&&d| d != 5.0).count();
    assert!(
        swept > 0 && swept < want.len() / 4,
        "{swept} of {} cells sweep",
        want.len()
    );
}

/// The box's face arrays as one record per node of a box of `dims` nodes
/// (cells one fewer a side, no padding): a face has weight where the arrays
/// hold it.
fn records(faces: &[Vec<f32>; 3], dims: [u32; 3]) -> Vec<FaceSample> {
    let cells = dims.map(|n| n - 1);
    let mut out = Vec::new();
    for z in 0..dims[2] as i32 {
        for y in 0..dims[1] as i32 {
            for x in 0..dims[0] as i32 {
                let mut record = FaceSample::default();
                for (axis, axis_faces) in faces.iter().enumerate() {
                    if let Some(i) = face_index([x, y, z], axis, 0, cells) {
                        record.velocity[axis] = axis_faces[i];
                        record.weight[axis] = 1.0;
                    }
                }
                out.push(record);
            }
        }
    }
    out
}

/// A history row from pooled face records: the velocity of each weighted
/// face and a zero.
fn row_of(pool: &[FaceSample]) -> Vec<f32> {
    pool.iter()
        .flat_map(|s| {
            let v = |a: usize| {
                if s.weight[a] > 0.0 {
                    s.velocity[a]
                } else {
                    0.0
                }
            };
            [v(0), v(1), v(2), 0.0]
        })
        .collect()
}

#[test]
fn whitewater_engine_substep_force_hit_values_and_fusion() {
    let mut h = Harness::new();
    // The box's first cell below zero on every axis, crossing tile faces; its
    // low corner is where the force lattice starts.
    let origin = [-5, -4, -9];
    let table = BoxTable::new(origin, [9; 3]);
    let pool_tiles = table.pool_tiles;
    let len = pool_len(pool_tiles) as usize;
    let low = origin.map(|c| c as f32 - 0.5);
    let pool: Vec<_> = [0, 1, 2, 4]
        .into_iter()
        .map(|kind| WhitewaterParticle {
            position_lifetime: [low[0] + 4.0, low[1] + 4.0, low[2] + 4.0, 1.0],
            velocity: [3.0, 0.0, 0.0],
            kind,
            id: 128,
            ..Default::default()
        })
        .collect();
    let p = h.array(&pool, 4);
    let solid = vec![10.0; 729];
    let s = h.array(&table.to_pool(&solid, CANONICAL_SOLID), len);
    let old: [Vec<f32>; 3] = std::array::from_fn(|a| vec![if a == 0 { 1.0 } else { 0.0 }; 576]);
    let new: [Vec<f32>; 3] = std::array::from_fn(|a| vec![if a == 0 { 3.0 } else { 0.0 }; 576]);
    let pooled =
        |faces: &[Vec<f32>; 3]| table.to_pool(&records(faces, [9; 3]), FaceSample::default());
    let (old_pool, new_pool) = (pooled(&old), pooled(&new));
    let current = h.array(&new_pool, len);
    let history: Vec<f32> = [&old_pool, &new_pool, &new_pool]
        .into_iter()
        .flat_map(|pool| row_of(pool))
        .collect();
    let histories = h.array(&history, history.len());
    let tiles = h.array(&table.words, table.words.len());
    // Last slot is inactive, so neither its face values nor event may advance.
    let schedule = [
        0.01f32,
        0.01,
        0.0,
        0.0,
        0.03,
        0.04,
        f32::from_bits(0x80000000),
        0.0,
        0.0,
        0.04,
        f32::from_bits(0x80000000),
        0.0,
    ];
    let clock = h.array(&schedule, 12);
    let force: Vec<_> = (0..8)
        .flat_map(|_| [1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])
        .collect();
    let impulse: Vec<_> = (0..8).flat_map(|_| [2.0f32, 0.0, 0.0, 0.0]).collect();
    let f = h.array(&force, 64);
    let i = h.array(&impulse, 32);
    let values = vec![
        ("bounds_lo_x", low[0]),
        ("bounds_lo_y", low[1]),
        ("bounds_lo_z", low[2]),
        ("gravity_x", 0.0),
        ("gravity_y", -10.0),
        ("gravity_z", 0.0),
        ("dt", 0.04),
        ("substep_count", 3.0),
        ("field_nodes_x", 2.0),
        ("field_nodes_y", 2.0),
        ("field_nodes_z", 2.0),
        ("field_spacing", 8.0),
        ("force_lattices", 1.0),
        ("cell_size", 1.0),
        ("pool_tiles", pool_tiles as f32),
    ];
    let got: Vec<WhitewaterParticle> = run(
        &mut h,
        &mut AdvectWhitewater::new(),
        &[
            ("pool", p.0),
            ("faces", current.0),
            ("solid", s.0),
            ("substep_schedule", clock.0),
            ("substep_faces", histories.0),
            ("forces", f.0),
            ("impulses", i.0),
            ("tiles", tiles.0),
        ],
        4,
        &params(&values),
    );
    let grid = Box3 {
        cells: [8; 3],
        center: low.map(|l| l + 4.0),
        size: [8.0; 3],
    };
    for (index, particle) in pool.iter().enumerate() {
        let mut want = *particle;
        for (dt, faces, event) in [(0.01, &old, false), (0.03, &new, true)] {
            if event && want.kind != 1 {
                want.velocity[0] += 2.0;
            }
            let fields = Fields {
                faces: faces.each_ref().map(Vec::as_slice),
                face_cells: [8; 3],
                solid: &solid,
            };
            want = cpu::advect(
                want,
                &fields,
                &grid,
                Advect {
                    dt,
                    gravity: [1.0, -10.0, 0.0],
                    ..Advect::flip()
                },
                None,
            )
            .0;
        }
        for (a, b) in got[index]
            .position_lifetime
            .iter()
            .chain(&got[index].velocity)
            .zip(want.position_lifetime.iter().chain(&want.velocity))
        {
            assert!(
                (a - b).abs() < 2e-5,
                "kind {} GPU {:?} CPU {want:?}",
                particle.kind,
                got[index]
            );
        }
    }
    let combined: Vec<WhitewaterParticle> = fused(
        &mut h,
        vec![member::<AdvectWhitewater>(
            0,
            (0..8).map(InputSource::External).collect(),
        )],
        &[
            &p.1,
            &current.1,
            &s.1,
            &clock.1,
            &histories.1,
            &f.1,
            &i.1,
            &tiles.1,
        ],
        4,
        &values,
    );
    assert_eq!(
        bytemuck::cast_slice::<_, u8>(&combined),
        bytemuck::cast_slice::<_, u8>(&got)
    );
}

#[test]
fn whitewater_engine_outflow_values_and_fusion() {
    use super::keep_whitewater::KeepWhitewater;
    use manifold_water_liquid::{
        bodies::LiquidBody, bodies::LiquidShape, bodies::pack_distance_atlas,
        fluid_particles::CellRange,
    };
    let mut h = Harness::new();
    // The region and the particles in scene metres, the box of cells 0..8
    // held as tiles 0 and 1 on each axis.
    let table = BoxTable::new([0; 3], [9; 3]);
    let pool_tiles = table.pool_tiles;
    let len = pool_len(pool_tiles) as usize;
    let pool: Vec<_> = [0, 1, 2, 4]
        .into_iter()
        .flat_map(|kind| {
            [3.5, 4.0, 4.5].map(|x| WhitewaterParticle {
                position_lifetime: [x, 4.0, 4.0, 1.0],
                kind,
                ..Default::default()
            })
        })
        .collect();
    let p = h.array(&pool, 12);
    let range = h.array(&vec![CellRange { start: 0, count: 0 }; len], len);
    let order = h.array(&[0u32], 1);
    let solid = h.array(&table.to_pool(&[10.0f32; 729], CANONICAL_SOLID), len);
    let tiles = h.array(&table.words, table.words.len());
    let body = LiquidBody {
        rotation: [0.0, 0.0, 0.0, 1.0],
        angular_velocity: [0.0, 0.0, 0.0, 3.0],
        ..Default::default()
    };
    let shape = LiquidShape {
        origin_spacing: [0.0, 0.0, 0.0, 8.0],
        dims_x: 2,
        dims_y: 2,
        dims_z: 2,
        scale_min: [1.0; 4],
        ..Default::default()
    };
    let regions = h.array(&[body], 1);
    let shapes = h.array(&[shape], 1);
    let mut atlas = Vec::new();
    pack_distance_atlas(&[-4.0, 4.0, -4.0, 4.0, -4.0, 4.0, -4.0, 4.0], &mut atlas);
    let a = h.array(&atlas, 4);
    let values = [
        ("cell_size", 1.0),
        ("pool_tiles", pool_tiles as f32),
        ("region_count", 1.0),
    ];
    let got: Vec<u32> = run(
        &mut h,
        &mut KeepWhitewater::new(),
        &[
            ("pool", p.0),
            ("binned", p.0),
            ("cell_ranges", range.0),
            ("order", order.0),
            ("solid", solid.0),
            ("regions", regions.0),
            ("shapes", shapes.0),
            ("atlas", a.0),
            ("tiles", tiles.0),
        ],
        12,
        &params(&values),
    );
    // Engine removes d<0: surface exactly at zero survives, all four types.
    assert_eq!(got, [0, 1, 1].repeat(4));
    let combined: Vec<u32> = fused(
        &mut h,
        vec![member::<KeepWhitewater>(
            0,
            (0..9).map(InputSource::External).collect(),
        )],
        &[
            &p.1, &p.1, &range.1, &order.1, &solid.1, &regions.1, &shapes.1, &a.1, &tiles.1,
        ],
        12,
        &values,
    );
    assert_eq!(combined, got);
}

/// Whitewater leaves where the liquid's move removes water: beyond the plane
/// of an Open face, on the side it opens to, and below the kill height. With
/// neither, every live slot stays; the fused kernel agrees.
#[test]
fn whitewater_leaves_past_open_faces_and_below_the_kill_height() {
    use super::keep_whitewater::KeepWhitewater;
    use manifold_water_liquid::fluid_particles::CellRange;
    let mut h = Harness::new();
    let table = BoxTable::new([0; 3], [9; 3]);
    let pool_tiles = table.pool_tiles;
    let len = pool_len(pool_tiles) as usize;
    // Rows y 3.5 and 4.5, columns x 3.5, 4.0 and 4.5; foam, bubble and spray.
    let pool: Vec<_> = [3.5, 4.5]
        .into_iter()
        .flat_map(|y| {
            [(3.5, 0), (4.0, 1), (4.5, 2)].map(|(x, kind)| WhitewaterParticle {
                position_lifetime: [x, y, 4.0, 1.0],
                kind,
                ..Default::default()
            })
        })
        .collect();
    let p = h.array(&pool, 6);
    let range = h.array(&vec![CellRange { start: 0, count: 0 }; len], len);
    let order = h.array(&[0u32], 1);
    let solid = h.array(&table.to_pool(&[10.0f32; 729], CANONICAL_SOLID), len);
    let tiles = h.array(&table.words, table.words.len());
    let base = [("cell_size", 1.0), ("pool_tiles", pool_tiles as f32)];
    // The +x face opens at 4.2 and the -x face at 3.8: the outer columns go.
    let open = [
        base[0],
        base[1],
        ("open_faces", 0b11 as f32),
        ("bounds_lo_x", 3.8),
        ("bounds_hi_x", 4.2),
        ("bounds_hi_y", 8.0),
        ("bounds_hi_z", 8.0),
    ];
    // A Wall face (bit clear) keeps what is past its plane.
    let mut wall = open;
    wall[2] = ("open_faces", 0b10 as f32);
    // The kill height at 4.0 takes the lower row.
    let killed = [base[0], base[1], ("kill_height", 4.0)];
    type Case<'a> = (&'a [(&'static str, f32)], [u32; 6]);
    let cases: [Case; 4] = [
        (&base, [1; 6]),
        (&open, [0, 1, 0, 0, 1, 0]),
        (&wall, [1, 1, 0, 1, 1, 0]),
        (&killed, [0, 0, 0, 1, 1, 1]),
    ];
    for (values, want) in cases {
        let got: Vec<u32> = run(
            &mut h,
            &mut KeepWhitewater::new(),
            &[
                ("pool", p.0),
                ("binned", p.0),
                ("cell_ranges", range.0),
                ("order", order.0),
                ("solid", solid.0),
                ("tiles", tiles.0),
            ],
            6,
            &params(values),
        );
        assert_eq!(got, want, "{values:?}");
        let combined: Vec<u32> = fused(
            &mut h,
            vec![member::<KeepWhitewater>(
                0,
                (0..9).map(InputSource::External).collect(),
            )],
            &[
                &p.1, &p.1, &range.1, &order.1, &solid.1, &solid.1, &solid.1, &solid.1, &tiles.1,
            ],
            6,
            values,
        );
        assert_eq!(combined, got, "{values:?}");
    }
}

/// The pooled field of `dense` (x fastest, its first cell at world cell
/// `origin`, `dims` cells a side) under `words`: the box's cells in the slots
/// the table gives them, `canon` elsewhere. A cell of a tile the table lacks
/// is dropped.
fn pool_under<T: Copy>(
    dense: &[T],
    words: &[u32],
    pool_tiles: u32,
    origin: [i32; 3],
    dims: [u32; 3],
    canon: T,
) -> Vec<T> {
    let mut pool = vec![canon; pool_len(pool_tiles) as usize];
    for (i, &value) in dense.iter().enumerate() {
        let i = i as u32;
        let cell = [
            origin[0] + (i % dims[0]) as i32,
            origin[1] + (i / dims[0] % dims[1]) as i32,
            origin[2] + (i / (dims[0] * dims[1])) as i32,
        ];
        let at = pool_index_under(words, pool_tiles, cell);
        if at + 1 < pool.len() {
            pool[at] = value;
        }
    }
    pool
}

/// A box of 9³ face records: node `i` has the velocity (i, i + 1000, i +
/// 2000) plus `bias`, no weight on its y face where `i % 5 == 0` and none on
/// its z face where `i % 7 == 0`.
fn box_faces(bias: f32) -> Vec<FaceSample> {
    (0..729)
        .map(|i| FaceSample {
            velocity: [
                i as f32 + bias,
                (i + 1000) as f32 + bias,
                (i + 2000) as f32 + bias,
                0.0,
            ],
            weight: [
                1.0,
                if i % 5 == 0 { 0.0 } else { 1.0 },
                if i % 7 == 0 { 0.0 } else { 1.0 },
                0.0,
            ],
        })
        .collect()
}

/// A substep as the step captured it: its table and its pooled faces.
struct Substep {
    words: Vec<u32>,
    faces: Vec<FaceSample>,
}

/// Every substep captured as a row in order (an active plan each), then the
/// rows remapped onto `final_words`: the history's faces as floats, and its
/// schedule words.
fn capture_and_remap(
    h: &mut Harness,
    pool_tiles: u32,
    substeps: &[Substep],
    final_words: &[u32],
) -> (Vec<f32>, Vec<u32>) {
    let len = pool_len(pool_tiles) as usize;
    let rows = substeps.len();
    let tables: Vec<_> = substeps
        .iter()
        .map(|s| h.array(&s.words, s.words.len()))
        .collect();
    let faces: Vec<_> = substeps.iter().map(|s| h.array(&s.faces, len)).collect();
    let plans: Vec<_> = (0..rows)
        .map(|k| {
            let (step, elapsed, event) = [
                (0.01f32, 0.02f32, 0),
                (0.03, 0.04, 0x80000002),
                (0.05, 0.06, 0),
            ][k];
            h.array(
                &[step.to_bits(), elapsed.to_bits(), 0, 0, 0, 0, 0, event],
                8,
            )
        })
        .collect();
    let final_table = h.array(final_words, final_words.len());
    let mut history = SubstepHistory::default();
    history.prepare(&h.device);
    history.reserve(&h.device, pool_tiles, rows as u32).unwrap();
    let mut enc = h.device.create_encoder("whitewater-engine-seam");
    for k in 0..rows {
        history.capture(
            &mut enc,
            k as u32,
            &plans[k].1,
            &PooledFaces {
                faces: &faces[k].1,
                table: &tables[k].1,
                pool_tiles,
            },
        );
    }
    history.remap(&mut enc, &final_table.1, rows as u32);
    let staged = h.array::<f32>(&[], rows * len * 4);
    enc.copy_buffer_to_buffer(
        history.faces.as_ref().unwrap(),
        &staged.1,
        (rows * len * 16) as u64,
    );
    enc.commit_and_wait_completed();
    (
        read(&staged.1, rows * len * 4),
        read(history.schedule.as_ref().unwrap(), rows * 4),
    )
}

/// Every cell of tiles -2 to 1 on x and y and -3 to 0 on z: the tiles the
/// seam tests hold and the absent ring round them.
fn volume() -> impl Iterator<Item = [i32; 3]> {
    (-24..8).flat_map(|z| (-16..16).flat_map(move |y| (-16..16).map(move |x| [x, y, z])))
}

/// After the remap, each cell of a tile the final table holds reads, through
/// the final table, what its substep read through its own: the weighted face
/// velocity of that record. Returns how many of those values are not zero.
fn assert_remapped(
    history: &[f32],
    substeps: &[Substep],
    final_words: &[u32],
    pool_tiles: u32,
) -> usize {
    let len = pool_len(pool_tiles) as usize;
    let mut nonzero = 0;
    for (k, substep) in substeps.iter().enumerate() {
        for cell in volume() {
            let at = pool_index_under(final_words, pool_tiles, cell);
            if at + 1 == len {
                continue;
            }
            let own = &substep.faces[pool_index_under(&substep.words, pool_tiles, cell)];
            for axis in 0..3 {
                let want = if own.weight[axis] > 0.0 {
                    own.velocity[axis]
                } else {
                    0.0
                };
                assert_eq!(
                    history[(k * len + at) * 4 + axis],
                    want,
                    "row {k} cell {cell:?} axis {axis}"
                );
                nonzero += usize::from(want != 0.0);
            }
            assert_eq!(history[(k * len + at) * 4 + 3], 0.0);
        }
        // The canonical element is the substep's own.
        let canonical = &substep.faces[len - 1];
        for axis in 0..3 {
            let want = if canonical.weight[axis] > 0.0 {
                canonical.velocity[axis]
            } else {
                0.0
            };
            assert_eq!(history[(k * len + len - 1) * 4 + axis], want);
        }
    }
    nonzero
}

/// A tile that changes slot in the second substep: the first substep's
/// velocity read through the final table is still the first substep's, not
/// the second's, on both sides of the tile face on x.
#[test]
fn whitewater_engine_seam_publishes_accepted_substeps() {
    let mut h = Harness::new();
    // A table whose lattice starts below zero on every axis: the records
    // cross tile faces on all three and the world origin on x and y, the box
    // straddling tiles -1 and 0 on x.
    let origin = [-5, -4, -9];
    let table = BoxTable::new(origin, [9; 3]);
    let pool_tiles = table.pool_tiles;
    let len = pool_len(pool_tiles) as usize;
    // The same tiles in the opposite slot order: every tile changes slot.
    let held = table.held();
    let swapped: Vec<_> = held
        .iter()
        .map(|&(tile, slot)| (tile, pool_tiles - 1 - slot))
        .collect();
    assert!(held.iter().zip(&swapped).all(|(a, b)| a.1 != b.1));
    let words = table_words_of(pool_tiles, &swapped);
    let substeps = [
        Substep {
            faces: pool_under(
                &box_faces(0.0),
                &table.words,
                pool_tiles,
                origin,
                [9; 3],
                FaceSample::default(),
            ),
            words: table.words.clone(),
        },
        Substep {
            faces: pool_under(
                &box_faces(10_000.0),
                &words,
                pool_tiles,
                origin,
                [9; 3],
                FaceSample::default(),
            ),
            words: words.clone(),
        },
    ];
    let (history, schedule) = capture_and_remap(&mut h, pool_tiles, &substeps, &words);
    assert_eq!(
        schedule,
        [
            0.01f32.to_bits(),
            0.02f32.to_bits(),
            0,
            0,
            0.03f32.to_bits(),
            0.04f32.to_bits(),
            0x80000002,
            0
        ]
    );
    let nonzero = assert_remapped(&history, &substeps, &words, pool_tiles);
    // Every weighted velocity of the box's records, in both substeps, was read
    // back through the final table.
    let weighted = |bias: f32| -> usize {
        box_faces(bias)
            .iter()
            .map(|s| {
                (0..3)
                    .filter(|&a| s.weight[a] > 0.0 && s.velocity[a] != 0.0)
                    .count()
            })
            .sum()
    };
    assert_eq!(nonzero, weighted(0.0) + weighted(10_000.0));
    // Cells either side of the tile face between tiles -1 and 0 on x: node
    // (4, 3, 7) and (5, 3, 7) of the box.
    for (cell, node) in [([-1, -1, -2], 598), ([0, -1, -2], 599)] {
        let at = pool_index_under(&words, pool_tiles, cell);
        let first = pool_index_under(&table.words, pool_tiles, cell);
        assert_ne!(at / 512, first / 512, "{cell:?} changed slot");
        for (axis, base) in [0, 1000, 2000].into_iter().enumerate() {
            assert_eq!(
                history[at * 4 + axis],
                (node + base) as f32,
                "substep 0 at {cell:?} axis {axis}"
            );
            assert_eq!(
                history[(len + at) * 4 + axis],
                (node + base) as f32 + 10_000.0,
                "substep 1 at {cell:?} axis {axis}"
            );
        }
    }
}

/// A tile the final table lacks drops out; a tile the substep lacked reads
/// its canonical element, and the tiles both hold keep their velocities
/// whatever slot each table gave them.
#[test]
fn whitewater_engine_seam_drops_and_gains_tiles() {
    let mut h = Harness::new();
    let origin = [-5, -4, -9];
    let table = BoxTable::new(origin, [9; 3]);
    let pool_tiles = table.pool_tiles;
    let len = pool_len(pool_tiles) as usize;
    let dropped = [0, 0, -1];
    let gained = [1, 0, -1];
    let held = table.held();
    assert!(held.iter().any(|&(tile, _)| tile == dropped));
    assert!(held.iter().all(|&(tile, _)| tile != gained));
    // The second substep holds the gained tile where the dropped one was,
    // and every slot rotated.
    let rotated: Vec<_> = held
        .iter()
        .map(|&(tile, slot)| {
            let tile = if tile == dropped { gained } else { tile };
            (tile, (slot + 3) % pool_tiles)
        })
        .collect();
    let words = table_words_of(pool_tiles, &rotated);
    let canonical = |v: f32| FaceSample {
        velocity: [v, v + 1.0, v + 2.0, 0.0],
        weight: [1.0, 1.0, 1.0, 0.0],
    };
    let mut first = pool_under(
        &box_faces(0.0),
        &table.words,
        pool_tiles,
        origin,
        [9; 3],
        FaceSample::default(),
    );
    let mut second = pool_under(
        &box_faces(10_000.0),
        &words,
        pool_tiles,
        origin,
        [9; 3],
        FaceSample::default(),
    );
    *first.last_mut().unwrap() = canonical(7.0);
    *second.last_mut().unwrap() = canonical(17.0);
    let substeps = [
        Substep {
            words: table.words.clone(),
            faces: first,
        },
        Substep {
            words: words.clone(),
            faces: second,
        },
    ];
    let (history, _) = capture_and_remap(&mut h, pool_tiles, &substeps, &words);
    let nonzero = assert_remapped(&history, &substeps, &words, pool_tiles);
    assert!(nonzero > 3_000, "{nonzero} velocities checked");
    let read_cell = |row: usize, cell: [i32; 3]| -> [f32; 3] {
        let at = pool_index_under(&words, pool_tiles, cell);
        std::array::from_fn(|a| history[(row * len + at) * 4 + a])
    };
    // Cell (9, 2, -3) is in the gained tile: substep 0 lacked it, substep 1
    // holds it with no weighted face.
    assert_eq!(read_cell(0, [9, 2, -3]), [7.0, 8.0, 9.0]);
    assert_eq!(read_cell(1, [9, 2, -3]), [0.0; 3]);
    // Cell (1, 1, -2) is in the dropped tile: through the final table it is
    // the canonical element, not the node (6, 5, 7)'s velocity.
    assert_eq!(read_cell(0, [1, 1, -2]), [7.0, 8.0, 9.0]);
    assert_eq!(read_cell(1, [1, 1, -2]), [17.0, 18.0, 19.0]);
}

#[test]
fn whitewater_engine_seam_skips_inactive_rows_and_resumes_capture() {
    let mut h = Harness::new();
    // A uniform grid pooled under a world-tile table at a negative origin, so
    // every face reads `velocity`.
    let origin = [-3, -8, 2];
    let table = BoxTable::new(origin, [9; 3]);
    let pool_tiles = table.pool_tiles;
    let len = pool_len(pool_tiles) as usize;
    let faces = |velocity| {
        let sample = FaceSample {
            velocity,
            weight: [1.0; 4],
        };
        table.to_pool(&vec![sample; 729], sample)
    };
    let tiles = h.array(&table.words, table.words.len());
    // The table the frame ends on: the same tiles, the opposite slot order.
    let swapped: Vec<_> = table
        .held()
        .iter()
        .map(|&(tile, slot)| (tile, pool_tiles - 1 - slot))
        .collect();
    let final_words = table_words_of(pool_tiles, &swapped);
    let final_table = h.array(&final_words, final_words.len());
    let [first, poison, resumed] =
        [[1.0, 2.0, 3.0, 0.0], [f32::NAN; 4], [4.0, 5.0, 6.0, 0.0]].map(|velocity| {
            let pool = faces(velocity);
            h.array(&pool, pool.len())
        });
    let active = h.array(&[0.01f32.to_bits(), 0.01f32.to_bits(), 0, 0, 0, 0, 0, 0], 8);
    let inactive = h.array(&[0, 0.01f32.to_bits(), 0, 0, 0, 0, 0, 0x80000002], 8);
    let mut history = SubstepHistory::default();
    history.prepare(&h.device);
    history.reserve(&h.device, pool_tiles, 2).unwrap();
    let held = h.array::<f32>(&[], 2 * len * 4);
    let final_grids = h.array::<f32>(&[], 2 * len * 4);
    let held_schedule = h.array::<u32>(&[], 8);
    let mut enc = h
        .device
        .create_encoder("whitewater-engine-inactive-history");
    let pooled = |faces| PooledFaces {
        faces,
        table: &tiles.1,
        pool_tiles,
    };
    history.capture(&mut enc, 0, &active.1, &pooled(&first.1));
    // An inactive row must neither read this poisoned grid nor overwrite a
    // previous active capture. A never-active row starts with zero storage.
    history.capture(&mut enc, 0, &inactive.1, &pooled(&poison.1));
    history.capture(&mut enc, 1, &inactive.1, &pooled(&poison.1));
    enc.copy_buffer_to_buffer(
        history.faces.as_ref().unwrap(),
        &held.1,
        (2 * len * 16) as u64,
    );
    enc.copy_buffer_to_buffer(history.schedule.as_ref().unwrap(), &held_schedule.1, 32);
    history.capture(&mut enc, 0, &active.1, &pooled(&resumed.1));
    // The remap leaves the never-captured row alone: its table copy is empty.
    history.remap(&mut enc, &final_table.1, 2);
    enc.copy_buffer_to_buffer(
        history.faces.as_ref().unwrap(),
        &final_grids.1,
        (2 * len * 16) as u64,
    );
    enc.commit_and_wait_completed();
    assert_eq!(
        read::<u32>(&held_schedule.1, 8),
        [0, 0.01f32.to_bits(), 0x80000002, 0].repeat(2),
    );
    let row = |velocity: [f32; 3]| -> Vec<f32> {
        (0..len)
            .flat_map(|_| [velocity[0], velocity[1], velocity[2], 0.0])
            .collect()
    };
    let held_values = read::<f32>(&held.1, 2 * len * 4);
    let final_values = read::<f32>(&final_grids.1, 2 * len * 4);
    assert_eq!(held_values[..len * 4], row([1.0, 2.0, 3.0]));
    assert_eq!(final_values[..len * 4], row([4.0, 5.0, 6.0]));
    assert_eq!(held_values[len * 4..], vec![0.0; len * 4]);
    assert_eq!(final_values[len * 4..], vec![0.0; len * 4]);
}

/// Runs the surface distance stage on the pooled field `phi` of `table`'s
/// pool under `tiles` and returns its output.
fn surface_on_gpu(
    h: &mut Harness,
    stage: &SurfaceDistance,
    table: &BoxTable,
    phi: &[f32],
    tiles: &GpuBuffer,
    cell: f32,
    plan: Option<&GpuBuffer>,
) -> Vec<f32> {
    let n = phi.len();
    let source = h.array(phi, n);
    let mut enc = h.device.create_encoder("whitewater-surface-distance");
    let output = match plan {
        Some(plan) => stage.encode_gated(&mut enc, &source.1, tiles, table.pool_tiles, cell, plan),
        None => stage.encode(&mut enc, &source.1, tiles, table.pool_tiles, cell),
    }
    .clone();
    let staged = h.array::<f32>(&[], n);
    enc.copy_buffer_to_buffer(&output, &staged.1, (n * 4) as u64);
    enc.commit_and_wait_completed();
    read(&staged.1, n)
}

/// A clock plan (gpu_flip_step/common.wgsl ClockPlan): live, with `step_dt`.
fn live_plan(h: &mut Harness, step_dt: f32) -> GpuBuffer {
    let mut words = [0u32; 12];
    words[0] = step_dt.to_bits();
    words[11] = 1;
    h.array(&words, 12).1
}

/// Every scratch buffer of the stage except its sweep grid, as words.
fn scratch_words(h: &mut Harness, stage: &SurfaceDistance) -> Vec<Vec<u32>> {
    let buffers = stage.scratch();
    let mut enc = h
        .device
        .create_encoder("whitewater-surface-distance-scratch");
    let staged: Vec<_> = buffers[..5]
        .iter()
        .map(|b| {
            let words = (b.size() / 4) as usize;
            let s = h.array::<u32>(&[], words);
            enc.copy_buffer_to_buffer(b, &s.1, b.size());
            (s.1, words)
        })
        .collect();
    enc.commit_and_wait_completed();
    staged.iter().map(|(b, w)| read(b, *w)).collect()
}

/// A sphere of radius 1.6 m centred in a 24³ grid of 0.25 m cells.
fn sphere_phi(centre: f32) -> Vec<f32> {
    const N: usize = 24;
    (0..N * N * N)
        .map(|i| {
            let c = [i % N, i / N % N, i / (N * N)].map(|v| (v as f32 + 0.5) * 0.25 - centre);
            (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt() - 1.6
        })
        .collect()
}

/// The grid of `sphere_phi` as 24³ cells from world cell (-12, 3, -11): four
/// tiles a side, the sphere's shell across the tile face between tiles -1 and
/// 0 on x and missing the outer tiles.
fn sphere_table() -> BoxTable {
    BoxTable::new([-12, 3, -11], [24; 3])
}

/// The clock gate: a live active plan (and the always-active zero plan of
/// `encode`) give bit-identical output; an inactive slot leaves the output
/// and every field and state word untouched, rewriting only its sweep grid
/// to zero groups.
#[test]
fn whitewater_surface_distance_clock_gate() {
    let mut h = Harness::new();
    let table = sphere_table();
    let domain = Domain::new(&table);
    let tiles = h.array(&table.words, table.words.len());
    let mut stage = SurfaceDistance::default();
    stage.prepare(&h.device);
    stage.reserve(&h.device, table.pool_tiles).unwrap();
    let a = table.to_pool(&sphere_phi(3.0), 1.25);
    let b = table.to_pool(&sphere_phi(2.5), 1.25);
    let ungated = surface_on_gpu(&mut h, &stage, &table, &a, &tiles.1, 0.25, None);
    let active = live_plan(&mut h, 0.01);
    let gated = surface_on_gpu(&mut h, &stage, &table, &a, &tiles.1, 0.25, Some(&active));
    assert!(
        ungated
            .iter()
            .zip(&gated)
            .all(|(x, y)| x.to_bits() == y.to_bits()),
        "active slot differs from the zero plan"
    );
    let want = tile_surface_distance(&domain, &domain.lift(&a), 0.25);
    for (g, c) in domain.lift(&gated).iter().zip(&want) {
        assert!((g - c).abs() < 2e-6, "stage {g} CPU {c}");
    }
    // Tiles the sphere misses and does not border read 5h; the rest sweep.
    let far = want.iter().filter(|&&d| d == 1.25).count();
    assert!(
        far > want.len() / 4 && want.len() - far > 3_000,
        "{far} of {} cells read 5h",
        want.len()
    );
    let before = scratch_words(&mut h, &stage);
    let inactive = live_plan(&mut h, 0.0);
    let after_output = surface_on_gpu(&mut h, &stage, &table, &b, &tiles.1, 0.25, Some(&inactive));
    assert!(
        after_output
            .iter()
            .zip(&gated)
            .all(|(x, y)| x.to_bits() == y.to_bits()),
        "inactive slot changed the output"
    );
    assert_eq!(
        scratch_words(&mut h, &stage),
        before,
        "inactive slot touched the scratch"
    );
    let mut enc = h.device.create_encoder("whitewater-surface-distance-args");
    let args = h.array::<u32>(&[], 3);
    enc.copy_buffer_to_buffer(&stage.scratch()[5], &args.1, 12);
    enc.commit_and_wait_completed();
    assert_eq!(
        read::<u32>(&args.1, 3),
        vec![0, 1, 1],
        "inactive sweep grid"
    );
    // And the next active slot computes from the new input again.
    let next = surface_on_gpu(&mut h, &stage, &table, &b, &tiles.1, 0.25, Some(&active));
    let want = tile_surface_distance(&domain, &domain.lift(&b), 0.25);
    for (g, c) in domain.lift(&next).iter().zip(&want) {
        assert!((g - c).abs() < 2e-6, "stage {g} CPU {c}");
    }
}

/// The gated stage under encode replay: a sequence of active and inactive
/// slots, with the plan's and the input's contents changing in the same
/// buffers between slots, recorded once and replayed. After every slot the
/// replayed stage's output and scratch equal a directly encoded twin's.
#[test]
fn whitewater_surface_distance_replay_matches_direct() {
    use manifold_gpu::GpuReplayCache;
    let device = manifold_gpu::testkit::test_device();
    let table = sphere_table();
    let pool_tiles = table.pool_tiles;
    let len = pool_len(pool_tiles) as usize;
    let make = || {
        let mut stage = SurfaceDistance::default();
        stage.prepare(&device);
        stage.reserve(&device, pool_tiles).unwrap();
        let plan = device.create_buffer_shared(48);
        let source = device.create_buffer_shared(len as u64 * 4);
        let tiles = device.create_buffer_shared(table.words.len() as u64 * 4);
        // SAFETY: a shared buffer of exactly these words; no GPU work in flight.
        unsafe { tiles.write(0, bytemuck::cast_slice(&table.words)) };
        (stage, plan, source, tiles)
    };
    let (direct, direct_plan, direct_source, direct_tiles) = make();
    let (replayed, replay_plan, replay_source, replay_tiles) = make();
    let mut cache = Some(GpuReplayCache::default());
    let inputs = [3.0, 2.5, 2.75].map(|centre| table.to_pool(&sphere_phi(centre), 1.25));
    let slots = [
        (0.01f32, 0),
        (0.0, 1),
        (0.01, 1),
        (0.01, 0),
        (0.0, 2),
        (0.0, 0),
        (0.01, 2),
        (0.01, 0),
    ];
    let words = |stage: &SurfaceDistance| -> Vec<Vec<u32>> {
        let mut enc = device.create_encoder("surface replay readback");
        let staged: Vec<_> = stage.scratch()[..6]
            .iter()
            .map(|b| {
                let s = device.create_buffer_shared(b.size());
                enc.copy_buffer_to_buffer(b, &s, b.size());
                (s, (b.size() / 4) as usize)
            })
            .collect();
        enc.commit_and_wait_completed();
        staged.iter().map(|(s, n)| read::<u32>(s, *n)).collect()
    };
    for (slot, &(step_dt, input)) in slots.iter().enumerate() {
        let mut plan = [0u32; 12];
        plan[0] = step_dt.to_bits();
        plan[11] = 1;
        for (p, s) in [
            (&direct_plan, &direct_source),
            (&replay_plan, &replay_source),
        ] {
            // SAFETY: shared buffers sized for the writes; no GPU work in flight.
            unsafe {
                p.write(0, bytemuck::cast_slice(&plan));
                s.write(0, bytemuck::cast_slice(&inputs[input]));
            }
        }
        let mut enc = device.create_encoder("surface direct");
        direct.encode_gated(
            &mut enc,
            &direct_source,
            &direct_tiles,
            pool_tiles,
            0.25,
            &direct_plan,
        );
        enc.commit_and_wait_completed();
        let mut enc = device.create_encoder("surface replay");
        enc.begin_replay(&device, cache.take().expect("cache"));
        replayed.encode_gated(
            &mut enc,
            &replay_source,
            &replay_tiles,
            pool_tiles,
            0.25,
            &replay_plan,
        );
        cache = Some(enc.end_replay());
        enc.commit_and_wait_completed();
        assert!(
            words(&direct) == words(&replayed),
            "slot {slot} (step {step_dt}, input {input}): replay differs from direct"
        );
    }
    let stats = cache.expect("cache").stats();
    assert!(stats.replayed > 0, "nothing replayed: {stats:?}");
    eprintln!("SURFACE replay: {stats:?}");
}
