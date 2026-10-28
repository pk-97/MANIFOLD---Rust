//! Live field placement and influence ownership, without golden recording.
use manifold_gpu::BufferUse;
use super::super::whitewater_obstacle_source::WhitewaterSource;
use super::super::whitewater_step::empty_slot;
use super::fused_tests::gpu::identity_record;
use super::fused_tests::{CELL, low_corner, middle, step_shape};
use super::*;
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::testkit::array_harness::{Harness, read};
use manifold_water_liquid::testkit::world_box::{BoxTable, table_words_of};

const CAPACITY: u32 = 1024;
const EMITTERS: usize = 64;
const PORTS: [&str; 7] = [
    "pool_out",
    "state_out",
    "counts_out",
    "foam_particles",
    "bubble_particles",
    "spray_particles",
    "dust_particles",
];

fn shared(device: &GpuDevice, bytes: &[u8]) -> GpuBuffer {
    let buffer = device.create_buffer(bytes.len() as u64, BufferUse::Upload);
    // SAFETY: fresh storage of exactly this size, before submission.
    unsafe { buffer.write(0, bytes) };
    buffer
}

struct Fixture {
    shape: StepShape,
    bounds_lo: [f32; 3],
    particles: GpuBuffer,
    identity: GpuBuffer,
    solid: GpuBuffer,
    distance: GpuBuffer,
    tiles: GpuBuffer,
    faces: GpuBuffer,
    source: GpuBuffer,
}

impl Fixture {
    /// A sphere of liquid in the middle of `table`'s box with 64 emitters
    /// inside it, faces that drift and shear, and `influence` of obstacle
    /// source `kind` on every element.
    fn new(device: &GpuDevice, table: &BoxTable, h: f32, kind: u32, influence: f32) -> Self {
        let [nx, ny, _] = table.cells.map(u64::from);
        let count = (nx * ny * u64::from(table.cells[2])) as usize;
        let at = |i: usize| {
            let i = i as u64;
            [i % nx, i / nx % ny, i / (nx * ny)].map(|c| c as f32)
        };
        let centre = table.cells.map(|n| 0.5 * (n as f32 - 1.0));
        let phi: Vec<f32> = (0..count)
            .map(|i| {
                let q = at(i);
                let r = (0..3).map(|a| (q[a] - centre[a]).powi(2)).sum::<f32>();
                (r.sqrt() - 2.5) * h
            })
            .collect();
        let faces: Vec<FaceSample> = (0..count)
            .map(|i| {
                let q = at(i);
                FaceSample {
                    velocity: [2.0 + q[1] * 0.5, 5.0 + q[2] * 0.5, -1.0 + q[0] * 0.5, 0.0],
                    weight: [1.0, 1.0, 1.0, 0.0],
                }
            })
            .collect();
        let m = middle(table, h);
        let emitters: Vec<_> = (0..EMITTERS)
            .map(|i| {
                let q = [i % 4, i / 4 % 4, i / 16];
                let p: [f32; 3] = std::array::from_fn(|a| m[a] + (q[a] as f32 - 1.5) * 0.9 * h);
                FluidParticle {
                    position_radius: [p[0], p[1], p[2], 0.25 * h],
                    velocity: [0.0; 3],
                    id: i as u32,
                }
            })
            .collect();
        Self::assemble(
            device,
            step_shape(table, h, CAPACITY),
            low_corner(table, h),
            &table.words,
            (
                table.to_pool(&phi, 3.0 * h),
                table.to_pool(&faces, FaceSample::default()),
            ),
            &emitters,
            (kind, influence),
        )
    }

    /// Liquid at rest in every element of a pool of `pool_tiles` tiles whose
    /// table is `words`, the same faces everywhere, and four emitters in the
    /// tile at the origin.
    fn uniform(
        device: &GpuDevice,
        words: &[u32],
        pool_tiles: u32,
        h: f32,
        source: (u32, f32),
    ) -> Self {
        let shape = StepShape::new(
            TilePool {
                pool_tiles,
                cell_size: h,
            },
            CAPACITY,
        )
        .unwrap();
        let elements = shape.elements() as usize;
        let face = FaceSample {
            velocity: [2.0, 5.0, -1.0, 0.0],
            weight: [1.0, 1.0, 1.0, 0.0],
        };
        let emitters: Vec<_> = (0..4)
            .map(|i| FluidParticle {
                position_radius: [3.0 * h + 0.2 * h * i as f32, 3.0 * h, 3.0 * h, 0.25 * h],
                velocity: [0.0; 3],
                id: i,
            })
            .collect();
        Self::assemble(
            device,
            shape,
            [0.0; 3],
            words,
            (vec![-1.0; elements], vec![face; elements]),
            &emitters,
            source,
        )
    }

    fn assemble(
        device: &GpuDevice,
        shape: StepShape,
        bounds_lo: [f32; 3],
        words: &[u32],
        (phi, faces): (Vec<f32>, Vec<FaceSample>),
        emitters: &[FluidParticle],
        (kind, influence): (u32, f32),
    ) -> Self {
        let elements = shape.elements() as usize;
        let source = WhitewaterSource {
            influence,
            dust_strength: 1.0,
            kind,
            pad: 0,
        };
        Self {
            shape,
            bounds_lo,
            particles: shared(device, bytemuck::cast_slice(emitters)),
            identity: identity_record(device, emitters.len() as u32),
            solid: shared(device, bytemuck::cast_slice(&vec![0.1f32; elements])),
            distance: shared(device, bytemuck::cast_slice(&phi)),
            tiles: shared(device, bytemuck::cast_slice(words)),
            faces: shared(device, bytemuck::cast_slice(&faces)),
            source: shared(device, bytemuck::cast_slice(&vec![source; elements])),
        }
    }

    fn inputs(&self) -> StepInputs<'_> {
        StepInputs {
            motion: None,
            particles: &self.particles,
            identity: &self.identity,
            solid: &self.solid,
            obstacle_source: Some(&self.source),
            faces: &self.faces,
            distance: &self.distance,
            tiles: &self.tiles,
        }
    }

    fn frame(&self, epoch: u32) -> StepFrame {
        StepFrame {
            shape: self.shape,
            ticks: 1,
            dt: 0.015625,
            epoch,
            seed: 0.375,
            gravity: [0.0; 3],
            bounds_lo: self.bounds_lo,
            removal: Removal::NONE,
            wavecrest_emission: 80.0,
            turbulence_emission: 80.0,
            min_turbulence: 0.0,
            max_turbulence: 1.0,
            inside_emission: true,
            generation_rate: 1.0,
            spray_speed: 1.0,
            dust_emission: true,
            boundary_dust: true,
            dust_rate: 80.0,
            influence_base: 1.0,
            influence_decay: 16.0,
            min_energy: 0.0,
            max_energy: 1.0,
            preserve_foam: false,
        }
    }
}

fn boundary(device: &GpuDevice) -> (GpuBuffer, GpuBuffer) {
    (
        shared(
            device,
            bytemuck::cast_slice(&vec![empty_slot(); CAPACITY as usize]),
        ),
        shared(device, bytemuck::cast_slice(&[0u32; 8])),
    )
}

fn capture(device: &GpuDevice, enc: &mut manifold_gpu::GpuEncoder, step: &Step) -> Vec<GpuBuffer> {
    PORTS
        .iter()
        .map(|port| {
            let source = step.tick_output(port).unwrap();
            let out = device.create_buffer(source.size, BufferUse::Upload);
            enc.copy_buffer_to_buffer(source, &out, source.size);
            out
        })
        .collect()
}

/// A copy of the influence the stage holds after its last tick.
fn capture_influence(
    device: &GpuDevice,
    enc: &mut manifold_gpu::GpuEncoder,
    step: &Step,
) -> GpuBuffer {
    let field = &step.fields().influence[step.influence_current];
    let copied = device.create_buffer(field.size, BufferUse::Upload);
    enc.copy_buffer_to_buffer(field, &copied, field.size);
    copied
}

fn assert_rows(got: &[GpuBuffer], want: &[GpuBuffer], label: &str) {
    for ((got, want), port) in got.iter().zip(want).zip(PORTS) {
        let a = read::<u32>(got, got.size as usize / 4);
        let b = read::<u32>(want, want.size as usize / 4);
        assert_eq!(a.len(), b.len());
        if let Some(i) = a.iter().zip(&b).position(|(a, b)| a != b) {
            panic!("{label} {port} word {i}: {:08x} != {:08x}", a[i], b[i]);
        }
    }
}

/// A stage that has run other pools answers a frame as a fresh one does:
/// the pool grows, the cell size changes, and the same pool repeats.
#[test]
fn whitewater_live_stage_follows_pool_changes() {
    let h = Harness::new();
    let device = &h.device;
    let mut live = Step::default();
    // All transitions share one encoder.
    let fixtures: Vec<_> = [
        (BoxTable::new([-4; 3], [8; 3]), CELL),
        (BoxTable::new([-5, -6, -7], [20, 10, 12]), CELL),
        (BoxTable::new([-4; 3], [8; 3]), 1.25 * CELL),
        (BoxTable::new([-4; 3], [8; 3]), 1.25 * CELL),
    ]
    .into_iter()
    .map(|(table, cell)| Fixture::new(device, &table, cell, 2, 2.0))
    .collect();
    assert_ne!(fixtures[0].shape.pool, fixtures[1].shape.pool);
    assert_ne!(fixtures[0].shape.pool, fixtures[2].shape.pool);
    let mut enc = device.create_encoder("whitewater live pool");
    let mut rows = Vec::new();
    for fixture in &fixtures {
        let frame = fixture.frame(0);
        let inputs = fixture.inputs();
        let (pool, state) = boundary(device);
        let mut fresh = Step::default();
        for step in [&mut live, &mut fresh] {
            step.advance_tick(
                &mut GpuEncoder::new(&mut enc, device),
                &frame,
                &inputs,
                &pool,
                &state,
                true,
            )
            .unwrap();
            assert_eq!(step.shape, Some(fixture.shape));
        }
        rows.push((
            capture(device, &mut enc, &live),
            capture(device, &mut enc, &fresh),
        ));
    }
    enc.commit_and_wait_completed();
    for (i, (got, want)) in rows.iter().enumerate() {
        assert_rows(got, want, &format!("transition {i}"));
    }
    assert!(read::<u32>(&rows[0].0[1], 8)[3] > 0, "fixture must emit");
}

#[test]
fn whitewater_influence_swap_lifecycle() {
    let h = Harness::new();
    let device = &h.device;
    let mut live = Step::default();
    let (pool, state) = boundary(device);
    // Nonzero obstacle influence, decay, epoch reset, disable/re-enable,
    // another obstacle impulse, then pool replacement without an epoch.
    let cases = [
        (8, 0, true, 2, 4.0),
        (8, 0, true, 0, 0.0),
        (8, 0, true, 0, 0.0),
        (8, 1, true, 0, 0.0),
        (8, 1, false, 0, 0.0),
        (8, 1, true, 0, 0.0),
        (8, 1, true, 2, 3.0),
        (16, 1, true, 0, 0.0),
        (16, 1, true, 2, 0.5),
        (16, 1, true, 0, 0.0),
    ];
    let fixtures: Vec<_> = cases
        .iter()
        .map(|&(n, _, _, kind, influence)| {
            Fixture::new(
                device,
                &BoxTable::new([-4; 3], [n; 3]),
                CELL,
                kind,
                influence,
            )
        })
        .collect();
    let mut expected = 1.0f32;
    let mut previous = None;
    let mut enc = device.create_encoder("whitewater influence lifecycle");
    let mut rows = Vec::new();
    let mut influences = Vec::new();
    for (i, (&(n, epoch, enabled, kind, source), fixture)) in
        cases.iter().zip(&fixtures).enumerate()
    {
        let frame = fixture.frame(epoch);
        let inputs = fixture.inputs();
        let reset = previous != Some((n, epoch));
        if enabled {
            if reset {
                expected = 1.0;
            }
            expected = if expected < 1.0 {
                (expected + 0.25).min(1.0)
            } else {
                (expected - 0.25).max(1.0)
            };
            if kind == 2 {
                expected = source;
            }
            previous = Some((n, epoch));
        } else {
            previous = None;
        }
        let before = live.influence_current;
        live.advance_tick(
            &mut GpuEncoder::new(&mut enc, device),
            &frame,
            &inputs,
            &pool,
            &state,
            enabled,
        )
        .unwrap();
        assert_eq!(
            live.influence_current,
            if enabled { 1 - before } else { before },
            "tick {i}: one swap per update"
        );
        let got = capture(device, &mut enc, &live);
        // Independent influence oracle: a fresh stage resets to the CPU
        // recurrence's result, with decay disabled. It never reads the live
        // stage's influence buffers, epoch or current index.
        let reference_frame = StepFrame {
            influence_base: expected,
            influence_decay: 0.0,
            ..frame
        };
        let mut reference = Step::default();
        reference
            .advance_tick(
                &mut GpuEncoder::new(&mut enc, device),
                &reference_frame,
                &inputs,
                &pool,
                &state,
                enabled,
            )
            .unwrap();
        rows.push((got, capture(device, &mut enc, &reference)));
        if enabled {
            influences.push((i, capture_influence(device, &mut enc, &live), expected));
        } else {
            assert_eq!(live.influence_epoch, None);
        }
        // Boundary capture closes before the next tick in this encoder.
        enc.copy_buffer_to_buffer(live.tick_output("pool_out").unwrap(), &pool, pool.size);
        enc.copy_buffer_to_buffer(live.tick_output("state_out").unwrap(), &state, state.size);
    }
    enc.commit_and_wait_completed();
    for (i, (got, want)) in rows.iter().enumerate() {
        assert_rows(got, want, &format!("tick {i}"));
    }
    for (i, buffer, expected) in influences {
        for (element, value) in read::<f32>(&buffer, buffer.size as usize / 4)
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                value.to_bits(),
                expected.to_bits(),
                "tick {i} influence element {element}"
            );
        }
    }
    let first_state = read::<u32>(&rows[0].0[1], 8);
    assert!(first_state[3] > 0, "emission must exercise influence");
    let counts = read::<u32>(&rows[0].0[2], COUNT_WORDS);
    assert!(
        counts[..3].iter().sum::<u32>() > 0,
        "normal emission must exercise influence: {counts:?}"
    );
    assert!(
        counts[8] > 0,
        "dust must exercise the same new influence buffer: {counts:?}"
    );
}

/// Influence belongs to a world tile but is stored in a pool slot: a slot
/// the table gives to another tile, or that was free, starts over at the
/// base level, while a slot that keeps its tile decays on. Free slots are
/// never read.
#[test]
fn whitewater_influence_restarts_where_a_slot_changes_tile() {
    let h = Harness::new();
    let device = &h.device;
    let pool_tiles = 3;
    let [a, b, c] = [[0, 0, 0], [1, 0, 0], [0, 1, 0]];
    let all = vec![(a, 0), (b, 1), (c, 2)];
    // Each tick's tiles and whether the obstacle source is wired, with the
    // influence of every slot after it (None where the slot is free). The
    // first tick sets 3 everywhere; decay is a quarter a tick down to 1.
    let ticks = [
        (all.clone(), true, [Some(3.0f32), Some(3.0), Some(3.0)]),
        (all.clone(), false, [Some(2.75), Some(2.75), Some(2.75)]),
        // Slots 1 and 2 swap tiles.
        (
            vec![(a, 0), (c, 1), (b, 2)],
            false,
            [Some(2.5), Some(1.0), Some(1.0)],
        ),
        // Slots 1 and 2 go free.
        (vec![(a, 0)], false, [Some(2.25), None, None]),
        // Both come back as the tiles they held before they went free.
        (all.clone(), false, [Some(2.0), Some(1.0), Some(1.0)]),
    ];
    let live = Fixture::uniform(
        device,
        &table_words_of(pool_tiles, &all),
        pool_tiles,
        CELL,
        (2, 3.0),
    );
    let (pool, state) = boundary(device);
    let mut step = Step::default();
    let mut enc = device.create_encoder("whitewater influence slot rows");
    let mut seen = Vec::new();
    // The table words are read until the encoder retires.
    let mut tables = Vec::new();
    // Dust needs the obstacle source, which the later ticks leave unwired.
    let frame = StepFrame {
        dust_emission: false,
        ..live.frame(0)
    };
    for (held, source, _) in &ticks {
        let tiles = shared(
            device,
            bytemuck::cast_slice(&table_words_of(pool_tiles, held)),
        );
        let mut inputs = live.inputs();
        inputs.tiles = &tiles;
        if !source {
            inputs.obstacle_source = None;
        }
        step.advance_tick(
            &mut GpuEncoder::new(&mut enc, device),
            &frame,
            &inputs,
            &pool,
            &state,
            true,
        )
        .unwrap();
        seen.push(capture_influence(device, &mut enc, &step));
        tables.push(tiles);
    }
    enc.commit_and_wait_completed();
    drop(tables);
    for (tick, ((_, _, slots), buffer)) in ticks.iter().zip(&seen).enumerate() {
        let values = read::<f32>(buffer, buffer.size as usize / 4);
        for (slot, want) in slots.iter().enumerate() {
            let Some(want) = want else { continue };
            for (i, value) in values[slot * 512..(slot + 1) * 512].iter().enumerate() {
                assert_eq!(
                    value.to_bits(),
                    want.to_bits(),
                    "tick {tick} slot {slot} element {i}"
                );
            }
        }
        // No slot owns the canonical element: it decays as slot 0, which
        // never changes tile, does.
        let canon = slots[0].expect("slot 0 is always held");
        assert_eq!(
            values[3 * 512].to_bits(),
            canon.to_bits(),
            "tick {tick} canonical"
        );
    }
}
