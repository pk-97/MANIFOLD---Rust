//! Live field placement and influence ownership, without golden recording.
use super::*;
use crate::testkit::liquid_surface::{Harness, read};
use super::super::whitewater_obstacle_source::WhitewaterSource;
use crate::water::fluid_particles::FluidParticle;
use super::super::whitewater_step::empty_slot;

const CAPACITY: u32 = 1024;
const EMITTERS: usize = 64;
const PORTS: [&str; 7] = ["pool_out", "state_out", "counts_out", "foam_particles",
    "bubble_particles", "spray_particles", "dust_particles"];

fn shared(device: &GpuDevice, bytes: &[u8]) -> GpuBuffer {
    let buffer = device.create_buffer_shared(bytes.len() as u64);
    // SAFETY: fresh storage of exactly this size, before submission.
    unsafe { buffer.write(0, bytes) };
    buffer
}

struct Fixture {
    shape: StepShape,
    particles: GpuBuffer,
    solid: GpuBuffer,
    distance: GpuBuffer,
    level: GpuBuffer,
    faces: [GpuBuffer; 3],
    source: GpuBuffer,
}

impl Fixture {
    fn new(device: &GpuDevice, face_cells: [u32; 3], pad: u32, kind: u32, influence: f32) -> Self {
        let nodes = face_cells.map(|n| n + 2 * pad + 1);
        let size = nodes.map(|n| (n - 1) as f32 * 0.1);
        let shape = StepShape::new(nodes, nodes, face_cells, 1.0,
            Some(Transform { pos: size.map(|n| n * 0.5), scale: size, ..Default::default() }), CAPACITY).unwrap();
        let sphere = |q: [f32; 3]| (q.iter().map(|v| v * v).sum::<f32>().sqrt() - 2.5) * 0.1;
        let phi: Vec<f32> = (0..cell_total(face_cells)).map(|i| {
            let [nx, ny, _] = face_cells.map(u64::from);
            let q = [i % nx, i / nx % ny, i / (nx * ny)];
            sphere(std::array::from_fn(|a| q[a] as f32 + 0.5 - face_cells[a] as f32 * 0.5))
        }).collect();
        let level: Vec<f32> = (0..cell_total(nodes)).map(|i| {
            let [nx, ny, _] = nodes.map(u64::from);
            let q = [i % nx, i / nx % ny, i / (nx * ny)];
            sphere(std::array::from_fn(|a| q[a] as f32 - (nodes[a] - 1) as f32 * 0.5))
        }).collect();
        let particles: Vec<_> = (0..EMITTERS).map(|i| {
            let q = [i % 4, i / 4 % 4, i / 16];
            let p: [f32; 3] = std::array::from_fn(|a| size[a] * 0.5 + (q[a] as f32 - 1.5) * 0.09);
            FluidParticle { position_radius: [p[0], p[1], p[2], 0.025], velocity: [0.0; 3], id: i as u32 }
        }).collect();
        let faces = std::array::from_fn(|a| {
            let mut dims = face_cells;
            dims[a] += 1;
            let values: Vec<f32> = (0..cell_total(dims)).map(|i| {
                let q = [i % u64::from(dims[0]), i / u64::from(dims[0]) % u64::from(dims[1]), i / u64::from(dims[0] * dims[1])];
                [2.0, 5.0, -1.0][a] + q[(a + 1) % 3] as f32 * 0.5
            }).collect();
            shared(device, bytemuck::cast_slice(&values))
        });
        let sources = vec![WhitewaterSource { influence, dust_strength: 1.0, kind, pad: 0 }; cell_total(nodes) as usize];
        Self { shape, particles: shared(device, bytemuck::cast_slice(&particles)),
            solid: shared(device, bytemuck::cast_slice(&vec![0.1f32; cell_total(nodes) as usize])),
            distance: shared(device, bytemuck::cast_slice(&phi)),
            level: shared(device, bytemuck::cast_slice(&level)), faces,
            source: shared(device, bytemuck::cast_slice(&sources)) }
    }

    fn inputs(&self, tick_mode: bool) -> StepInputs<'_> {
        StepInputs { motion: None, particles: &self.particles, solid: &self.solid,
            obstacle_source: Some(&self.source), faces: FaceSource::Axes(self.faces.each_ref()), level_set: &self.level,
            distance: tick_mode.then_some(&self.distance) }
    }

    fn frame(&self, epoch: u32) -> StepFrame {
        StepFrame { shape: self.shape, count: Some(EMITTERS as u32), ticks: 1, dt: 0.015625,
            epoch, seed: 0.375, gravity: [0.0; 3], wavecrest_emission: 80.0,
            turbulence_emission: 80.0, min_turbulence: 0.0, max_turbulence: 1.0,
            inside_emission: true, generation_rate: 1.0, spray_speed: 1.0,
            dust_emission: true, boundary_dust: true, dust_rate: 80.0,
            influence_base: 1.0, influence_decay: 16.0, min_energy: 0.0, max_energy: 1.0,
            preserve_foam: false }
    }
}

fn boundary(device: &GpuDevice) -> (GpuBuffer, GpuBuffer) {
    (shared(device, bytemuck::cast_slice(&vec![empty_slot(); CAPACITY as usize])),
        shared(device, bytemuck::cast_slice(&[0u32; 8])))
}

fn capture(device: &GpuDevice, enc: &mut manifold_gpu::GpuEncoder, step: &Step) -> Vec<GpuBuffer> {
    PORTS.iter().map(|port| {
        let source = step.tick_output(port).unwrap();
        let out = device.create_buffer_shared(source.size);
        enc.copy_buffer_to_buffer(source, &out, source.size);
        out
    }).collect()
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

#[test]
fn whitewater_alias_follows_live_padding() {
    let h = Harness::new();
    let device = &h.device;
    let mut live = Step::default();
    // The last two transitions change only mode, so shape alone cannot be
    // the reservation key. All transitions share one encoder.
    let fixtures: Vec<_> = [([8; 3], 0), ([8, 10, 8], 2), ([10, 8, 10], 0)]
        .into_iter().map(|(cells, pad)| Fixture::new(device, cells, pad, 2, 2.0)).collect();
    let mut enc = device.create_encoder("whitewater live padding");
    let mut rows = Vec::new();
    for (index, tick_mode) in [(0, true), (1, true), (2, true), (2, false), (2, true)] {
        let fixture = &fixtures[index];
        let frame = fixture.frame(0);
        let inputs = fixture.inputs(tick_mode);
        let (pool, state) = boundary(device);
        let mut fresh = Step::default();
        for step in [&mut live, &mut fresh] {
            if tick_mode {
                step.advance_tick(&mut GpuEncoder::new(&mut enc, device), &frame, &inputs, &pool, &state, true).unwrap();
            } else {
                step.advance(&mut GpuEncoder::new(&mut enc, device), &Retired, true, &frame, &inputs).unwrap();
            }
            let owned = !tick_mode || index == 1;
            assert_eq!(step.fields().distance.is_some(), owned);
            assert_eq!(step.fields().surface.is_some(), owned);
        }
        rows.push((capture(device, &mut enc, &live), capture(device, &mut enc, &fresh)));
        if !tick_mode {
            let f = live.fields();
            let distance = device.create_buffer_shared(f.distance.as_ref().unwrap().size);
            let surface = device.create_buffer_shared(distance.size);
            enc.copy_buffer_to_buffer(f.distance.as_ref().unwrap(), &distance, distance.size);
            enc.copy_buffer_to_buffer(f.surface.as_ref().unwrap(), &surface, surface.size);
            rows.push((vec![distance], vec![surface]));
        }
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
    // another obstacle impulse, then shape replacement without an epoch.
    let cases = [(8, 0, true, 2, 4.0), (8, 0, true, 0, 0.0), (8, 0, true, 0, 0.0),
        (8, 1, true, 0, 0.0), (8, 1, false, 0, 0.0), (8, 1, true, 0, 0.0),
        (8, 1, true, 2, 3.0), (10, 1, true, 0, 0.0), (10, 1, true, 2, 0.5),
        (10, 1, true, 0, 0.0)];
    let fixtures: Vec<_> = cases.iter().map(|&(n, _, _, kind, influence)|
        Fixture::new(device, [n; 3], 0, kind, influence)).collect();
    let mut expected = 1.0f32;
    let mut previous = None;
    let mut enc = device.create_encoder("whitewater influence lifecycle");
    let mut rows = Vec::new();
    let mut influences = Vec::new();
    for (i, (&(n, epoch, enabled, kind, source), fixture)) in cases.iter().zip(&fixtures).enumerate() {
        let frame = fixture.frame(epoch);
        let inputs = fixture.inputs(true);
        let reset = previous != Some((n, epoch));
        if enabled {
            if reset { expected = 1.0; }
            expected = if expected < 1.0 { (expected + 0.25).min(1.0) } else { (expected - 0.25).max(1.0) };
            if kind == 2 { expected = source; }
            previous = Some((n, epoch));
        } else {
            previous = None;
        }
        let before = live.influence_current;
        live.advance_tick(&mut GpuEncoder::new(&mut enc, device), &frame, &inputs, &pool, &state, enabled).unwrap();
        assert_eq!(live.influence_current, if enabled { 1 - before } else { before }, "tick {i}: one swap per update");
        let got = capture(device, &mut enc, &live);
        // Independent influence oracle: a fresh stage resets to the CPU
        // recurrence's result, with decay disabled. It never reads the live
        // stage's influence buffers, epoch or current index.
        let reference_frame = StepFrame { influence_base: expected, influence_decay: 0.0, ..frame };
        let mut reference = Step::default();
        reference.advance_tick(&mut GpuEncoder::new(&mut enc, device), &reference_frame, &inputs, &pool, &state, enabled).unwrap();
        rows.push((got, capture(device, &mut enc, &reference)));
        if enabled {
            let field = &live.fields().influence[live.influence_current];
            let copied = device.create_buffer_shared(field.size);
            enc.copy_buffer_to_buffer(field, &copied, field.size);
            influences.push((i, copied, expected));
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
        for (node, value) in read::<f32>(&buffer, buffer.size as usize / 4).into_iter().enumerate() {
            assert_eq!(value.to_bits(), expected.to_bits(), "tick {i} influence node {node}");
        }
    }
    let first_state = read::<u32>(&rows[0].0[1], 8);
    assert!(first_state[3] > 0, "emission must exercise influence");
    let counts = read::<u32>(&rows[0].0[2], COUNT_WORDS);
    assert!(counts[..3].iter().sum::<u32>() > 0, "normal emission must exercise influence: {counts:?}");
    assert!(counts[8] > 0, "dust must exercise the same new influence buffer: {counts:?}");
}
