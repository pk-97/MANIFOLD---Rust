//! GPU proofs for the wet-surface look: the blend of albedo and roughness
//! against the closed form of Lekner and Dorf, read from a wetness field on
//! world tiles with one tile unheld, and that a point outside the field's
//! extent, over an unheld tile, or in a scene without the field is left alone.
use super::water_march_gpu_tests::shared_buffer;
use super::wetness::{WetUniforms, wet_uniforms};
use super::{FIELD_TILES_WGSL, FieldObject, model_matrix};
use crate::testkit::field_tiles::FieldTable;
use manifold_gpu::*;
use manifold_node_engine::scene::world_tiles::READ_SIDE;

/// The display cell in metres: a tile is 0.5 m.
const CELL: f32 = 0.0625;

/// The albedo the probe kernel passes in, one per channel, and its roughness.
const ALBEDO: [f32; 3] = [0.2, 0.5, 0.8];
const ROUGHNESS: f32 = 0.6;

/// Evaluates the look at the points in `probe_in`: (albedo rgb, roughness).
const WET_KERNEL: &str = "\n\
@group(0) @binding(91) var<storage, read> probe_in: array<vec4<f32>>;\n\
@group(0) @binding(90) var<storage, read_write> probe_out: array<vec4<f32>>;\n\
@compute @workgroup_size(64)\n\
fn cs_probe(@builtin(global_invocation_id) id: vec3<u32>) {\n\
    let i = id.x;\n\
    if i >= arrayLength(&probe_out) { return; }\n\
    let surface = look_wet_surface(probe_in[i].xyz, vec3<f32>(0.2, 0.5, 0.8), 0.6);\n\
    probe_out[i] = vec4<f32>(surface.albedo, surface.roughness);\n\
}\n";

/// Tiles -1 and 1 along x are held, tile 0 between them is not; y and z stay
/// in tile 0.
fn table() -> FieldTable {
    FieldTable::new(CELL, 2, &[[-1, 0, 0], [1, 0, 0]])
}

/// The wetness of the held nodes: linear in local y, `0.2 + 1.5 y`.
fn wetness_at(y: f64) -> f64 {
    0.2 + 1.5 * y
}

/// The model of the test water: moved by (2, 0, -1).
fn water_model() -> [[f32; 4]; 4] {
    model_matrix([2.0, 0.0, -1.0], [0.0; 3], [1.0; 3])
}

/// A world point at `local` in the water's space.
fn world_point(local: [f32; 3]) -> [f32; 4] {
    [2.0 + local[0], local[1], -1.0 + local[2], 0.0]
}

/// The look at `points` with `uniforms`, reading the field's wetness
/// `wetness` under `tiles` and `extent`.
fn probe(
    device: &GpuDevice,
    uniforms: &WetUniforms,
    wetness: &GpuBuffer,
    tiles: &GpuBuffer,
    extent: &GpuBuffer,
    points: &[[f32; 4]],
) -> Vec<[f32; 4]> {
    let wgsl = format!(
        "{READ_SIDE}{FIELD_TILES_WGSL}{}{WET_KERNEL}",
        include_str!("../shaders/look_wet_surface.wgsl")
    );
    let pipeline = device.create_compute_pipeline(&wgsl, "cs_probe", "wet probe");
    let flat: Vec<f32> = points.iter().flatten().copied().collect();
    let input = shared_buffer(device, &flat);
    let out = device.create_buffer_shared((points.len() * 16) as u64);
    let mut enc = device.create_encoder("wet probe");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes {
                binding: 68,
                data: bytemuck::bytes_of(uniforms),
            },
            GpuBinding::Buffer {
                binding: 69,
                buffer: wetness,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 70,
                buffer: tiles,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 71,
                buffer: extent,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 90,
                buffer: &out,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 91,
                buffer: &input,
                offset: 0,
            },
        ],
        [points.len().div_ceil(64) as u32, 1, 1],
        "wet probe",
    );
    enc.commit_and_wait_completed();
    let values: &[f32] = unsafe {
        std::slice::from_raw_parts(
            out.mapped_ptr().expect("shared").cast::<f32>(),
            points.len() * 4,
        )
    };
    values
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect()
}

/// The soaked albedo of `look_wet_surface.wgsl`, in f64.
fn soaked(albedo: f64) -> f64 {
    let (n, r_e) = (1.333f64, 0.066f64);
    let r_i = 1.0 - (1.0 - r_e) / (n * n);
    (1.0 - r_e) * (1.0 - r_i) * albedo / (1.0 - r_i * albedo)
}

#[test]
fn wetness_blends_albedo_and_smooths_roughness() {
    let device = manifold_gpu::testkit::test_device();
    let table = table();
    // Dry is the canonical wetness.
    let wetness_pool = table.pool(0.0, |p| wetness_at(f64::from(table.position(p)[1])) as f32);
    let wetness = shared_buffer(&device, &wetness_pool);
    let tiles = shared_buffer(&device, &table.words);
    let extent = shared_buffer(&device, &table.extent());
    let levelset = device.create_buffer_shared(16);
    let object = FieldObject {
        slot: 0,
        levelset: &levelset,
        velocity: None,
        foam: None,
        mist: None,
        wetness: Some(&wetness),
        tiles: &tiles,
        extent: &extent,
        pool_tiles: table.pool_tiles,
        cell_size: CELL,
        model: water_model(),
        prev_model: water_model(),
        ripple_detail: 1.0,
        uniforms: bytemuck::Zeroable::zeroed(),
    };
    let on = wet_uniforms(Some(&object)).expect("a real model inverts");
    let off = wet_uniforms(None).expect("off is not an error");

    // Over the two held tiles, nodes on both sides of the points.
    let inside = [
        [0.6, 0.1, 0.2],
        [0.85, 0.35, 0.3],
        [0.55, 0.25, 0.1],
        [-0.3, 0.15, 0.2],
        [-0.5, 0.3, 0.3],
    ];
    let mut points: Vec<[f32; 4]> = inside.iter().map(|&p| world_point(p)).collect();
    // In the unheld tile between them, inside the extent: the canonical
    // wetness is dry.
    let unheld = [[0.2, 0.2, 0.2], [0.0, 0.3, 0.1]];
    points.extend(unheld.iter().map(|&p| world_point(p)));
    // Outside the extent on three different sides.
    let outside = [
        world_point([1.2, 0.2, 0.2]),
        world_point([0.6, -0.2, 0.2]),
        world_point([0.6, 0.2, 0.6]),
    ];
    points.extend(outside);

    let wet = probe(&device, &on, &wetness, &tiles, &extent, &points);
    for (point, local) in wet.iter().zip(&inside) {
        let w = wetness_at(f64::from(local[1]));
        for channel in 0..3 {
            let a = f64::from(ALBEDO[channel]);
            let want = a + w * (soaked(a) - a);
            assert!(
                (f64::from(point[channel]) - want).abs() < 1e-5,
                "wetness {w}, channel {channel}: albedo {} against {want}",
                point[channel]
            );
        }
        let want = (f64::from(ROUGHNESS) * (1.0 - w)).max(0.01);
        assert!(
            (f64::from(point[3]) - want).abs() < 1e-5,
            "wetness {w}: roughness {} against {want}",
            point[3]
        );
    }
    // Wet enough to see: the higher point is darker and smoother than the lower.
    assert!(wet[1][0] < wet[0][0] && wet[1][3] < wet[0][3]);

    // A mesh over an unheld tile or outside the extent is dry, bit for bit.
    let unchanged = [ALBEDO[0], ALBEDO[1], ALBEDO[2], ROUGHNESS].map(f32::to_bits);
    for (i, point) in wet.iter().enumerate().skip(inside.len()) {
        assert_eq!(point.map(f32::to_bits), unchanged, "dry point {i}");
    }
    // And so is every point when the scene has no wetness field.
    let dry = probe(&device, &off, &wetness, &tiles, &extent, &points);
    for (i, point) in dry.iter().enumerate() {
        assert_eq!(
            point.map(f32::to_bits),
            unchanged,
            "point {i} with the look off"
        );
    }
}
