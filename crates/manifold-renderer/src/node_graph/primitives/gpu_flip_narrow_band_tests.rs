//! CPU naga and opt-in GPU proofs for `gpu_flip_narrow_band.wgsl`.
//!
//! The GPU module is deliberately kept behind `gpu-proofs`: these tests use a
//! real native Metal device and are not part of the ordinary renderer test
//! set.  The unconditional naga test still catches malformed WGSL and checks
//! the complete binding/uniform contract.

const SHADER: &str = include_str!("shaders/gpu_flip_narrow_band.wgsl");

#[cfg(test)]
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct NbParams {
    n: [u32; 3],
    slots: u32,
    minimum: [f32; 3],
    h: f32,
    dt: f32,
    axis: u32,
    initialized: u32,
    closed_faces: u32,
}

#[cfg(feature = "gpu-proofs")]
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct NbFace {
    velocity: [f32; 4],
    weight: [f32; 4],
}

#[cfg(feature = "gpu-proofs")]
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct NbParticle {
    position_radius: [f32; 4],
    velocity: [f32; 3],
    id: u32,
}

#[cfg(feature = "gpu-proofs")]
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct NbRange {
    start: u32,
    count: u32,
}

#[cfg(test)]
mod cpu_tests {
    use super::{NbParams, SHADER};

    #[test]
    fn gpu_flip_narrow_band_shader_validates() {
        let module = naga::front::wgsl::parse_str(SHADER)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(SHADER)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}"));
        let entries: Vec<&str> = module
            .entry_points
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        for entry in [
            "nb_advect_phi",
            "nb_advect_faces",
            "nb_union",
            "nb_distance_seed",
            "nb_distance_sweep",
            "nb_band_mask",
            "nb_combine_faces",
            "nb_delete",
            "nb_reseed_flags",
            "nb_reseed_status",
            "nb_reseed_write",
        ] {
            assert!(entries.contains(&entry), "missing entry {entry}");
        }

        let mut bindings: Vec<(u32, u32)> = module
            .global_variables
            .iter()
            .filter_map(|(_, global)| global.binding.as_ref().map(|b| (b.group, b.binding)))
            .collect();
        bindings.sort_unstable();
        assert_eq!(
            bindings,
            (0..=12).map(|binding| (0, binding)).collect::<Vec<_>>()
        );

        let params = module
            .global_variables
            .iter()
            .find(|(_, global)| {
                global.space == naga::AddressSpace::Uniform
                    && global
                        .binding
                        .as_ref()
                        .is_some_and(|b| b.group == 0 && b.binding == 0)
            })
            .expect("NbParams uniform at binding 0")
            .1;
        let naga::TypeInner::Struct { members, span } = &module.types[params.ty].inner else {
            panic!("NbParams binding is not a struct");
        };
        assert_eq!(members.len(), 8);
        assert_eq!(*span as usize, std::mem::size_of::<NbParams>());
        assert_eq!(std::mem::size_of::<NbParams>(), 48);
        assert_eq!(
            std::mem::offset_of!(NbParams, n),
            members[0].offset as usize
        );
        assert_eq!(
            std::mem::offset_of!(NbParams, slots),
            members[1].offset as usize
        );
        assert_eq!(
            std::mem::offset_of!(NbParams, minimum),
            members[2].offset as usize
        );
        assert_eq!(
            std::mem::offset_of!(NbParams, h),
            members[3].offset as usize
        );
        assert_eq!(
            std::mem::offset_of!(NbParams, dt),
            members[4].offset as usize
        );
        assert_eq!(
            std::mem::offset_of!(NbParams, axis),
            members[5].offset as usize
        );
        assert_eq!(
            std::mem::offset_of!(NbParams, initialized),
            members[6].offset as usize
        );
        assert_eq!(
            std::mem::offset_of!(NbParams, closed_faces),
            members[7].offset as usize
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::super::prefix_scan::PrefixScan;
    use super::{NbFace, NbParams, NbParticle, NbRange, SHADER};
    use manifold_gpu::{GpuBinding, GpuBuffer};

    use super::super::liquid_surface_tests::read;

    const N: [usize; 3] = [8, 8, 8];
    const H: f32 = 1.0;

    fn cells(n: [usize; 3]) -> usize {
        n.iter().product()
    }

    fn faces(n: [usize; 3]) -> usize {
        (n[0] + 1) * (n[1] + 1) * (n[2] + 1)
    }

    fn index(p: [usize; 3], n: [usize; 3]) -> usize {
        p[0] + n[0] * (p[1] + n[1] * p[2])
    }

    fn coords(i: usize, n: [usize; 3]) -> [usize; 3] {
        [i % n[0], (i / n[0]) % n[1], i / (n[0] * n[1])]
    }

    fn params(n: [usize; 3]) -> NbParams {
        NbParams {
            n: n.map(|v| v as u32),
            slots: 0,
            minimum: [0.0; 3],
            h: H,
            dt: 0.0,
            axis: 0,
            initialized: 1,
            closed_faces: 0,
        }
    }

    fn shared<T: bytemuck::Pod>(device: &crate::TestDevice, values: &[T]) -> GpuBuffer {
        let buffer = device.create_buffer_shared((size_of_val(values) as u64).max(16));
        buffer.zero_fill();
        if !values.is_empty() {
            // SAFETY: the shared buffer is sized for `values`; no work is in flight.
            unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
        }
        buffer
    }

    fn run<T: bytemuck::Pod>(
        device: &crate::TestDevice,
        entry: &str,
        uniform: &NbParams,
        bindings: Vec<GpuBinding<'_>>,
        output: &GpuBuffer,
        count: usize,
    ) -> Vec<T> {
        let pipeline = device.create_compute_pipeline(SHADER, entry, "gpu-flip-narrow-band-proof");
        let mut all = vec![GpuBinding::Bytes {
            binding: 0,
            data: bytemuck::bytes_of(uniform),
        }];
        all.extend(bindings);
        let mut encoder = device.create_encoder("gpu-flip-narrow-band-proof");
        encoder.dispatch_compute(&pipeline, &all, [count.div_ceil(256) as u32, 1, 1], entry);
        encoder.commit_and_wait_completed();
        read(output, count)
    }

    fn bind(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
        GpuBinding::Buffer {
            binding,
            buffer,
            offset: 0,
        }
    }

    fn close(actual: f32, expected: f64, scale: f64, what: &str) {
        assert!(
            (f64::from(actual) - expected).abs() <= 2e-5 * scale.max(1.0),
            "{what}: {actual} vs {expected}"
        );
    }

    fn scalar_cell_values(f: impl Fn([usize; 3]) -> f32) -> Vec<f32> {
        (0..cells(N)).map(|i| f(coords(i, N))).collect()
    }

    #[test]
    fn gpu_flip_narrow_band_mask() {
        let device = crate::test_device();
        let phi = scalar_cell_values(|p| match p[0] {
            0 => -3.0,
            1 => -2.5,
            2 => -3.5,
            _ => 1.0,
        });
        let solid = vec![4.0_f32; faces(N)];
        let phi_buffer = shared(&device, &phi);
        let solid_buffer = shared(&device, &solid);
        let output = shared(&device, &vec![0_u32; cells(N)]);
        let got: Vec<u32> = run(
            &device,
            "nb_band_mask",
            &params(N),
            vec![
                bind(1, &phi_buffer),
                bind(6, &solid_buffer),
                bind(7, &output),
            ],
            &output,
            cells(N),
        );
        for (i, value) in got.iter().enumerate() {
            let x = coords(i, N)[0];
            assert_eq!(*value, u32::from(x != 0 && x != 2), "cell {i}");
        }

        // A negative cell outside the liquid band remains included when the
        // solid signed distance is inside the three-cell proximity band.
        let solid_buffer = shared(&device, &vec![3.0_f32; faces(N)]);
        let phi = scalar_cell_values(|_| -4.0);
        let phi_buffer = shared(&device, &phi);
        let got: Vec<u32> = run(
            &device,
            "nb_band_mask",
            &params(N),
            vec![
                bind(1, &phi_buffer),
                bind(6, &solid_buffer),
                bind(7, &output),
            ],
            &output,
            cells(N),
        );
        assert!(got.iter().all(|&value| value == 1));
    }

    #[test]
    fn gpu_flip_narrow_band_advect() {
        let device = crate::test_device();
        let phi = scalar_cell_values(|p| p[0] as f32 + 2.0 * p[1] as f32 - p[2] as f32);
        let face_count = faces(N);
        let faces_in: Vec<NbFace> = (0..face_count)
            .map(|_| NbFace {
                velocity: [0.5, -0.25, 0.125, 0.0],
                weight: [1.0; 4],
            })
            .collect();
        let phi_buffer = shared(&device, &phi);
        let face_buffer = shared(&device, &faces_in);
        let output = shared(&device, &vec![0.0_f32; cells(N)]);
        let mut uniform = params(N);
        uniform.dt = 0.25;
        let got: Vec<f32> = run(
            &device,
            "nb_advect_phi",
            &uniform,
            vec![
                bind(1, &phi_buffer),
                bind(3, &face_buffer),
                bind(2, &output),
            ],
            &output,
            cells(N),
        );
        for (i, actual) in got.iter().enumerate() {
            let p = coords(i, N).map(|v| v as f64 + 0.5);
            let departure = [p[0] - 0.125, p[1] + 0.0625, p[2] - 0.03125];
            let clamped = [
                departure[0].clamp(0.5, N[0] as f64 - 0.5),
                departure[1].clamp(0.5, N[1] as f64 - 0.5),
                departure[2].clamp(0.5, N[2] as f64 - 0.5),
            ];
            close(
                *actual,
                clamped[0] - 0.5 + 2.0 * (clamped[1] - 0.5) - (clamped[2] - 0.5),
                8.0,
                "advected phi",
            );
        }
    }

    #[test]
    fn gpu_flip_narrow_band_combine() {
        let device = crate::test_device();
        let grid = vec![
            NbFace {
                velocity: [7.0, 8.0, 9.0, 0.0],
                weight: [0.75, 0.75, 0.75, 0.0]
            };
            faces(N)
        ];
        let particle = vec![
            NbFace {
                velocity: [1.0, 2.0, 3.0, 0.0],
                weight: [1.0, 1.0, 1.0, 0.0]
            };
            faces(N)
        ];
        let solid = vec![3.0_f32; faces(N)];
        for (phi_value, expected) in [(-2.0_f32, [1.0_f32, 2.0, 3.0]), (-2.01, [7.0, 8.0, 9.0])] {
            let phi = vec![phi_value; cells(N)];
            let phi_buffer = shared(&device, &phi);
            let grid_buffer = shared(&device, &grid);
            let particle_buffer = shared(&device, &particle);
            let solid_buffer = shared(&device, &solid);
            let got: Vec<NbFace> = run(
                &device,
                "nb_combine_faces",
                &params(N),
                vec![
                    bind(1, &phi_buffer),
                    bind(3, &grid_buffer),
                    bind(4, &particle_buffer),
                    bind(6, &solid_buffer),
                ],
                &particle_buffer,
                faces(N),
            );
            let face = got[index([3, 3, 3], N.map(|v| v + 1))];
            for (axis, expected) in expected.iter().enumerate() {
                assert_eq!(
                    face.velocity[axis], *expected,
                    "phi {phi_value}, axis {axis}"
                );
            }
        }
    }

    #[test]
    fn gpu_flip_narrow_band_delete() {
        let device = crate::test_device();
        let phi = vec![-4.0_f32; cells(N)];
        let solid: Vec<f32> = (0..faces(N))
            .map(|i| {
                if coords(i, N.map(|v| v + 1))[0] < 4 {
                    2.0
                } else {
                    4.0
                }
            })
            .collect();
        let particles = vec![
            NbParticle {
                position_radius: [2.2, 2.2, 2.2, 1.0],
                velocity: [1.0, 0.0, 0.0],
                id: 1,
            },
            NbParticle {
                position_radius: [6.2, 2.2, 2.2, 1.0],
                velocity: [2.0, 0.0, 0.0],
                id: 2,
            },
            NbParticle {
                position_radius: [6.2, 3.2, 2.2, 0.0],
                velocity: [3.0, 0.0, 0.0],
                id: 3,
            },
        ];
        let phi_buffer = shared(&device, &phi);
        let solid_buffer = shared(&device, &solid);
        let particle_buffer = shared(&device, &particles);
        let mut uniform = params(N);
        uniform.slots = particles.len() as u32;
        let got: Vec<NbParticle> = run(
            &device,
            "nb_delete",
            &uniform,
            vec![
                bind(1, &phi_buffer),
                bind(6, &solid_buffer),
                bind(9, &particle_buffer),
            ],
            &particle_buffer,
            particles.len(),
        );
        assert_eq!(
            got[0].position_radius[3], 1.0,
            "deep particle near solid is retained"
        );
        assert_eq!(
            got[1].position_radius[3], 0.0,
            "deep particle far from solid is deleted"
        );
        assert_eq!(
            got[2].position_radius[3], 0.0,
            "already-dead slot remains clear"
        );
    }

    #[test]
    fn gpu_flip_narrow_band_redistance() {
        let device = crate::test_device();
        // Rectangular extents catch swapped line axes; shallow fields require
        // propagation rather than returning the input distance unchanged.
        let n = [5, 7, 3];
        for plane in 0..4 {
            let expected: Vec<f32> = (0..cells(n))
                .map(|i| {
                    let p = coords(i, n);
                    if plane < 3 {
                        p[plane] as f32 - 1.5
                    } else {
                        p.iter().sum::<usize>() as f32 - 4.5
                    }
                })
                .collect();
            let phi: Vec<f32> = expected.iter().map(|v| 0.125 * v.signum()).collect();
            let phi_buffer = shared(&device, &phi);
            let output = shared(&device, &vec![0.0_f32; cells(n)]);
            let mut uniform = params(n);
            let _: Vec<f32> = run(
                &device,
                "nb_distance_seed",
                &uniform,
                vec![bind(1, &phi_buffer), bind(2, &output)],
                &output,
                cells(n),
            );
            for axis in 0usize..3 {
                uniform.axis = axis as u32;
                let _: Vec<f32> = run(
                    &device,
                    "nb_distance_sweep",
                    &uniform,
                    vec![bind(2, &output)],
                    &output,
                    n[(axis + 1) % 3] * n[(axis + 2) % 3],
                );
            }
            let got: Vec<f32> = read(&output, cells(n));
            for (actual, expected) in got.iter().zip(expected) {
                close(*actual, f64::from(expected), 8.0, "redistanced phi");
            }
        }
    }

    #[test]
    fn gpu_flip_narrow_band_union() {
        let device = crate::test_device();
        let history = shared(&device, &vec![-10.0_f32; cells(N)]);
        let particle_values = scalar_cell_values(|p| if p[0] < 4 { 3.0 } else { -12.0 });
        let particles = shared(&device, &particle_values);
        let output = shared(&device, &vec![99.0_f32; cells(N)]);
        for initialized in [0, 1] {
            let mut uniform = params(N);
            uniform.initialized = initialized;
            let got: Vec<f32> = run(
                &device,
                "nb_union",
                &uniform,
                vec![bind(1, &history), bind(5, &particles), bind(2, &output)],
                &output,
                cells(N),
            );
            for (i, &actual) in got.iter().enumerate() {
                let expected = if initialized == 0 {
                    particle_values[i]
                } else {
                    particle_values[i].min(-9.0)
                };
                assert_eq!(actual, expected, "cell {i}, initialized {initialized}");
            }
        }
    }

    #[test]
    fn gpu_flip_narrow_band_advect_faces() {
        let device = crate::test_device();
        let phi = shared(&device, &vec![-4.0_f32; cells(N)]);
        // u=y, v=2, w=0: the analytic departure has y decreased by 2dt.
        let input: Vec<NbFace> = (0..faces(N))
            .map(|i| {
                let p = coords(i, N.map(|n| n + 1));
                NbFace {
                    velocity: [p[1] as f32 + 0.5, 2.0, 0.0, 0.0],
                    weight: [1.0; 4],
                }
            })
            .collect();
        let source = shared(&device, &input);
        let output = shared(&device, &vec![NbFace::default(); faces(N)]);
        let mut uniform = params(N);
        uniform.dt = 0.25;
        uniform.closed_faces = 0b11_1111;
        let got: Vec<NbFace> = run(
            &device,
            "nb_advect_faces",
            &uniform,
            vec![bind(1, &phi), bind(3, &source), bind(4, &output)],
            &output,
            faces(N),
        );
        for (i, face) in got.iter().enumerate() {
            let p = coords(i, N.map(|n| n + 1));
            for a in 0..3 {
                if (0..3).any(|b| b != a && p[b] == N[b]) {
                    assert_eq!(face.weight[a], 0.0, "padding {i}/{a}");
                    assert_eq!(face.velocity[a], 0.0);
                } else if p[a] == 0 || p[a] == N[a] {
                    assert_eq!(face.velocity[a], 0.0, "wall {i}/{a}");
                    assert_eq!(face.weight[a], 1.0);
                } else {
                    let expected = match a {
                        0 => (p[1] as f64).max(0.5),
                        1 => 2.0,
                        _ => 0.0,
                    };
                    close(face.velocity[a], expected, 8.0, "advected face");
                    assert_eq!(face.weight[a], 1.0);
                }
            }
        }
    }

    #[test]
    fn gpu_flip_narrow_band_reseed() {
        let device = crate::test_device();
        let eligible = [index([2, 3, 3], N), index([3, 3, 3], N)];
        let mut previous = vec![-2.0_f32; cells(N)];
        for &i in &eligible {
            previous[i] = -3.0;
        }
        let previous_buffer = shared(&device, &previous);
        let phi = shared(&device, &vec![-2.0_f32; cells(N)]);
        let solid = shared(&device, &vec![4.0_f32; faces(N)]);
        let mut ranges = vec![NbRange::default(); cells(N)];
        let mut live = 0;
        for (i, range) in ranges.iter_mut().enumerate() {
            range.start = live;
            range.count = u32::from(eligible.contains(&i));
            live += range.count;
        }
        let ranges_buffer = shared(&device, &ranges);
        let mut input = vec![NbParticle::default(); 20];
        for (slot, x) in [2.0, 3.0].into_iter().enumerate() {
            input[slot] = NbParticle {
                position_radius: [x + 0.1, 3.1, 3.1, 0.31],
                velocity: [9.0; 3],
                id: 100 + slot as u32,
            };
        }
        let particles = shared(&device, &input);
        let flags = shared(&device, &vec![99_u32; 8 * cells(N)]);
        let ranks = shared(&device, &vec![0_u32; 8 * cells(N)]);
        let status = shared(&device, &[0_u32; 2]);
        let velocity = shared(
            &device,
            &vec![
                NbFace {
                    velocity: [1.0, 2.0, 3.0, 0.0],
                    weight: [1.0; 4]
                };
                faces(N)
            ],
        );
        let mut uniform = params(N);
        uniform.slots = input.len() as u32;
        let got: Vec<u32> = run(
            &device,
            "nb_reseed_flags",
            &uniform,
            vec![
                bind(1, &phi),
                bind(6, &solid),
                bind(8, &previous_buffer),
                bind(9, &particles),
                bind(10, &ranges_buffer),
                bind(11, &flags),
            ],
            &flags,
            8 * cells(N),
        );
        for (i, &flag) in got.iter().enumerate() {
            assert_eq!(
                flag,
                u32::from(eligible.contains(&(i / 8)) && i % 8 != 0),
                "site {i}"
            );
        }
        let mut scan = PrefixScan::default();
        scan.prepare(&device);
        scan.parents(&device, got.len()).unwrap();
        let mut encoder = device.create_encoder("narrow-band-reseed-scan");
        scan.encode_into(&mut encoder, got.len(), &flags, &ranks);
        encoder.commit_and_wait_completed();
        // A shortage must prevent every append, including the prefix that fits.
        for capacity in [10, 20] {
            uniform.slots = capacity;
            let result: Vec<u32> = run(
                &device,
                "nb_reseed_status",
                &uniform,
                vec![
                    bind(10, &ranges_buffer),
                    bind(11, &ranks),
                    bind(12, &status),
                ],
                &status,
                2,
            );
            assert_eq!(result, vec![16_u32.saturating_sub(capacity), 16]);
            let pipeline =
                device.create_compute_pipeline(SHADER, "nb_reseed_write", "nb-reseed-write");
            let mut encoder = device.create_encoder("nb-reseed-write");
            encoder.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniform),
                    },
                    bind(3, &velocity),
                    bind(9, &particles),
                    bind(10, &ranges_buffer),
                    bind(11, &ranks),
                    bind(12, &status),
                ],
                [(8 * cells(N)).div_ceil(256) as u32, 1, 1],
                "nb-reseed-write",
            );
            encoder.commit_and_wait_completed();
            let written: Vec<NbParticle> = read(&particles, input.len());
            if capacity == 10 {
                assert_eq!(
                    bytemuck::cast_slice::<_, u8>(&written),
                    bytemuck::cast_slice::<_, u8>(&input)
                );
                continue;
            }
            for slot in 0..2 {
                assert_eq!(written[slot].id, input[slot].id);
            }
            for (rank, particle) in written[2..16].iter().enumerate() {
                let site = rank % 7 + 1;
                let x = 2.0 + (rank / 7) as f32;
                assert_eq!(
                    &particle.position_radius[..3],
                    &[
                        x + 0.25 + 0.5 * (site & 1) as f32,
                        3.25 + 0.5 * ((site >> 1) & 1) as f32,
                        3.25 + 0.5 * ((site >> 2) & 1) as f32
                    ]
                );
                assert_eq!(particle.velocity, [1.0, 2.0, 3.0]);
                assert_eq!(particle.position_radius[3], 0.31017);
            }
            assert!(written[16..].iter().all(|p| p.position_radius[3] == 0.0));
        }
    }
}
