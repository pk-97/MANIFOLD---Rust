//! Focused GPU proofs for the mesh diagram's scene-depth occlusion path.
//!
//! These tests bind the production `SHADER` directly. They deliberately use
//! small deterministic fixtures so a numerical assertion catches a broken
//! depth interpolation or blend state rather than merely proving that a PNG
//! is non-empty.

use super::*;
use manifold_gpu::BufferUse;
use manifold_gpu::{
    GpuBinding, GpuLoadAction, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};
use manifold_node_engine::gpu::headless_readback::{
    encode_rgba8_png, readback_raw_halves, readback_srgb_rgba8,
};
use manifold_node_engine::scene::camera::Camera;

const W: u32 = 128;
const H: u32 = 128;

fn vertex(position: [f32; 3]) -> MeshVertex {
    MeshVertex {
        position,
        normal: [0.0, 0.0, 1.0],
        uv: [0.0, 0.0],
        tangent: [0.0, 0.0, 0.0, 0.0],
        ..bytemuck::Zeroable::zeroed()
    }
}

fn vertices_buffer(
    device: &manifold_gpu::GpuDevice,
    vertices: &[MeshVertex],
) -> manifold_gpu::GpuBuffer {
    let buffer = device.create_buffer(std::mem::size_of_val(vertices) as u64, BufferUse::Upload);
    // SAFETY: the shared buffer was allocated to exactly this byte length and
    // is not referenced by a submitted command until after this write.
    unsafe {
        buffer.write(0, bytemuck::cast_slice(vertices));
    }
    buffer
}

fn texture_f32(
    device: &manifold_gpu::GpuDevice,
    value: f32,
    label: &str,
) -> manifold_gpu::GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width: W,
        height: H,
        depth: 1,
        format: GpuTextureFormat::R32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD,
        label,
        mip_levels: 1,
    });
    let bytes = vec![value; (W * H) as usize];
    device.upload_texture(&texture, bytemuck::cast_slice(&bytes));
    texture
}

fn uniforms(
    camera: &Camera,
    tri_count: u32,
    vertex_count: u32,
    occlusion: u32,
    fragments_gain: f32,
) -> DiagramUniforms {
    let view_proj = camera.view_proj(W as f32 / H as f32);
    DiagramUniforms {
        view_proj,
        model: super::super::render_scene::model_matrix([0.0; 3], [0.0; 3], [1.0; 3]),
        viewport: [W as f32, H as f32, 0.0, 0.0],
        radius: 1.0,
        line_width: 4.0,
        geometry_hue: 0.52,
        path_hue: 0.13,
        grid: 0,
        fragments: 1,
        ghosts: 0,
        vectors: 0,
        trails: 0,
        tri_count,
        vertex_count,
        history_head: 0,
        history_len: 0,
        history_capacity: HISTORY_SAMPLES,
        history_stride: MAX_VERTICES,
        axes: 0,
        topology_mismatch: 0,
        occlusion,
        mode: 0,
        _depth_pad: 0,
        inv_view_proj: super::super::render_scene::mat4_inverse(view_proj).unwrap(),
        camera_pos_far: [camera.pos[0], camera.pos[1], camera.pos[2], camera.far],
        brightness: [1.0, fragments_gain, 1.0, 1.0],
        trails_brightness: 1.0,
        _brightness_pad0: 0.0,
        _brightness_pad1: 0.0,
        _brightness_pad2: 0.0,
        copy_count: 1,
        instances_wired: 0,
        instance_history_stride: 0,
        _instances_pad: 0,
        copy_inner: [1.0, 1.0, 1.0, 0.0],
    }
}

fn bindings<'a>(
    u: &'a DiagramUniforms,
    current: &'a manifold_gpu::GpuBuffer,
    scene: &'a manifold_gpu::GpuTexture,
    sampler: &'a manifold_gpu::GpuSampler,
) -> [GpuBinding<'a>; 8] {
    [
        GpuBinding::Bytes {
            binding: 0,
            data: bytemuck::bytes_of(u),
        },
        GpuBinding::Buffer {
            binding: 1,
            buffer: current,
            offset: 0,
        },
        GpuBinding::Buffer {
            binding: 2,
            buffer: current,
            offset: 0,
        },
        GpuBinding::Buffer {
            binding: 3,
            buffer: current,
            offset: 0,
        },
        GpuBinding::Buffer {
            binding: 4,
            buffer: current,
            offset: 0,
        },
        GpuBinding::Texture {
            binding: 8,
            texture: scene,
        },
        GpuBinding::Sampler {
            binding: 9,
            sampler,
        },
        // Unused unless instances_wired is set; an existing buffer keeps the
        // shared layout valid either way.
        GpuBinding::Buffer {
            binding: 10,
            buffer: current,
            offset: 0,
        },
    ]
}

fn rgba_signal(raw: &[u8], x: usize, y: usize) -> f32 {
    let mut signal = 0.0f32;
    for yy in y.saturating_sub(2)..=(y + 2).min(H as usize - 1) {
        for xx in x.saturating_sub(2)..=(x + 2).min(W as usize - 1) {
            let offset = (yy * W as usize + xx) * 8;
            for channel in 0..3 {
                signal = signal.max(
                    half::f16::from_bits(u16::from_le_bytes([
                        raw[offset + channel * 2],
                        raw[offset + channel * 2 + 1],
                    ]))
                    .to_f32(),
                );
            }
        }
    }
    signal
}

#[test]
fn sloping_depth_line_keeps_front_and_rejects_back_xray_is_unchanged() {
    let guard = manifold_gpu::testkit::test_device();
    let device = guard.arc();
    let camera = Camera::look_at(
        [0.0, 0.0, -1.0],
        [0.0, 0.0, 3.0],
        [0.0, 1.0, 0.0],
        1.0,
        0.1,
        10.0,
    );
    let current = vertices_buffer(
        &device,
        &[
            // The edge spans reversed-Z raw device depth from about 0.66 to 0.04 while
            // remaining inside the view, so a constant 0.5 occluder splits it.
            vertex([-0.06, -0.05, -0.85]),
            vertex([0.8, 0.55, 1.2]),
            vertex([-0.06, 0.05, -0.85]),
        ],
    );
    let scene = texture_f32(&device, 0.5, "mesh-diagram-constant-scene-depth");
    let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
        min_filter: manifold_gpu::GpuFilterMode::Nearest,
        mag_filter: manifold_gpu::GpuFilterMode::Nearest,
        ..Default::default()
    });
    let target = manifold_node_engine::gpu::render_target::RenderTarget::new(
        &device,
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        "mesh-diagram-depth-colour",
    );
    let xray_target = manifold_node_engine::gpu::render_target::RenderTarget::new(
        &device,
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        "mesh-diagram-xray",
    );
    let msaa = device.create_texture_msaa_memoryless(
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        MSAA_SAMPLE_COUNT,
        "mesh-diagram-depth-colour-msaa",
    );
    let xray_msaa = device.create_texture_msaa_memoryless(
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        MSAA_SAMPLE_COUNT,
        "mesh-diagram-xray-msaa",
    );
    let depth_pipeline = device.create_render_pipeline(
        &manifold_gpu::RenderPipelineDesc::new(
            SHADER,
            "vs_main",
            "fs_depth_color",
            "mesh-diagram-depth-colour",
        )
        .color(GpuTextureFormat::Rgba16Float, Some(DIAGRAM_BLEND))
        .samples(MSAA_SAMPLE_COUNT),
    );
    let xray_pipeline = device.create_render_pipeline(
        &manifold_gpu::RenderPipelineDesc::new(SHADER, "vs_main", "fs_main", "mesh-diagram-xray")
            .color(GpuTextureFormat::Rgba16Float, Some(DIAGRAM_BLEND))
            .samples(MSAA_SAMPLE_COUNT),
    );
    let u_depth = uniforms(&camera, 1, 3, 1, 1.0);
    let u_xray = uniforms(&camera, 1, 3, 0, 1.0);
    let b_depth = bindings(&u_depth, &current, &scene, &sampler);
    let b_xray = bindings(&u_xray, &current, &scene, &sampler);
    let mut encoder = device.create_encoder("mesh-diagram-depth-line-proof");
    encoder.draw(
        &manifold_gpu::DrawPass::msaa(&msaa, &target.texture, GpuLoadAction::Clear),
        &[manifold_gpu::Draw::new(&depth_pipeline, &b_depth, 18, 2)],
        "mesh-diagram-depth-line-proof",
    );
    encoder.draw(
        &manifold_gpu::DrawPass::msaa(&xray_msaa, &xray_target.texture, GpuLoadAction::Clear),
        &[manifold_gpu::Draw::new(&xray_pipeline, &b_xray[..6], 18, 2)],
        "mesh-diagram-xray-proof",
    );
    encoder.commit_and_wait().expect("GPU work completes");
    let depth_raw =
        readback_raw_halves(&device, &target.texture, W, H).expect("GPU readback completes");
    let xray_raw =
        readback_raw_halves(&device, &xray_target.texture, W, H).expect("GPU readback completes");
    let front = camera
        .project_to_pixel(
            [
                -0.06 * 0.98 + 0.8 * 0.02,
                -0.05 * 0.98 + 0.55 * 0.02,
                -0.85 * 0.98 + 1.2 * 0.02,
            ],
            W,
            H,
        )
        .unwrap();
    let back = camera
        .project_to_pixel(
            [
                -0.06 * 0.2 + 0.8 * 0.8,
                -0.05 * 0.2 + 0.55 * 0.8,
                -0.85 * 0.2 + 1.2 * 0.8,
            ],
            W,
            H,
        )
        .unwrap();
    let front_signal = rgba_signal(
        &depth_raw,
        front.px.round() as usize,
        front.py.round() as usize,
    );
    let back_signal = rgba_signal(
        &depth_raw,
        back.px.round() as usize,
        back.py.round() as usize,
    );
    assert!(
        front_signal > 0.1,
        "front half of sloping line disappeared: {front_signal}"
    );
    assert!(
        back_signal < 0.01,
        "back half crossed the 0.5 scene depth: {back_signal}"
    );
    assert!(
        rgba_signal(
            &xray_raw,
            back.px.round() as usize,
            back.py.round() as usize
        ) > 0.1,
        "X-ray path changed to depth rejection"
    );
    if let Ok(path) = std::env::var("MANIFOLD_MATH_DEPTH_DETAIL_PREVIEW") {
        let left =
            readback_srgb_rgba8(&device, &target.texture, W, H).expect("GPU readback completes");
        let right = readback_srgb_rgba8(&device, &xray_target.texture, W, H)
            .expect("GPU readback completes");
        let mut side_by_side = vec![0u8; (W * 2 * H * 4) as usize];
        for y in 0..H as usize {
            let row = y * W as usize * 4;
            side_by_side[y * (W as usize * 2) * 4..y * (W as usize * 2) * 4 + W as usize * 4]
                .copy_from_slice(&left[row..row + W as usize * 4]);
            side_by_side[y * (W as usize * 2) * 4 + W as usize * 4..(y + 1) * (W as usize * 2) * 4]
                .copy_from_slice(&right[row..row + W as usize * 4]);
        }
        std::fs::write(path, encode_rgba8_png(&side_by_side, W * 2, H)).unwrap();
    }
}

#[test]
fn math_view_axes_follow_representative_faces_and_toggle_without_hiding_outlines() {
    let guard = manifold_gpu::testkit::test_device();
    let device = guard.arc();
    let camera = Camera::look_at([0.0, 0.0, 5.0], [0.0; 3], [0.0, 1.0, 0.0], 1.0, 0.05, 100.0);
    let target = manifold_node_engine::gpu::render_target::RenderTarget::new(
        &device,
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        "math-axes-proof",
    );
    let msaa = device.create_texture_msaa_memoryless(
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        MSAA_SAMPLE_COUNT,
        "math-axes-proof",
    );
    let far = texture_f32(&device, 0.0, "math-axes-far-depth");
    let sampler = device.create_sampler(&GpuSamplerDesc::default());
    let mut preview = vec![0u8; (W * 2 * H * 2 * 4) as usize];
    // Two objects have their first face at the same world position. The
    // representative faces have distinct anchors after each object transform.
    for (object, offset) in [-0.8, 0.8].into_iter().enumerate() {
        let mut vertices = Vec::new();
        for face in 0..9 {
            let (x, y) = if face == 0 {
                (-offset, 1.8)
            } else {
                ((face % 3) as f32 * 0.5 - 0.5, (face / 3) as f32 * 0.8 - 0.8)
            };
            for delta in [[-0.1, -0.05, -0.03], [0.1, -0.05, -0.03], [0.0, 0.1, 0.06]] {
                vertices.push(vertex([x + delta[0], y + delta[1], delta[2]]));
            }
        }
        let buffer = vertices_buffer(&device, &vertices);
        for occlusion in [0, 1] {
            let pipeline = device.create_render_pipeline(
                &manifold_gpu::RenderPipelineDesc::new(
                    SHADER,
                    "vs_main",
                    if occlusion == 0 {
                        "fs_main"
                    } else {
                        "fs_depth_color"
                    },
                    "math-axes-proof",
                )
                .color(GpuTextureFormat::Rgba16Float, Some(DIAGRAM_BLEND))
                .samples(MSAA_SAMPLE_COUNT),
            );
            for axes in [1, 0] {
                let mut u = uniforms(&camera, 9, 27, occlusion, 1.0);
                u.axes = axes;
                u.radius = 2.0;
                u.line_width = 1.0;
                u.geometry_hue = 0.5;
                u.model = super::super::render_scene::model_matrix(
                    [offset, 0.0, 0.0],
                    [0.0; 3],
                    [1.0; 3],
                );
                let b = bindings(&u, &buffer, &far, &sampler);
                let mut encoder = device.create_encoder("math-axes-proof");
                encoder.draw(
                    &manifold_gpu::DrawPass::msaa(&msaa, &target.texture, GpuLoadAction::Clear),
                    &[manifold_gpu::Draw::new(
                        &pipeline,
                        if occlusion == 0 { &b[..6] } else { &b },
                        18,
                        1 + 9 * 4 + 3 + 3 * 3,
                    )],
                    "math-axes-proof",
                );
                encoder.commit_and_wait().expect("GPU work completes");
                let pixels = readback_srgb_rgba8(&device, &target.texture, W, H)
                    .expect("GPU readback completes");
                let red_near = |position| {
                    let p = camera.project_to_pixel(position, W, H).unwrap();
                    let (x, y) = (p.px.round() as i32, p.py.round() as i32);
                    ((y - 2)..=(y + 2)).any(|py| {
                        ((x - 2)..=(x + 2)).any(|px| {
                            if px < 0 || py < 0 || px >= W as i32 || py >= H as i32 {
                                return false;
                            }
                            let i = ((py as u32 * W + px as u32) * 4) as usize;
                            let c = &pixels[i..i + 3];
                            c[0] > 100
                                && u16::from(c[0]) > u16::from(c[1]) * 3 / 2
                                && u16::from(c[0]) > u16::from(c[2]) * 3 / 2
                        })
                    })
                };
                for y in [-0.8, 0.0, 0.8] {
                    assert_eq!(
                        red_near([offset + 0.15, y, 0.0]),
                        axes != 0,
                        "representative frame missing or Axes Off ignored: object={object}, y={y}, depth={occlusion}"
                    );
                }
                assert!(
                    !red_near([0.15, 1.8, 0.0]),
                    "axes still attached to the shared first face"
                );
                assert!(
                    pixels
                        .chunks_exact(4)
                        .filter(|c| c[1] > 100 && c[2] > 100 && c[0] < 50)
                        .count()
                        > 10,
                    "Axes toggle must retain cyan fragment outlines"
                );
                if occlusion == 1 {
                    for y in 0..H as usize {
                        let destination = ((object * H as usize + y) * (W * 2) as usize
                            + (1 - axes) as usize * W as usize)
                            * 4;
                        let source = y * W as usize * 4;
                        preview[destination..destination + W as usize * 4]
                            .copy_from_slice(&pixels[source..source + W as usize * 4]);
                    }
                }
            }
        }
    }
    if let Ok(path) = std::env::var("MANIFOLD_MATH_AXES_PREVIEW") {
        std::fs::write(path, encode_rgba8_png(&preview, W * 2, H * 2)).unwrap();
    }
}

/// Marks only grow: a face smaller than `MARK_SIZE` × radius grows about its
/// centre to that size, and a face already that big keeps its exact corners.
#[test]
fn math_view_marks_grow_small_faces_and_keep_big_ones() {
    let guard = manifold_gpu::testkit::test_device();
    let device = guard.arc();
    let camera = Camera::look_at([0.0, 0.0, 5.0], [0.0; 3], [0.0, 1.0, 0.0], 1.0, 0.05, 100.0);
    let target = manifold_node_engine::gpu::render_target::RenderTarget::new(
        &device,
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        "math-mark-size",
    );
    let msaa = device.create_texture_msaa_memoryless(
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        MSAA_SAMPLE_COUNT,
        "math-mark-size",
    );
    let pipeline = device.create_render_pipeline(
        &manifold_gpu::RenderPipelineDesc::new(SHADER, "vs_main", "fs_main", "math-mark-size")
            .color(GpuTextureFormat::Rgba16Float, Some(DIAGRAM_BLEND))
            .samples(MSAA_SAMPLE_COUNT),
    );
    let far = texture_f32(&device, 0.0, "math-mark-size-depth");
    let sampler = device.create_sampler(&GpuSamplerDesc::default());
    // Centred on the origin; its top corner is the farthest, 1.2 out.
    let face = [[-1.0, -0.6, 0.0], [1.0, -0.6, 0.0], [0.0, 1.2, 0.0]];
    let radius = 10.0;
    // The minimum mark as a scale of `face`: farthest corner at 0.8.
    let minimum = radius * MARK_SIZE / 1.2;
    let draw = |scale: f32| {
        let buffer = vertices_buffer(&device, &face.map(|p| vertex(p.map(|c| c * scale))));
        let u = DiagramUniforms {
            radius,
            geometry_hue: 0.5,
            ..uniforms(&camera, 1, 3, 0, 1.0)
        };
        let b = bindings(&u, &buffer, &far, &sampler);
        let mut encoder = device.create_encoder("math-mark-size");
        encoder.draw(
            &manifold_gpu::DrawPass::msaa(&msaa, &target.texture, GpuLoadAction::Clear),
            &[manifold_gpu::Draw::new(&pipeline, &b[..6], 18, 2)],
            "math-mark-size",
        );
        encoder.commit_and_wait().expect("GPU work completes");
        readback_srgb_rgba8(&device, &target.texture, W, H).expect("GPU readback completes")
    };
    let lit = |pixels: &[u8], point: [f32; 3]| {
        let p = camera.project_to_pixel(point, W, H).unwrap();
        let (x, y) = (p.px.round() as i32, p.py.round() as i32);
        ((y - 1)..=(y + 1)).any(|py| {
            ((x - 1)..=(x + 1)).any(|px| {
                let i = ((py as u32 * W + px as u32) * 4) as usize;
                pixels[i + 1] > 100 && pixels[i + 2] > 100
            })
        })
    };
    // Bottom-edge and left-edge midpoints of `face` drawn at `scale`.
    let edges = |scale: f32| [[0.0, -0.6 * scale, 0.0], [-0.5 * scale, 0.3 * scale, 0.0]];

    // A millimetre face grows to the minimum mark.
    let small = draw(0.001);
    for point in edges(minimum) {
        assert!(
            lit(&small, point),
            "a small face grows to the minimum mark: {point:?} dark"
        );
    }
    assert!(
        !lit(&small, [0.0; 3]),
        "the grown mark is an outline, not a dot"
    );

    // A face bigger than the minimum keeps its own corners.
    let big = draw(2.0);
    for point in edges(2.0) {
        assert!(
            lit(&big, point),
            "a big face draws at its own size: {point:?} dark"
        );
    }
    assert!(
        !lit(&big, edges(minimum)[0]),
        "a big face never shrinks to the minimum mark"
    );
}
