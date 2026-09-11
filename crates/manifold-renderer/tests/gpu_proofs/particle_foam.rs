//! Actual foam raster coverage and submerged-particle rejection.
use bytemuck::Zeroable;
use manifold_gpu::{
    GpuBinding, GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use manifold_renderer::node_graph::camera::{Camera, delinearize_depth};
use manifold_renderer::node_graph::primitives::{SurfacePixelUniforms, SurfaceSplatUniforms};
use manifold_renderer::node_graph::water::WaterParticle;

#[test]
fn particle_foam_surface_and_depth_mask_native() {
    let device = GpuDevice::new();
    let shader = concat!(
        include_str!("../../src/node_graph/primitives/shaders/particle_splat_common.wgsl"),
        "\n",
        include_str!("../../src/node_graph/primitives/shaders/particle_foam_splat.wgsl")
    );
    let splat = device.create_compute_pipeline(shader, "cs_main", "foam-splat-proof");
    let resolve = device.create_compute_pipeline(
        include_str!("../../src/node_graph/primitives/shaders/particle_thickness_resolve.wgsl"),
        "cs_main",
        "foam-resolve-proof",
    );
    let width = 128;
    let camera = Camera::look_at([0.0, 0.0, 3.0], [0.0; 3], [0.0, 1.0, 0.0], 0.7, 0.05, 20.0);
    let radius = 0.0234375;
    let uniforms = SurfaceSplatUniforms {
        view: camera.view,
        tan_half_fov: (0.7_f32 * 0.5).tan(),
        near: camera.near,
        far: camera.far,
        radius,
        width,
        height: width,
        count: 1,
        _pad: 0,
    };
    let pixels = SurfacePixelUniforms {
        width,
        height: width,
        _pad0: 0,
        _pad1: 0,
    };
    let depth = device.create_texture(&GpuTextureDesc {
        width,
        height: width,
        depth: 1,
        format: GpuTextureFormat::R32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
        label: "foam-depth",
        mip_levels: 1,
    });
    let raw = delinearize_depth(3.0, camera.near, camera.far);
    device.upload_texture(
        &depth,
        bytemuck::cast_slice(&vec![raw; (width * width) as usize]),
    );
    let output = device.create_texture(&GpuTextureDesc {
        width,
        height: width,
        depth: 1,
        format: GpuTextureFormat::R16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::RENDER_TARGET_FULL,
        label: "foam-out",
        mip_levels: 1,
    });
    let particle = device.create_buffer_shared(96);
    let fraction = device.create_buffer_shared(4);
    let scratch = device.create_buffer(u64::from(width * width) * 4);
    let readback = device.create_buffer_shared(u64::from(width * width) * 2);
    for (z, amount, visible) in [
        (-radius, 1.0_f32, true),
        (-radius, 0.0, false),
        (-0.5, 1.0, false),
    ] {
        let mut p = WaterParticle::zeroed();
        p.position_mass = [0.0, 0.0, z, 1.0];
        unsafe {
            particle.write(0, bytemuck::bytes_of(&p));
            fraction.write(0, bytemuck::bytes_of(&amount));
        }
        let mut encoder = device.create_encoder("foam-coverage-proof");
        encoder.clear_buffer(&scratch);
        encoder.dispatch_compute(
            &splat,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &particle,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &fraction,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: &depth,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &scratch,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "foam-splat-proof",
        );
        encoder.dispatch_compute(
            &resolve,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&pixels),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &scratch,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: &output,
                },
            ],
            [(width * width).div_ceil(256), 1, 1],
            "foam-resolve-proof",
        );
        encoder.copy_texture_to_buffer(&output, &readback, width, width, width * 2);
        encoder.commit_and_wait_completed();
        let halves = unsafe {
            std::slice::from_raw_parts(
                readback.mapped_ptr().unwrap().cast::<u16>(),
                (width * width) as usize,
            )
        };
        let values: Vec<_> = halves
            .iter()
            .map(|&b| half::f16::from_bits(b).to_f32())
            .collect();
        assert!(
            values
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
        );
        assert_eq!(
            values.iter().any(|&v| v > 0.1),
            visible,
            "z={z} amount={amount}"
        );
    }
}
