//! Native recurrence values; the headless preset verifies host integration.
use bytemuck::{Pod, Zeroable};
use manifold_gpu::{GpuBinding, GpuDevice};
use manifold_renderer::node_graph::water::WaterParticle;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    count: u32,
    dt: f32,
    gain: f32,
    decay: f32,
}

#[test]
fn water_foam_recurrence_native() {
    let device = GpuDevice::new();
    let shader = include_str!("../../src/node_graph/primitives/shaders/water_foam.wgsl");
    let pipeline = device.create_compute_pipeline(shader, "cs_main", "foam-recurrence-proof");
    let mut particles = [WaterParticle::zeroed(); 5];
    for p in &mut particles {
        p.position_mass[3] = 1.0;
    }
    particles[1].velocity_density[0] = 1.0;
    particles[1].affine_x[1] = 10.0;
    particles[1].affine_y[0] = -10.0;
    particles[2].velocity_density[0] = 1.0;
    particles[2].affine_x[0] = 8.0;
    particles[2].affine_y[1] = -8.0;
    particles[4].position_mass[3] = 0.0;
    let previous = [0.0_f32, 0.0, 0.0, 0.8, 0.8];
    let pb = device.create_buffer_shared(5 * 96);
    let prev = device.create_buffer_shared(5 * 4);
    let output = device.create_buffer_shared(5 * 4);
    unsafe {
        pb.write(0, bytemuck::cast_slice(&particles));
        prev.write(0, bytemuck::cast_slice(&previous));
    }
    let decay = std::f32::consts::LN_2 / 2.0;
    for dt in [2.0, 0.0] {
        let uniforms = Uniforms {
            count: 5,
            dt,
            gain: 3.0,
            decay,
        };
        let mut encoder = device.create_encoder("foam-recurrence");
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &pb,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &prev,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &output,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "foam-recurrence",
        );
        encoder.commit_and_wait_completed();
        let values =
            unsafe { std::slice::from_raw_parts(output.mapped_ptr().unwrap().cast::<f32>(), 5) };
        assert!(values
            .iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
        if dt == 0.0 {
            for i in 0..4 {
                assert_eq!(values[i].to_bits(), previous[i].to_bits());
            }
        } else {
            // sqrt(8²+(-8)²)>10 and speed=1 saturate both smoothsteps.
            let equilibrium = 3.0 / (3.0 + decay);
            let expected = [
                0.0,
                0.0,
                equilibrium * (1.0 - (-(3.0 + decay) * dt).exp()),
                0.4,
                0.0,
            ];
            for (actual, expected) in values.iter().zip(expected) {
                assert!((actual - expected).abs() < 1e-5, "{actual} != {expected}");
            }
        }
        assert_eq!(values[4], 0.0);
    }
}
