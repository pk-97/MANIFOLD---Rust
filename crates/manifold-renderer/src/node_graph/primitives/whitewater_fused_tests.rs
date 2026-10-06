//! P2: copied text checks on the CPU, bitwise emitter oracles on the GPU.
use super::*;

const ATOMS: [(&str, &str); 8] = [
    ("jitter", include_str!("shaders/jitter_particles_body.wgsl")),
    ("sample", include_str!("shaders/sample_faces_at_particles_body.wgsl")),
    ("velocity", include_str!("shaders/whitewater_emitter_velocity_body.wgsl")),
    ("energy", include_str!("shaders/energy_potential_body.wgsl")),
    ("wavecrest", include_str!("shaders/wavecrest_potential_body.wgsl")),
    ("inside", include_str!("shaders/inside_turbulence_potential_body.wgsl")),
    ("count", include_str!("shaders/turbulence_emission_count_body.wgsl")),
    ("dust", include_str!("shaders/dust_potential_body.wgsl")),
];

fn function<'a>(source: &'a str, name: &str) -> &'a str {
    let start = source.find(&format!("fn {name}(")).expect("function present");
    let body = source[start..].find('{').unwrap() + start;
    let mut depth = 0;
    for (i, c) in source[body..].char_indices() {
        match c { '{' => depth += 1, '}' => depth -= 1, _ => {} }
        if depth == 0 { return &source[start..=body + i]; }
    }
    panic!("unclosed function {name}")
}

fn random_calls(source: &str) -> Vec<&str> {
    source.match_indices("ww_random(").map(|(start, _)| {
        let mut depth = 1;
        let args = start + "ww_random(".len();
        for (i, c) in source[args..].char_indices() {
            match c { '(' => depth += 1, ')' => depth -= 1, _ => {} }
            if depth == 0 { return &source[start..=args + i]; }
        }
        panic!("unclosed random call")
    }).collect()
}

#[test]
fn whitewater_rng_calls_are_the_atoms() {
    for (phase, atom) in ATOMS {
        let original = function(atom, "body");
        let name = format!("ww_phase_{phase}");
        let fused = function(WHITEWATER_FUSED_SHADER, &name);
        assert_eq!(random_calls(fused), random_calls(original), "{phase}: full RNG argument text/order");
        assert_eq!(fused, original.replacen("fn body(", &format!("fn {name}("), 1), "{phase}: body text changed");
    }
    assert_eq!(random_calls(WHITEWATER_FUSED_SHADER).len(), 5, "extra random draw outside the copied phases");
    for (entry, order) in [
        ("ww_emit", &["jitter", "sample", "velocity", "energy", "wavecrest", "inside", "count"][..]),
        ("ww_dust", &["dust", "energy", "count"][..]),
    ] {
        let calls: Vec<_> = function(WHITEWATER_FUSED_SHADER, entry).split("ww_phase_").skip(1)
            .map(|tail| tail.split('(').next().unwrap()).collect();
        assert_eq!(calls, order, "{entry}: phase call order");
    }
}

#[test]
fn whitewater_fused_variants_validate_on_cpu() {
    for packed in [false, true] {
        let source = fused_source(packed);
        let module = naga::front::wgsl::parse_str(&source).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module).unwrap_or_else(|e| panic!("packed={packed}: {e:?}"));
        assert_eq!(std::mem::size_of::<EmitParams>(), 144);
        assert_eq!(module.entry_points.len(), 2);
        for entry in &module.entry_points { assert_eq!(entry.workgroup_size, [256, 1, 1]); }
    }
}

fn synthetic_faces() -> [Vec<f32>; 3] {
    std::array::from_fn(|a| vec![[2.0, 1.0, -0.5][a]; face_len([8; 3], a) as usize])
}

fn synthetic_shape() -> StepShape {
    StepShape::new([13; 3], [13; 3], [8; 3], 1.0,
        Some(Transform { pos: [0.6; 3], scale: [1.2; 3], ..Default::default() }), 256).unwrap()
}

fn synthetic_records() -> Vec<FluidParticle> {
    (0..8).map(|i| FluidParticle {
        position_radius: [0.45 + 0.015 * i as f32, 0.5, 0.5, if i == 6 { 0.0 } else if i == 7 { -1.0 } else { 0.025 }],
        velocity: [17.0, -3.25, 0.75], id: 255 - i,
    }).collect()
}

#[test]
fn whitewater_half_integer_fixture_has_saturated_energy() {
    use super::super::whitewater_particle_cpu::{Box3, energy, jitter, sample_faces};
    let faces = synthetic_faces();
    let shape = synthetic_shape();
    let grid = Box3 { cells: shape.cells, center: shape.center, size: shape.size };
    for (axis, face) in faces.iter().enumerate() {
        assert_eq!(bytemuck::cast_slice::<_, u8>(face).len(), face_len([8; 3], axis) as usize * 4);
    }
    for (i, particle) in synthetic_records().into_iter().take(6).enumerate() {
        let sampled = sample_faces(jitter(particle, i as u32, shape.cell_size, 0.375, 7.0),
            faces.each_ref().map(Vec::as_slice), [8; 3], &grid);
        assert_eq!(energy(sampled, 0.0, 1.0).to_bits(), 1.0f32.to_bits(), "emitter {i}");
    }
}

#[cfg(feature = "gpu-proofs")]
mod gpu {
    use super::*;
    use super::super::super::liquid_surface_tests::read;
    use super::super::super::whitewater_pool_cpu::empty_slot;
    use super::super::super::whitewater_scene_tests::{Show, whitewater_render_def, with_tick_probe};
    use super::super::super::gpu_flip_preset::WaterScene;

    const PORTS: [&str; 6] = ["proof_sampled", "proof_unscaled", "proof_energy", "proof_counts", "proof_dust_energy", "proof_dust_counts"];

    fn equal_words(got: &[u32], want: &[u32], tick: usize, name: &str) {
        assert_eq!(got.len(), want.len(), "tick {tick} {name}: length");
        if let Some(i) = got.iter().zip(want).position(|(a, b)| a != b) {
            let stride = if name.contains("sampled") || name.contains("unscaled") { 8 } else { 1 };
            panic!("tick {tick} {name} element {} word {}: fused {:08x}, reference {:08x}", i / stride, i % stride, got[i], want[i]);
        }
    }

    fn scene(dust: bool) {
        let def = if dust { super::super::super::whitewater_golden_tests::all_emitters(None) }
            else { with_tick_probe(whitewater_render_def(WaterScene::dam_break(64))) };
        let mut fused = Show::new_with_emitter_oracle(def.clone(), (96, 54), false, &[], Some(false));
        let mut reference = Show::new_with_emitter_oracle(def, (96, 54), false, &[], Some(true));
        fused.restart();
        reference.restart();
        const TICKS: usize = 120;
        let mut ticks = 0;
        let mut saw_counts = false;
        let mut saw_dust_counts = false;
        for _ in 0..2 * TICKS {
            fused.frame(false);
            reference.frame(false);
            assert!(fused.errors().is_empty());
            assert!(reference.errors().is_empty());
            let [due] = fused.probes(["ticks"]);
            assert_eq!([due], reference.probes(["ticks"]));
            assert!(due == 0.0 || due == 1.0);
            if due == 0.0 { continue; }
            ticks += 1;
            for port in &PORTS[..if dust { 6 } else { 4 }] {
                if !dust && *port == "proof_unscaled" { continue; }
                let a = fused.provided_all_bytes("whitewater", port);
                let b = reference.provided_all_bytes("whitewater", port);
                equal_words(bytemuck::cast_slice(&a), bytemuck::cast_slice(&b), ticks, port);
                let nonzero = bytemuck::cast_slice::<_, u32>(&a).iter().any(|&word| word != 0);
                if *port == "proof_counts" { saw_counts |= nonzero; }
                if *port == "proof_dust_counts" { saw_dust_counts |= nonzero; }
            }
            if ticks == TICKS { break; }
        }
        assert_eq!(ticks, TICKS, "scene did not exercise enough accepted ticks");
        assert!(saw_counts, "scene never produced nonzero normal emission counts");
        assert!(!dust || saw_dust_counts, "scene never produced nonzero dust emission counts");
    }

    fn shared<T: bytemuck::Pod>(device: &GpuDevice, data: &[T]) -> GpuBuffer {
        let bytes = bytemuck::cast_slice(data);
        let buffer = device.create_buffer_shared(bytes.len().max(16) as u64);
        buffer.zero_fill();
        // SAFETY: fresh shared storage, sized for the source; not submitted yet.
        unsafe { buffer.write(0, bytes) };
        buffer
    }

    struct Fixture {
        shape: StepShape,
        records: Vec<FluidParticle>,
        particles: GpuBuffer,
        solid: GpuBuffer,
        distance: GpuBuffer,
        faces: [GpuBuffer; 3],
        source: GpuBuffer,
        pool: GpuBuffer,
        state: GpuBuffer,
    }

    impl Fixture {
        fn new(device: &GpuDevice) -> Self {
            let shape = synthetic_shape();
            let records = synthetic_records();
            Self {
                shape, particles: shared(device, &records), records,
                solid: shared(device, &vec![0.1f32; 13 * 13 * 13]),
                distance: shared(device, &vec![-1.0f32; 8 * 8 * 8]),
                faces: synthetic_faces().map(|face| shared(device, &face)),
                source: shared(device, &vec![WhitewaterSource { influence: 1.0, dust_strength: 1.0, kind: 2, pad: 0 }; 13 * 13 * 13]),
                pool: shared(device, &vec![empty_slot(); 256]), state: shared(device, &[0u32; 8]),
            }
        }
        fn frame(&self, dust: bool) -> StepFrame {
            StepFrame { shape: self.shape, count: Some(8), ticks: 3, dt: 0.015625, epoch: 7, seed: 0.375,
                gravity: [0.0; 3], wavecrest_emission: 0.0, turbulence_emission: 32.0,
                min_turbulence: -1.0, max_turbulence: -0.5, inside_emission: true,
                generation_rate: 1.0, spray_speed: 2.0, dust_emission: dust, boundary_dust: true,
                dust_rate: 32.0, influence_base: 1.0, influence_decay: 0.0, min_energy: 0.0, max_energy: 1.0,
                preserve_foam: true }
        }
        fn inputs(&self) -> StepInputs<'_> {
            StepInputs { motion: None, particles: &self.particles, solid: &self.solid,
                obstacle_source: Some(&self.source), faces: self.faces.each_ref(),
                level_set: &self.distance, distance: Some(&self.distance) }
        }
    }

    fn snapshots(device: &GpuDevice, enc: &mut manifold_gpu::GpuEncoder, step: &Step) -> Vec<GpuBuffer> {
        step.reference.snapshots.as_ref().unwrap().iter().map(|src| {
            let dst = device.create_buffer_shared(src.size);
            enc.copy_buffer_to_buffer(src, &dst, src.size);
            dst
        }).collect()
    }

    fn synthetic(dust: bool) {
        let device = crate::test_device();
        let fixtures = [Fixture::new(&device), Fixture::new(&device)];
        let mut stages = [Step::default(), Step::default()];
        stages[0].reference.capture = true;
        stages[1].reference.capture = true;
        stages[1].reference.enabled = true;
        // Exactly representable rates put per_tick immediately around 0.5,
        // then at 1.5. ticks=3 distinguishes round-then-multiply from its inverse.
        let cases = [(31.99999, 8, 0), (32.0, 8, 3), (32.00001, 8, 3), (96.0, 8, 6), (32.0, 0, 0), (32.0, 8, 3)];
        for (tick, (rate, count, expected)) in cases.into_iter().enumerate() {
            let mut encoder = device.create_encoder("whitewater P2 synthetic tick");
            let mut captured = Vec::new();
            for (stage, fixture) in stages.iter_mut().zip(&fixtures) {
                let mut frame = fixture.frame(dust);
                frame.turbulence_emission = rate;
                frame.dust_rate = rate;
                frame.count = Some(count);
                stage.advance_tick(&mut GpuEncoder::new(&mut encoder, &device), &frame, &fixture.inputs(),
                    &fixture.pool, &fixture.state, true).unwrap();
                captured.push(snapshots(&device, &mut encoder, stage));
            }
            encoder.commit_and_wait_completed();
            for (i, name) in PORTS[..if dust { 6 } else { 4 }].iter().enumerate() {
                if !dust && i == 1 { continue; }
                let a = read::<u32>(&captured[0][i], captured[0][i].size as usize / 4);
                let b = read::<u32>(&captured[1][i], captured[1][i].size as usize / 4);
                equal_words(&a, &b, tick, name);
                if i == 3 || i == 5 {
                    assert_eq!(&a[..6], &[expected; 6], "tick {tick} {name}: half-integer count coverage");
                    assert_eq!(&a[6..8], &[0, 0], "dead slots must not emit");
                }
            }
            if count > 0 && dust {
                let unscaled = read::<FluidParticle>(&captured[0][1], 8);
                assert_eq!(bytemuck::bytes_of(&unscaled[6]), bytemuck::bytes_of(&fixtures[0].records[6]));
                assert_eq!(bytemuck::bytes_of(&unscaled[7]), bytemuck::bytes_of(&fixtures[0].records[7]));
            }
        }
        assert_eq!(stages[0].reference.emit_dispatches.get(), 5);
        assert_eq!(stages[1].reference.emit_dispatches.get(), 35);
        assert_eq!(stages[0].reference.dust_dispatches.get(), if dust { 5 } else { 0 });
        assert_eq!(stages[1].reference.dust_dispatches.get(), if dust { 15 } else { 0 });
    }

    #[test]
    fn whitewater_fused_emit_matches_reference() {
        scene(false);
        synthetic(false);
    }

    #[test]
    fn whitewater_fused_dust_matches_reference() {
        scene(true);
        synthetic(true);
    }

    #[test]
    fn whitewater_fused_pipelines_are_dispatch_legal() {
        let device = crate::test_device();
        let mut stage = Step::default();
        stage.pipelines.prepare(&device);
        stage.reference.enabled = true;
        stage.reference.reserve(&device, 1).unwrap();
        for pipeline in &stage.pipelines.fused {
            let max = pipeline.max_threads_per_threadgroup();
            println!("fused {}: max_threads={max:?}, dispatched=256", pipeline.label);
            // GPU proofs are Metal-only for now; Vulkan reports None here.
            assert!(max.is_some_and(|n| n >= 256));
        }
        stage.reference.print_pipeline_limits();
    }
}
