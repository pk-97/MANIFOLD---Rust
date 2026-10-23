//! The density projection's source.

use super::*;

fn density_support_sources() -> [String; 2] {
    const CULL: &str = "                    if finite(q) && any(abs(centre - q) >= vec3<f32>(1.0)) {\n                        continue;\n                    }\n";
    let source = super::super::gpu_flip_step::STEP_SHADER;
    assert_eq!(source.matches(CULL).count(), 1, "density support predicate occurs once");
    let original = source.replacen(CULL, "", 1);
    [original.as_str(), source].map(|shader| {
        with_stats_layout(&format!("{LIQUID_POSE}\n{LIQUID_COLLIDER}\n{FORCE_LATTICE}\n{shader}"))
    })
}

#[test]
fn gpu_flip_density_support_shader_validates() {
    for source in density_support_sources() {
        let module = naga::front::wgsl::parse_str(&source).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&source)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module).expect("density support shader validates");
    }
}

#[test]
fn gpu_flip_density_support_matches_original() {
    let mut pass = Pass::new();
    let pipelines = density_support_sources().map(|source| {
        pass.device.create_compute_pipeline(&source, "density_source", "density-support-proof")
    });
    let mut particles = Vec::new();
    let mut rng = Stream::new(0xde8517);
    // Near rest rather than clamp-saturated: eight slightly jittered sites
    // per cell. Leave one corner empty to exercise the face-neighbour air rule.
    for c in 0..cell_len() - 1 {
        let p = cell_coords(c);
        for site in 0..8 {
            let q: [f32; 3] = std::array::from_fn(|a| p[a] as f32 + 0.25 + 0.5 * ((site >> a) & 1) as f32 + rng.signed(0.004));
            let x: [f32; 3] = std::array::from_fn(|a| MIN[a] + H * q[a]);
            particles.push(FluidParticle { position_radius: [x[0], x[1], x[2], 0.08], ..FluidParticle::default() });
        }
    }
    let centre = [2.5_f32; 3];
    let x = centre.map(|q| q * H);
    particles.push(FluidParticle { position_radius: [MIN[0] + x[0], MIN[1] + x[1], MIN[2] + x[2], 0.08], ..FluidParticle::default() });
    let mut support = [0; 2];
    for a in 0..3 {
        for side in [-1.0_f32, 1.0] {
            let boundary = MIN[a] + H * (centre[a] + side);
            for direction in [-1, 0, 1] {
                let mut x: [f32; 3] = std::array::from_fn(|b| MIN[b] + H * centre[b]);
                x[a] = boundary;
                // Also include world-coordinate neighbours; several ULPs
                // survive the world-to-lattice rounding at every box offset.
                for _ in 0..4 {
                    x[a] = match direction { -1 => x[a].next_down(), 1 => x[a].next_up(), _ => x[a] };
                }
                let q: [f32; 3] = std::array::from_fn(|b| (x[b] - MIN[b]) / H);
                let outside = (0..3).any(|b| (centre[b] - q[b]).abs() >= 1.0);
                support[usize::from(outside)] += 1;
                particles.push(FluidParticle { position_radius: [x[0], x[1], x[2], 0.08], ..FluidParticle::default() });
            }
        }
    }
    assert!(support.iter().all(|&count| count > 0), "positive and culled zero-support probes: {support:?}");
    let mut water = vec![1.0_f32; cell_len()];
    water[0] = 0.0;
    let mask: Vec<u32> = (0..cell_len()).map(|c| u32::from(c % 4 != 0)).collect();
    for nonfinite in [false, true] {
        let mut input = particles.clone();
        if nonfinite {
            for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                let mut p = input[0];
                p.position_radius[0] = value;
                input.push(p);
            }
        }
        let (sorted, ranges) = cpu_sort(&input);
        for (body_count, distance) in [(0, 4.0 * H), (1, 4.0 * H), (1, -4.0 * H)] {
            for narrow in [false, true] {
                pass.bound.clear();
                let threads = pass.every_tile();
                pass.bind(1, &ranges).bind(2, &sorted).bind(6, &water)
                    .bind(9, &vec![distance; face_len()]).bind(45, &mask)
                    .bind(46, &[0_u32; 12]).bind(5, &vec![f32::NAN; cell_len()]);
                let step = StepParams {
                    capacity: sorted.len() as u32, body_count, rate: 4.0,
                    narrow_band: if narrow { 2 } else { 0 }, ..lattice()
                };
                let outputs = pipelines.each_ref().map(|pipeline| {
                    let mut bindings = vec![GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&step) }];
                    bindings.extend(pass.bound.iter().map(|(binding, buffer)| GpuBinding::Buffer { binding: *binding, buffer, offset: 0 }));
                    let mut enc = pass.device.create_encoder("density support parity");
                    enc.dispatch_compute(pipeline, &bindings, [threads.div_ceil(256) as u32, 1, 1], "density-support-proof");
                    enc.commit_and_wait_completed();
                    pass.bound::<f32>(5, cell_len())
                });
                for (c, (&want, &got)) in outputs[0].iter().zip(&outputs[1]).enumerate() {
                    let context = format!("cell{c} body{body_count} sdf{distance} narrow{narrow} nonfinite{nonfinite}");
                    if want.is_finite() {
                        assert_eq!(got.to_bits(), want.to_bits(), "{context}: finite output bits");
                    } else if want.is_nan() {
                        assert!(got.is_nan(), "{context}: NaN classification");
                    } else {
                        assert!(got.is_infinite() && got.is_sign_negative() == want.is_sign_negative(), "{context}: infinity classification");
                    }
                    if c == 0 || (narrow && mask[c] == 0) {
                        assert_eq!(got, 0.0, "{context}: air/inactive mask");
                    }
                }
                if !nonfinite {
                    assert!(outputs[0].iter().all(|v| v.is_finite()), "finite fixture yields finite output");
                    if body_count == 0 {
                        let value = outputs[0][cell_index([2, 2, 2])];
                        assert!(value < 0.0 && value.abs() < 2.0, "positive marker contribution is visible below the density clamp: {value}");
                        assert!(outputs[0].iter().any(|v| v.abs() > 0.0 && v.abs() < 2.0), "nonzero unsaturated near-rest outputs");
                    }
                }
            }
        }
    }
}

