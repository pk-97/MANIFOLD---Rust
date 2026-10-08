use manifold_gpu::{
    GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuCompareFunction, GpuDepthStencilDesc,
    GpuEncoder, GpuLoadAction, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};

use manifold_node_engine::mesh::{InstanceTransform, MeshVertex};

use super::{SHADER, Uniforms};

const SIZE: u32 = 8;

fn identity() -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn mirrored() -> [[f32; 4]; 4] {
    [
        [-1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn vertex(position: [f32; 3]) -> MeshVertex {
    MeshVertex {
        position,
        normal: [0.0; 3],
        _pad0: 0.0,
        _pad1: 0.0,
        uv: [0.0; 2],
        _pad2: [0.0; 2],
        tangent: [0.0; 4],
        color: [1.0; 4],
    }
}

fn quad(out: &mut Vec<MeshVertex>, a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3]) {
    out.extend([
        vertex(a),
        vertex(b),
        vertex(c),
        vertex(a),
        vertex(c),
        vertex(d),
    ]);
}

fn cube(z0: f32, z1: f32) -> Vec<MeshVertex> {
    let lo = -0.8;
    let hi = 0.8;
    let mut out = Vec::with_capacity(36);

    // Outward-facing winding for all six faces. The shader's determinant
    // correction makes this same mesh valid under a mirrored model matrix.
    quad(
        &mut out,
        [lo, lo, z0],
        [lo, hi, z0],
        [hi, hi, z0],
        [hi, lo, z0],
    );
    quad(
        &mut out,
        [lo, lo, z1],
        [hi, lo, z1],
        [hi, hi, z1],
        [lo, hi, z1],
    );
    quad(
        &mut out,
        [lo, lo, z0],
        [lo, lo, z1],
        [lo, hi, z1],
        [lo, hi, z0],
    );
    quad(
        &mut out,
        [hi, lo, z0],
        [hi, hi, z0],
        [hi, hi, z1],
        [hi, lo, z1],
    );
    quad(
        &mut out,
        [lo, lo, z0],
        [hi, lo, z0],
        [hi, lo, z1],
        [lo, lo, z1],
    );
    quad(
        &mut out,
        [lo, hi, z0],
        [lo, hi, z1],
        [hi, hi, z1],
        [hi, hi, z0],
    );
    out
}

fn depth_occluder(z: f32) -> Vec<MeshVertex> {
    let lo = -0.8;
    let hi = 0.8;
    let mut out = Vec::with_capacity(6);
    quad(&mut out, [lo, lo, z], [hi, lo, z], [hi, hi, z], [lo, hi, z]);
    out
}

fn buffer<T: bytemuck::Pod>(
    device: &manifold_gpu::GpuDevice,
    data: &[T],
) -> manifold_gpu::GpuBuffer {
    let bytes = bytemuck::cast_slice(data);
    let buffer = device.create_buffer_shared(bytes.len() as u64);
    unsafe { buffer.write(0, bytes) };
    buffer
}

fn texture(device: &manifold_gpu::GpuDevice, format: GpuTextureFormat, label: &str) -> GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: SIZE,
        height: SIZE,
        depth: 1,
        format,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
        label,
        mip_levels: 1,
    })
}

fn blend() -> GpuBlendState {
    GpuBlendState {
        src_factor: GpuBlendFactor::One,
        dst_factor: GpuBlendFactor::One,
        operation: GpuBlendOp::Add,
        src_alpha_factor: GpuBlendFactor::One,
        dst_alpha_factor: GpuBlendFactor::One,
        alpha_operation: GpuBlendOp::Add,
    }
}

fn camera() -> manifold_node_engine::scene::camera::Camera {
    use manifold_node_engine::scene::camera::{Camera, CameraMode};
    let mut camera = Camera::look_at(
        [0.0, 0.0, -1.0],
        [0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        1.0,
        0.1,
        10.0,
    );
    camera.mode = CameraMode::Orthographic { half_height: 1.0 };
    camera
}

fn uniforms(model: [[f32; 4]; 4], multiplier: f32) -> Uniforms {
    uniforms_for_camera(camera(), model, multiplier)
}
fn uniforms_for_camera(
    camera: manifold_node_engine::scene::camera::Camera,
    model: [[f32; 4]; 4],
    multiplier: f32,
) -> Uniforms {
    Uniforms {
        view_proj: camera.view_proj(1.0),
        model,
        inverse_view_proj: super::mat4_inverse(camera.view_proj(1.0)).unwrap(),
        eye: [0.0, 0.0, 0.0, 1.0],
        parameters: [multiplier, 0.0, 0.0, 0.0],
        appearance: [1.0, 0.0, 0.0, 0.0],
    }
}

fn read_center(device: &manifold_gpu::GpuDevice, texture: &GpuTexture) -> f32 {
    let row_bytes = SIZE * 4;
    let readback = device.create_buffer_shared(u64::from(row_bytes * SIZE));
    let mut encoder = device.create_encoder("volume optics readback");
    encoder.copy_texture_to_buffer(texture, &readback, SIZE, SIZE, row_bytes);
    encoder.commit_and_wait_completed();
    let ptr = readback
        .mapped_ptr()
        .expect("shared readback buffer must be CPU mapped");
    let offset = ((SIZE / 2) * SIZE + SIZE / 2) as usize * 4;
    unsafe { ptr.add(offset).cast::<f32>().read_unaligned() }
}

fn render_path(
    device: &manifold_gpu::GpuDevice,
    vertices: &[MeshVertex],
    model: [[f32; 4]; 4],
    multiplier: f32,
    occluder: Option<&[MeshVertex]>,
) -> f32 {
    render_path_with_camera(device, vertices, model, multiplier, occluder, camera())
}

fn render_path_with_camera(
    device: &manifold_gpu::GpuDevice,
    vertices: &[MeshVertex],
    model: [[f32; 4]; 4],
    multiplier: f32,
    occluder: Option<&[MeshVertex]>,
    camera: manifold_node_engine::scene::camera::Camera,
) -> f32 {
    let path = texture(
        device,
        GpuTextureFormat::R32Float,
        "volume optics path test",
    );
    let depth = texture(
        device,
        GpuTextureFormat::Depth32Float,
        "volume optics depth test",
    );
    let pipeline = device.create_render_pipeline(
        SHADER,
        "vs_main",
        "fs_path",
        GpuTextureFormat::R32Float,
        Some(blend()),
        "volume optics path proof",
    );
    let nearest = device.create_render_pipeline_depth_only(
        SHADER,
        "vs_main",
        "fs_nearest",
        GpuTextureFormat::Depth32Float,
        "volume optics depth proof",
    );
    let depth_state = device.create_depth_stencil_state(&GpuDepthStencilDesc {
        compare: GpuCompareFunction::Greater,
        write_enabled: true,
    });
    let instance = buffer(
        device,
        &[InstanceTransform {
            pos_scale: [0.0, 0.0, 0.0, 1.0],
            rot_pad: [0.0; 4],
        }],
    );
    let mesh = buffer(device, vertices);
    let u = uniforms_for_camera(camera, model, multiplier);
    fn bindings<'a>(
        mesh: &'a manifold_gpu::GpuBuffer,
        u: &'a Uniforms,
        instance: &'a manifold_gpu::GpuBuffer,
        depth: &'a GpuTexture,
    ) -> [GpuBinding<'a>; 5] {
        [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(u),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: mesh,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: instance,
                offset: 0,
            },
            GpuBinding::Texture {
                binding: 3,
                texture: depth,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: mesh,
                offset: 0,
            },
        ]
    }

    // Establish an opaque depth map with the production nearest-surface
    // pipeline. An empty batch is the far-plane clear; an optional quad is
    // the opaque occluder used by the clipping proof.
    let mut depth_encoder = device.create_encoder("volume optics depth prepass");
    if let Some(occluder_vertices) = occluder {
        let occluder_buffer = buffer(device, occluder_vertices);
        let occluder_u = uniforms_for_camera(camera, identity(), 1.0);
        let occluder_bindings = bindings(&occluder_buffer, &occluder_u, &instance, &depth);
        let draw = GpuEncoder::depth_msaa_draw(
            &nearest,
            &occluder_bindings,
            occluder_vertices.len() as u32,
            1,
        );
        depth_encoder.draw_instanced_depth_only_batch(
            &depth,
            &depth_state,
            std::slice::from_ref(&draw),
            "volume optics opaque depth",
        );
    } else {
        depth_encoder.draw_instanced_depth_only_batch(
            &depth,
            &depth_state,
            &[],
            "volume optics far depth",
        );
    }
    depth_encoder.commit_and_wait_completed();

    let bindings = bindings(&mesh, &u, &instance, &depth);
    let mut encoder = device.create_encoder("volume optics path proof");
    encoder.draw_instanced(
        &pipeline,
        &path,
        &bindings,
        manifold_gpu::DrawCount::Direct { vertices: vertices.len() as u32, instances: 1 },
        GpuLoadAction::Clear,
        "volume optics path",
    );
    encoder.commit_and_wait_completed();
    read_center(device, &path)
}

fn render_nearest(device: &manifold_gpu::GpuDevice, vertices: &[MeshVertex]) -> f32 {
    let depth = texture(
        device,
        GpuTextureFormat::Depth32Float,
        "volume nearest proof",
    );
    let nearest = device.create_render_pipeline_depth_only(
        SHADER,
        "vs_main",
        "fs_nearest",
        GpuTextureFormat::Depth32Float,
        "volume nearest proof",
    );
    let state = device.create_depth_stencil_state(&GpuDepthStencilDesc {
        compare: GpuCompareFunction::Greater,
        write_enabled: true,
    });
    let mesh = buffer(device, vertices);
    let instances = buffer(
        device,
        &[InstanceTransform {
            pos_scale: [0.0, 0.0, 0.0, 1.0],
            rot_pad: [0.0; 4],
        }],
    );
    let u = uniforms(identity(), 1.0);
    let bindings = [
        GpuBinding::Bytes {
            binding: 0,
            data: bytemuck::bytes_of(&u),
        },
        GpuBinding::Buffer {
            binding: 1,
            buffer: &mesh,
            offset: 0,
        },
        GpuBinding::Buffer {
            binding: 2,
            buffer: &instances,
            offset: 0,
        },
        GpuBinding::Texture {
            binding: 3,
            texture: &depth,
        },
        GpuBinding::Buffer {
            binding: 4,
            buffer: &mesh,
            offset: 0,
        },
    ];
    let draw = GpuEncoder::depth_msaa_draw(&nearest, &bindings, vertices.len() as u32, 1);
    let mut encoder = device.create_encoder("volume nearest proof");
    encoder.draw_instanced_depth_only_batch(
        &depth,
        &state,
        std::slice::from_ref(&draw),
        "volume nearest surface",
    );
    encoder.commit_and_wait_completed();
    read_center(device, &depth)
}

#[test]
fn production_volume_optics_pipelines_compile() {
    let device = manifold_gpu::testkit::test_device();
    let _path = device.create_render_pipeline(
        SHADER,
        "vs_main",
        "fs_path",
        GpuTextureFormat::R32Float,
        Some(blend()),
        "volume optics compile path",
    );
    let _nearest = device.create_render_pipeline_depth_only(
        SHADER,
        "vs_main",
        "fs_nearest",
        GpuTextureFormat::Depth32Float,
        "volume optics compile nearest",
    );
}

#[test]
fn volume_path_matches_one_slab_thickness() {
    let value = render_path(
        &manifold_gpu::testkit::test_device(),
        &cube(0.2, 0.6),
        identity(),
        1.0,
        None,
    );
    assert!((value - 0.4).abs() < 0.00001, "path={value}");
}

#[test]
fn volume_path_excludes_air_between_separated_slabs() {
    let mut vertices = cube(0.2, 0.4);
    vertices.extend(cube(0.7, 0.9));
    let value = render_path(&manifold_gpu::testkit::test_device(), &vertices, identity(), 1.0, None);
    assert!((value - 0.4).abs() < 0.00001, "path={value}");
}

#[test]
fn opaque_depth_truncates_second_slab() {
    let value = render_path(
        &manifold_gpu::testkit::test_device(),
        &cube(0.2, 0.8),
        identity(),
        1.0,
        Some(&depth_occluder(0.35)),
    );
    assert!((value - 0.15).abs() < 0.00001, "path={value}");
}

#[test]
fn mirrored_model_preserves_signed_path() {
    let device = manifold_gpu::testkit::test_device();
    let vertices = cube(0.2, 0.6);
    let ordinary = render_path(&device, &vertices, identity(), 1.0, None);
    let mirrored = render_path(&device, &vertices, mirrored(), 1.0, None);
    assert!(ordinary > 0.1, "ordinary path={ordinary}");
    assert!(
        (ordinary - mirrored).abs() < 0.00001,
        "ordinary={ordinary}, mirrored={mirrored}"
    );
}

#[test]
fn density_multiplier_scales_path() {
    let device = manifold_gpu::testkit::test_device();
    let vertices = cube(0.2, 0.6);
    let base = render_path(&device, &vertices, identity(), 1.0, None);
    let doubled = render_path(&device, &vertices, identity(), 2.0, None);
    assert!(
        (doubled - base * 2.0).abs() < 0.00001,
        "base={base}, doubled={doubled}"
    );
}

#[test]
fn nearest_depth_matches_front_surface() {
    let nearest = render_nearest(&manifold_gpu::testkit::test_device(), &cube(0.2, 0.6));
    let expected = camera()
        .project_to_pixel([0.0, 0.0, 0.2], SIZE, SIZE)
        .unwrap()
        .depth;
    assert!(
        (nearest - expected).abs() < 0.00001,
        "nearest={nearest}, expected={expected}"
    );
}

#[test]
fn perspective_camera_measures_positive_world_space_path() {
    let mut camera = camera();
    camera.mode = manifold_node_engine::scene::camera::CameraMode::Perspective { fov_y: 1.0 };
    let value = render_path_with_camera(
        &manifold_gpu::testkit::test_device(),
        &cube(0.2, 0.6),
        identity(),
        1.0,
        None,
        camera,
    );
    let ray_xy = (1.0 / SIZE as f32) * (0.5_f32).tan();
    let expected = 0.4 * (1.0 + 2.0 * ray_xy * ray_xy).sqrt();
    assert!(
        (value - expected).abs() < 0.00001,
        "path={value}, expected={expected}"
    );
}
