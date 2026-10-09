use crate::testkit::array_harness::{Harness, params, read};
use super::super::offset_lattice::OffsetLattice;
use super::super::redistance_lattice::RedistanceLattice;
use super::*;

#[test]
fn fluid_fill_pits_gpu_values_match_f64_and_zero_identity() {
    let mut h = Harness::new();
    let n = 16;
    let spacing = 0.25;
    let grow = 0.45;
    let field = spheres(
        n,
        spacing,
        &[DVec3::new(1.4, 2., 2.), DVec3::new(2.6, 2., 2.)],
        0.8,
    );
    let field: Vec<f32> = field.into_iter().map(|v| v as f32).collect();
    let (input, _) = h.array(&field, field.len());
    let (grown, grown_buf) = h.array::<f32>(&[], field.len());
    let (rebuilt, rebuilt_buf) = h.array::<f32>(&[], field.len());
    let (closed, closed_buf) = h.array::<f32>(&[], field.len());
    let mut offset = OffsetLattice::new();
    let mut distance = RedistanceLattice::new();
    for amount in [0., grow] {
        let (_, errors) = h.run(
            &mut offset,
            &[("levelset", input)],
            &[("out", grown)],
            &params(&[("offset", -amount)]),
        );
        assert!(errors.is_empty(), "{errors:?}");
        let p = params(&[
            ("nodes_x", n as f32),
            ("nodes_y", n as f32),
            ("nodes_z", n as f32),
            ("size_x", 3.75),
            ("size_y", 3.75),
            ("size_z", 3.75),
            ("band", amount + 0.5),
            ("enabled", amount),
        ]);
        let (_, errors) = h.run(
            &mut distance,
            &[("levelset", grown)],
            &[("out", rebuilt)],
            &p,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let (_, errors) = h.run(
            &mut offset,
            &[("levelset", rebuilt)],
            &[("out", closed)],
            &params(&[("offset", amount)]),
        );
        assert!(errors.is_empty(), "{errors:?}");
        let result = read::<f32>(&closed_buf, field.len());
        if amount == 0. {
            assert_eq!(
                result.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                field.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
            );
        } else {
            let grown = read::<f32>(&grown_buf, field.len());
            for (&got, &want) in grown.iter().zip(&field) {
                assert_eq!(got, want - amount);
            }
            let reference = redistance(
                &grown.iter().map(|&v| f64::from(v)).collect::<Vec<_>>(),
                n,
                spacing,
                f64::from(amount + 0.5),
            );
            let rebuilt = read::<f32>(&rebuilt_buf, field.len());
            for (i, (&actual, expected)) in rebuilt.iter().zip(reference).enumerate() {
                assert!(
                    (f64::from(actual) - expected).abs() < 2e-5,
                    "redistance {i}: {actual} != {expected}"
                );
                assert!((result[i] - (actual + amount)).abs() < 1e-6);
            }
        }
    }
}
#[test]
fn fluid_fill_pits_gpu_fused_redistance_shrink_matches_unfused() {
    use manifold_gpu::GpuBinding;
    let mut h = Harness::new();
    let n = 8;
    let total = n * n * n;
    let values: Vec<f32> = spheres(n, 0.25, &[DVec3::splat(0.9)], 0.65)
        .into_iter()
        .map(|v| v as f32)
        .collect();
    let (input, input_buf) = h.array(&values, total);
    let (mid, _) = h.array::<f32>(&[], total);
    let (out, out_buf) = h.array::<f32>(&[], total);
    let (_, fused_buf) = h.array::<f32>(&[], total);
    let source = fused_redistance_offset();
    let pipeline = h.device.create_compute_pipeline(
        &source,
        crate::freeze::codegen::ENTRY,
        "closing fused proof",
    );
    for enabled in [0., 1.] {
        let p = params(&[
            ("nodes_x", 8.),
            ("nodes_y", 8.),
            ("nodes_z", 8.),
            ("size_x", 1.75),
            ("size_y", 1.75),
            ("size_z", 1.75),
            ("band", 0.6),
            ("enabled", enabled),
        ]);
        let (_, errors) = h.run(
            &mut RedistanceLattice::new(),
            &[("levelset", input)],
            &[("out", mid)],
            &p,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let (_, errors) = h.run(
            &mut OffsetLattice::new(),
            &[("levelset", mid)],
            &[("out", out)],
            &params(&[("offset", 0.2)]),
        );
        assert!(errors.is_empty(), "{errors:?}");
        let uniforms: [f32; 12] = [8., 8., 8., 1.75, 1.75, 1.75, 0.6, enabled, 0.2, 0., 0., 0.];
        let mut encoder = h.device.create_encoder("closing fused proof");
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::cast_slice(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &input_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &fused_buf,
                    offset: 0,
                },
            ],
            [(total as u32).div_ceil(256), 1, 1],
            "closing fused proof",
        );
        encoder.commit_and_wait_completed();
        for (a, b) in read::<f32>(&out_buf, total)
            .iter()
            .zip(read::<f32>(&fused_buf, total))
        {
            assert!((a - b).abs() < 2e-6, "fused {b} != standalone {a}");
        }
    }
}
