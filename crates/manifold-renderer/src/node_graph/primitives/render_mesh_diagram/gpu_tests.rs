use super::*;
use crate::headless_readback::readback_raw_halves;
use crate::node_graph::camera::CameraMode;
use manifold_gpu::GpuTextureFormat;

#[test]
fn math_view_world_grid_matches_camera_and_ignores_object_transform() {
    const W: u32 = 640;
    const H: u32 = 360;
    let guard = crate::test_device();
    let device = guard.arc();
    let target = crate::render_target::RenderTarget::new(
        &device,
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        "world-grid-proof",
    );
    let msaa = device.create_texture_msaa_memoryless(
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        MSAA_SAMPLE_COUNT,
        "world-grid-proof",
    );
    let buffer = device.create_buffer_shared(3 * std::mem::size_of::<MeshVertex>() as u64);
    buffer.zero_fill();
    let pipeline = device.create_render_pipeline_msaa(
        SHADER,
        "vs_main",
        "fs_main",
        GpuTextureFormat::Rgba16Float,
        Some(DIAGRAM_BLEND),
        MSAA_SAMPLE_COUNT,
        "world-grid-proof",
    );
    let camera = Camera::look_at(
        [8.0, 7.0, 10.0],
        [0.0; 3],
        [0.0, 1.0, 0.0],
        1.0,
        0.05,
        200.0,
    );
    let render = |camera: Camera, transform: Transform, enabled: u32| {
        let view_proj = camera.view_proj(W as f32 / H as f32);
        let u = DiagramUniforms {
            view_proj,
            model: RenderMeshDiagram::model_matrix(transform, &camera),
            viewport: [W as f32, H as f32, 0.0, 0.0],
            radius: 0.2,
            line_width: 1.0,
            grid: enabled,
            inv_view_proj: super::super::render_scene::mat4_inverse(view_proj).unwrap(),
            camera_pos_far: [camera.pos[0], camera.pos[1], camera.pos[2], camera.far],
            ..bytemuck::Zeroable::zeroed()
        };
        let mut encoder = device.create_encoder("world-grid-proof");
        encoder.draw_instanced_msaa(
            &pipeline,
            &msaa,
            &target.texture,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&u),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &buffer,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &buffer,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &buffer,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &buffer,
                    offset: 0,
                },
            ],
            18,
            1,
            GpuLoadAction::Clear,
            "world-grid-proof",
        );
        encoder.commit_and_wait_completed();
        readback_raw_halves(&device, &target.texture, W, H)
    };
    let mut previous = None;
    for camera in [
        camera,
        Camera {
            mode: CameraMode::Orthographic { half_height: 9.0 },
            ..camera
        },
    ] {
        let reference = render(camera, Transform::default(), 1);
        let moved = render(
            camera,
            Transform {
                pos: [9.0, -3.0, 4.0],
                rot_euler: [0.4, -0.7, 1.2],
                scale: [0.2, 3.0, 1.5],
                ..Transform::default()
            },
            1,
        );
        assert_eq!(reference, moved, "object pose must not move the world grid");
        // Known world coordinates must land on the camera's projected pixels,
        // far beyond the old radius-limited square. Test alpha directly so
        // the absence of blending cannot turn transparent cell interiors lit.
        let alpha_at = |point: [f32; 3]| {
            let pixel = camera.project_to_pixel(point, W, H).unwrap();
            assert!(
                pixel.px >= 0.0 && pixel.px < W as f32 && pixel.py >= 0.0 && pixel.py < H as f32
            );
            let x = pixel.px as usize;
            let y = pixel.py as usize;
            let offset = (y * W as usize + x) * 8 + 6;
            half::f16::from_bits(u16::from_le_bytes([
                reference[offset],
                reference[offset + 1],
            ]))
            .to_f32()
        };
        assert!(
            alpha_at([4.0, 0.0, 0.5]) > 0.08,
            "grid must align to a projected world line beyond sample radius"
        );
        assert!(
            alpha_at([4.5, 0.0, 0.5]) < 0.01,
            "cell interior must stay clear"
        );
        assert!(
            render(camera, Transform::default(), 0)
                .iter()
                .all(|value| *value == 0),
            "Grid off must clear the grid"
        );
        if let Some(previous) = previous {
            assert_ne!(
                reference, previous,
                "changing projection must move the grid"
            );
        }
        previous = Some(reference);
    }
}
