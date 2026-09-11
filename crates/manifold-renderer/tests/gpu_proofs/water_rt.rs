//! Native secondary water rays against analytic parallel interfaces.
//! Black environment and black-albedo emissive panels isolate intersection
//! geometry, Snell refraction, Beer absorption, and direct Sun visibility.
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use manifold_gpu::raytrace::{
    GiMaterial, MetalShadowRayTracer, RtCasterParams, RtObjectGeometry, SVT_SLOT_NONE,
    ShadowRayParams, ShadowRayTracer, WaterRayParams, ensure_normal_sources,
};
use manifold_gpu::{
    GpuBinding, GpuBuffer, GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension,
    GpuTextureFormat, GpuTextureUsage,
};

use crate::harness;

const WIDTH: u32 = 4;
const HEIGHT: u32 = 1;
const VOLUME: u32 = 64;
const NEAR: f32 = 0.1;
const FAR: f32 = 10.0;
const TAN_HALF_FOV: f32 = 0.5;
const IOR: f32 = 4.0 / 3.0;
const ABSORPTION: [f32; 3] = [0.2, 0.5, 0.9];
const INSIDE_EMISSION: [f32; 3] = [2.0, 1.0, 0.5];
const REFLECTED_EMISSION: [f32; 3] = [0.25, 0.5, 1.0];
const EXIT_EMISSION: [f32; 3] = [1.5, 0.75, 0.25];
const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

// One fixture-only dispatch initializes the scalar slab. Voxel centers at
// z=-1 +/- 1/32 and z=0 +/- 1/32 straddle its two faces; trilinear sampling
// reaches iso=.5 exactly at z=-1 and z=0. This is not the ray-march oracle.
const SLAB: &str = r#"
@group(0) @binding(0) var density: texture_storage_3d<rgba16float, write>;
@compute @workgroup_size(4,4,4)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims=textureDimensions(density);
    if(any(id>=dims)){return;}
    let z=-2.0+(f32(id.z)+0.5)*4.0/f32(dims.z);
    let value=select(0.0,1.0,z > -1.0 && z < 0.0);
    textureStore(density,vec3<i32>(id),vec4<f32>(value,0.0,0.0,0.0));
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
}

struct Panel {
    corners: [[f32; 3]; 4],
    normal: [f32; 3],
    emission: [f32; 3],
    casts_shadow: bool,
}

impl Panel {
    fn xy(z: f32, x: [f32; 2], y: [f32; 2], emission: [f32; 3]) -> Self {
        Self {
            corners: [
                [x[0], y[0], z],
                [x[1], y[0], z],
                [x[1], y[1], z],
                [x[0], y[1], z],
            ],
            normal: [0.0, 0.0, 1.0],
            emission,
            casts_shadow: false,
        }
    }

    fn vertices(&self) -> [Vertex; 6] {
        let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        [0, 1, 2, 0, 2, 3].map(|i| Vertex {
            position: self.corners[i],
            normal: self.normal,
            uv: uvs[i],
        })
    }
}

fn buffer<T: Copy>(device: &GpuDevice, values: &[T]) -> GpuBuffer {
    let bytes = std::mem::size_of_val(values);
    let buffer = device.create_buffer_shared((bytes as u64).max(16));
    unsafe {
        std::ptr::copy_nonoverlapping(
            values.as_ptr().cast::<u8>(),
            buffer.mapped_ptr().unwrap(),
            bytes,
        );
    }
    buffer
}

fn upload_2d(
    device: &GpuDevice,
    size: [u32; 2],
    format: GpuTextureFormat,
    values: &[f32],
) -> GpuTexture {
    assert_eq!(
        values.len() * 4,
        size[0] as usize * size[1] as usize * format.bytes_per_pixel() as usize
    );
    let texture = device.create_texture(&GpuTextureDesc {
        width: size[0],
        height: size[1],
        depth: 1,
        format,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
        label: "water-rt-input",
        mip_levels: 1,
    });
    device.upload_texture(&texture, bytemuck::cast_slice(values));
    texture
}

fn output_texture(device: &GpuDevice) -> GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: WIDTH,
        height: HEIGHT,
        depth: 1,
        format: GpuTextureFormat::Rgba32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
        label: "water-rt-output",
        mip_levels: 1,
    })
}

// Right-handed camera at (0,1,1), looking along -Z; water entry at world Z=0.
// Aspect is explicitly one, so each ray's x/-z slope is NDC.x*tanHalfFov.
fn incident_slope(pixel: usize) -> f64 {
    ((pixel as f64 + 0.5) / f64::from(WIDTH) * 2.0 - 1.0) * f64::from(TAN_HALF_FOV)
}

fn snell(pixel: usize) -> (f64, f64) {
    let slope = incident_slope(pixel);
    let sin_incident = slope / (1.0 + slope * slope).sqrt();
    let sin_transmitted = sin_incident / f64::from(IOR);
    let cos_transmitted = (1.0 - sin_transmitted * sin_transmitted).sqrt();
    (sin_transmitted / cos_transmitted, cos_transmitted)
}

fn raw_depth(eye_depth: f64) -> f32 {
    let near = f64::from(NEAR);
    let far = f64::from(FAR);
    (far * (eye_depth - near) / (eye_depth * (far - near))) as f32
}

#[derive(Clone, Copy)]
enum Scene {
    InsideHit,
    ExitPatches,
}

fn panels(scene: Scene) -> Vec<Panel> {
    match scene {
        Scene::InsideHit => vec![
            Panel::xy(-0.5, [-1.0, 1.0], [0.5, 1.5], INSIDE_EMISSION),
            // Entirely behind the camera (world z=1), hence absent from its
            // primary image. Reflected rays hit at x=3*incident_slope.
            Panel::xy(2.0, [-1.5, 1.5], [0.75, 1.25], REFLECTED_EMISSION),
            Panel {
                corners: [
                    [-1.5, 1.5, -0.25],
                    [0.0, 1.5, -0.25],
                    [0.0, 1.5, 0.25],
                    [-1.5, 1.5, 0.25],
                ],
                normal: [0.0, -1.0, 0.0],
                emission: [0.0; 3],
                casts_shadow: true,
            },
        ],
        Scene::ExitPatches => (0..WIDTH as usize)
            .map(|pixel| {
                let slope = incident_slope(pixel);
                let (inside_slope, _) = snell(pixel);
                // Entry x=slope; 1m in water, then 1m in air. Parallel surfaces
                // require the outgoing ray to recover the incoming slope.
                let x = (2.0 * slope + inside_slope) as f32;
                Panel::xy(-2.0, [x - 0.01, x + 0.01], [0.95, 1.05], EXIT_EMISSION)
            })
            .collect(),
    }
}

struct Samples {
    reflection: [[f32; 4]; WIDTH as usize],
    transmission: [[f32; 4]; WIDTH as usize],
}

fn run(scene: Scene, reject_coverage: bool) -> Samples {
    let device = harness::shared().device.as_ref();
    let panels = panels(scene);
    let vertices: Vec<_> = panels
        .iter()
        .map(|p| buffer(device, &p.vertices()))
        .collect();
    let objects: Vec<_> = panels
        .iter()
        .zip(&vertices)
        .map(|(panel, vertices)| RtObjectGeometry {
            vertex_buffer: vertices,
            vertex_stride: std::mem::size_of::<Vertex>() as u32,
            vertex_offset: 0,
            index_buffer: None,
            triangle_count: 2,
            transform: IDENTITY,
            normal_offset: 12,
            uv_offset: 24,
            alpha_mask: false,
            translucent: false,
            alpha_cutoff: 0.5,
            base_color_texture: None,
            mr_texture: None,
            normal_texture: None,
            emissive_texture: None,
            emissive_uv_m: [1.0, 0.0, 0.0, 1.0],
            emissive_uv_t: [0.0; 2],
            cast_shadows: panel.casts_shadow,
            instances_addr: 0,
            instances_buffer: None,
            instance_slots: 1,
        })
        .collect();
    let materials: Vec<_> = panels
        .iter()
        .map(|p| GiMaterial::new([0.0; 3], p.emission, [0.0, 1.0, 0.0, 0.0], [0.0; 4]))
        .collect();
    let tracer = MetalShadowRayTracer::new(device);
    let accel = tracer.build_accel(device, &objects, &materials);
    let started = Instant::now();
    while !accel.ready.load(Ordering::Acquire) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "water proof TLAS build did not complete"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut sources = None;
    let mut source_capacity = 0;
    let textures = ensure_normal_sources(&mut sources, &mut source_capacity, device, &objects);
    let sources = sources.expect("normal source table");
    let materials = buffer(device, &materials);
    let mut depths = [raw_depth(1.0); WIDTH as usize];
    // camera-forward normal components: world +Z is negative forward.
    let mut normals: [[f32; 4]; WIDTH as usize] = [[0.0, 0.0, -1.0, 1.0]; WIDTH as usize];
    let mut opaque = [1.0; WIDTH as usize];
    if reject_coverage {
        normals[0][3] = 0.0;
        opaque[1] = raw_depth(0.5);
        depths[2] = 1.0;
    }
    let size = [WIDTH, HEIGHT];
    let depth = upload_2d(device, size, GpuTextureFormat::R32Float, &depths);
    let normals = upload_2d(
        device,
        size,
        GpuTextureFormat::Rgba32Float,
        bytemuck::cast_slice(&normals),
    );
    let opaque = upload_2d(device, size, GpuTextureFormat::Depth32Float, &opaque);
    let environment = upload_2d(
        device,
        [1, 1],
        GpuTextureFormat::Rgba32Float,
        &[0.0, 0.0, 0.0, 1.0],
    );
    let density = device.create_texture(&GpuTextureDesc {
        width: VOLUME,
        height: VOLUME,
        depth: VOLUME,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D3,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::SHADER_READ,
        label: "water-rt-analytic-slab",
        mip_levels: 1,
    });
    let slab = device.create_compute_pipeline(SLAB, "cs_main", "water-rt-slab");
    let reflection = output_texture(device);
    let transmission = output_texture(device);
    let read_reflection = device.create_buffer_shared(u64::from(WIDTH * HEIGHT * 16));
    let read_transmission = device.create_buffer_shared(u64::from(WIDTH * HEIGHT * 16));
    let casters = [RtCasterParams::new([0.0, 1.0, 0.0], 0.0, [0.0; 3], 0)];
    let params = ShadowRayParams::new(
        &casters,
        1,
        0,
        size,
        size,
        0.0,
        0,
        0,
        [0.0, 1.0, 1.0],
        IDENTITY,
        1,
        0.6,
        0.1,
        0.0,
        0,
        0.0,
        SVT_SLOT_NONE,
    );
    let params_buffer = device.create_buffer_shared(std::mem::size_of::<ShadowRayParams>() as u64);
    let mut inv_view = IDENTITY;
    inv_view[3] = [0.0, 1.0, 1.0, 1.0];
    let attenuation = match scene {
        Scene::InsideHit => ABSORPTION,
        Scene::ExitPatches => [1.0; 3],
    };
    let water = WaterRayParams {
        inv_view,
        projection: [NEAR, FAR, TAN_HALF_FOV, 1.0],
        material: [IOR, 0.0, 1.0, 0.5],
        attenuation: [attenuation[0], attenuation[1], attenuation[2], 0.0],
        screen: [WIDTH, HEIGHT, 0, 0],
    };
    let water_buffer = device.create_buffer_shared(std::mem::size_of::<WaterRayParams>() as u64);
    harness::retry_on_gpu_commit_error(|| {
        let mut encoder = device.create_encoder("water-rt-proof");
        encoder.dispatch_compute(
            &slab,
            &[GpuBinding::Texture {
                binding: 0,
                texture: &density,
            }],
            [VOLUME.div_ceil(4); 3],
            "water-rt-slab",
        );
        tracer.dispatch_water_rays(
            &mut encoder,
            device,
            &accel,
            &params,
            &params_buffer,
            &water,
            &water_buffer,
            &materials,
            &sources,
            &objects,
            &textures,
            &depth,
            &normals,
            &density,
            &opaque,
            &environment,
            &reflection,
            &transmission,
        );
        encoder.copy_texture_to_buffer(&reflection, &read_reflection, WIDTH, HEIGHT, WIDTH * 16);
        encoder.copy_texture_to_buffer(
            &transmission,
            &read_transmission,
            WIDTH,
            HEIGHT,
            WIDTH * 16,
        );
        encoder.commit_and_wait_completed();
    });
    let read = |buffer: &GpuBuffer| unsafe {
        *buffer
            .mapped_ptr()
            .unwrap()
            .cast::<[[f32; 4]; WIDTH as usize]>()
    };
    let result = Samples {
        reflection: read(&read_reflection),
        transmission: read(&read_transmission),
    };
    assert!(
        result
            .reflection
            .iter()
            .chain(&result.transmission)
            .flatten()
            .all(|v| v.is_finite())
    );
    result
}

#[test]
fn water_rt_snell_beer_inside_hit_offscreen_reflection_and_sun_visibility() {
    let samples = run(Scene::InsideHit, false);
    for pixel in 0..WIDTH as usize {
        let (_, cos_transmitted) = snell(pixel);
        let distance = 0.5 / cos_transmitted;
        // The opaque plane lies inside the liquid: no exit-interface or
        // Fresnel mixture can obscure the primary optical-path oracle.
        // 0.5mm covers the documented voxel/1024 bias (61um) and f32 depth.
        assert!(
            (f64::from(samples.transmission[pixel][3]) - distance).abs() < 0.0005,
            "water path pixel {pixel}: {} != {distance}",
            samples.transmission[pixel][3]
        );
        for (channel, &emission) in INSIDE_EMISSION.iter().enumerate() {
            let sigma = -f64::from(ABSORPTION[channel]).ln();
            let expected = f64::from(emission) * (-sigma * distance).exp();
            assert!(
                (f64::from(samples.transmission[pixel][channel]) - expected).abs() < 0.001,
                "Beer pixel {pixel}/{channel}: {} != {expected}",
                samples.transmission[pixel][channel]
            );
            assert!(
                (samples.reflection[pixel][channel] - REFLECTED_EMISSION[channel]).abs() < 0.0001,
                "offscreen panel pixel {pixel}/{channel}"
            );
        }
        assert_eq!(
            samples.reflection[pixel][3],
            if pixel < 2 { 0.0 } else { 1.0 },
            "hard Sun visibility {pixel}"
        );
    }
    let slope = incident_slope(0);
    let unrefracted_path = 0.5 * (1.0 + slope * slope).sqrt();
    let (_, cos_transmitted) = snell(0);
    assert!(
        (unrefracted_path - 0.5 / cos_transmitted).abs() > 0.01,
        "fixture must distinguish Snell refraction"
    );
}

#[test]
fn water_rt_parallel_interfaces_recover_incident_direction() {
    let samples = run(Scene::ExitPatches, false);
    for pixel in 0..WIDTH as usize {
        let (inside_slope, cos_transmitted) = snell(pixel);
        let slope = incident_slope(pixel);
        // An incorrect ray which stays at its underwater angle misses each
        // 2cm patch by at least 3cm, far beyond the bounded bisection error.
        assert!((slope - inside_slope).abs() > 0.03);
        let f0 = ((f64::from(IOR) - 1.0) / (f64::from(IOR) + 1.0)).powi(2);
        let exit_fresnel = f0 + (1.0 - f0) * (1.0 - cos_transmitted).powi(5);
        for (channel, &emission) in EXIT_EMISSION.iter().enumerate() {
            let expected = f64::from(emission) * (1.0 - exit_fresnel);
            assert!(
                (f64::from(samples.transmission[pixel][channel]) - expected).abs() < 0.001,
                "two-interface patch pixel {pixel}/{channel}: {} != {expected}",
                samples.transmission[pixel][channel]
            );
            assert_eq!(
                samples.reflection[pixel][channel], 0.0,
                "reflection sees black environment"
            );
        }
    }
}

#[test]
fn water_rt_foreground_and_missing_water_coverage_zero_both_outputs() {
    let samples = run(Scene::InsideHit, true);
    for pixel in 0..3 {
        assert_eq!(
            samples.reflection[pixel], [0.0; 4],
            "rejected reflection pixel {pixel}"
        );
        assert_eq!(
            samples.transmission[pixel], [0.0; 4],
            "rejected transmission pixel {pixel}"
        );
    }
    assert!(
        samples.reflection[3][2] > 0.9,
        "control pixel must retain reflection"
    );
    assert!(
        samples.transmission[3][0] > 0.5,
        "control pixel must retain transmission"
    );
}
